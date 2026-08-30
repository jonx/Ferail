//! Persistent hash cache for the duplicate finder, backing
//! [`ferail_fs_native::DupeHashCache`] with the `files` table.
//!
//! This is the single biggest speed lever in the funnel
//! ([docs/features/DUPLICATES.md]): full-hashing a tree the first time is
//! disk-bound and slow; a rescan that finds the same `(path, size,
//! mtime)` reuses the stored BLAKE3 and never re-reads the file. The
//! `files` table already carries `full_hash` with an index, and
//! `upsert_file` drops stale derived data when an mtime changes, so a
//! changed file can never resolve to its old hash.
//!
//! Mirrors the read-through / write-through pattern the prefetch worker
//! uses against the same `Arc<Mutex<MetadataDb>>`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use ferail_fs_native::DupeHashCache;
use ferail_meta::{FileMetaRecord, MetadataDb};

/// `DupeHashCache` over the shared metadata DB. Cheap to construct; holds
/// only the shared handle. `indexed_at_unix` is stamped at construction
/// so the worker, which can't read the clock cheaply on the hot path:
/// writes a consistent timestamp for the whole scan.
pub struct DbHashCache {
    db: Arc<Mutex<MetadataDb>>,
    indexed_at_unix: i64,
}

impl DbHashCache {
    pub fn new(db: Arc<Mutex<MetadataDb>>, indexed_at_unix: i64) -> Self {
        Self {
            db,
            indexed_at_unix,
        }
    }
}

impl DupeHashCache for DbHashCache {
    fn get_full(&self, path: &Path, size: u64, mtime_unix: i64) -> Option<String> {
        let path_str = path.to_str()?;
        let guard = self.db.lock().ok()?;
        let rec = guard.get_file(path_str).ok().flatten()?;
        // Only trust the stored hash when size *and* mtime still match:
        // a renamed-in-place or rewritten file at the same path must be
        // re-hashed. (upsert_file also guards mtime, this is belt-and-
        // suspenders against partial rows.)
        if rec.size == size && rec.mtime_unix == mtime_unix {
            rec.full_hash
        } else {
            None
        }
    }

    fn put_full(&self, path: &Path, size: u64, mtime_unix: i64, hash: &str) {
        let Some(path_str) = path.to_str() else {
            return;
        };
        let Ok(guard) = self.db.lock() else { return };
        let rec = FileMetaRecord {
            path: path_str.to_string(),
            mtime_unix,
            size,
            magic_label: None,
            description: None,
            partial_hash: None,
            full_hash: Some(hash.to_string()),
            mime: None,
            quarantined: None,
            quarantine_agent: None,
            quarantine_iso: None,
            quarantine_where_from: None,
            indexed_at_unix: self.indexed_at_unix,
        };
        // Write-through; a failed upsert just means the next scan re-hashes.
        let _ = guard.upsert_file(&rec);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Arc<Mutex<MetadataDb>> {
        Arc::new(Mutex::new(MetadataDb::in_memory().unwrap()))
    }

    #[test]
    fn round_trips_a_hash() {
        let cache = DbHashCache::new(db(), 100);
        let p = Path::new("/tmp/x.bin");
        assert!(cache.get_full(p, 10, 5).is_none(), "empty initially");
        cache.put_full(p, 10, 5, "DEADBEEF");
        assert_eq!(cache.get_full(p, 10, 5).as_deref(), Some("DEADBEEF"));
    }

    #[test]
    fn stale_size_or_mtime_misses() {
        let cache = DbHashCache::new(db(), 100);
        let p = Path::new("/tmp/y.bin");
        cache.put_full(p, 10, 5, "AAA");
        assert!(cache.get_full(p, 11, 5).is_none(), "size changed → miss");
        // mtime change wipes the row via upsert's stale-clear, but the
        // lookup guard also rejects it.
        assert!(cache.get_full(p, 10, 6).is_none(), "mtime changed → miss");
    }
}
