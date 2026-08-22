//! Copy / move engine for file operations (docs/features/FILE_OPS.md).
//!
//! Pure, synchronous, worker-thread functions — the GPUI side owns
//! scheduling, dialogs, and undo. Contract mirrors `recursive_size`:
//! cooperative cancellation via `&AtomicBool`, progress via a shared
//! [`TransferProgress`] sink, partial results reported honestly.
//!
//! Speed ladder (fastest legal mechanism wins, per top-level item):
//!   1. Same-volume copy on APFS → `clonefile(2)`: copy-on-write, instant,
//!      zero bytes moved, whole tree in one syscall.
//!   2. Cross-volume copy → `copyfile(3)` with a status callback:
//!      kernel-optimized, preserves holes (sparse), xattrs/ACLs/flags,
//!      and reports intra-file progress + honors cancellation.
//!   3. Anything else (non-mac, or copyfile unavailable) → a chunked
//!      read/write fallback.
//!
//! Same-volume move stays `rename(2)` — instant, beats every copy.
//!
//! Never touches UI, pasteboard, SQLite, or AppKit.

use ferail_core::msgid;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
#[cfg(not(target_os = "macos"))]
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Streaming copy buffer for the chunked fallback. Large enough that
/// syscall overhead is negligible, small enough that progress ticks and
/// cancellation land quickly even on slow volumes.
#[cfg(not(target_os = "macos"))]
const COPY_CHUNK: usize = 8 * 1024 * 1024;

/// Throttle for publishing the current-item name: at most once per this
/// interval, so a directory of a million tiny files never pays a
/// format+alloc per file (the byte/item counters are lock-free atomics;
/// only the name is gated).
const NAME_PUBLISH_INTERVAL: Duration = Duration::from_millis(100);

/// Why one item in a file operation failed, classified so the UI can both
/// explain it in plain terms and decide whether *coping* with it is possible
/// (re-running elevated, or naming the process that holds a locked file).
///
/// The flat `Result<_, String>` the engine returned before threw away the
/// `ErrorKind` / OS code at the `format!` boundary; this keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileOpErrorKind {
    /// EACCES/EPERM, Windows `ERROR_ACCESS_DENIED`. The one class where
    /// retrying with elevated privileges plausibly helps.
    PermissionDenied,
    /// The file is open in another process — unix ETXTBSY/EBUSY, Windows
    /// `ERROR_SHARING_VIOLATION`/`ERROR_LOCK_VIOLATION`. Elevation does *not*
    /// help (it can't release another process's handle); closing it does.
    Locked,
    /// The source/destination vanished mid-operation.
    NotFound,
    /// ENOSPC/EDQUOT, Windows `ERROR_DISK_FULL`.
    NoSpace,
    /// EROFS / write-protected device. A read-only *volume*, not an attribute.
    ReadOnly,
    NameTooLong,
    AlreadyExists,
    /// Anything not specifically classified — the raw text still carries it.
    Other,
}

impl FileOpErrorKind {
    /// A short, plain-language label for the per-item failure list, e.g.
    /// "Documents — in use by another program".
    pub fn summary(self) -> &'static str {
        match self {
            Self::PermissionDenied => msgid!("permission denied"),
            Self::Locked => msgid!("in use by another program"),
            Self::NotFound => msgid!("no longer exists"),
            Self::NoSpace => msgid!("no space left on the destination"),
            Self::ReadOnly => msgid!("destination is read-only"),
            Self::NameTooLong => msgid!("name too long"),
            Self::AlreadyExists => msgid!("already exists"),
            Self::Other => msgid!("could not be completed"),
        }
    }

    /// One practical next step for this failure class (centralises the advice
    /// the GPUI notification used to string-match into existence).
    pub fn advice(self) -> &'static str {
        match self {
            Self::PermissionDenied => "Retry as administrator, or check the item's permissions.",
            Self::Locked => {
                "The file is open in another program. Close it and retry, or see what's using it."
            }
            Self::NotFound => {
                msgid!("The item may have moved or been deleted. Refresh the folder and try again.")
            }
            Self::NoSpace => {
                msgid!("Free space on the destination volume or choose another destination.")
            }
            Self::ReadOnly => msgid!(
                "The destination is read-only. Choose a writable folder or change the volume's permissions."
            ),
            Self::NameTooLong => {
                msgid!("Use a shorter name or move the item to a path with fewer nested folders.")
            }
            Self::AlreadyExists => {
                msgid!("Choose a different name or remove the existing item, then try again.")
            }
            Self::Other => msgid!(
                "Refresh the folder and try again. If it keeps failing, inspect the item in your file manager."
            ),
        }
    }

    /// True when re-running this item with elevated privileges could succeed.
    /// Only a bare permission denial qualifies — a locked file, a full disk,
    /// or a read-only volume aren't fixed by becoming root/admin.
    pub fn is_elevation_recoverable(self) -> bool {
        matches!(self, Self::PermissionDenied)
    }

    /// True when the failure is a file held open by another process.
    pub fn is_lock(self) -> bool {
        matches!(self, Self::Locked)
    }
}

/// A structured, per-item file-operation failure: what kind, which path, and
/// the untouched raw OS detail (never hidden — power users want it, and it's
/// what the Copy-to-clipboard bug-report action carries).
#[derive(Clone, Debug)]
pub struct FileOpError {
    pub kind: FileOpErrorKind,
    /// The item the failure is about (the source or the destination).
    pub path: PathBuf,
    /// The raw underlying error text (OS error + message).
    pub raw: String,
    /// The raw OS error code, when one was available.
    pub os_code: Option<i32>,
}

impl FileOpError {
    /// Classify a `std::io::Error` from an op on `path`: the portable
    /// `ErrorKind` cases first, then the raw OS code for the ones it doesn't
    /// name (locked file, full/read-only volume). Raw text always preserved.
    pub fn from_io(err: &std::io::Error, path: &Path) -> Self {
        FileOpError {
            kind: classify_io_kind(err),
            path: path.to_path_buf(),
            raw: err.to_string(),
            os_code: err.raw_os_error(),
        }
    }

    /// A non-`io::Error` failure (e.g. "no free name") with an explicit kind.
    pub fn other(path: &Path, kind: FileOpErrorKind, raw: impl Into<String>) -> Self {
        FileOpError {
            kind,
            path: path.to_path_buf(),
            raw: raw.into(),
            os_code: None,
        }
    }

    /// The item's basename for the user-facing list, falling back to the full
    /// path for roots.
    pub fn item_label(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

impl fmt::Display for FileOpError {
    /// The detailed form (path + raw) for logs and the bug-report clipboard.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.raw)
    }
}

fn classify_io_kind(err: &std::io::Error) -> FileOpErrorKind {
    use std::io::ErrorKind as K;
    match err.kind() {
        K::PermissionDenied => FileOpErrorKind::PermissionDenied,
        K::NotFound => FileOpErrorKind::NotFound,
        K::AlreadyExists => FileOpErrorKind::AlreadyExists,
        _ => classify_os_code(err.raw_os_error()),
    }
}

/// Map a raw OS error code to a [`FileOpErrorKind`] for the cases `ErrorKind`
/// doesn't surface portably. Uses `libc` constants on unix (errno values vary
/// across unix flavours, so literals would be wrong) and the documented
/// `winerror.h` values on Windows.
fn classify_os_code(code: Option<i32>) -> FileOpErrorKind {
    let Some(code) = code else {
        return FileOpErrorKind::Other;
    };
    #[cfg(unix)]
    {
        match code {
            c if c == libc::EACCES || c == libc::EPERM => FileOpErrorKind::PermissionDenied,
            c if c == libc::ETXTBSY || c == libc::EBUSY => FileOpErrorKind::Locked,
            c if c == libc::ENOSPC || c == libc::EDQUOT => FileOpErrorKind::NoSpace,
            c if c == libc::EROFS => FileOpErrorKind::ReadOnly,
            c if c == libc::ENAMETOOLONG => FileOpErrorKind::NameTooLong,
            c if c == libc::ENOENT => FileOpErrorKind::NotFound,
            c if c == libc::EEXIST => FileOpErrorKind::AlreadyExists,
            _ => FileOpErrorKind::Other,
        }
    }
    #[cfg(windows)]
    {
        // winerror.h values. Literals (not a crate dep) — this whole arm only
        // compiles on Windows, so it can't be exercised from the macOS build;
        // it's a pure data mapping, verified against the SDK headers.
        match code {
            5 => FileOpErrorKind::PermissionDenied, // ERROR_ACCESS_DENIED
            32 | 33 => FileOpErrorKind::Locked,     // SHARING / LOCK_VIOLATION
            39 | 112 => FileOpErrorKind::NoSpace,   // HANDLE_DISK_FULL / DISK_FULL
            19 => FileOpErrorKind::ReadOnly,        // ERROR_WRITE_PROTECT
            206 => FileOpErrorKind::NameTooLong,    // ERROR_FILENAME_EXCED_RANGE
            2 | 3 => FileOpErrorKind::NotFound,     // FILE / PATH_NOT_FOUND
            80 | 183 => FileOpErrorKind::AlreadyExists, // FILE_EXISTS / ALREADY_EXISTS
            _ => FileOpErrorKind::Other,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = code;
        FileOpErrorKind::Other
    }
}

/// Shared, lock-light progress sink for a transfer.
///
/// The worker bumps the atomic counters on its hot path — no allocation,
/// no channel, no waiting on the UI. The UI samples this on its own clock
/// (~10 Hz) and derives rate/ETA itself. That decoupling is the Prime
/// Directive made structural: the copy can never be slowed or stalled by
/// the act of displaying its progress, no matter how many files.
pub struct TransferProgress {
    bytes_total: AtomicU64,
    items_total: AtomicU64,
    bytes_done: AtomicU64,
    items_done: AtomicU64,
    /// Items counted so far during the planning walk, before totals are
    /// known — drives the "Preparing — N items" phase so a huge tree
    /// doesn't look hung while `plan_transfer` walks it.
    planned: AtomicU64,
    planning: AtomicBool,
    name: Mutex<NameState>,
}

struct NameState {
    last: Option<Instant>,
    name: Arc<str>,
}

impl Default for TransferProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl TransferProgress {
    pub fn new() -> Self {
        Self {
            bytes_total: AtomicU64::new(0),
            items_total: AtomicU64::new(0),
            bytes_done: AtomicU64::new(0),
            items_done: AtomicU64::new(0),
            planned: AtomicU64::new(0),
            planning: AtomicBool::new(true),
            name: Mutex::new(NameState {
                last: None,
                name: Arc::from(""),
            }),
        }
    }

    /// Planning-walk progress (no totals yet).
    pub fn note_planned(&self) {
        self.planned.fetch_add(1, Ordering::Relaxed);
    }
    pub fn planned(&self) -> u64 {
        self.planned.load(Ordering::Relaxed)
    }
    pub fn is_planning(&self) -> bool {
        self.planning.load(Ordering::Relaxed)
    }

    /// Planning done: totals are known, switch to the determinate phase.
    pub fn begin_transfer(&self, bytes_total: u64, items_total: u64) {
        self.bytes_total.store(bytes_total, Ordering::Relaxed);
        self.items_total.store(items_total, Ordering::Relaxed);
        self.planning.store(false, Ordering::Relaxed);
    }

    /// Hot-path counters — relaxed atomics, no allocation, no lock.
    pub fn add_bytes(&self, n: u64) {
        self.bytes_done.fetch_add(n, Ordering::Relaxed);
    }
    pub fn add_items(&self, n: u64) {
        self.items_done.fetch_add(n, Ordering::Relaxed);
    }

    pub fn bytes_done(&self) -> u64 {
        self.bytes_done.load(Ordering::Relaxed)
    }
    pub fn bytes_total(&self) -> u64 {
        self.bytes_total.load(Ordering::Relaxed)
    }
    pub fn items_done(&self) -> u64 {
        self.items_done.load(Ordering::Relaxed)
    }
    pub fn items_total(&self) -> u64 {
        self.items_total.load(Ordering::Relaxed)
    }

    /// Publish `path`'s file name as the current item, throttled to
    /// [`NAME_PUBLISH_INTERVAL`]. The to_string_lossy + alloc only runs
    /// when the gate passes — so the per-file cost in a small-file storm
    /// is one uncontended lock + one `Instant::now`, both cheap.
    pub fn note_current(&self, path: &Path) {
        let Ok(mut g) = self.name.lock() else { return };
        let now = Instant::now();
        let due = match g.last {
            None => true,
            Some(t) => now.duration_since(t) >= NAME_PUBLISH_INTERVAL,
        };
        if due {
            let label = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy();
            g.name = Arc::from(label.as_ref());
            g.last = Some(now);
        }
    }

    /// Snapshot the current-item name for the UI sampler.
    pub fn current(&self) -> Arc<str> {
        self.name
            .lock()
            .map(|g| g.name.clone())
            .unwrap_or_else(|_| Arc::from(""))
    }
}

/// Everything `plan_transfer` learns up front so the UI can size the
/// progress bar and raise the collision dialog before any bytes move.
#[derive(Debug)]
pub struct OpPlan {
    /// Top-level items being copied/moved (as given, after validation).
    pub sources: Vec<PathBuf>,
    pub dest_dir: PathBuf,
    /// Sum of file sizes across all sources (directories walk free).
    pub total_bytes: u64,
    /// Files + directories, for outcome bookkeeping.
    pub total_items: u64,
    /// Destination paths (`dest_dir/<name>`) that already exist.
    pub conflicts: Vec<PathBuf>,
    /// Per-source byte subtotal, parallel to `sources`. Lets the clone /
    /// rename fast paths credit an item's whole weight in one jump
    /// without re-walking it.
    pub source_bytes: Vec<u64>,
    /// Per-source item subtotal, parallel to `sources`.
    pub source_items: Vec<u64>,
}

/// How to resolve a name collision for one top-level item. `run_copy`/
/// `run_move` take a `Fn(&Path) -> CollisionPolicy`, so the caller can
/// answer per item (the UI prompts per conflict with an apply-to-rest
/// shortcut) or batch by ignoring the path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollisionPolicy {
    /// Remove the existing destination just before copying that item.
    Replace,
    /// Pick a fresh name (`"name 2"`, `"name 3"`, …).
    KeepBoth,
    /// Leave the existing destination alone; don't transfer that item.
    Skip,
}

/// What actually happened. `created` pairs each transferred top-level
/// source with the destination it produced (post collision-renaming),
/// which is exactly what undo needs.
#[derive(Default)]
pub struct OpOutcome {
    pub created: Vec<(PathBuf, PathBuf)>,
    pub skipped: u64,
    pub replaced: u64,
    pub cancelled: bool,
    /// Items that could not be transferred, each with its classified cause.
    /// A failed item no longer aborts the batch — the engine records it here
    /// and moves on, so a 10-item paste that trips on item 3 still attempts
    /// 4..10 and reports exactly what failed and why.
    pub failed: Vec<FileOpError>,
}

impl OpOutcome {
    /// True when at least one item failed (distinct from cancellation).
    pub fn has_failures(&self) -> bool {
        !self.failed.is_empty()
    }
}

/// Naming scheme for collision-free names.
#[derive(Clone, Copy)]
pub enum NameScheme {
    /// Finder Duplicate flavor: `name copy`, `name copy 2`, …
    Copy,
    /// Finder paste-collision flavor: `name 2`, `name 3`, …
    Numbered,
}

/// First non-existing `dest_dir/<variant of name>` under `scheme`.
/// Extension is preserved (`a.txt` → `a 2.txt`). `None` after 9999
/// candidates (pathological; caller treats as an error).
///
/// The candidate file name is built by string concatenation, NOT
/// `Path::set_extension`: on a stem like `"a.tar 2"` set_extension
/// treats `"tar 2"` as the extension and *replaces* it, turning
/// `a.tar.gz` into `a.gz` — and collapsing every numbered candidate
/// to the same name, so the loop exhausted all 9999 and the whole
/// operation failed with "no free name".
pub fn pick_available_name(dest_dir: &Path, name: &OsStr, scheme: NameScheme) -> Option<PathBuf> {
    let as_path = Path::new(name);
    let stem = as_path.file_stem().unwrap_or(name).to_string_lossy();
    let ext = as_path.extension().map(|e| e.to_string_lossy());
    for n in 1..=9999u32 {
        let candidate_stem = match scheme {
            NameScheme::Copy if n == 1 => format!("{stem} copy"),
            NameScheme::Copy => format!("{stem} copy {n}"),
            // Numbered starts at 2: "name" exists, next is "name 2".
            NameScheme::Numbered => format!("{stem} {}", n + 1),
        };
        let candidate_name = match &ext {
            Some(e) => format!("{candidate_stem}.{e}"),
            None => candidate_stem,
        };
        let candidate = dest_dir.join(candidate_name);
        if !candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Whether two paths live on the same filesystem (move can use the
/// `rename` fast path, copy can use `clonefile`). Resolves against the
/// nearest existing ancestor so a not-yet-created destination still
/// answers. [win-parity: compare `GetVolumePathNameW` roots]
pub fn same_volume(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fn dev_of(p: &Path) -> Option<u64> {
            let mut cur = Some(p);
            while let Some(c) = cur {
                if let Ok(m) = fs::symlink_metadata(c) {
                    return Some(m.dev());
                }
                cur = c.parent();
            }
            None
        }
        match (dev_of(a), dev_of(b)) {
            (Some(da), Some(db)) => da == db,
            _ => false,
        }
    }
    #[cfg(windows)]
    {
        // Volume identity = the NTFS volume serial number from
        // GetFileInformationByHandle, on each path's nearest existing
        // ancestor. Comparing drive-letter prefixes would lie under
        // junction-mounted volumes (a D: volume mounted at C:\mnt\d);
        // the serial is the ground truth MoveFileEx itself honors.
        fn serial_of(p: &Path) -> Option<u32> {
            use std::os::windows::ffi::OsStrExt;
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
                OPEN_EXISTING,
            };
            use windows::core::PCWSTR;
            let wide: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
            // Access rights 0: attribute-only query; BACKUP_SEMANTICS lets
            // CreateFileW open directories.
            let handle = unsafe {
                CreateFileW(
                    PCWSTR::from_raw(wide.as_ptr()),
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    None,
                )
            }
            .ok()?;
            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            let got = unsafe { GetFileInformationByHandle(handle, &mut info) };
            unsafe {
                let _ = CloseHandle(handle);
            }
            got.ok().map(|()| info.dwVolumeSerialNumber)
        }
        fn serial_of_nearest(p: &Path) -> Option<u32> {
            let mut cur = Some(p);
            while let Some(c) = cur {
                if let Some(s) = serial_of(c) {
                    return Some(s);
                }
                cur = c.parent();
            }
            None
        }
        match (serial_of_nearest(a), serial_of_nearest(b)) {
            (Some(sa), Some(sb)) => sa == sb,
            _ => false,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Conservative: forces the copy+delete path, which is always
        // correct, just slower.
        let _ = (a, b);
        false
    }
}

/// Free bytes available to an unprivileged caller on the filesystem
/// holding `path` (not the volume root — any path under it answers).
/// `None` when the query isn't available (non-mac, or `statvfs` failed),
/// in which case the caller should skip the precheck rather than block a
/// transfer it can't size. [win-parity: GetDiskFreeSpaceExW]
#[cfg(target_os = "macos")]
pub fn available_space(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: zeroed statvfs is valid; `c` is a live NUL-terminated path.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    // f_bavail counts blocks available to non-root; f_frsize is the
    // fundamental block size (fall back to f_bsize if it's 0).
    let frsize = if s.f_frsize != 0 {
        s.f_frsize
    } else {
        s.f_bsize
    };
    Some((s.f_bavail as u64).saturating_mul(frsize as u64))
}

#[cfg(not(target_os = "macos"))]
pub fn available_space(_path: &Path) -> Option<u64> {
    None
}

/// Walk the sources, total the work (overall + per-source), and detect
/// top-level collisions. Increments `prog.note_planned()` as it walks so
/// the UI can show "Preparing — N items", then `begin_transfer` flips the
/// sink into its determinate phase. Errors when a source is missing, the
/// destination isn't a directory, or the destination sits *inside* a
/// source (copying a folder into its own subtree never terminates).
pub fn plan_transfer(
    sources: &[PathBuf],
    dest_dir: &Path,
    prog: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<OpPlan, String> {
    if !dest_dir.is_dir() {
        return Err(format!("{} is not a folder", dest_dir.display()));
    }
    let mut total_bytes = 0u64;
    let mut total_items = 0u64;
    let mut conflicts = Vec::new();
    let mut source_bytes = Vec::with_capacity(sources.len());
    let mut source_items = Vec::with_capacity(sources.len());
    for src in sources {
        let meta = fs::symlink_metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
        if dest_dir.starts_with(src) {
            return Err(format!(
                "can't copy \u{201c}{}\u{201d} into itself",
                src.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        let Some(name) = src.file_name() else {
            return Err(format!("{}: no file name", src.display()));
        };
        if dest_dir.join(name).exists() {
            conflicts.push(dest_dir.join(name));
        }
        let mut sb = 0u64;
        let mut si = 0u64;
        if meta.is_dir() && !meta.is_symlink() {
            // Iterative walk, same shape as recursive_size.
            let mut stack = vec![src.clone()];
            while let Some(dir) = stack.pop() {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                si += 1;
                prog.note_planned();
                let Ok(rd) = fs::read_dir(&dir) else { continue };
                for dirent in rd.flatten() {
                    let p = dirent.path();
                    let Ok(m) = fs::symlink_metadata(&p) else {
                        continue;
                    };
                    if m.is_dir() && !m.is_symlink() {
                        stack.push(p);
                    } else {
                        si += 1;
                        prog.note_planned();
                        sb = sb.saturating_add(m.len());
                    }
                }
            }
        } else {
            si += 1;
            prog.note_planned();
            sb = sb.saturating_add(meta.len());
        }
        total_bytes = total_bytes.saturating_add(sb);
        total_items = total_items.saturating_add(si);
        source_bytes.push(sb);
        source_items.push(si);
    }
    prog.begin_transfer(total_bytes, total_items);
    Ok(OpPlan {
        sources: sources.to_vec(),
        dest_dir: dest_dir.to_path_buf(),
        total_bytes,
        total_items,
        conflicts,
        source_bytes,
        source_items,
    })
}

/// What [`resolve_dest`] decided for one top-level item.
enum Resolution {
    /// Collision + Skip policy: leave the existing item, don't transfer.
    Skip,
    /// Transfer to `dst`. `replaced` is true when an existing destination was
    /// removed to make room (Replace policy).
    Proceed { dst: PathBuf, replaced: bool },
}

/// Resolve where a top-level item lands under `policy`. Replace deletes the
/// existing destination here — immediately before its copy starts, never
/// earlier. No longer mutates the outcome (the caller owns the counters now,
/// so a per-item failure can be recorded without double-counting).
fn resolve_dest(
    src: &Path,
    dest_dir: &Path,
    policy: CollisionPolicy,
) -> Result<Resolution, FileOpError> {
    let Some(name) = src.file_name() else {
        return Err(FileOpError::other(
            src,
            FileOpErrorKind::Other,
            format!("{}: no file name", src.display()),
        ));
    };
    let plain = dest_dir.join(name);
    if !plain.exists() && !plain.is_symlink() {
        return Ok(Resolution::Proceed {
            dst: plain,
            replaced: false,
        });
    }
    match policy {
        CollisionPolicy::Skip => Ok(Resolution::Skip),
        CollisionPolicy::Replace => {
            let meta =
                fs::symlink_metadata(&plain).map_err(|e| FileOpError::from_io(&e, &plain))?;
            let removed = if meta.is_dir() && !meta.is_symlink() {
                fs::remove_dir_all(&plain)
            } else {
                fs::remove_file(&plain)
            };
            removed.map_err(|e| FileOpError::from_io(&e, &plain))?;
            Ok(Resolution::Proceed {
                dst: plain,
                replaced: true,
            })
        }
        CollisionPolicy::KeepBoth => {
            match pick_available_name(dest_dir, name, NameScheme::Numbered) {
                Some(dst) => Ok(Resolution::Proceed {
                    dst,
                    replaced: false,
                }),
                None => Err(FileOpError::other(
                    &plain,
                    FileOpErrorKind::AlreadyExists,
                    format!(
                        "no free name for {} in {}",
                        name.to_string_lossy(),
                        dest_dir.display()
                    ),
                )),
            }
        }
    }
}

/// Copy one leaf file using the fastest available mechanism: `copyfile`
/// on macOS (sparse + metadata + intra-file progress/cancel), a chunked
/// read/write loop elsewhere. Returns `false` on cancellation (partial
/// destination removed). Credits bytes to `prog` as they land.
fn copy_leaf_file(
    src: &Path,
    dst: &Path,
    prog: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<bool, FileOpError> {
    #[cfg(target_os = "macos")]
    {
        mac::copy_file(src, dst, prog, cancel)
    }
    #[cfg(not(target_os = "macos"))]
    {
        copy_file_chunked(src, dst, prog, cancel)
    }
}

/// Chunked read/write fallback (non-macOS). Ticks `prog` per chunk and
/// honors `cancel` between chunks. A cancelled partial destination is
/// removed so nothing half-written survives. Carries permissions
/// best-effort (macOS uses `copyfile` which carries far more).
#[cfg(not(target_os = "macos"))]
fn copy_file_chunked(
    src: &Path,
    dst: &Path,
    prog: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<bool, FileOpError> {
    let mut reader = fs::File::open(src).map_err(|e| FileOpError::from_io(&e, src))?;
    let mut writer = fs::File::create(dst).map_err(|e| FileOpError::from_io(&e, dst))?;
    let mut buf = vec![0u8; COPY_CHUNK];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| FileOpError::from_io(&e, src))?;
        if n == 0 {
            break;
        }
        // Cancel only while bytes remain — a file whose final chunk
        // already landed is complete, not partial, and must survive.
        if cancel.load(Ordering::Relaxed) {
            drop(writer);
            let _ = fs::remove_file(dst);
            return Ok(false);
        }
        writer
            .write_all(&buf[..n])
            .map_err(|e| FileOpError::from_io(&e, dst))?;
        prog.add_bytes(n as u64);
    }
    if let Ok(meta) = fs::metadata(src) {
        let _ = fs::set_permissions(dst, meta.permissions());
    }
    Ok(true)
}

/// Copy one top-level item (file, dir tree, or symlink) by real byte
/// movement — the path taken when the instant clone isn't available
/// (cross-volume, non-APFS, non-mac). Returns `false` on cancellation;
/// the partially-copied current file is removed but already-completed
/// files inside the item stay (the caller reports the partial state).
fn copy_item(
    src: &Path,
    dst: &Path,
    prog: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<bool, FileOpError> {
    let meta = fs::symlink_metadata(src).map_err(|e| FileOpError::from_io(&e, src))?;
    if meta.is_symlink() {
        recreate_symlink(src, dst)?;
        prog.add_items(1);
        return Ok(true);
    }
    if !meta.is_dir() {
        prog.note_current(src);
        let ok = copy_leaf_file(src, dst, prog, cancel)?;
        if ok {
            prog.add_items(1);
        }
        return Ok(ok);
    }
    // Directory: depth-first with explicit stack of (src, dst) pairs.
    fs::create_dir_all(dst).map_err(|e| FileOpError::from_io(&e, dst))?;
    prog.add_items(1);
    let mut stack: Vec<(PathBuf, PathBuf)> = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((sdir, ddir)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let rd = fs::read_dir(&sdir).map_err(|e| FileOpError::from_io(&e, &sdir))?;
        for dirent in rd.flatten() {
            let sp = dirent.path();
            let Some(name) = sp.file_name() else { continue };
            let dp = ddir.join(name);
            let Ok(m) = fs::symlink_metadata(&sp) else {
                continue;
            };
            if m.is_symlink() {
                recreate_symlink(&sp, &dp)?;
                prog.add_items(1);
            } else if m.is_dir() {
                fs::create_dir_all(&dp).map_err(|e| FileOpError::from_io(&e, &dp))?;
                prog.add_items(1);
                stack.push((sp, dp));
            } else {
                prog.note_current(&sp);
                if !copy_leaf_file(&sp, &dp, prog, cancel)? {
                    return Ok(false);
                }
                prog.add_items(1);
            }
        }
    }
    Ok(true)
}

/// Recreate a symlink at `dst` pointing wherever `src` points.
/// Links are never followed (same stance as the disk-usage walker) —
/// copying a folder of symlinks must not balloon into copying their
/// targets.
fn recreate_symlink(src: &Path, dst: &Path) -> Result<(), FileOpError> {
    let target = fs::read_link(src).map_err(|e| FileOpError::from_io(&e, src))?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, dst).map_err(|e| FileOpError::from_io(&e, dst))
    }
    #[cfg(windows)]
    {
        // Windows symlinks are typed. Classify by the target's metadata,
        // resolving a relative target against the link's parent; an
        // unresolvable (dangling) target defaults to a file symlink, same as
        // what Explorer produces. std passes ALLOW_UNPRIVILEGED_CREATE, so
        // this works under Developer Mode without elevation; otherwise the
        // privilege error surfaces through the normal failure report.
        let resolved = if target.is_absolute() {
            target.clone()
        } else {
            src.parent()
                .map_or_else(|| target.clone(), |p| p.join(&target))
        };
        let is_dir = fs::metadata(&resolved).map(|m| m.is_dir()).unwrap_or(false);
        let made = if is_dir {
            std::os::windows::fs::symlink_dir(&target, dst)
        } else {
            std::os::windows::fs::symlink_file(&target, dst)
        };
        made.map_err(|e| FileOpError::from_io(&e, dst))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, dst);
        Err(FileOpError::other(
            src,
            FileOpErrorKind::Other,
            format!(
                "symlink {} not copied (unsupported on this platform)",
                src.display()
            ),
        ))
    }
}

/// Try the instant same-volume APFS clone for a whole top-level item.
/// Returns `true` only when the clone actually succeeded; `false` means
/// "fall back to a real byte copy" (cross-volume, non-APFS, non-mac, or
/// any clonefile refusal — clonefile is atomic, so a refusal leaves no
/// partial destination behind).
fn try_clone(src: &Path, dst: &Path, dest_dir: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        same_volume(src, dest_dir) && mac::clone_path(src, dst).is_ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (src, dst, dest_dir);
        false
    }
}

/// Copy every planned item into the destination. `policy_for` returns
/// the collision policy for each top-level source, consulted only when
/// that item's destination already exists. Progress is reported through
/// `prog` (cumulative bytes against `plan.total_bytes`).
pub fn run_copy(
    plan: &OpPlan,
    policy_for: &dyn Fn(&Path) -> CollisionPolicy,
    prog: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<OpOutcome, String> {
    let mut outcome = OpOutcome::default();
    for (i, src) in plan.sources.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            return Ok(outcome);
        }
        // A per-item resolution failure (bad name, failed Replace-delete, no
        // free name) is recorded and skipped — it no longer aborts the batch.
        let (dst, replaced) = match resolve_dest(src, &plan.dest_dir, policy_for(src)) {
            Ok(Resolution::Skip) => {
                outcome.skipped += 1;
                continue;
            }
            Ok(Resolution::Proceed { dst, replaced }) => (dst, replaced),
            Err(e) => {
                outcome.failed.push(e);
                continue;
            }
        };
        if replaced {
            outcome.replaced += 1;
        }
        prog.note_current(src);
        // Tier 1: same-volume clone — instant, whole tree, zero bytes.
        let item = if try_clone(src, &dst, &plan.dest_dir) {
            prog.add_bytes(plan.source_bytes.get(i).copied().unwrap_or(0));
            prog.add_items(plan.source_items.get(i).copied().unwrap_or(0));
            Ok(true)
        } else {
            copy_item(src, &dst, prog, cancel)
        };
        match item {
            Ok(true) => outcome.created.push((src.clone(), dst)),
            // Cancellation mid-item stops the whole batch (partial reported).
            Ok(false) => {
                outcome.cancelled = true;
                return Ok(outcome);
            }
            // A hard failure on this item is recorded; the batch continues.
            Err(e) => outcome.failed.push(e),
        }
    }
    Ok(outcome)
}

/// Move every planned item. Same-volume items take the `rename` fast
/// path (instant — their planned bytes tick through progress in one
/// jump); cross-volume items copy then delete the source, and the
/// delete only runs when that item's copy fully succeeded.
pub fn run_move(
    plan: &OpPlan,
    policy_for: &dyn Fn(&Path) -> CollisionPolicy,
    prog: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<OpOutcome, String> {
    let mut outcome = OpOutcome::default();
    for (i, src) in plan.sources.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            return Ok(outcome);
        }
        let (dst, replaced) = match resolve_dest(src, &plan.dest_dir, policy_for(src)) {
            Ok(Resolution::Skip) => {
                outcome.skipped += 1;
                continue;
            }
            Ok(Resolution::Proceed { dst, replaced }) => (dst, replaced),
            Err(e) => {
                outcome.failed.push(e);
                continue;
            }
        };
        if replaced {
            outcome.replaced += 1;
        }
        prog.note_current(src);
        let item: Result<bool, FileOpError> = if same_volume(src, &plan.dest_dir) {
            match fs::rename(src, &dst) {
                Ok(()) => {
                    // Credit the whole item's bytes/items in one jump.
                    prog.add_bytes(plan.source_bytes.get(i).copied().unwrap_or(0));
                    prog.add_items(plan.source_items.get(i).copied().unwrap_or(0));
                    Ok(true)
                }
                Err(e) => Err(FileOpError::from_io(&e, src)),
            }
        } else {
            // Cross-volume: copy, then delete the source only when its copy
            // fully landed (a cancelled copy leaves the source intact).
            match copy_item(src, &dst, prog, cancel) {
                Ok(false) => Ok(false),
                Ok(true) => match fs::symlink_metadata(src) {
                    Ok(meta) => {
                        let removed = if meta.is_dir() && !meta.is_symlink() {
                            fs::remove_dir_all(src)
                        } else {
                            fs::remove_file(src)
                        };
                        removed
                            .map(|_| true)
                            .map_err(|e| FileOpError::from_io(&e, src))
                    }
                    Err(e) => Err(FileOpError::from_io(&e, src)),
                },
                Err(e) => Err(e),
            }
        };
        match item {
            Ok(true) => outcome.created.push((src.clone(), dst)),
            Ok(false) => {
                outcome.cancelled = true;
                return Ok(outcome);
            }
            Err(e) => outcome.failed.push(e),
        }
    }
    Ok(outcome)
}

/// macOS-native copy primitives: `clonefile(2)` (instant CoW clone) and
/// `copyfile(3)` with a status callback (sparse-aware, metadata-aware,
/// cancellable byte copy). Kept self-contained behind one cfg so the
/// rest of the engine stays platform-neutral.
#[cfg(target_os = "macos")]
mod mac {
    use super::{AtomicBool, FileOpError, FileOpErrorKind, Ordering, Path, TransferProgress};
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    /// `clonefile` flag: clone a symlink as a symlink (don't follow it),
    /// matching the engine's never-follow-links stance.
    const CLONE_NOFOLLOW: u32 = 0x0001;

    fn cstr(p: &Path) -> Option<CString> {
        CString::new(p.as_os_str().as_bytes()).ok()
    }

    /// APFS copy-on-write clone of `src` → `dst` (file or whole tree),
    /// instant and zero-copy. Caller guarantees `dst` does not yet exist
    /// (clonefile requires it). Any error is the caller's cue to fall
    /// back to a real byte copy.
    pub fn clone_path(src: &Path, dst: &Path) -> std::io::Result<()> {
        let (Some(s), Some(d)) = (cstr(src), cstr(dst)) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path has interior NUL",
            ));
        };
        // SAFETY: both pointers are valid NUL-terminated C strings that
        // outlive the call; clonefile reads them and returns.
        let rc = unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), CLONE_NOFOLLOW) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Context threaded into the C status callback. Holds raw pointers to
    /// the progress sink and cancel flag, both of which outlive the
    /// `copyfile` call below (they're borrowed for its whole duration),
    /// plus the running per-file copied total so we can credit deltas.
    struct CbCtx {
        prog: *const TransferProgress,
        cancel: *const AtomicBool,
        last_copied: i64,
    }

    /// copyfile status callback. Fires during the data copy; we credit
    /// the byte delta to `prog` and abort (COPYFILE_QUIT) the instant the
    /// cancel flag is set — that's our intra-file cancellation.
    extern "C" fn status_cb(
        what: libc::c_int,
        _stage: libc::c_int,
        state: libc::copyfile_state_t,
        _src: *const libc::c_char,
        _dst: *const libc::c_char,
        ctx: *mut libc::c_void,
    ) -> libc::c_int {
        if ctx.is_null() {
            return libc::COPYFILE_CONTINUE;
        }
        // SAFETY: `ctx` is the `&mut CbCtx` we handed copyfile via
        // COPYFILE_STATE_STATUS_CTX; it lives across the whole call.
        let ctx = unsafe { &mut *(ctx as *mut CbCtx) };
        // SAFETY: cancel outlives the copyfile call (borrowed below).
        if unsafe { &*ctx.cancel }.load(Ordering::Relaxed) {
            return libc::COPYFILE_QUIT;
        }
        if what == libc::COPYFILE_COPY_DATA {
            let mut copied: libc::off_t = 0;
            // SAFETY: COPYFILE_STATE_COPIED yields an off_t (bytes copied
            // so far); `state` is the live state object.
            let ok = unsafe {
                libc::copyfile_state_get(
                    state,
                    libc::COPYFILE_STATE_COPIED as u32,
                    &mut copied as *mut libc::off_t as *mut libc::c_void,
                )
            };
            if ok == 0 {
                let delta = copied as i64 - ctx.last_copied;
                if delta > 0 {
                    // SAFETY: prog outlives the copyfile call.
                    unsafe { &*ctx.prog }.add_bytes(delta as u64);
                    ctx.last_copied = copied as i64;
                }
            }
        }
        libc::COPYFILE_CONTINUE
    }

    /// Cross-volume single-file copy via `copyfile(3)`. Preserves holes
    /// (sparse), xattrs, ACLs, BSD flags, and timestamps
    /// (`COPYFILE_DATA | COPYFILE_METADATA | COPYFILE_DATA_SPARSE`).
    /// Progress + cancellation ride the status callback. Returns
    /// `Ok(false)` if cancelled (partial destination removed). `src` must
    /// be a regular file — symlinks and directories are handled by the
    /// walker in `copy_item`.
    pub fn copy_file(
        src: &Path,
        dst: &Path,
        prog: &TransferProgress,
        cancel: &AtomicBool,
    ) -> Result<bool, FileOpError> {
        let (Some(s), Some(d)) = (cstr(src), cstr(dst)) else {
            return Err(FileOpError::other(
                src,
                FileOpErrorKind::Other,
                format!("{}: path has interior NUL", src.display()),
            ));
        };
        // SAFETY: alloc returns an opaque handle we free below.
        let state = unsafe { libc::copyfile_state_alloc() };
        if state.is_null() {
            return Err(FileOpError::other(
                src,
                FileOpErrorKind::Other,
                format!("{}: copyfile_state_alloc failed", src.display()),
            ));
        }
        let mut ctx = CbCtx {
            prog: prog as *const _,
            cancel: cancel as *const _,
            last_copied: 0,
        };
        let flags = (libc::COPYFILE_DATA
            | libc::COPYFILE_METADATA
            | libc::COPYFILE_NOFOLLOW
            | libc::COPYFILE_DATA_SPARSE) as libc::copyfile_flags_t;
        // SAFETY: state is live; the callback fn pointer and ctx both
        // outlive the copyfile call; the C strings are valid.
        let rc = unsafe {
            libc::copyfile_state_set(
                state,
                libc::COPYFILE_STATE_STATUS_CB as u32,
                status_cb as *const libc::c_void,
            );
            libc::copyfile_state_set(
                state,
                libc::COPYFILE_STATE_STATUS_CTX as u32,
                &mut ctx as *mut CbCtx as *const libc::c_void,
            );
            libc::copyfile(s.as_ptr(), d.as_ptr(), state, flags)
        };
        let err = std::io::Error::last_os_error();
        // SAFETY: state was allocated above and not freed yet.
        unsafe {
            libc::copyfile_state_free(state);
        }
        if rc == 0 {
            return Ok(true);
        }
        // The callback returned COPYFILE_QUIT → copyfile fails with
        // ECANCELED. Treat any cancel-flagged failure as cancellation,
        // remove the partial, and report it honestly (not an error).
        if cancel.load(Ordering::Relaxed) || err.raw_os_error() == Some(libc::ECANCELED) {
            let _ = std::fs::remove_file(dst);
            return Ok(false);
        }
        let _ = std::fs::remove_file(dst);
        // The copy itself failed — classify the captured OS error against the
        // source (the item the user asked to move/copy).
        Err(FileOpError::from_io(&err, src))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ferail-fileops-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(p: &Path, contents: &[u8]) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, contents).unwrap();
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn copies_a_tree_with_progress() {
        let root = scratch("copy-tree");
        let src = root.join("src/proj");
        write(&src.join("a.txt"), b"hello");
        write(&src.join("sub/b.bin"), &[7u8; 1000]);
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();

        let cancel = no_cancel();
        let prog = TransferProgress::new();
        let plan = plan_transfer(std::slice::from_ref(&src), &dest, &prog, &cancel).unwrap();
        assert_eq!(plan.total_bytes, 1005);
        assert!(plan.conflicts.is_empty());
        assert!(!prog.is_planning());
        assert_eq!(prog.bytes_total(), 1005);

        let out = run_copy(&plan, &|_| CollisionPolicy::KeepBoth, &prog, &cancel).unwrap();
        assert!(!out.cancelled);
        assert_eq!(out.created.len(), 1);
        // Whole item's bytes credited, whether by clone (mac) or copy.
        assert_eq!(prog.bytes_done(), 1005);
        assert_eq!(fs::read(dest.join("proj/a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(dest.join("proj/sub/b.bin")).unwrap().len(), 1000);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn keep_both_picks_numbered_names() {
        let root = scratch("keep-both");
        let src = root.join("src/a.txt");
        write(&src, b"new");
        let dest = root.join("dest");
        write(&dest.join("a.txt"), b"old");
        write(&dest.join("a 2.txt"), b"older");

        let cancel = no_cancel();
        let prog = TransferProgress::new();
        let plan = plan_transfer(&[src], &dest, &prog, &cancel).unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        let out = run_copy(&plan, &|_| CollisionPolicy::KeepBoth, &prog, &cancel).unwrap();
        assert_eq!(out.created[0].1, dest.join("a 3.txt"));
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"old");
        assert_eq!(fs::read(dest.join("a 3.txt")).unwrap(), b"new");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn keep_both_preserves_multi_dot_names() {
        let root = scratch("keep-both-multidot");
        let dest = root.join("dest");
        write(&dest.join("a.tar.gz"), b"old");

        // `a.tar.gz` collides → the candidate must be `a.tar 2.gz`,
        // not `a.gz` (set_extension on stem "a.tar 2" replaced the
        // "tar 2" pseudo-extension and collapsed every candidate to
        // the same name).
        let picked = pick_available_name(
            &dest,
            std::ffi::OsStr::new("a.tar.gz"),
            NameScheme::Numbered,
        )
        .unwrap();
        assert_eq!(picked, dest.join("a.tar 2.gz"));

        // With the first candidate occupied, numbering advances
        // instead of failing after 9999 identical candidates.
        write(&dest.join("a.tar 2.gz"), b"older");
        let picked = pick_available_name(
            &dest,
            std::ffi::OsStr::new("a.tar.gz"),
            NameScheme::Numbered,
        )
        .unwrap();
        assert_eq!(picked, dest.join("a.tar 3.gz"));

        // Duplicate flavor keeps the full extension too.
        let picked =
            pick_available_name(&dest, std::ffi::OsStr::new("a.tar.gz"), NameScheme::Copy).unwrap();
        assert_eq!(picked, dest.join("a.tar copy.gz"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn replace_and_skip_policies() {
        let root = scratch("replace-skip");
        let src_a = root.join("src/a.txt");
        let src_b = root.join("src/b.txt");
        write(&src_a, b"new-a");
        write(&src_b, b"new-b");
        let dest = root.join("dest");
        write(&dest.join("a.txt"), b"old-a");
        write(&dest.join("b.txt"), b"old-b");

        let cancel = no_cancel();
        let prog = TransferProgress::new();
        let plan = plan_transfer(std::slice::from_ref(&src_a), &dest, &prog, &cancel).unwrap();
        let out = run_copy(&plan, &|_| CollisionPolicy::Replace, &prog, &cancel).unwrap();
        assert_eq!(out.replaced, 1);
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"new-a");

        let prog2 = TransferProgress::new();
        let plan = plan_transfer(&[src_b], &dest, &prog2, &cancel).unwrap();
        let out = run_copy(&plan, &|_| CollisionPolicy::Skip, &prog2, &cancel).unwrap();
        assert_eq!(out.skipped, 1);
        assert!(out.created.is_empty());
        assert_eq!(fs::read(dest.join("b.txt")).unwrap(), b"old-b");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_before_run_copies_nothing() {
        let root = scratch("cancel-pre");
        let src = root.join("src/a.bin");
        write(&src, &[1u8; 500]);
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();

        let prog = TransferProgress::new();
        let plan = plan_transfer(&[src], &dest, &prog, &no_cancel()).unwrap();
        let cancel = AtomicBool::new(true);
        let out = run_copy(&plan, &|_| CollisionPolicy::KeepBoth, &prog, &cancel).unwrap();
        assert!(out.cancelled);
        assert!(out.created.is_empty());
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&root);
    }

    /// A cancel that lands mid-file must leave no half-written
    /// destination — the guarantee undo and the user both rely on. We
    /// copy a file large enough that it can't finish in a single
    /// callback/chunk, then a watcher trips `cancel` the moment any bytes
    /// have moved. (Exercises copyfile's COPYFILE_QUIT on mac, the
    /// chunked loop's between-chunk check elsewhere.)
    #[test]
    fn cancel_mid_file_removes_partial() {
        use std::sync::atomic::Ordering;
        let root = scratch("cancel-midfile");
        let src = root.join("big.bin");
        write(&src, &vec![9u8; 32 * 1024 * 1024]);
        let dst = root.join("big-copy.bin");

        let prog = Arc::new(TransferProgress::new());
        prog.begin_transfer(32 * 1024 * 1024, 1);
        let cancel = Arc::new(AtomicBool::new(false));
        let watcher = {
            let prog = prog.clone();
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                // Spin until the copy has moved *some* but not all bytes,
                // then cancel.
                let total = prog.bytes_total();
                loop {
                    let d = prog.bytes_done();
                    if d > 0 && d < total {
                        cancel.store(true, Ordering::Relaxed);
                        return;
                    }
                    if d >= total {
                        return; // finished before we could catch it
                    }
                    std::hint::spin_loop();
                }
            })
        };
        let ok = copy_leaf_file(&src, &dst, &prog, &cancel).unwrap();
        let _ = watcher.join();
        // If the watcher caught it mid-flight the copy was cancelled and
        // the partial removed; if the file was tiny enough to finish in
        // one shot it completed cleanly. Either way: never a partial.
        if !ok {
            assert!(!dst.exists(), "cancelled copy left a partial destination");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn move_renames_on_same_volume() {
        let root = scratch("move");
        let src = root.join("src/dir");
        write(&src.join("f.txt"), b"payload");
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();
        assert!(same_volume(&src, &dest));

        let cancel = no_cancel();
        let prog = TransferProgress::new();
        let plan = plan_transfer(std::slice::from_ref(&src), &dest, &prog, &cancel).unwrap();
        let out = run_move(&plan, &|_| CollisionPolicy::KeepBoth, &prog, &cancel).unwrap();
        assert!(!src.exists());
        assert_eq!(fs::read(dest.join("dir/f.txt")).unwrap(), b"payload");
        assert_eq!(out.created, vec![(src, dest.join("dir"))]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_rejects_dest_inside_source() {
        let root = scratch("self-copy");
        let src = root.join("outer");
        let dest = src.join("inner");
        fs::create_dir_all(&dest).unwrap();
        let cancel = no_cancel();
        let prog = TransferProgress::new();
        let err = plan_transfer(&[src], &dest, &prog, &cancel).unwrap_err();
        assert!(err.contains("into itself"), "{err}");
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_recreated_not_followed() {
        let root = scratch("symlink");
        let src = root.join("src/dir");
        write(&src.join("real.txt"), b"data");
        std::os::unix::fs::symlink("real.txt", src.join("link")).unwrap();
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();

        let cancel = no_cancel();
        let prog = TransferProgress::new();
        let plan = plan_transfer(&[src], &dest, &prog, &cancel).unwrap();
        run_copy(&plan, &|_| CollisionPolicy::KeepBoth, &prog, &cancel).unwrap();
        let copied_link = dest.join("dir/link");
        assert!(fs::symlink_metadata(&copied_link).unwrap().is_symlink());
        assert_eq!(
            fs::read_link(&copied_link).unwrap(),
            PathBuf::from("real.txt")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pick_available_name_schemes() {
        let root = scratch("naming");
        write(&root.join("a.txt"), b"x");
        write(&root.join("a 2.txt"), b"x");
        assert_eq!(
            pick_available_name(&root, OsStr::new("a.txt"), NameScheme::Numbered).unwrap(),
            root.join("a 3.txt")
        );
        assert_eq!(
            pick_available_name(&root, OsStr::new("a.txt"), NameScheme::Copy).unwrap(),
            root.join("a copy.txt")
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A same-volume copy clones (CoW) on APFS; modifying the source
    /// afterward must not bleed into the copy. Proves the copy is a
    /// fully independent file, clone or not.
    #[test]
    fn copy_is_independent_of_source() {
        let root = scratch("independent");
        let src = root.join("src/data.bin");
        write(&src, &[1u8; 4096]);
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();

        let cancel = no_cancel();
        let prog = TransferProgress::new();
        let plan = plan_transfer(std::slice::from_ref(&src), &dest, &prog, &cancel).unwrap();
        run_copy(&plan, &|_| CollisionPolicy::KeepBoth, &prog, &cancel).unwrap();
        // Mutate the source after the copy/clone.
        fs::write(&src, [2u8; 4096]).unwrap();
        let copied = fs::read(dest.join("data.bin")).unwrap();
        assert_eq!(copied, vec![1u8; 4096], "copy must not track source edits");
        let _ = fs::remove_dir_all(&root);
    }

    /// copyfile must carry xattrs (Finder tags, quarantine, where-from
    /// all ride xattrs) — the headline reason for moving off the
    /// hand-rolled byte loop, which dropped them. Exercises the copyfile
    /// path directly so it's covered regardless of volume layout.
    #[cfg(target_os = "macos")]
    #[test]
    fn copyfile_preserves_xattr() {
        let root = scratch("xattr");
        let src = root.join("tagged.txt");
        write(&src, b"hello");
        xattr::set(&src, "com.ferail.test", b"v1").unwrap();
        let dst = root.join("tagged-copy.txt");

        let prog = TransferProgress::new();
        prog.begin_transfer(5, 1);
        let ok = mac::copy_file(&src, &dst, &prog, &no_cancel()).unwrap();
        assert!(ok);
        assert_eq!(fs::read(&dst).unwrap(), b"hello");
        let got = xattr::get(&dst, "com.ferail.test").unwrap();
        assert_eq!(got.as_deref(), Some(&b"v1"[..]), "xattr lost in copy");
        let _ = fs::remove_dir_all(&root);
    }

    /// Free-space query answers for an ordinary directory (not just a
    /// volume root) and reports a plausible nonzero figure — that's what
    /// the cross-volume precheck relies on.
    #[cfg(target_os = "macos")]
    #[test]
    fn available_space_reports_free_bytes() {
        let root = scratch("freespace");
        let avail = available_space(&root).expect("statvfs answers for a temp dir");
        assert!(avail > 0, "expected nonzero free space, got {avail}");
        let _ = fs::remove_dir_all(&root);
    }

    /// copyfile must not inflate a sparse file into fully-allocated
    /// blocks (a VM image / disk image would balloon). We make a file
    /// with a large hole and assert the copy's allocated blocks didn't
    /// grow past the source's.
    #[cfg(target_os = "macos")]
    #[test]
    fn copyfile_preserves_sparse() {
        use std::os::unix::fs::MetadataExt;
        let root = scratch("sparse");
        let src = root.join("sparse.bin");
        {
            let f = fs::File::create(&src).unwrap();
            // 64 MiB logical size, almost entirely a hole.
            f.set_len(64 * 1024 * 1024).unwrap();
        }
        // Touch a few bytes near the end so there's real data too.
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = fs::OpenOptions::new().write(true).open(&src).unwrap();
            f.seek(SeekFrom::Start(64 * 1024 * 1024 - 8)).unwrap();
            f.write_all(b"tailtail").unwrap();
        }
        let dst = root.join("sparse-copy.bin");
        let prog = TransferProgress::new();
        prog.begin_transfer(64 * 1024 * 1024, 1);
        mac::copy_file(&src, &dst, &prog, &no_cancel()).unwrap();

        let src_blocks = fs::metadata(&src).unwrap().blocks();
        let dst_blocks = fs::metadata(&dst).unwrap().blocks();
        // Logical sizes match; allocated blocks of the copy don't exceed
        // the source's (sparseness preserved, not inflated).
        assert_eq!(
            fs::metadata(&dst).unwrap().len(),
            64 * 1024 * 1024,
            "logical size must match"
        );
        assert!(
            dst_blocks <= src_blocks,
            "copy inflated sparse file: src {src_blocks} blocks, dst {dst_blocks}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A hard failure on one top-level item must not abandon the rest: the
    /// engine records it in `outcome.failed` (classified) and keeps copying.
    /// Driven deterministically (no uid/permission dependence) by handing
    /// `run_copy` a plan whose second source never existed — its copy faults
    /// `NotFound` while the first lands normally.
    #[test]
    fn partial_failure_continues_and_is_recorded() {
        let root = scratch("partial-failure");
        let good = root.join("src/good.txt");
        write(&good, b"kept");
        let ghost = root.join("src/ghost.txt"); // never created
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();

        let cancel = no_cancel();
        let prog = TransferProgress::new();
        // Build the plan by hand so the missing source reaches run_copy
        // (plan_transfer would reject it up front — that's a separate path).
        let plan = OpPlan {
            sources: vec![good.clone(), ghost.clone()],
            dest_dir: dest.clone(),
            total_bytes: 4,
            total_items: 2,
            conflicts: vec![],
            source_bytes: vec![4, 0],
            source_items: vec![1, 1],
        };
        let out = run_copy(&plan, &|_| CollisionPolicy::KeepBoth, &prog, &cancel).unwrap();

        assert!(!out.cancelled);
        assert_eq!(out.created.len(), 1, "the good item still copied");
        assert_eq!(fs::read(dest.join("good.txt")).unwrap(), b"kept");
        assert!(out.has_failures());
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].kind, FileOpErrorKind::NotFound);
        assert_eq!(out.failed[0].item_label(), "ghost.txt");
        let _ = fs::remove_dir_all(&root);
    }

    /// The classifier turns raw OS errors into the kinds the UI keys off —
    /// especially the elevation-recoverable / lock distinction that drives
    /// the Retry-as-administrator vs. who's-locking-it affordances.
    #[cfg(unix)]
    #[test]
    fn classify_maps_os_errors_to_kinds() {
        let p = Path::new("/tmp/whatever");
        let of = |code: i32| FileOpError::from_io(&std::io::Error::from_raw_os_error(code), p);

        assert_eq!(of(libc::EACCES).kind, FileOpErrorKind::PermissionDenied);
        assert_eq!(of(libc::EPERM).kind, FileOpErrorKind::PermissionDenied);
        assert_eq!(of(libc::ETXTBSY).kind, FileOpErrorKind::Locked);
        assert_eq!(of(libc::ENOSPC).kind, FileOpErrorKind::NoSpace);
        assert_eq!(of(libc::EROFS).kind, FileOpErrorKind::ReadOnly);
        assert_eq!(of(libc::ENOENT).kind, FileOpErrorKind::NotFound);

        // The predicates the GPUI layer gates retry/escalation on.
        assert!(FileOpErrorKind::PermissionDenied.is_elevation_recoverable());
        assert!(!FileOpErrorKind::Locked.is_elevation_recoverable());
        assert!(FileOpErrorKind::Locked.is_lock());
        assert!(of(libc::EACCES).os_code.is_some());
    }
}
