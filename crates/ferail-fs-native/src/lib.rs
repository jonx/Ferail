//! Native filesystem backend (cross-platform std::fs). Iter-2 ships a
//! synchronous, single-batch implementation; threading + change-watching
//! land with the macOS shell crate in iter-3.
//!
//! `display_size` is pre-formatted at enumerate time per the no-alloc-on-paint
//! contract. The modification time is intentionally *not* pre-formatted: the UI
//! renders it live from `mtime_unix` via [`ferail_core::humanize_mtime`] so a
//! relative label ("4 seconds ago") keeps counting instead of freezing. Only
//! the relative form is timezone-free; an *absolute* hour-of-day still awaits
//! the macOS shell crate's `NSDateFormatter`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use ferail_core::{
    tr, trn, EntryKind, EnumerationError, EnumerationHandle, FileEntry, FsBackend, NodeId,
};

pub mod archive;
mod directory_reader;
mod disk_usage_scanner;
mod dupes;
pub mod file_ops;
mod icons;
pub mod image_meta;
mod magic;
pub mod media;
pub mod paths;
pub mod perceptual;
mod search;
pub mod stat_info;
pub mod verify;
mod volumes;
pub mod xattr_info;
pub use archive::scratch;
pub use archive::{
    add_to_archive, archive_stamp, commit_archive_edits, convert_archive, create_archive,
    extract_all as extract_archive, extract_entries as extract_archive_entries,
    inspect_archive_additions, materialize_archive_entry, probe_format as probe_archive_format,
    read_entry_bytes as read_archive_entry_bytes, read_summary as read_archive_summary,
    read_toc as read_archive_toc, AddOutcome, ArchiveAddition, ArchiveEditPlan, ArchiveError,
    ArchiveRename, ArchiveStamp, ArchiveSummary, ConvertOptions, ConvertOutcome, CreateOptions,
    ExtractOptions, ExtractOutcome, SkipReason, SkippedEntry,
};
pub use disk_usage_scanner::{recursive_size, recursive_totals, SubtreeTotals, DEFAULT_DU_BATCH};
pub use dupes::{
    clone_dedup, DupeFact, DupeHashCache, DupeMember, DupeMode, DupeOpts, DupePhase, DupeStats,
    SimilarImageIndexEntry, SimilarImageInfo, DEFAULT_DUPE_BATCH, PARTIAL_HASH_BYTES,
};
pub use icons::fetch_icon_rgba;
pub use magic::{
    detect_magic, detect_magic_info, sniff_bytes_info, CpuArch, ElfOs, MagicInfo, MagicType,
    PeSubsystem, MAGIC_REVISION,
};
pub use paths::home_dir;
pub use perceptual::PerceptualThumbnail;
pub use search::{SearchHit, SearchQuery, SearchStats, DEFAULT_SEARCH_BATCH};
pub use volumes::list_volumes;
#[cfg(not(target_os = "macos"))]
pub use volumes::volume_info_for_path;
pub use xattr_info::{
    clear_quarantine, details_from as quarantine_details_from, fetch_quarantine_info,
    QuarantineInfo,
};

const ROOT_NODE_RAW: u64 = 1;

pub struct NativeFs {
    inner: Mutex<Inner>,
}

struct Inner {
    next_id: u64,
    paths: BTreeMap<NodeId, PathBuf>,
    by_path: HashMap<PathBuf, NodeId>,
}

impl NativeFs {
    pub fn new() -> Self {
        let home = home_dir();
        let root = NodeId::from_raw(ROOT_NODE_RAW).expect("nonzero");
        let mut paths = BTreeMap::new();
        let mut by_path = HashMap::new();
        paths.insert(root, home.clone());
        by_path.insert(home, root);
        Self {
            inner: Mutex::new(Inner {
                next_id: ROOT_NODE_RAW + 1,
                paths,
                by_path,
            }),
        }
    }

    pub fn root(&self) -> NodeId {
        NodeId::from_raw(ROOT_NODE_RAW).expect("nonzero")
    }

    pub fn path_for(&self, id: NodeId) -> Option<PathBuf> {
        ferail_core::path_guard::assert_path_resolution_allowed("NativeFs::path_for");
        self.inner.lock().ok()?.paths.get(&id).cloned()
    }

    pub fn id_for_path(&self, path: &Path) -> NodeId {
        // Identity contract (shared with ferail-core::NodeStore):
        // keys are lexically normalized — trailing slash, `./`, and
        // doubled separators can't mint two NodeIds for one path.
        // Case and symlinks are deliberately NOT folded here; see
        // `normalize_path_key`'s doc for the boundary rules.
        let path = ferail_core::node_store::normalize_path_key(path);
        let mut inner = self.inner.lock().expect("fs lock");
        if let Some(id) = inner.by_path.get(&path) {
            return *id;
        }
        let id = NodeId::from_raw(inner.next_id).expect("nonzero");
        inner.next_id += 1;
        inner.paths.insert(id, path.clone());
        inner.by_path.insert(path, id);
        id
    }
}

impl Default for NativeFs {
    fn default() -> Self {
        Self::new()
    }
}

/// Default batch size for streaming enumeration — see
/// [`NativeFs::enumerate_streaming`]. 256 is the spec target; tune via
/// the screenshot timing pass before changing.
pub const DEFAULT_ENUMERATION_BATCH: usize = 256;

impl NativeFs {
    /// Stream entries from `path` in batches of up to `batch_size`,
    /// invoking `on_batch` each time the buffer fills or the native directory
    /// reader drains. Pure-function: returns `Some(error)` on hard failure
    /// (typically the initial directory open) and `None` on either
    /// successful completion or cooperative cancellation.
    ///
    /// `cancel` is checked between entries and (lazily) between batches.
    /// Setting it stops the worker after the next iteration; whatever's
    /// already buffered gets flushed via `on_batch` before returning so
    /// the caller can apply partial results.
    ///
    /// This is the worker-thread half of the streaming-enumeration
    /// design ([docs/features/STREAMING_ENUMERATION.md]). The host
    /// application owns thread spawning + event-loop dispatch.
    pub fn enumerate_streaming(
        &self,
        path: &Path,
        batch_size: usize,
        cancel: &AtomicBool,
        mut on_batch: impl FnMut(Vec<FileEntry>),
    ) -> Option<EnumerationError> {
        ferail_core::path_guard::assert_off_ui_thread("NativeFs::enumerate_streaming");
        let mut buffer: Vec<FileEntry> = Vec::with_capacity(batch_size);
        let result = directory_reader::for_each(path, cancel, |entry| {
            let id = self.id_for_path(&entry.path);
            buffer.push(self.file_entry_from_directory_entry_with_id(&entry, id));
            if buffer.len() >= batch_size {
                on_batch(std::mem::take(&mut buffer));
                buffer.reserve(batch_size);
            }
            true
        });
        if !buffer.is_empty() {
            on_batch(buffer);
        }
        result.err().map(|error| map_io_error(&error))
    }

    /// Build a `FileEntry` for an arbitrary path (used by global search,
    /// where results — e.g. Spotlight hits — arrive as bare paths from
    /// outside the current directory). Reads `symlink_metadata` so a
    /// symlink is reported as a link, never followed. Returns `None` for
    /// non-UTF-8 names or unreadable metadata, matching enumerate policy.
    pub fn file_entry_for_path(&self, path: &Path) -> Option<FileEntry> {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)?;
        let metadata = std::fs::symlink_metadata(path).ok()?;
        Some(self.file_entry_from_metadata(path, name, &metadata))
    }

    /// Shared `FileEntry` construction from a resolved `(path, name,
    /// metadata)`. Pre-formats the display strings per the
    /// no-alloc-on-paint contract.
    fn file_entry_from_metadata(
        &self,
        path: &Path,
        name: String,
        metadata: &std::fs::Metadata,
    ) -> FileEntry {
        let id = self.id_for_path(path);
        self.file_entry_from_metadata_with_id(name, metadata, id)
    }

    /// Build a row with an identity owned by the caller rather than inserting
    /// the path into NativeFs's process-lifetime maps. Recursive result
    /// surfaces use this to keep millions of ephemeral rows surface-local.
    fn file_entry_from_metadata_with_id(
        &self,
        name: String,
        metadata: &std::fs::Metadata,
        id: NodeId,
    ) -> FileEntry {
        let ft = metadata.file_type();
        let kind = if ft.is_dir() {
            EntryKind::Directory
        } else if ft.is_symlink() {
            EntryKind::Symlink
        } else {
            EntryKind::File
        };
        let size = metadata.len();
        let mtime_unix = metadata
            .modified()
            .ok()
            .and_then(system_time_unix)
            .unwrap_or(0);
        let created_unix = metadata.created().ok().and_then(system_time_unix);
        let hidden = entry_is_hidden(&name, metadata);
        let locked = entry_is_locked(metadata);
        self.file_entry_from_values(
            name,
            kind,
            size,
            mtime_unix,
            created_unix,
            hidden,
            locked,
            id,
        )
    }

    fn file_entry_from_directory_entry_with_id(
        &self,
        entry: &crate::directory_reader::DirectoryEntry,
        id: NodeId,
    ) -> FileEntry {
        self.file_entry_from_values(
            entry.name.clone(),
            entry.kind,
            entry.size,
            entry.mtime.and_then(system_time_unix).unwrap_or(0),
            entry.created.and_then(system_time_unix),
            entry.hidden,
            entry.locked,
            id,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn file_entry_from_values(
        &self,
        name: String,
        kind: EntryKind,
        size: u64,
        mtime_unix: i64,
        created_unix: Option<i64>,
        hidden: bool,
        locked: bool,
        id: NodeId,
    ) -> FileEntry {
        let display_size = if matches!(kind, EntryKind::Directory) {
            String::new()
        } else {
            humanize_bytes(size)
        };
        let display_kind = describe_kind(kind, &name);
        // User-facing leaf (macOS shows an on-disk `:` as `/`, Finder-style),
        // and a precomputed hazard flag so the dense row paint never runs the
        // deceptive-character analysis itself.
        let display_name = crate::paths::display_leaf(&name).into_owned();
        let name_has_hazards = ferail_core::name_hazards::has_hazards(&display_name);
        let name: Arc<str> = name.into();
        let display_name: Arc<str> = if display_name.as_str() == name.as_ref() {
            name.clone()
        } else {
            display_name.into()
        };
        let empty = ferail_core::empty_entry_text();
        FileEntry {
            id,
            name,
            display_name,
            name_has_hazards,
            kind,
            size,
            mtime_unix,
            display_size: display_size.into(),
            display_kind: display_kind.into(),
            display_magic: empty.clone(),
            display_description: empty,
            details_loaded: false,
            is_quarantined: false,
            quarantine: None,
            hidden,
            created_unix,
            locked,
        }
    }
}

fn system_time_unix(time: std::time::SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
}

/// Platform "locked" semantics for `FileEntry::locked`, evaluated once
/// at enumerate time from the stat already in hand. macOS: the
/// user/system immutable flags — what Finder's "Locked" checkbox sets.
/// Windows: the read-only attribute, its closest native analogue.
#[cfg(target_os = "macos")]
pub fn entry_is_locked(metadata: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    // From <sys/stat.h>: UF_IMMUTABLE | SF_IMMUTABLE.
    const IMMUTABLE: u32 = 0x2 | 0x2_0000;
    (metadata.st_flags() & IMMUTABLE) != 0
}

#[cfg(windows)]
pub fn entry_is_locked(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
    (metadata.file_attributes() & FILE_ATTRIBUTE_READONLY) != 0
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn entry_is_locked(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Platform "hidden file" semantics for `FileEntry::hidden`, evaluated
/// once at enumerate time. Finder hides dot-files AND files carrying
/// the `UF_HIDDEN` BSD flag (e.g. `~/Library`); Explorer hides files
/// with `FILE_ATTRIBUTE_HIDDEN` (e.g. `$RECYCLE.BIN`, `desktop.ini`).
/// Dot-prefix is honored on every platform so cross-platform repos
/// (.git, .config) behave consistently.
#[cfg(target_os = "macos")]
pub fn entry_is_hidden(name: &str, metadata: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    // From <sys/stat.h>: UF_HIDDEN — "file is hidden in GUI".
    const UF_HIDDEN: u32 = 0x8000;
    name.starts_with('.') || (metadata.st_flags() & UF_HIDDEN) != 0
}

#[cfg(windows)]
pub fn entry_is_hidden(name: &str, metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // From winnt.h. SYSTEM-attribute files are deliberately NOT
    // treated as hidden here — Explorer's "hide protected operating
    // system files" is a separate, second toggle; v1 mirrors the
    // primary hidden-files toggle only.
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    name.starts_with('.') || (metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN) != 0
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn entry_is_hidden(name: &str, _metadata: &std::fs::Metadata) -> bool {
    name.starts_with('.')
}

/// Depth-first listing of every entry beneath `root`, for the recursive
/// (Shift-invoked) flavor of "Copy File List". Appends the full path of
/// each file *and* directory, a directory's own line immediately
/// followed by its contents, children sorted by name so the pasted list
/// is stable across runs. Symlinks are listed but never descended into
/// (`symlink_metadata` reports the link itself, so a link to a
/// directory is not `is_dir()`), which also breaks cycles. Unreadable
/// directories are skipped in place, matching the search walker.
/// Blocking: touches the disk on every level, so it must stay off the
/// UI thread.
pub fn list_subtree_paths(root: &Path, include_hidden: bool, out: &mut Vec<PathBuf>) {
    ferail_core::path_guard::assert_off_ui_thread("list_subtree_paths");
    let Ok(read_dir) = std::fs::read_dir(root) else {
        return;
    };
    let mut children: Vec<(String, PathBuf, bool)> = Vec::new();
    for dirent in read_dir.flatten() {
        let path = dirent.path();
        let Some(name) = path.file_name().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !include_hidden && entry_is_hidden(&name, &metadata) {
            continue;
        }
        children.push((name, path, metadata.is_dir()));
    }
    children.sort_by_key(|(name, _, _)| name.to_lowercase());
    for (_, path, is_dir) in children {
        out.push(path.clone());
        if is_dir {
            list_subtree_paths(&path, include_hidden, out);
        }
    }
}

pub(crate) fn map_io_error(e: &std::io::Error) -> EnumerationError {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => EnumerationError::PermissionDenied,
        std::io::ErrorKind::NotFound => EnumerationError::NotFound,
        _ => EnumerationError::Other(e.to_string()),
    }
}

impl FsBackend for NativeFs {
    fn enumerate(&self, node: NodeId) -> EnumerationHandle {
        let Some(path) = self.path_for(node) else {
            return EnumerationHandle {
                initial: Vec::new(),
                error: Some(EnumerationError::NotFound),
            };
        };
        let read_dir = match std::fs::read_dir(&path) {
            Ok(rd) => rd,
            Err(e) => {
                let kind = match e.kind() {
                    std::io::ErrorKind::PermissionDenied => EnumerationError::PermissionDenied,
                    std::io::ErrorKind::NotFound => EnumerationError::NotFound,
                    _ => EnumerationError::Other(e.to_string()),
                };
                return EnumerationHandle {
                    initial: Vec::new(),
                    error: Some(kind),
                };
            }
        };
        let mut entries: Vec<FileEntry> = Vec::new();
        for dirent in read_dir.flatten() {
            let child_path = dirent.path();
            let Some(name) = child_path
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            let metadata = match dirent.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            // Shared with the lazy enumerate path: one constructor keeps the
            // pre-formatted display strings (incl. `display_name`) and the
            // hazard flag from drifting between the two listing routes.
            entries.push(self.file_entry_from_metadata(&child_path, name, &metadata));
        }
        // Directories first, then case-insensitive name. Cached keys:
        // one lowercase per entry, not two per comparison.
        entries.sort_by_cached_key(|e| {
            (
                !matches!(e.kind, EntryKind::Directory),
                e.name.to_lowercase(),
            )
        });
        EnumerationHandle {
            initial: entries,
            error: None,
        }
    }
}

/// Move `path` into the user's Trash, with the OS's full Trash
/// semantics: undo (`Cmd+Z` in Finder), audible feedback, the Trash
/// icon's bounce-into animation, and the proper per-volume `.Trashes`
/// directory for non-boot volumes.
///
/// macOS: `NSFileManager.trashItemAtURL:resultingItemURL:error:`.
/// On success returns the item's new location inside the Trash
/// (`Some(trashed_path)`) — that's what trash-undo renames back
/// (docs/features/FILE_OPS.md). On failure the file remains in place —
/// the caller should surface the error (we no longer have a
/// "delete-anyway" fallback).
#[cfg(target_os = "macos")]
pub fn move_to_trash(path: &Path) -> std::io::Result<Option<PathBuf>> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    let path_str = path.to_str().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path is not valid UTF-8")
    })?;

    unsafe {
        let ns_path = NSString::from_str(path_str);
        let url = NSURL::fileURLWithPath(&ns_path);
        let fm = NSFileManager::defaultManager();
        let mut resulting: Option<objc2::rc::Retained<NSURL>> = None;
        match fm.trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting)) {
            Ok(()) => Ok(resulting
                .and_then(|u| u.path())
                .map(|p| PathBuf::from(p.to_string()))),
            Err(err) => {
                let msg = format!(
                    "trashItemAtURL({}) failed: {}",
                    path.display(),
                    err.localizedDescription(),
                );
                // NSFileWriteNoPermissionError (513): the item is owned by
                // another user — e.g. a root-owned Apple app in /Applications.
                // Surface it as a typed PermissionDenied (keyed off the Cocoa
                // error *code*, not the localized text) so the shell can offer
                // an elevated retry, exactly as copy/move already do.
                const NS_FILE_WRITE_NO_PERMISSION_ERROR: isize = 513;
                if err.code() == NS_FILE_WRITE_NO_PERMISSION_ERROR {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        msg,
                    ))
                } else {
                    Err(std::io::Error::other(msg))
                }
            }
        }
    }
}

/// The user's primary Trash (`~/.Trash` [mac]). An elevated trash worker runs
/// as root, whose own `trashItemAtURL` would target root's Trash — so it moves
/// protected items into *this* path instead, where the user expects them.
pub fn home_trash_dir() -> PathBuf {
    paths::home_dir().join(".Trash")
}

/// Trash directories visible to this user: `~/.Trash` plus each
/// mounted volume's `.Trashes/<uid>` when it exists. Used by Empty
/// Trash to cover per-volume trashes, not just the boot volume's.
/// [win-parity: `SHEmptyRecycleBinW` handles this wholesale]
#[cfg(target_os = "macos")]
pub fn trash_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home_trash = paths::home_dir().join(".Trash");
    if home_trash.is_dir() {
        dirs.push(home_trash);
    }
    let uid = unsafe { libc::getuid() };
    if let Ok(rd) = std::fs::read_dir("/Volumes") {
        for dirent in rd.flatten() {
            let t = dirent.path().join(".Trashes").join(uid.to_string());
            if t.is_dir() {
                dirs.push(t);
            }
        }
    }
    dirs
}

#[cfg(not(any(target_os = "macos", target_os = "aros")))]
pub fn trash_dirs() -> Vec<PathBuf> {
    Vec::new()
}

/// Send `path` to the Windows Recycle Bin via `IFileOperation`.
///
/// `SHFileOperationW` rejects the `\\?\` extended-length prefix that
/// `std::fs::canonicalize` returns on Windows, which left long or verbatim
/// paths unable to use the Recycle Bin. `IFileOperation` is the modern shell
/// file-operation API and works through an `IShellItem`, so it is the right
/// surface for long-path-compatible recycle semantics.
///
/// Suppresses confirmation prompts and error UI (`FOF_NOCONFIRMATION |
/// FOF_NOERRORUI | FOF_SILENT`) so the worker does not pop a system dialog.
/// Errors come back through the returned HRESULT / abort flag.
#[cfg(windows)]
pub fn move_to_trash(path: &Path) -> std::io::Result<Option<PathBuf>> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName,
        FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
    };

    struct ComGuard(bool);
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    fn io_other(msg: impl Into<String>) -> std::io::Error {
        std::io::Error::other(msg.into())
    }

    fn io_from_windows(context: &str, err: windows::core::Error) -> std::io::Error {
        io_other(format!("{context}: {err}"))
    }

    fn eq_ascii_u16(a: u16, b: u8) -> bool {
        let lower = if a >= b'A' as u16 && a <= b'Z' as u16 {
            a + 32
        } else {
            a
        };
        lower == (b as char).to_ascii_lowercase() as u16
    }

    fn nul_terminate(mut wide: Vec<u16>) -> Vec<u16> {
        wide.push(0);
        wide
    }

    fn shell_parsing_names(path: &Path) -> Vec<Vec<u16>> {
        let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
        let verbatim = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
        let fallback = if raw.starts_with(&verbatim)
            && raw.len() >= 8
            && eq_ascii_u16(raw[4], b'U')
            && eq_ascii_u16(raw[5], b'N')
            && eq_ascii_u16(raw[6], b'C')
            && raw[7] == b'\\' as u16
        {
            let mut unc = vec![b'\\' as u16, b'\\' as u16];
            unc.extend_from_slice(&raw[8..]);
            Some(unc)
        } else if raw.starts_with(&verbatim) {
            Some(raw[4..].to_vec())
        } else {
            None
        };

        let raw = nul_terminate(raw);
        match fallback.map(nul_terminate) {
            Some(fallback) if fallback != raw => vec![raw, fallback],
            _ => vec![raw],
        }
    }

    let co = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() };
    let _com = match co {
        Ok(()) => ComGuard(true),
        Err(e) if e.code() == RPC_E_CHANGED_MODE => ComGuard(false),
        Err(e) => return Err(io_from_windows("CoInitializeEx", e)),
    };

    let mut last_item_error = None;
    let mut item = None;
    for wide in shell_parsing_names(path) {
        match unsafe { SHCreateItemFromParsingName(PCWSTR::from_raw(wide.as_ptr()), None) } {
            Ok(shell_item) => {
                item = Some(shell_item);
                break;
            }
            Err(e) => last_item_error = Some(e),
        }
    }
    let item: IShellItem = item.ok_or_else(|| {
        last_item_error
            .map(|e| io_from_windows("SHCreateItemFromParsingName", e))
            .unwrap_or_else(|| io_other("SHCreateItemFromParsingName: no parsing candidates"))
    })?;

    let op: IFileOperation = unsafe {
        CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| io_from_windows("CoCreateInstance(FileOperation)", e))?
    };

    unsafe {
        op.SetOperationFlags(
            FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT | FOFX_RECYCLEONDELETE,
        )
        .map_err(|e| io_from_windows("IFileOperation::SetOperationFlags", e))?;
        op.DeleteItem(&item, None)
            .map_err(|e| io_from_windows("IFileOperation::DeleteItem", e))?;
        op.PerformOperations()
            .map_err(|e| io_from_windows("IFileOperation::PerformOperations", e))?;
        if op
            .GetAnyOperationsAborted()
            .map_err(|e| io_from_windows("IFileOperation::GetAnyOperationsAborted", e))?
            .as_bool()
        {
            return Err(io_other(format!(
                "IFileOperation aborted moving {} to the Recycle Bin",
                path.display()
            )));
        }
    }

    // IFileOperation does not report the recycled location, so Recycle Bin
    // restore-undo is not available on Windows yet.
    Ok(None)
}
/// Linux: the freedesktop Trash spec (`$XDG_DATA_HOME/Trash/{files,info}` plus
/// per-volume `.Trash-<uid>`), via the `trash` crate. The recycled location
/// isn't surfaced (the file list refreshes from disk), so returns `Ok(None)`.
#[cfg(target_os = "linux")]
pub fn move_to_trash(path: &Path) -> std::io::Result<Option<PathBuf>> {
    trash::delete(path).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(None)
}

/// AROS: the AmigaOS convention is a per-volume `Trashcan` drawer at the
/// volume root (`SYS:Trashcan`, `RAM:Trashcan`, …). Wanderer treats it
/// specially and empties it on request. We move the item into its OWN
/// volume's Trashcan (an intra-volume rename — instant, and it never
/// crosses devices), creating the drawer on first use. Returns the landed
/// path so the UI can reveal it.
#[cfg(target_os = "aros")]
pub fn move_to_trash(path: &Path) -> std::io::Result<Option<PathBuf>> {
    let trashcan = aros_trashcan_for(path).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("no volume root for {}", path.display()),
        )
    })?;
    std::fs::create_dir_all(&trashcan)?;
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "source has no file name")
    })?;
    // Collision-free landing name inside the Trashcan.
    let mut dest = trashcan.join(name);
    let mut n = 2u32;
    while dest.exists() {
        let base = name.to_string_lossy();
        dest = trashcan.join(format!("{base}.{n}"));
        n = n.saturating_add(1);
    }
    std::fs::rename(path, &dest)?;
    Ok(Some(dest))
}

/// `DEV:Trashcan` for the volume `path` lives on. AROS paths are
/// `DEV:rest`; the volume root is everything through the first `:`.
#[cfg(target_os = "aros")]
pub(crate) fn aros_trashcan_for(path: &Path) -> Option<PathBuf> {
    let s = path.to_string_lossy();
    let colon = s.find(':')?;
    let dev = &s[..=colon]; // includes the ':'
    Some(PathBuf::from(format!("{dev}Trashcan")))
}

/// AROS: the per-volume Trashcan drawers that exist, for Empty Trash.
/// Probes the standard mounted volumes' roots.
#[cfg(target_os = "aros")]
pub fn trash_dirs() -> Vec<PathBuf> {
    ["SYS:", "RAM:", "MacRW:", "Work:"]
        .iter()
        .map(|v| PathBuf::from(format!("{v}Trashcan")))
        .filter(|p| p.is_dir())
        .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "aros", windows)))]
pub fn move_to_trash(path: &Path) -> std::io::Result<Option<PathBuf>> {
    // Conservative on other targets — refuse rather than silently delete.
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "move_to_trash not implemented on this OS for {}",
            path.display()
        ),
    ))
}

/// Volume metadata fetched in one batched NSURL `resourceValuesForKeys`
/// call. Capacity fields are `None` for non-local (network) volumes —
/// querying SMB / NFS for capacity can do a remote round-trip and we
/// don't want that on the section-rebuild path. They're also `None`
/// when the platform is non-macOS or the lookup itself failed.
#[derive(Clone, Debug)]
pub struct VolumeInfo {
    pub path: PathBuf,
    pub name: String,
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub is_local: bool,
    /// "Can the user eject this?" in the Finder sense — removable or
    /// ejectable media, external (non-internal) disks, disk images,
    /// network mounts. Drives the sidebar ⏏ affordance and the
    /// same-device eject-all grouping.
    pub is_removable: bool,
    /// Filesystem format (e.g. "apfs", "exfat"). `None` off macOS or when
    /// `statfs` failed. Populated for the Get Info panel's volume rows.
    pub format: Option<String>,
    /// Mounted read-only (a CD/DVD, a locked SD card, a read-only disk
    /// image, an `ro` mount). Drives the status bar's "is read-only"
    /// label — "0 B free" on a CD is technically true but the wrong
    /// message — and the Get Info / preview Access row. The macOS boot
    /// volume is deliberately exempt: the sealed system snapshot
    /// statfs's as read-only, but the Macintosh HD the user sees is
    /// writable via the firmlinked Data volume.
    pub read_only: bool,
    /// BSD device node (e.g. "/dev/disk3s1s1"). Same availability as `format`.
    pub bsd_device: Option<String>,
    /// Opaque whole-device grouping key: volumes with the same `device_id`
    /// are partitions/containers of one physical device, so ejecting one
    /// should offer to eject the others (Finder's "eject all" prompt).
    /// macOS: the whole-disk BSD name ("disk4"); Windows: the physical
    /// disk number(s) ("disk3"); Linux: the parent block device ("sdb").
    /// `None` when unknown or for network mounts — never grouped.
    pub device_id: Option<String>,
}

/// Look up a volume's metadata for the volume root at `path` (e.g.
/// `/`, `/Volumes/External`). Returns `None` on non-macOS, on lookup
/// failure, or when `path` isn't a volume root.
///
/// Reads the cached, mount-stamped NSURL keys only — does NOT trigger
/// purgeable-content scans (`AvailableCapacityForImportantUsageKey`)
/// and does NOT spin up sleeping disks. For non-local volumes we
/// return name + flags but null out the capacity fields, since some
/// network filesystems will issue a remote round-trip even for the
/// "cheap" capacity keys.
#[cfg(target_os = "macos")]
pub fn volume_info_for_path(path: &Path) -> Option<VolumeInfo> {
    ferail_core::path_guard::assert_off_ui_thread("volume_info_for_path");
    use objc2::msg_send;
    use objc2::msg_send_id;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::ClassType;
    use objc2_foundation::{
        NSArray, NSString, NSURLResourceKey, NSURLVolumeAvailableCapacityKey,
        NSURLVolumeIsEjectableKey, NSURLVolumeIsInternalKey, NSURLVolumeIsLocalKey,
        NSURLVolumeIsRemovableKey, NSURLVolumeLocalizedNameKey, NSURLVolumeTotalCapacityKey, NSURL,
    };

    let path_str = path.to_str()?;
    unsafe {
        let ns_path = NSString::from_str(path_str);
        let url: Retained<NSURL> = NSURL::fileURLWithPath(&ns_path);

        // Build an `NSArray<NSURLResourceKey>` via the class method
        // `arrayWithObjects:count:`. The typed `from_slice` constructor
        // wants `IsRetainable`, which NSString doesn't satisfy because
        // it has a mutable subclass — but at runtime AppKit just wants
        // a count + a pointer to a contiguous block of `id`. Apple's
        // constants are immortal `&'static NSString` so the lifetime
        // is fine.
        let key_ptrs: [*const NSURLResourceKey; 7] = [
            NSURLVolumeLocalizedNameKey,
            NSURLVolumeTotalCapacityKey,
            NSURLVolumeAvailableCapacityKey,
            NSURLVolumeIsLocalKey,
            NSURLVolumeIsRemovableKey,
            NSURLVolumeIsEjectableKey,
            NSURLVolumeIsInternalKey,
        ];
        let keys: Retained<NSArray<NSURLResourceKey>> = msg_send_id![
            NSArray::<NSURLResourceKey>::class(),
            arrayWithObjects: key_ptrs.as_ptr(),
            count: key_ptrs.len(),
        ];
        let dict = url.resourceValuesForKeys_error(&keys).ok()?;

        // The dictionary returns `AnyObject` values; per Apple's
        // contract each key maps to a known concrete type (NSString
        // for names, NSNumber for capacities and booleans). We send
        // the appropriate selector directly rather than downcasting,
        // which keeps the code working across objc2 versions that
        // change the downcast API.
        let lookup_string = |key: &NSURLResourceKey| -> Option<String> {
            let obj: &AnyObject = dict.get(key)?;
            let ns: &NSString = &*(obj as *const AnyObject as *const NSString);
            Some(ns.to_string())
        };
        let lookup_u64 = |key: &NSURLResourceKey| -> Option<u64> {
            let obj: &AnyObject = dict.get(key)?;
            let v: std::os::raw::c_longlong = msg_send![obj, longLongValue];
            if v < 0 {
                None
            } else {
                Some(v as u64)
            }
        };
        let lookup_bool = |key: &NSURLResourceKey| -> Option<bool> {
            let obj: &AnyObject = dict.get(key)?;
            let b: bool = msg_send![obj, boolValue];
            Some(b)
        };

        let name = lookup_string(NSURLVolumeLocalizedNameKey)
            .or_else(|| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| path_str.to_string());
        let is_local = lookup_bool(NSURLVolumeIsLocalKey).unwrap_or(true);
        // Finder-parity "can the user eject this?". IsRemovableKey alone
        // only covers removable *media* (SD cards, USB sticks) — an
        // external USB/Thunderbolt HDD or SSD reports removable=false,
        // ejectable=false, but internal=false, and disk images report
        // ejectable=true. Any of the three signals means Finder draws
        // the ⏏, so we match. A missing IsInternal answer counts as
        // internal, so an inconclusive lookup never grows an eject
        // button on the boot volume.
        let is_removable = lookup_bool(NSURLVolumeIsRemovableKey).unwrap_or(false)
            || lookup_bool(NSURLVolumeIsEjectableKey).unwrap_or(false)
            || !lookup_bool(NSURLVolumeIsInternalKey).unwrap_or(true);
        // Skip capacity for non-local volumes — see fn doc.
        let (total_bytes, available_bytes) = if is_local {
            (
                lookup_u64(NSURLVolumeTotalCapacityKey),
                lookup_u64(NSURLVolumeAvailableCapacityKey),
            )
        } else {
            (None, None)
        };

        let (format, bsd_device, read_only) = crate::stat_info::volume_fs_info(path);
        // Group by the whole-disk BSD name so the eject flow can find the
        // other volumes of a multi-partition external device. Local disks
        // only — an SMB `f_mntfromname` is a URL, not a disk node.
        let device_id = if is_local {
            bsd_device.as_deref().and_then(volumes::whole_disk_bsd)
        } else {
            None
        };
        Some(VolumeInfo {
            path: path.to_path_buf(),
            name,
            total_bytes,
            available_bytes,
            is_local,
            is_removable,
            format,
            read_only,
            bsd_device,
            device_id,
        })
    }
}

/// True when macOS reports `path` as an iCloud-synced item. Two
/// signals, cheapest first:
///
/// 1. **Path prefix** — everything macOS stores under
///    `~/Library/Mobile Documents/` is ubiquity-managed (iCloud Drive
///    and per-app containers). A `starts_with`, no syscall.
/// 2. **`NSURLIsUbiquitousItemKey`** — catches the Desktop & Documents
///    folders that iCloud manages *in place*: their path stays
///    `~/Desktop` / `~/Documents`, so the prefix check alone misses
///    them. This is exactly the signal Finder uses to draw its cloud
///    badge.
///
/// Reads cached resource values only — never downloads a placeholder or
/// spins a sleeping disk. Always `false` off macOS. Per the prime
/// directive, callers must not invoke this from the paint path.
#[cfg(target_os = "macos")]
pub fn path_is_cloud_synced(path: &Path) -> bool {
    if let Some(home) = std::env::var_os("HOME") {
        let mobile = PathBuf::from(home).join("Library/Mobile Documents");
        if path.starts_with(&mobile) {
            return true;
        }
    }
    url_is_ubiquitous(path).unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
pub fn path_is_cloud_synced(_path: &Path) -> bool {
    false
}

/// Read `NSURLIsUbiquitousItemKey` for `path`. `None` when the path is
/// non-UTF-8, the URL lookup fails, or the key is absent (a non-cloud
/// item). Mirrors the cached-key discipline of `volume_info_for_path`.
#[cfg(target_os = "macos")]
fn url_is_ubiquitous(path: &Path) -> Option<bool> {
    use objc2::msg_send;
    use objc2::msg_send_id;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::ClassType;
    use objc2_foundation::{NSArray, NSString, NSURLIsUbiquitousItemKey, NSURLResourceKey, NSURL};

    let path_str = path.to_str()?;
    unsafe {
        let ns_path = NSString::from_str(path_str);
        let url: Retained<NSURL> = NSURL::fileURLWithPath(&ns_path);
        let key_ptrs: [*const NSURLResourceKey; 1] = [NSURLIsUbiquitousItemKey];
        let keys: Retained<NSArray<NSURLResourceKey>> = msg_send_id![
            NSArray::<NSURLResourceKey>::class(),
            arrayWithObjects: key_ptrs.as_ptr(),
            count: key_ptrs.len(),
        ];
        let dict = url.resourceValuesForKeys_error(&keys).ok()?;
        let obj: &AnyObject = dict.get(NSURLIsUbiquitousItemKey)?;
        let b: bool = msg_send![obj, boolValue];
        Some(b)
    }
}

/// iCloud state of a path, mirroring Finder's stable downloaded-vs-placeholder
/// distinction. `None` (no badge) is the implicit third state: not an iCloud
/// item at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloudState {
    /// In iCloud and materialized on this Mac (Finder shows no badge; we draw
    /// a solid cloud). The common case for synced Desktop/Documents.
    Downloaded,
    /// In iCloud but a not-downloaded placeholder — APFS dataless, evicted by
    /// "Optimize Mac Storage" (Finder shows its download cloud; we draw an
    /// outline cloud). "Set up for cloud but the local copy isn't here."
    Placeholder,
}

/// Whether the filesystem metadata marks `path` as a cloud file whose bytes
/// are not currently local. This check never opens the file data or asks a
/// provider to hydrate it.
#[cfg(target_os = "macos")]
pub fn is_cloud_placeholder(path: &Path) -> bool {
    use std::os::macos::fs::MetadataExt as _;
    const SF_DATALESS: u32 = 0x4000_0000;
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.st_flags() & SF_DATALESS != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn is_cloud_placeholder(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    std::fs::symlink_metadata(path)
        .map(|metadata| {
            metadata.file_attributes()
                & (FILE_ATTRIBUTE_OFFLINE
                    | FILE_ATTRIBUTE_RECALL_ON_OPEN
                    | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
                != 0
        })
        .unwrap_or(false)
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn is_cloud_placeholder(_path: &Path) -> bool {
    false
}

/// The iCloud [`CloudState`] of `path`, or `None` when it isn't an iCloud item.
///
/// Reads cached resource values plus the stat flags only — never reads file
/// data, so it never downloads a placeholder or spins a sleeping disk (the
/// `SF_DATALESS` flag is visible to `lstat` without materializing the file).
/// Always `None` off macOS. Per the prime directive, callers must not invoke
/// this from the paint path.
#[cfg(target_os = "macos")]
pub fn cloud_state(path: &Path) -> Option<CloudState> {
    if !path_is_cloud_synced(path) {
        return None;
    }
    // <sys/stat.h>: SF_DATALESS — "file is dataless object" (the placeholder
    // for a not-yet-materialized iCloud / FileProvider item). `lstat` reading
    // st_flags does not trigger a download.
    Some(if is_cloud_placeholder(path) {
        CloudState::Placeholder
    } else {
        CloudState::Downloaded
    })
}

#[cfg(not(target_os = "macos"))]
pub fn cloud_state(_path: &Path) -> Option<CloudState> {
    None
}

/// The sidebar's well-known Locations that macOS reports as iCloud items,
/// mapped to their [`CloudState`], as an owned map the UI can probe per render
/// without touching the filesystem. Computed off the render thread (startup +
/// volume/wake refresh); empty off macOS. Feeds the trailing cloud badge in
/// the Locations section.
pub fn cloud_synced_locations() -> std::collections::HashMap<PathBuf, CloudState> {
    paths::well_known_locations()
        .into_iter()
        .filter_map(|loc| cloud_state(&loc.path).map(|state| (loc.path, state)))
        .collect()
}

pub fn describe_kind(kind: EntryKind, name: &str) -> String {
    match kind {
        // English on purpose: `display_kind` doubles as data
        // (`ferail_core::formats_compatible` matches these words), so the
        // UI translates it at render time with `tr_dyn`; the `msgid!`
        // marks put the words into the catalog.
        EntryKind::Directory => ferail_core::msgid!("Folder").to_string(),
        EntryKind::Symlink => ferail_core::msgid!("Symlink").to_string(),
        EntryKind::File => match name.rsplit_once('.') {
            Some((_, ext)) if !ext.is_empty() && ext.len() <= 8 => ext.to_uppercase(),
            _ => ferail_core::msgid!("File").to_string(),
        },
    }
}

pub fn humanize_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

/// One-line summary of a folder's recursive contents for the file
/// list's Description column, e.g. `"1.204 files · 88 folders"`. Both
/// counts come from the same walk that computed the folder's recursive
/// size, so they describe exactly the entries that total covers. Counts
/// are grouped with the app-wide `.` separator and singularised
/// ("1 file"); a folder with only files or only sub-folders drops the
/// empty half, and a truly empty folder returns `"Empty"`.
pub fn folder_contents_summary(file_count: u64, dir_count: u64) -> String {
    // `{count}` (not the implicit `{n}`) so the number keeps its digit
    // grouping; the plural category is still chosen from the raw count.
    fn files(n: u64) -> ferail_core::i18n::Text {
        trn!(
            "{count} file",
            "{count} files",
            n,
            count = ferail_core::counts::format_count(n)
        )
    }
    fn folders(n: u64) -> ferail_core::i18n::Text {
        trn!(
            "{count} folder",
            "{count} folders",
            n,
            count = ferail_core::counts::format_count(n)
        )
    }
    match (file_count, dir_count) {
        (0, 0) => tr!("Empty").into_string(),
        (f, 0) => files(f).into_string(),
        (0, d) => folders(d).into_string(),
        (f, d) => format!("{} \u{b7} {}", files(f), folders(d)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trashing must work on the `\\?\`-prefixed paths the file list uses
    /// (std::fs::canonicalize yields them on Windows). The Windows path now
    /// feeds an IShellItem to IFileOperation so Recycle Bin moves are not
    /// limited to the legacy SHFileOperationW surface.
    #[cfg(windows)]
    #[test]
    fn move_to_trash_handles_verbatim_prefix() {
        let dir = std::env::temp_dir().join(format!("ferail-trash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("to-trash.txt");
        std::fs::write(&f, b"bye").unwrap();
        // canonicalize gives a `\\?\C:\...` path on Windows.
        let canonical = std::fs::canonicalize(&f).unwrap();
        assert!(
            canonical.to_string_lossy().starts_with(r"\\?\"),
            "precondition: canonical path is verbatim: {canonical:?}"
        );
        move_to_trash(&canonical).expect("trash a \\\\?\\ path");
        assert!(!f.exists(), "file was removed from its folder");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn humanize_bytes_small() {
        assert_eq!(humanize_bytes(0), "0 B");
        assert_eq!(humanize_bytes(512), "512 B");
        assert_eq!(humanize_bytes(1023), "1023 B");
    }

    #[test]
    fn humanize_bytes_units() {
        assert_eq!(humanize_bytes(1024), "1.0 KB");
        assert_eq!(humanize_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(humanize_bytes(4_404_019), "4.2 MB");
    }

    #[test]
    fn folder_contents_summary_cases() {
        // Empty folder — distinct from "we couldn't count".
        assert_eq!(folder_contents_summary(0, 0), "Empty");
        // Singular vs plural, and the one-sided drops.
        assert_eq!(folder_contents_summary(1, 0), "1 file");
        assert_eq!(folder_contents_summary(3, 0), "3 files");
        assert_eq!(folder_contents_summary(0, 1), "1 folder");
        assert_eq!(folder_contents_summary(0, 5), "5 folders");
        // Both halves, joined with the column's ` · ` separator.
        assert_eq!(
            folder_contents_summary(128, 12),
            "128 files \u{b7} 12 folders"
        );
        // Digit grouping on large trees, app-wide `.` separator.
        assert_eq!(
            folder_contents_summary(1_204, 88),
            "1.204 files \u{b7} 88 folders"
        );
    }

    #[test]
    fn enumeration_root_yields_entries() {
        let fs = NativeFs::new();
        let h = fs.enumerate(fs.root());
        // $HOME usually has at least one entry. Guard for CI sandboxes.
        if h.initial.is_empty() {
            return;
        }
        for e in &h.initial {
            assert!(!e.name.is_empty());
            assert!(!e.name.contains('/'));
        }
    }

    #[test]
    fn id_for_path_folds_mechanical_spellings() {
        let fs = NativeFs::new();
        let canonical = fs.id_for_path(Path::new("/tmp/ferail-id-test"));
        assert_eq!(fs.id_for_path(Path::new("/tmp/ferail-id-test/")), canonical);
        assert_eq!(
            fs.id_for_path(Path::new("/tmp/./ferail-id-test")),
            canonical
        );
        assert_eq!(fs.id_for_path(Path::new("/tmp//ferail-id-test")), canonical);
        // Stored path is the normalized spelling.
        assert_eq!(
            fs.path_for(canonical),
            Some(PathBuf::from("/tmp/ferail-id-test"))
        );
        // Case variants stay distinct (per-volume property; see
        // ferail_core::node_store::normalize_path_key).
        assert_ne!(fs.id_for_path(Path::new("/tmp/FERAIL-ID-TEST")), canonical);
    }

    #[test]
    fn dotfiles_are_hidden_on_every_platform() {
        let dir = std::env::temp_dir().join(format!("ferail-hidden-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dot = dir.join(".dotfile");
        let plain = dir.join("plain.txt");
        std::fs::write(&dot, b"x").unwrap();
        std::fs::write(&plain, b"x").unwrap();
        let dot_meta = std::fs::symlink_metadata(&dot).unwrap();
        let plain_meta = std::fs::symlink_metadata(&plain).unwrap();
        assert!(entry_is_hidden(".dotfile", &dot_meta));
        assert!(!entry_is_hidden("plain.txt", &plain_meta));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The recursive Copy File List walker: depth-first, a directory's
    /// own line immediately followed by its name-sorted contents, and
    /// hidden entries skipped unless asked for.
    #[test]
    fn list_subtree_paths_walks_depth_first_and_skips_hidden() {
        let dir = std::env::temp_dir().join(format!("ferail-subtree-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("b_sub").join("inner")).unwrap();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        std::fs::write(dir.join("b_sub").join("file.txt"), b"x").unwrap();
        std::fs::write(dir.join("b_sub").join("inner").join("deep.txt"), b"x").unwrap();
        std::fs::write(dir.join("b_sub").join(".hidden"), b"x").unwrap();

        let mut out = Vec::new();
        list_subtree_paths(&dir, false, &mut out);
        assert_eq!(
            out,
            vec![
                dir.join("a.txt"),
                dir.join("b_sub"),
                dir.join("b_sub").join("file.txt"),
                dir.join("b_sub").join("inner"),
                dir.join("b_sub").join("inner").join("deep.txt"),
            ]
        );

        out.clear();
        list_subtree_paths(&dir, true, &mut out);
        assert!(out.contains(&dir.join("b_sub").join(".hidden")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// macOS Finder parity: a filename whose on-disk POSIX byte is a colon
    /// (what `ls` shows) must enumerate with a slash in `display_name` while
    /// the raw `name` stays the colon for path operations. The reverse
    /// (typed `/` → on-disk `:`) is covered by `paths::on_disk_leaf`.
    #[cfg(target_os = "macos")]
    #[test]
    fn colon_name_enumerates_with_slash_display() {
        let dir = std::env::temp_dir().join(format!("ferail-colon-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The colon is the only separator a POSIX leaf can hold here; Finder
        // shows it as `/`. (You cannot create a literal `/` leaf via POSIX.)
        std::fs::write(dir.join("a:b"), b"x").unwrap();
        let fs = NativeFs::new();
        let handle = fs.enumerate(fs.id_for_path(&dir));
        let entry = handle
            .initial
            .iter()
            .find(|e| e.name.as_ref() == "a:b")
            .expect("colon file enumerated by raw name");
        assert_eq!(
            entry.display_name.as_ref(),
            "a/b",
            "on-disk ':' shows as '/'"
        );
        assert!(
            !entry.name_has_hazards,
            "a Finder-style slash name is not a deceptive-character hazard"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// UF_HIDDEN must register as hidden even without a dot prefix —
    /// this is what makes `~/Library` disappear like it does in
    /// Finder. Sets the flag via chflags(1) on a temp file.
    #[cfg(target_os = "macos")]
    #[test]
    fn uf_hidden_flag_is_hidden_on_macos() {
        let dir = std::env::temp_dir().join(format!("ferail-ufhidden-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let flagged = dir.join("flagged.txt");
        std::fs::write(&flagged, b"x").unwrap();
        let status = std::process::Command::new("chflags")
            .arg("hidden")
            .arg(&flagged)
            .status()
            .expect("chflags available on macOS");
        assert!(status.success(), "chflags hidden failed");
        let meta = std::fs::symlink_metadata(&flagged).unwrap();
        assert!(entry_is_hidden("flagged.txt", &meta));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn volume_info_for_root_is_local_with_capacity() {
        // The boot volume is always mounted on a macOS test runner.
        // Don't assert the name string (users can rename the volume in
        // Disk Utility) — just shape: local, removable=false, both
        // capacity numbers populated and total >= available.
        let info =
            volume_info_for_path(Path::new("/")).expect("boot volume info available on macOS");
        assert!(info.is_local, "/ should be local");
        assert!(!info.is_removable, "/ should not be removable");
        assert!(!info.name.is_empty(), "boot volume has a name");
        let total = info.total_bytes.expect("total capacity for /");
        let avail = info.available_bytes.expect("available capacity for /");
        assert!(total > 0);
        assert!(avail <= total);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cloud_sync_matches_mobile_documents_prefix() {
        // The path-prefix arm is a pure `starts_with`, independent of
        // whether the file exists — anything under the ubiquity
        // container counts as cloud-synced.
        let home = std::env::var_os("HOME").expect("HOME set on test runner");
        let container = PathBuf::from(&home).join("Library/Mobile Documents");
        assert!(
            path_is_cloud_synced(&container.join("com~apple~CloudDocs/Notes.txt")),
            "items inside the iCloud container are cloud-synced"
        );
        assert!(
            path_is_cloud_synced(&container),
            "the ubiquity container root itself is cloud-synced"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn boot_volume_is_not_cloud_synced() {
        // `/` is neither under the container nor an ubiquitous item, so
        // both detection arms must report false.
        assert!(!path_is_cloud_synced(Path::new("/")));
    }
}
