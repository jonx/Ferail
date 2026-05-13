//! Persistent metadata store. SQLite-backed; schema-versioned; lives at
//! `~/Library/Application Support/Feraille/metadata.db` in production and
//! in-memory in tests.
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

/// Default on-disk location for the metadata DB on macOS:
/// `~/Library/Application Support/Feraille/metadata.db`. Returns
/// `None` when `$HOME` is unset (test / CI environments) — callers
/// should fall back to `MetadataDb::in_memory` in that case.
pub fn default_db_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = std::path::PathBuf::from(home);
    p.push("Library");
    p.push("Application Support");
    p.push("Feraille");
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
