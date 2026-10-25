//! Persistent metadata store. SQLite-backed; schema-versioned; lives at
//! the platform-conventional app-data dir in production (see
//! [`default_db_path`]: Application Support on macOS, %APPDATA% on
//! Windows, XDG data dir elsewhere) and in-memory in tests.
//!
//! A single durable substrate for derived metadata: Ant Trail heat,
//! magic cache, quarantine cache, Disk Usage prefs. Ported from the Ferail
//! predecessor's `ferail-core::metadata` (`docs/done/ANT_TRAIL.md`,
//! `MAGIC_SNIFFING.md`); schema reused with macOS-flavored adjustments.
//!
//! All operations are blocking; the store should be wrapped in a
//! `Mutex`/`RwLock` at the App layer or driven from a worker thread.
//! Per the UI-nonblocking contract, read/write calls must NOT happen on
//! the paint thread.

pub mod db;

pub use db::{
    AntTrailEntry, FileMetaRecord, FolderSizeRecord, LayoutState, MetadataDb, MetadataError,
    ResetScope, Result, TabState, WindowState,
};
pub use ferail_core::favorites::{
    Favorite, FavoriteIcon, FavoriteId, FavoriteKind, FavoriteSort, FavoriteState, FavoriteTarget,
};

/// Default on-disk location for the metadata DB, per platform
/// convention:
///   macOS   `~/Library/Application Support/Ferail/metadata.db`
///   Windows `%APPDATA%\Ferail\metadata.db`
///   other   `$XDG_DATA_HOME/ferail/metadata.db`, falling back to
///           `~/.local/share/ferail/metadata.db`
/// Returns `None` when the platform's base env var is unset (bare
/// test / CI environments): callers fall back to
/// `MetadataDb::in_memory` in that case, losing persistence but
/// keeping every feature functional.
#[cfg(target_os = "macos")]
pub fn default_db_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = std::path::PathBuf::from(home);
    p.push("Library");
    p.push("Application Support");
    p.push("Ferail");
    p.push("metadata.db");
    Some(p)
}

#[cfg(windows)]
pub fn default_db_path() -> Option<std::path::PathBuf> {
    // Roaming AppData: the conventional home for per-user app state
    // that isn't cache (caches would go to %LOCALAPPDATA%). The DB is
    // small (KBs of preferences + Ant Trail counts), so roaming is
    // appropriate and survives profile-synced enterprise setups.
    let appdata = std::env::var_os("APPDATA")?;
    let mut p = std::path::PathBuf::from(appdata);
    p.push("Ferail");
    p.push("metadata.db");
    Some(p)
}

/// AROS: application state that is no document lives in `HOME:Data/<Name>`
/// of the user's home assign (never `SYS:`, which a system update replaces).
/// No environment variable, no fallback: without `HOME:` the database is
/// in memory and the caller says so. A DOS path: the bundled SQLite is
/// patched to take a path with a device or assign name as absolute
/// (`crates/libsqlite3-sys-0.28.0-aros.diff` in pomaros/rust-aros); the
/// C library and DOS pass such paths through unchanged.
#[cfg(target_os = "aros")]
pub fn default_db_path() -> Option<std::path::PathBuf> {
    static HAS_HOME: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    HAS_HOME
        .get_or_init(|| std::path::Path::new("HOME:").is_dir())
        .then(|| std::path::PathBuf::from("HOME:Data/Ferail/metadata.db"))
}

#[cfg(not(any(target_os = "macos", windows, target_os = "aros")))]
pub fn default_db_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = std::path::PathBuf::from(h);
                p.push(".local");
                p.push("share");
                p
            })
        })?;
    let mut p = base;
    p.push("ferail");
    p.push("metadata.db");
    Some(p)
}

/// Ensure the parent directory of `path` exists. Returns `Ok(())`
/// if the directory was created, was already there, or `path` has
/// no parent. Logs and returns the error otherwise.
pub fn ensure_parent_dir(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        create_dir_all_compat(parent)?;
    }
    Ok(())
}

/// `std::fs::create_dir_all` replacement that creates one component at a
/// time. A DOS handler may answer a mkdir whose parent is missing with an
/// error `create_dir_all`'s recover-on-NotFound recursion does not expect;
/// component-wise creation never asks that question. Identical behavior
/// elsewhere.
pub fn create_dir_all_compat(path: &std::path::Path) -> std::io::Result<()> {
    let mut cur = std::path::PathBuf::new();
    for comp in path.components() {
        cur.push(comp);
        match std::fs::create_dir(&cur) {
            Ok(()) => {}
            // Roots/prefixes ("/", "SYS:") and already-created components
            // error with platform-dependent codes; existing-dir is always
            // fine whatever the errno said.
            Err(_) if cur.is_dir() => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
