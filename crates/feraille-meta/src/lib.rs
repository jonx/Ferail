//! Persistent metadata store. SQLite-backed; schema-versioned; lives at
//! the platform-conventional app-data dir in production (see
//! [`default_db_path`]: Application Support on macOS, %APPDATA% on
//! Windows, XDG data dir elsewhere) and in-memory in tests.
//!
//! Replaces the hand-rolled in-memory caches scattered across
//! `feraille-app` (Ant Trail heat, magic cache, quarantine cache, Disk
//! Usage prefs) with a single durable substrate. Ported from the Ferail
//! predecessor's `ferail-core::metadata` (`docs/done/ANT_TRAIL.md`,
//! `MAGIC_SNIFFING.md`); schema reused with macOS-flavored adjustments.
//!
//! All operations are blocking; the store should be wrapped in a
//! `Mutex`/`RwLock` at the App layer or driven from a worker thread.
//! Per the UI-nonblocking contract, read/write calls must NOT happen on
//! the paint thread.

pub mod cache;
pub mod db;

pub use cache::MetadataCache;
pub use db::{
    AntTrailEntry, FileMetaRecord, LayoutState, MetadataDb, MetadataError, ResetScope,
    Result, TabState, WindowState,
};
pub use feraille_core::favorites::{
    Favorite, FavoriteIcon, FavoriteId, FavoriteKind, FavoriteSort, FavoriteState, FavoriteTarget,
};

/// Default on-disk location for the metadata DB, per platform
/// convention:
///   macOS   `~/Library/Application Support/Feraille/metadata.db`
///   Windows `%APPDATA%\Feraille\metadata.db`
///   other   `$XDG_DATA_HOME/feraille/metadata.db`, falling back to
///           `~/.local/share/feraille/metadata.db`
/// Returns `None` when the platform's base env var is unset (bare
/// test / CI environments) — callers fall back to
/// `MetadataDb::in_memory` in that case, losing persistence but
/// keeping every feature functional.
#[cfg(target_os = "macos")]
pub fn default_db_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = std::path::PathBuf::from(home);
    p.push("Library");
    p.push("Application Support");
    p.push("Feraille");
    p.push("metadata.db");
    Some(p)
}

#[cfg(windows)]
pub fn default_db_path() -> Option<std::path::PathBuf> {
    // Roaming AppData — the conventional home for per-user app state
    // that isn't cache (caches would go to %LOCALAPPDATA%). The DB is
    // small (KBs of preferences + Ant Trail counts), so roaming is
    // appropriate and survives profile-synced enterprise setups.
    let appdata = std::env::var_os("APPDATA")?;
    let mut p = std::path::PathBuf::from(appdata);
    p.push("Feraille");
    p.push("metadata.db");
    Some(p)
}

#[cfg(not(any(target_os = "macos", windows)))]
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
    p.push("feraille");
    p.push("metadata.db");
    Some(p)
}

/// Ensure the parent directory of `path` exists. Returns `Ok(())`
/// if the directory was created, was already there, or `path` has
/// no parent. Logs and returns the error otherwise.
pub fn ensure_parent_dir(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}
