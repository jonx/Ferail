//! Native filesystem backend (cross-platform std::fs). Iter-2 ships a
//! synchronous, single-batch implementation; threading + change-watching
//! land with the macOS shell crate in iter-3.
//!
//! Display strings (`display_size`, `display_mtime`) are pre-formatted at
//! enumerate time per the no-alloc-on-paint contract. Time formatting is
//! day-resolution only this iter — accurate hour-of-day requires local
//! timezone, deferred until the macOS shell crate brings `NSDateFormatter`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use feraille_core::{
    EntryKind, EnumerationError, EnumerationHandle, FileEntry, FsBackend, NodeId,
};

mod disk_usage_scanner;
mod icons;
mod magic;
pub use disk_usage_scanner::DEFAULT_DU_BATCH;
pub use icons::fetch_icon_rgba;
pub use magic::detect_magic;

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
            inner: Mutex::new(Inner { next_id: ROOT_NODE_RAW + 1, paths, by_path }),
        }
    }

    pub fn root(&self) -> NodeId {
        NodeId::from_raw(ROOT_NODE_RAW).expect("nonzero")
    }

    pub fn path_for(&self, id: NodeId) -> Option<PathBuf> {
        self.inner.lock().ok()?.paths.get(&id).cloned()
    }

    pub fn id_for_path(&self, path: &Path) -> NodeId {
        let mut inner = self.inner.lock().expect("fs lock");
        if let Some(id) = inner.by_path.get(path) {
            return *id;
        }
        let id = NodeId::from_raw(inner.next_id).expect("nonzero");
        inner.next_id += 1;
        inner.paths.insert(id, path.to_path_buf());
        inner.by_path.insert(path.to_path_buf(), id);
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
    /// invoking `on_batch` each time the buffer fills or `read_dir`
    /// drains. Pure-function: returns `Some(error)` on hard failure
    /// (typically the initial `read_dir` open) and `None` on either
    /// successful completion or cooperative cancellation.
    ///
    /// `cancel` is checked between entries and (lazily) between batches.
    /// Setting it stops the worker after the next iteration; whatever's
    /// already buffered gets flushed via `on_batch` before returning so
    /// the caller can apply partial results.
    ///
    /// This is the worker-thread half of the streaming-enumeration
    /// design ([docs/features/STREAMING_ENUMERATION.md]). The host
    /// (`feraille-app`) owns thread spawning + event-loop dispatch.
    pub fn enumerate_streaming(
        &self,
        path: &Path,
        batch_size: usize,
        cancel: &AtomicBool,
        mut on_batch: impl FnMut(Vec<FileEntry>),
    ) -> Option<EnumerationError> {
        let read_dir = match std::fs::read_dir(path) {
            Ok(rd) => rd,
            Err(e) => return Some(map_io_error(&e)),
        };
        let mut buffer: Vec<FileEntry> = Vec::with_capacity(batch_size);
        for dirent in read_dir.flatten() {
            if cancel.load(Ordering::Relaxed) {
                if !buffer.is_empty() {
                    on_batch(std::mem::take(&mut buffer));
                }
                return None;
            }
            let Some(entry) = self.dirent_to_file_entry(&dirent) else {
                continue;
            };
            buffer.push(entry);
            if buffer.len() >= batch_size {
                on_batch(std::mem::take(&mut buffer));
                buffer.reserve(batch_size);
            }
        }
        if !buffer.is_empty() {
            on_batch(buffer);
        }
        None
    }

    /// Build a `FileEntry` from a single `DirEntry`. Returns `None` for
    /// names that aren't valid UTF-8 or whose metadata can't be read —
    /// matching the existing eager `enumerate` policy of skipping them
    /// rather than failing the whole listing.
    fn dirent_to_file_entry(&self, dirent: &std::fs::DirEntry) -> Option<FileEntry> {
        let child_path = dirent.path();
        let name = child_path
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)?;
        let metadata = dirent.metadata().ok()?;
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
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let display_size = if matches!(kind, EntryKind::Directory) {
            String::new()
        } else {
            humanize_bytes(size)
        };
        let display_mtime = humanize_mtime(mtime_unix);
        let display_kind = describe_kind(kind, &name);
        let id = self.id_for_path(&child_path);
        Some(FileEntry {
            id,
            name,
            kind,
            size,
            mtime_unix,
            display_size,
            display_mtime,
            display_kind,
            display_magic: String::new(),
        })
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
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let display_size = if matches!(kind, EntryKind::Directory) {
                String::new()
            } else {
                humanize_bytes(size)
            };
            let display_mtime = humanize_mtime(mtime_unix);
            let display_kind = describe_kind(kind, &name);
            let id = self.id_for_path(&child_path);
            entries.push(FileEntry {
                id,
                name,
                kind,
                size,
                mtime_unix,
                display_size,
                display_mtime,
                display_kind,
                display_magic: String::new(),
            });
        }
        // Directories first, then case-insensitive name.
        entries.sort_by(|a, b| match (a.kind, b.kind) {
            (EntryKind::Directory, EntryKind::Directory) => {
                a.name.to_lowercase().cmp(&b.name.to_lowercase())
            }
            (EntryKind::Directory, _) => std::cmp::Ordering::Less,
            (_, EntryKind::Directory) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });
        EnumerationHandle { initial: entries, error: None }
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Hand `path` to the OS for default-app open. macOS: `open(1)`. Windows:
/// `cmd /C start`. Linux: `xdg-open`. Returns `Err` only if the launcher
/// itself failed to start; we can't tell whether the OS chose to do
/// anything useful with it.
#[cfg(target_os = "macos")]
pub fn open_with_default(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("open").arg(path).spawn().map(|_| ())
}

#[cfg(target_os = "windows")]
pub fn open_with_default(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("cmd")
        .args(["/C", "start", "", path.to_string_lossy().as_ref()])
        .spawn()
        .map(|_| ())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn open_with_default(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("xdg-open").arg(path).spawn().map(|_| ())
}

/// Move `path` into the user's Trash, with the OS's full Trash
/// semantics: undo (`Cmd+Z` in Finder), audible feedback, the Trash
/// icon's bounce-into animation, and the proper per-volume `.Trashes`
/// directory for non-boot volumes.
///
/// macOS: `NSFileManager.trashItemAtURL:resultingItemURL:error:`.
/// Returns `Ok(())` on success; the resulting URL inside the Trash is
/// discarded. On failure the file remains in place — the caller should
/// surface the error (we no longer have a "delete-anyway" fallback).
#[cfg(target_os = "macos")]
pub fn move_to_trash(path: &Path) -> std::io::Result<()> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    let path_str = path.to_str().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path is not valid UTF-8")
    })?;

    unsafe {
        let ns_path = NSString::from_str(path_str);
        let url = NSURL::fileURLWithPath(&ns_path);
        let fm = NSFileManager::defaultManager();
        match fm.trashItemAtURL_resultingItemURL_error(&url, None) {
            Ok(()) => Ok(()),
            Err(err) => Err(std::io::Error::other(format!(
                "trashItemAtURL({}) failed: {}",
                path.display(),
                err.localizedDescription(),
            ))),
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn move_to_trash(path: &Path) -> std::io::Result<()> {
    // Conservative on non-macOS — refuse rather than silently delete.
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("move_to_trash not implemented on this OS for {}", path.display()),
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
    pub is_removable: bool,
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
    use objc2::msg_send;
    use objc2::msg_send_id;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::ClassType;
    use objc2_foundation::{
        NSArray, NSString, NSURLResourceKey, NSURLVolumeAvailableCapacityKey,
        NSURLVolumeIsLocalKey, NSURLVolumeIsRemovableKey, NSURLVolumeLocalizedNameKey,
        NSURLVolumeTotalCapacityKey, NSURL,
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
        let key_ptrs: [*const NSURLResourceKey; 5] = [
            NSURLVolumeLocalizedNameKey,
            NSURLVolumeTotalCapacityKey,
            NSURLVolumeAvailableCapacityKey,
            NSURLVolumeIsLocalKey,
            NSURLVolumeIsRemovableKey,
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
            if v < 0 { None } else { Some(v as u64) }
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
        let is_removable = lookup_bool(NSURLVolumeIsRemovableKey).unwrap_or(false);
        // Skip capacity for non-local volumes — see fn doc.
        let (total_bytes, available_bytes) = if is_local {
            (
                lookup_u64(NSURLVolumeTotalCapacityKey),
                lookup_u64(NSURLVolumeAvailableCapacityKey),
            )
        } else {
            (None, None)
        };

        Some(VolumeInfo {
            path: path.to_path_buf(),
            name,
            total_bytes,
            available_bytes,
            is_local,
            is_removable,
        })
    }
}

#[cfg(not(target_os = "macos"))]
pub fn volume_info_for_path(_path: &Path) -> Option<VolumeInfo> {
    None
}

/// Volume metadata for every directory under `/Volumes` (macOS),
/// resolved through [`volume_info_for_path`]. Falls back to a
/// path-derived `VolumeInfo` when the NSURL lookup fails. Empty on
/// Linux/Windows for now.
pub fn list_volumes() -> Vec<VolumeInfo> {
    let mut out: Vec<VolumeInfo> = Vec::new();
    let Ok(read_dir) = std::fs::read_dir("/Volumes") else {
        return out;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        let info = volume_info_for_path(&path).unwrap_or_else(|| VolumeInfo {
            name: path
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| path.display().to_string()),
            path: path.clone(),
            total_bytes: None,
            available_bytes: None,
            is_local: true,
            is_removable: false,
        });
        out.push(info);
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn describe_kind(kind: EntryKind, name: &str) -> String {
    match kind {
        EntryKind::Directory => "Folder".to_string(),
        EntryKind::Symlink => "Symlink".to_string(),
        EntryKind::File => match name.rsplit_once('.') {
            Some((_, ext)) if !ext.is_empty() && ext.len() <= 8 => ext.to_uppercase(),
            _ => "File".to_string(),
        },
    }
}

fn humanize_bytes(bytes: u64) -> String {
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

fn humanize_mtime(unix: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(unix);
    let diff = now - unix;
    const DAY: i64 = 86_400;
    if diff < -DAY {
        return format_date(unix);
    }
    if diff < DAY {
        return "Today".to_string();
    }
    if diff < 2 * DAY {
        return "Yesterday".to_string();
    }
    if diff < 7 * DAY {
        return format!("{} days ago", diff / DAY);
    }
    if diff < 365 * DAY {
        return format_month_day(unix);
    }
    format_date(unix)
}

/// Days-from-unix-epoch → (Y, M, D) via Howard Hinnant's `civil_from_days`.
fn ymd(unix: i64) -> (i32, u32, u32) {
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn format_month_day(unix: i64) -> String {
    let (_, m, d) = ymd(unix);
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!("{} {}", NAMES[(m as usize - 1).min(11)], d)
}

fn format_date(unix: i64) -> String {
    let (y, m, d) = ymd(unix);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ymd_known_dates() {
        // 2026-05-01 00:00:00 UTC = 1777_593_600
        assert_eq!(ymd(1_777_593_600), (2026, 5, 1));
        // 1970-01-01 epoch
        assert_eq!(ymd(0), (1970, 1, 1));
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
            assert!(!e.display_mtime.is_empty());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn volume_info_for_root_is_local_with_capacity() {
        // The boot volume is always mounted on a macOS test runner.
        // Don't assert the name string (users can rename the volume in
        // Disk Utility) — just shape: local, removable=false, both
        // capacity numbers populated and total >= available.
        let info = volume_info_for_path(Path::new("/"))
            .expect("boot volume info available on macOS");
        assert!(info.is_local, "/ should be local");
        assert!(!info.is_removable, "/ should not be removable");
        assert!(!info.name.is_empty(), "boot volume has a name");
        let total = info.total_bytes.expect("total capacity for /");
        let avail = info.available_bytes.expect("available capacity for /");
        assert!(total > 0);
        assert!(avail <= total);
    }
}
