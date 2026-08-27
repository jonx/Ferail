//! Native batched directory enumeration shared by recursive tools.
//!
//! The portable implementation uses `read_dir` plus its cached `DirEntry`
//! metadata. macOS uses `getattrlistbulk`; Windows uses
//! `FileIdBothDirectoryInfo`. Both return directory batches and stat-like
//! attributes without opening every child. Callers supply policy (descent,
//! package handling, filtering, aggregation); this module only enumerates.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::SystemTime;

use ferail_core::EntryKind;

#[derive(Debug)]
pub(crate) struct DirectoryEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub allocated: u64,
    pub mtime: Option<SystemTime>,
    pub created: Option<SystemTime>,
    /// `(device, file id/inode, hard-link count)` when the platform can return
    /// it without opening every file.
    pub identity: Option<(u64, u64, u64)>,
    /// Native stat flags. On macOS this carries `UF_HIDDEN`, immutable,
    /// dataless and firmlink bits without another metadata call.
    pub flags: u32,
    pub hidden: bool,
    pub locked: bool,
    /// True when the directory entry itself is a mounted filesystem root.
    pub mount_point: bool,
}

impl DirectoryEntry {
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }

    pub fn is_symlink(&self) -> bool {
        self.kind == EntryKind::Symlink
    }

    fn from_metadata(path: PathBuf, name: String, metadata: &fs::Metadata) -> Self {
        let ft = metadata.file_type();
        let kind = if ft.is_dir() {
            EntryKind::Directory
        } else if ft.is_symlink() {
            EntryKind::Symlink
        } else {
            EntryKind::File
        };
        let flags = metadata_flags(metadata);
        let hidden = hidden_from_flags(&name, flags);
        let locked = locked_from_flags(flags);
        Self {
            path,
            name,
            kind,
            size: metadata.len(),
            allocated: allocated_size(metadata),
            mtime: metadata.modified().ok(),
            created: metadata.created().ok(),
            identity: file_identity(metadata),
            flags,
            hidden,
            locked,
            mount_point: false,
        }
    }
}

/// Enumerate one directory. `visitor` returns false to stop immediately.
pub(crate) fn for_each(
    path: &Path,
    cancel: &AtomicBool,
    visitor: impl FnMut(DirectoryEntry) -> bool,
) -> std::io::Result<()> {
    NativeDirectoryReader::new().for_each(path, cancel, visitor)
}

#[cfg(any(target_os = "macos", windows))]
const NATIVE_BUFFER_BYTES: usize = 256 * 1024;

/// One reusable native query buffer per enumeration worker. Directory trees
/// commonly contain far more folders than files-per-folder; allocating and
/// zeroing 256 KiB for every small directory otherwise dominates a hot scan.
struct NativeDirectoryReader {
    #[cfg(any(target_os = "macos", windows))]
    buffer: Vec<u8>,
}

impl NativeDirectoryReader {
    fn new() -> Self {
        Self {
            #[cfg(any(target_os = "macos", windows))]
            buffer: vec![0; NATIVE_BUFFER_BYTES],
        }
    }

    fn for_each(
        &mut self,
        path: &Path,
        cancel: &AtomicBool,
        visitor: impl FnMut(DirectoryEntry) -> bool,
    ) -> std::io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let mut visitor = visitor;
            match macos::for_each_bulk(path, cancel, &mut self.buffer, &mut visitor) {
                Ok(_) => return Ok(()),
                // Unsupported filesystems and an initial kernel rejection use the
                // portable path. Once entries were emitted, preserve the normal
                // partial-directory contract rather than emitting duplicates.
                Err(error) if error.entries_emitted == 0 => {}
                Err(error) => return Err(error.source),
            }
            portable_for_each(path, cancel, visitor)
        }
        #[cfg(target_os = "windows")]
        {
            let mut visitor = visitor;
            match windows::for_each_bulk(path, cancel, &mut self.buffer, &mut visitor) {
                Ok(()) => return Ok(()),
                Err(error) if error.entries_emitted == 0 => {}
                Err(error) => return Err(error.source),
            }
            portable_for_each(path, cancel, visitor)
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            portable_for_each(path, cancel, visitor)
        }
    }
}

const RECURSIVE_BATCH_SIZE: usize = 256;

struct WorkQueue<C> {
    items: VecDeque<Arc<C>>,
    stopped: bool,
}

enum WalkEvent<C> {
    Started(Arc<C>),
    Batch(Arc<C>, Vec<DirectoryEntry>),
    Done(Arc<C>, Option<std::io::Error>),
}

pub(crate) enum DirectoryWalkEvent<'a, C> {
    Started(&'a C),
    Batch(&'a C, Vec<DirectoryEntry>),
    Done(&'a C, Option<&'a std::io::Error>),
}

/// Walk a caller-defined directory graph with bounded native enumeration.
///
/// Workers do I/O only. Every callback and every decision to descend runs on
/// the coordinator thread, so callers can keep ordinary mutable sets,
/// counters, aggregation buffers, and identity allocators without locks.
/// Result batches are strictly bounded; queued contexts contain only folders
/// already discovered but not yet opened. No worker materializes a whole
/// directory at once.
pub(crate) fn walk<C: DirectoryContext + Send + Sync>(
    initial: C,
    cancel: &AtomicBool,
    workers: usize,
    mut visitor: impl FnMut(DirectoryWalkEvent<'_, C>) -> Vec<C>,
) {
    let workers = workers.max(1);
    if workers == 1 {
        let mut reader = NativeDirectoryReader::new();
        let mut queue = VecDeque::from([initial]);
        while let Some(context) = queue.pop_front() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let _ = visitor(DirectoryWalkEvent::Started(&context));
            let mut batch = Vec::with_capacity(RECURSIVE_BATCH_SIZE);
            let result = reader.for_each(context_path(&context), cancel, |entry| {
                batch.push(entry);
                if batch.len() >= RECURSIVE_BATCH_SIZE {
                    queue.extend(visitor(DirectoryWalkEvent::Batch(
                        &context,
                        std::mem::take(&mut batch),
                    )));
                    batch.reserve(RECURSIVE_BATCH_SIZE);
                }
                true
            });
            if !batch.is_empty() {
                queue.extend(visitor(DirectoryWalkEvent::Batch(&context, batch)));
            }
            let _ = visitor(DirectoryWalkEvent::Done(&context, result.as_ref().err()));
        }
        return;
    }

    let queue = Arc::new((
        Mutex::new(WorkQueue {
            items: VecDeque::from([Arc::new(initial)]),
            stopped: false,
        }),
        Condvar::new(),
    ));
    // At most two batches per worker can wait for the coordinator. This keeps
    // a scan over one enormous directory bounded to a few thousand entries.
    let (event_tx, event_rx) = mpsc::sync_channel::<WalkEvent<C>>(workers * 2);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let queue = queue.clone();
            let event_tx = event_tx.clone();
            scope.spawn(move || {
                let mut reader = NativeDirectoryReader::new();
                loop {
                    let context = {
                        let (lock, wake) = &*queue;
                        let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
                        while state.items.is_empty() && !state.stopped {
                            state = wake.wait(state).unwrap_or_else(|p| p.into_inner());
                        }
                        if state.stopped {
                            return;
                        }
                        state.items.pop_front().expect("non-empty work queue")
                    };

                    if event_tx.send(WalkEvent::Started(context.clone())).is_err() {
                        return;
                    }
                    let mut batch = Vec::with_capacity(RECURSIVE_BATCH_SIZE);
                    let result = reader.for_each(context_path(context.as_ref()), cancel, |entry| {
                        batch.push(entry);
                        if batch.len() >= RECURSIVE_BATCH_SIZE {
                            let outgoing = std::mem::take(&mut batch);
                            batch.reserve(RECURSIVE_BATCH_SIZE);
                            if event_tx
                                .send(WalkEvent::Batch(context.clone(), outgoing))
                                .is_err()
                            {
                                return false;
                            }
                        }
                        true
                    });
                    if !batch.is_empty()
                        && event_tx
                            .send(WalkEvent::Batch(context.clone(), batch))
                            .is_err()
                    {
                        return;
                    }
                    if event_tx
                        .send(WalkEvent::Done(context, result.err()))
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
        drop(event_tx);

        let mut pending = 1usize;
        let mut cancellation_applied = false;
        while pending > 0 {
            if cancel.load(Ordering::Relaxed) && !cancellation_applied {
                let (lock, wake) = &*queue;
                let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
                let removed = state.items.len();
                state.items.clear();
                state.stopped = true;
                pending = pending.saturating_sub(removed);
                cancellation_applied = true;
                wake.notify_all();
            }

            let Ok(event) = event_rx.recv() else {
                break;
            };
            match event {
                WalkEvent::Started(context) => {
                    let _ = visitor(DirectoryWalkEvent::Started(&context));
                }
                WalkEvent::Batch(context, entries) => {
                    if cancellation_applied {
                        continue;
                    }
                    let children = visitor(DirectoryWalkEvent::Batch(&context, entries));
                    if children.is_empty() {
                        continue;
                    }
                    pending = pending.saturating_add(children.len());
                    let (lock, wake) = &*queue;
                    let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
                    state.items.extend(children.into_iter().map(Arc::new));
                    wake.notify_all();
                }
                WalkEvent::Done(context, error) => {
                    let _ = visitor(DirectoryWalkEvent::Done(&context, error.as_ref()));
                    pending = pending.saturating_sub(1);
                }
            }
        }

        let (lock, wake) = &*queue;
        let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
        state.stopped = true;
        state.items.clear();
        wake.notify_all();
    });
}

/// The walker context's first field is intentionally not prescribed. A tiny
/// trait would add boilerplate to every scanner, so callers pass types that
/// implement this private path view.
pub(crate) trait DirectoryContext {
    fn directory_path(&self) -> &Path;
}

impl DirectoryContext for PathBuf {
    fn directory_path(&self) -> &Path {
        self
    }
}

fn context_path<C: DirectoryContext>(context: &C) -> &Path {
    context.directory_path()
}

/// Conservative concurrency for recursive scans. Parallel directory reads
/// are enabled only for local APFS volumes. Network/removable/unknown media
/// remain serial to avoid multiplying latency or seeking on spinning disks.
pub(crate) fn recommended_recursive_workers(_path: &Path) -> usize {
    #[cfg(target_os = "macos")]
    {
        let suitable = crate::volume_info_for_path(_path).is_some_and(|volume| {
            volume.is_local && !volume.is_removable && volume.format.as_deref() == Some("apfs")
        });
        if suitable {
            return std::thread::available_parallelism()
                .map(|count| count.get().min(8))
                .unwrap_or(4)
                .max(2);
        }
    }
    1
}

fn portable_for_each(
    path: &Path,
    cancel: &AtomicBool,
    mut visitor: impl FnMut(DirectoryEntry) -> bool,
) -> std::io::Result<()> {
    for dirent in fs::read_dir(path)?.flatten() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let child_path = dirent.path();
        let Some(name) = child_path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        // On Windows this comes from WIN32_FIND_DATA cached by enumeration and
        // does not open/hydrate a OneDrive placeholder.
        let Ok(metadata) = dirent.metadata() else {
            continue;
        };
        if !visitor(DirectoryEntry::from_metadata(child_path, name, &metadata)) {
            break;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn allocated_size(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    if metadata.file_type().is_symlink() {
        0
    } else {
        metadata.blocks().saturating_mul(512)
    }
}

#[cfg(not(unix))]
fn allocated_size(_metadata: &fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> Option<(u64, u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino(), metadata.nlink()))
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata) -> Option<(u64, u64, u64)> {
    None
}

#[cfg(target_os = "macos")]
fn metadata_flags(metadata: &fs::Metadata) -> u32 {
    use std::os::macos::fs::MetadataExt;
    metadata.st_flags()
}

#[cfg(not(any(target_os = "macos", windows)))]
fn metadata_flags(_metadata: &fs::Metadata) -> u32 {
    0
}

#[cfg(windows)]
fn metadata_flags(metadata: &fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes()
}

#[cfg(target_os = "macos")]
fn hidden_from_flags(name: &str, flags: u32) -> bool {
    const UF_HIDDEN: u32 = 0x8000;
    name.starts_with('.') || flags & UF_HIDDEN != 0
}

#[cfg(windows)]
fn hidden_from_flags(name: &str, flags: u32) -> bool {
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    name.starts_with('.') || flags & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(any(target_os = "macos", windows)))]
fn hidden_from_flags(name: &str, _flags: u32) -> bool {
    name.starts_with('.')
}

#[cfg(target_os = "macos")]
fn locked_from_flags(flags: u32) -> bool {
    const IMMUTABLE: u32 = 0x2 | 0x2_0000;
    flags & IMMUTABLE != 0
}

#[cfg(windows)]
fn locked_from_flags(flags: u32) -> bool {
    const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
    flags & FILE_ATTRIBUTE_READONLY != 0
}

#[cfg(not(any(target_os = "macos", windows)))]
fn locked_from_flags(_flags: u32) -> bool {
    false
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::ffi::OsString;
    use std::mem::{offset_of, size_of};
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
    use std::time::{Duration, UNIX_EPOCH};

    use ::windows::core::PCWSTR;
    use ::windows::Win32::Foundation::{
        CloseHandle, ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE,
    };
    use ::windows::Win32::Storage::FileSystem::{
        CreateFileW, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, FileIdInfo,
        GetFileInformationByHandleEx, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_BOTH_DIR_INFO, FILE_ID_INFO, FILE_LIST_DIRECTORY,
        FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    const WINDOWS_TO_UNIX_SECONDS: u64 = 11_644_473_600;

    pub(super) struct BulkError {
        pub source: std::io::Error,
        pub entries_emitted: usize,
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub(super) fn for_each_bulk(
        parent: &Path,
        cancel: &AtomicBool,
        buffer: &mut [u8],
        visitor: &mut impl FnMut(DirectoryEntry) -> bool,
    ) -> Result<(), BulkError> {
        let wide = wide_nul(parent).map_err(|source| BulkError {
            source,
            entries_emitted: 0,
        })?;
        let share = FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0);
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                share,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .map_err(|error| BulkError {
            source: io_error(error),
            entries_emitted: 0,
        })?;
        if handle == INVALID_HANDLE_VALUE {
            return Err(BulkError {
                source: std::io::Error::last_os_error(),
                entries_emitted: 0,
            });
        }
        let handle = OwnedHandle(handle);
        let mut volume = FILE_ID_INFO::default();
        unsafe {
            GetFileInformationByHandleEx(
                handle.0,
                FileIdInfo,
                (&mut volume as *mut FILE_ID_INFO).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        }
        .map_err(|error| BulkError {
            source: io_error(error),
            entries_emitted: 0,
        })?;

        let mut entries_emitted = 0usize;
        let mut restart = true;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(());
            }
            let class = if restart {
                FileIdBothDirectoryRestartInfo
            } else {
                FileIdBothDirectoryInfo
            };
            restart = false;
            match unsafe {
                GetFileInformationByHandleEx(
                    handle.0,
                    class,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                )
            } {
                Ok(()) => {}
                Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => return Ok(()),
                Err(error) => {
                    return Err(BulkError {
                        source: io_error(error),
                        entries_emitted,
                    });
                }
            }

            let mut offset = 0usize;
            loop {
                offset
                    .checked_add(size_of::<FILE_ID_BOTH_DIR_INFO>())
                    .filter(|end| *end <= buffer.len())
                    .ok_or_else(|| malformed(entries_emitted))?;
                let info = unsafe {
                    buffer
                        .as_ptr()
                        .add(offset)
                        .cast::<FILE_ID_BOTH_DIR_INFO>()
                        .read_unaligned()
                };
                let name_bytes = usize::try_from(info.FileNameLength)
                    .ok()
                    .filter(|bytes| bytes % 2 == 0)
                    .ok_or_else(|| malformed(entries_emitted))?;
                let name_start = offset
                    .checked_add(offset_of!(FILE_ID_BOTH_DIR_INFO, FileName))
                    .ok_or_else(|| malformed(entries_emitted))?;
                let name_end = name_start
                    .checked_add(name_bytes)
                    .filter(|end| *end <= buffer.len())
                    .ok_or_else(|| malformed(entries_emitted))?;
                let raw_name: Vec<u16> = buffer[name_start..name_end]
                    .chunks_exact(2)
                    .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                    .collect();
                if raw_name != [b'.' as u16] && raw_name != [b'.' as u16, b'.' as u16] {
                    let os_name = OsString::from_wide(&raw_name);
                    let path = parent.join(&os_name);
                    let name = String::from_utf16_lossy(&raw_name);
                    let is_reparse = info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0;
                    let kind = if is_reparse {
                        EntryKind::Symlink
                    } else if info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
                        EntryKind::Directory
                    } else {
                        EntryKind::File
                    };
                    let entry = DirectoryEntry {
                        path,
                        name: name.clone(),
                        kind,
                        size: nonnegative(info.EndOfFile),
                        allocated: if is_reparse {
                            0
                        } else {
                            nonnegative(info.AllocationSize)
                        },
                        mtime: system_time(info.LastWriteTime),
                        created: system_time(info.CreationTime),
                        // FILE_ID_BOTH_DIR_INFO obtains the identity in the
                        // same one-handle-per-directory query. Mark every
                        // entry as potentially linked: file IDs are unique on
                        // the volume, so the existing set only suppresses a
                        // repeated hard-link name and never distinct files.
                        identity: Some((volume.VolumeSerialNumber, info.FileId as u64, 2)),
                        flags: info.FileAttributes,
                        hidden: hidden_from_flags(&name, info.FileAttributes),
                        locked: locked_from_flags(info.FileAttributes),
                        mount_point: false,
                    };
                    entries_emitted = entries_emitted.saturating_add(1);
                    if !visitor(entry) {
                        return Ok(());
                    }
                }
                if info.NextEntryOffset == 0 {
                    break;
                }
                offset = offset
                    .checked_add(info.NextEntryOffset as usize)
                    .filter(|next| *next > offset && *next < buffer.len())
                    .ok_or_else(|| malformed(entries_emitted))?;
            }
        }
    }

    fn wide_nul(path: &Path) -> std::io::Result<Vec<u16>> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "directory path contains NUL",
            ));
        }
        wide.push(0);
        Ok(wide)
    }

    fn nonnegative(value: i64) -> u64 {
        u64::try_from(value).unwrap_or(0)
    }

    fn system_time(ticks: i64) -> Option<SystemTime> {
        let ticks = u64::try_from(ticks).ok()?;
        let seconds = ticks / 10_000_000;
        let nanos = ((ticks % 10_000_000) * 100) as u32;
        if seconds >= WINDOWS_TO_UNIX_SECONDS {
            UNIX_EPOCH.checked_add(Duration::new(seconds - WINDOWS_TO_UNIX_SECONDS, nanos))
        } else {
            UNIX_EPOCH.checked_sub(Duration::new(WINDOWS_TO_UNIX_SECONDS - seconds, nanos))
        }
    }

    fn malformed(entries_emitted: usize) -> BulkError {
        BulkError {
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "malformed Windows directory batch",
            ),
            entries_emitted,
        }
    }

    fn io_error(error: ::windows::core::Error) -> std::io::Error {
        let code = error.code().0 as u32;
        std::io::Error::from_raw_os_error((code & 0xffff) as i32)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::fs::File;
    use std::mem::size_of;
    use std::os::fd::AsRawFd;
    use std::time::{Duration, UNIX_EPOCH};

    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const SF_FIRMLINK: u32 = 0x0080_0000;
    const VREG: u32 = 1;
    const VDIR: u32 = 2;
    const VLNK: u32 = 5;

    #[derive(Debug)]
    pub(super) struct BulkError {
        pub source: std::io::Error,
        pub entries_emitted: usize,
    }

    #[derive(Clone, Copy, Debug, Default)]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) struct BulkStats {
        pub entries: usize,
        pub metadata_fallbacks: usize,
    }

    pub(super) fn for_each_bulk(
        path: &Path,
        cancel: &AtomicBool,
        buffer: &mut [u8],
        visitor: &mut impl FnMut(DirectoryEntry) -> bool,
    ) -> Result<BulkStats, BulkError> {
        let directory = File::open(path).map_err(|source| BulkError {
            source,
            entries_emitted: 0,
        })?;
        let mut attrs = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: libc::ATTR_CMN_RETURNED_ATTRS
                | libc::ATTR_CMN_NAME
                | ATTR_CMN_ERROR
                | libc::ATTR_CMN_DEVID
                | libc::ATTR_CMN_OBJTYPE
                | libc::ATTR_CMN_CRTIME
                | libc::ATTR_CMN_MODTIME
                | libc::ATTR_CMN_FLAGS
                | libc::ATTR_CMN_FILEID,
            volattr: 0,
            dirattr: libc::ATTR_DIR_MOUNTSTATUS | libc::ATTR_DIR_ALLOCSIZE,
            fileattr: libc::ATTR_FILE_LINKCOUNT
                | libc::ATTR_FILE_DATALENGTH
                | libc::ATTR_FILE_DATAALLOCSIZE,
            forkattr: 0,
        };
        let mut entries_emitted = 0usize;
        let mut metadata_fallbacks = 0usize;

        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(BulkStats {
                    entries: entries_emitted,
                    metadata_fallbacks,
                });
            }
            // SAFETY: `directory` remains open, `attrs` is fully initialized,
            // and the byte buffer is writable for its declared length.
            let count = unsafe {
                libc::getattrlistbulk(
                    directory.as_raw_fd(),
                    (&mut attrs as *mut libc::attrlist).cast(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    0,
                )
            };
            if count < 0 {
                return Err(BulkError {
                    source: std::io::Error::last_os_error(),
                    entries_emitted,
                });
            }
            if count == 0 {
                return Ok(BulkStats {
                    entries: entries_emitted,
                    metadata_fallbacks,
                });
            }

            let mut offset = 0usize;
            for _ in 0..count as usize {
                let Some(record_len) = read_at::<u32>(buffer, offset).map(|n| n as usize) else {
                    return Err(invalid_data(entries_emitted, "missing bulk record length"));
                };
                if record_len < size_of::<u32>() || offset + record_len > buffer.len() {
                    return Err(invalid_data(entries_emitted, "invalid bulk record length"));
                }
                let record = &buffer[offset..offset + record_len];
                offset += record_len;
                let Some((parsed, used_metadata_fallback)) = parse_record(path, record) else {
                    continue;
                };
                entries_emitted += 1;
                metadata_fallbacks += usize::from(used_metadata_fallback);
                if !visitor(parsed) {
                    return Ok(BulkStats {
                        entries: entries_emitted,
                        metadata_fallbacks,
                    });
                }
                if cancel.load(Ordering::Relaxed) {
                    return Ok(BulkStats {
                        entries: entries_emitted,
                        metadata_fallbacks,
                    });
                }
            }
        }
    }

    fn invalid_data(entries_emitted: usize, message: &'static str) -> BulkError {
        BulkError {
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, message),
            entries_emitted,
        }
    }

    fn parse_record(parent: &Path, record: &[u8]) -> Option<(DirectoryEntry, bool)> {
        let mut cursor = Cursor::new(record, size_of::<u32>());
        let returned: libc::attribute_set_t = cursor.read()?;
        if returned.commonattr & ATTR_CMN_ERROR != 0 {
            let error: u32 = cursor.read()?;
            if error != 0 {
                return None;
            }
        }

        let name = if returned.commonattr & libc::ATTR_CMN_NAME != 0 {
            cursor.read_name()?
        } else {
            return None;
        };
        let device = cursor.read_if::<libc::dev_t>(returned.commonattr & libc::ATTR_CMN_DEVID != 0);
        let object_type = cursor.read_if::<u32>(returned.commonattr & libc::ATTR_CMN_OBJTYPE != 0);
        let created_present = returned.commonattr & libc::ATTR_CMN_CRTIME != 0;
        let created = cursor
            .read_if::<libc::timespec>(created_present)
            .and_then(system_time);
        let mtime_present = returned.commonattr & libc::ATTR_CMN_MODTIME != 0;
        let mtime = cursor
            .read_if::<libc::timespec>(mtime_present)
            .and_then(system_time);
        let flags = cursor.read_if::<u32>(returned.commonattr & libc::ATTR_CMN_FLAGS != 0);
        let file_id = cursor.read_if::<u64>(returned.commonattr & libc::ATTR_CMN_FILEID != 0);

        let object_type = object_type?;
        let kind = match object_type {
            VDIR => EntryKind::Directory,
            VLNK => EntryKind::Symlink,
            VREG => EntryKind::File,
            _ => EntryKind::File,
        };

        let (mount_status, directory_allocated) = if kind == EntryKind::Directory {
            (
                cursor.read_if::<u32>(returned.dirattr & libc::ATTR_DIR_MOUNTSTATUS != 0),
                cursor.read_if::<i64>(returned.dirattr & libc::ATTR_DIR_ALLOCSIZE != 0),
            )
        } else {
            (None, None)
        };

        let (link_count, data_length, data_allocated) = if kind != EntryKind::Directory {
            (
                cursor.read_if::<u32>(returned.fileattr & libc::ATTR_FILE_LINKCOUNT != 0),
                cursor.read_if::<i64>(returned.fileattr & libc::ATTR_FILE_DATALENGTH != 0),
                cursor.read_if::<i64>(returned.fileattr & libc::ATTR_FILE_DATAALLOCSIZE != 0),
            )
        } else {
            (None, None, None)
        };

        let path = parent.join(&name);
        let complete = device.is_some()
            && file_id.is_some()
            && flags.is_some()
            // A filesystem can return a timestamp attribute whose value does
            // not map to `SystemTime` (or report no birth time). That is still
            // a complete bulk record and must not trigger one path-stat per
            // entry.
            && created_present
            && mtime_present
            && if kind == EntryKind::Directory {
                mount_status.is_some() && directory_allocated.is_some()
            } else {
                link_count.is_some() && data_length.is_some() && data_allocated.is_some()
            };
        if !complete || flags.is_some_and(|value| value & SF_FIRMLINK != 0) {
            let metadata = fs::symlink_metadata(&path).ok()?;
            let mut fallback = DirectoryEntry::from_metadata(path, name, &metadata);
            fallback.mount_point =
                mount_status.is_some_and(|status| status & libc::DIR_MNTSTATUS_MNTPOINT != 0);
            return Some((fallback, true));
        }

        let size = data_length.unwrap_or(0).max(0) as u64;
        let allocated = if kind == EntryKind::Directory {
            directory_allocated.unwrap_or(0)
        } else {
            data_allocated.unwrap_or(0)
        }
        .max(0) as u64;
        let flags = flags?;
        let hidden = hidden_from_flags(&name, flags);
        let locked = locked_from_flags(flags);
        Some((
            DirectoryEntry {
                path,
                name,
                kind,
                size,
                allocated: if kind == EntryKind::Symlink {
                    0
                } else {
                    allocated
                },
                mtime,
                created,
                identity: Some((device? as u64, file_id?, link_count.unwrap_or(1) as u64)),
                flags,
                hidden,
                locked,
                mount_point: mount_status
                    .is_some_and(|status| status & libc::DIR_MNTSTATUS_MNTPOINT != 0),
            },
            false,
        ))
    }

    fn system_time(value: libc::timespec) -> Option<SystemTime> {
        if !(0..1_000_000_000).contains(&value.tv_nsec) {
            return None;
        }
        let duration = Duration::new(value.tv_sec.unsigned_abs(), value.tv_nsec as u32);
        if value.tv_sec >= 0 {
            UNIX_EPOCH.checked_add(duration)
        } else {
            UNIX_EPOCH.checked_sub(duration)
        }
    }

    fn read_at<T>(bytes: &[u8], offset: usize) -> Option<T> {
        let end = offset.checked_add(size_of::<T>())?;
        if end > bytes.len() {
            return None;
        }
        // SAFETY: bounds are checked and packed kernel records require an
        // unaligned read.
        Some(unsafe { bytes.as_ptr().add(offset).cast::<T>().read_unaligned() })
    }

    struct Cursor<'a> {
        record: &'a [u8],
        offset: usize,
    }

    impl<'a> Cursor<'a> {
        fn new(record: &'a [u8], offset: usize) -> Self {
            Self { record, offset }
        }

        fn read<T>(&mut self) -> Option<T> {
            let value = read_at(self.record, self.offset)?;
            self.offset = self.offset.checked_add(size_of::<T>())?;
            Some(value)
        }

        fn read_if<T>(&mut self, present: bool) -> Option<T> {
            present.then(|| self.read()).flatten()
        }

        fn read_name(&mut self) -> Option<String> {
            let reference_offset = self.offset;
            let reference: libc::attrreference_t = self.read()?;
            let start = reference_offset.checked_add_signed(reference.attr_dataoffset as isize)?;
            let end = start.checked_add(reference.attr_length as usize)?;
            let mut bytes = self.record.get(start..end)?;
            if bytes.last() == Some(&0) {
                bytes = &bytes[..bytes.len().saturating_sub(1)];
            }
            std::str::from_utf8(bytes).ok().map(str::to_owned)
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn batched_reader_preserves_names_and_reports_stable_file_ids() {
        let root = std::env::temp_dir().join(format!(
            "ferail-windows-bulk-reader-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let original = root.join("café-数据.txt");
        let link = root.join("second-name.txt");
        fs::File::create(&original)
            .unwrap()
            .write_all(b"same NTFS record")
            .unwrap();
        fs::hard_link(&original, &link).unwrap();
        fs::create_dir(root.join("nested")).unwrap();

        let cancel = AtomicBool::new(false);
        let mut entries = Vec::new();
        for_each(&root, &cancel, |entry| {
            entries.push(entry);
            true
        })
        .unwrap();

        let first = entries
            .iter()
            .find(|entry| entry.path == original)
            .expect("Unicode file returned with its exact path");
        let second = entries
            .iter()
            .find(|entry| entry.path == link)
            .expect("hard-link name returned");
        assert_eq!(first.identity, second.identity);
        assert_eq!(first.size, 16);
        assert!(first.allocated >= first.size);
        assert!(entries.iter().any(|entry| {
            entry.path == root.join("nested") && entry.kind == EntryKind::Directory
        }));

        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn apfs_bulk_reader_returns_complete_stat_fields_without_per_entry_fallback() {
        let root = std::env::temp_dir().join(format!(
            "ferail-bulk-reader-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        let mut file = fs::File::create(root.join("sample.txt")).unwrap();
        file.write_all(b"bulk").unwrap();

        let cancel = AtomicBool::new(false);
        let mut names = Vec::new();
        let mut native_buffer = vec![0; NATIVE_BUFFER_BYTES];
        let stats = macos::for_each_bulk(&root, &cancel, &mut native_buffer, &mut |entry| {
            names.push(entry.name);
            true
        })
        .unwrap();
        let _ = fs::remove_dir_all(&root);

        names.sort();
        assert_eq!(names, ["nested", "sample.txt"]);
        assert_eq!(stats.entries, 2);
        assert_eq!(stats.metadata_fallbacks, 0);
    }
}

#[cfg(test)]
mod walk_tests {
    use super::*;
    use std::collections::HashSet;
    use std::io::Write as _;

    #[test]
    fn parallel_walk_visits_each_discovered_directory_once() {
        let root = std::env::temp_dir().join(format!(
            "ferail-parallel-walk-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("a/deep")).unwrap();
        fs::create_dir_all(root.join("b")).unwrap();
        fs::File::create(root.join("top.txt"))
            .unwrap()
            .write_all(b"top")
            .unwrap();
        fs::File::create(root.join("a/deep/leaf.txt"))
            .unwrap()
            .write_all(b"leaf")
            .unwrap();

        let cancel = AtomicBool::new(false);
        let mut started = HashSet::new();
        let mut entries = HashSet::new();
        walk(root.clone(), &cancel, 3, |event| match event {
            DirectoryWalkEvent::Started(path) => {
                assert!(started.insert(path.clone()), "directory started twice");
                Vec::new()
            }
            DirectoryWalkEvent::Batch(_, batch) => {
                let mut children = Vec::new();
                for entry in batch {
                    entries.insert(entry.path.clone());
                    if entry.is_dir() && !entry.is_symlink() {
                        children.push(entry.path);
                    }
                }
                children
            }
            DirectoryWalkEvent::Done(_, error) => {
                assert!(error.is_none(), "fixture directory failed: {error:?}");
                Vec::new()
            }
        });
        let _ = fs::remove_dir_all(&root);

        assert_eq!(started.len(), 4);
        assert_eq!(entries.len(), 5);
        assert!(entries.contains(&root.join("a/deep/leaf.txt")));
    }
}
