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
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use feraille_core::{EntryKind, EnumerationHandle, FileEntry, FsBackend, NodeId};

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

impl FsBackend for NativeFs {
    fn enumerate(&self, node: NodeId) -> EnumerationHandle {
        let Some(path) = self.path_for(node) else {
            return EnumerationHandle { initial: Vec::new() };
        };
        let read_dir = match std::fs::read_dir(&path) {
            Ok(rd) => rd,
            Err(_) => return EnumerationHandle { initial: Vec::new() },
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
        EnumerationHandle { initial: entries }
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

/// Move `path` into the user's Trash. macOS: rename into `~/.Trash`,
/// resolving collisions by suffixing ` 2`, ` 3`, … Returns the final
/// path if successful.
///
/// This intentionally avoids `NSWorkspace.recycle` for now — that
/// requires Objective-C bridging that lives in the macOS shell crate
/// (iter-4). The `~/.Trash` rename approach matches what the OS does
/// for files within the boot volume; it falls back to outright delete
/// on cross-volume errors.
#[cfg(target_os = "macos")]
pub fn move_to_trash(path: &Path) -> std::io::Result<PathBuf> {
    let home = home_dir();
    let trash = home.join(".Trash");
    if !trash.is_dir() {
        std::fs::create_dir_all(&trash)?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    let mut target = trash.join(name);
    if target.exists() {
        let stem = target.file_stem().map(|s| s.to_string_lossy().into_owned());
        let ext = target.extension().map(|s| s.to_string_lossy().into_owned());
        for n in 2..1000 {
            let candidate = match (&stem, &ext) {
                (Some(s), Some(e)) => trash.join(format!("{s} {n}.{e}")),
                (Some(s), None) => trash.join(format!("{s} {n}")),
                (None, _) => trash.join(format!("trash-{n}")),
            };
            if !candidate.exists() {
                target = candidate;
                break;
            }
        }
    }
    match std::fs::rename(path, &target) {
        Ok(_) => Ok(target),
        Err(e) if e.raw_os_error() == Some(libc_exdev()) => {
            // Cross-volume — fall back to copy + remove.
            copy_then_remove(path, &target)?;
            Ok(target)
        }
        Err(e) => Err(e),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn move_to_trash(path: &Path) -> std::io::Result<PathBuf> {
    // Conservative on non-macOS — refuse rather than silently delete.
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("move_to_trash not implemented on this OS for {}", path.display()),
    ))
}

#[cfg(target_os = "macos")]
fn libc_exdev() -> i32 {
    // EXDEV — cross-device link. Hard-coded to avoid pulling `libc` for one constant.
    18
}

#[cfg(target_os = "macos")]
fn copy_then_remove(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        // Defer recursive copy to iter-4 with the macOS shell crate.
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "cross-volume directory move not yet supported",
        ));
    }
    std::fs::copy(src, dst)?;
    std::fs::remove_file(src)?;
    Ok(())
}

/// Returns `(display_name, path)` pairs for every directory under `/Volumes`
/// (macOS). Empty on Linux/Windows for now.
pub fn list_volumes() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let Ok(read_dir) = std::fs::read_dir("/Volumes") else {
        return out;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            out.push((name.to_string(), path));
        }
    }
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
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
}
