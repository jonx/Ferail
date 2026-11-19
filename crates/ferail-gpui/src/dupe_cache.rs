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
        // Only trust the stored hash when the file is still in the state it
        // was computed from: a rewritten file at the same path must be
        // re-hashed. The key is the hash's own (`hash_size`,
        // `hash_mtime_unix`), not the row's, which may be newer.
        let stored = guard.get_full_hash(path_str).ok().flatten()?;
        (stored.size == size && stored.mtime_unix == mtime_unix).then_some(stored.hash)
    }

    fn put_full(&self, path: &Path, size: u64, mtime_unix: i64, hash: &str) {
        self.put_full_keyed(path, size, mtime_unix, None, hash);
    }

    fn put_full_keyed(
        &self,
        path: &Path,
        size: u64,
        mtime_unix: i64,
        mtime_nanos: Option<u32>,
        hash: &str,
    ) {
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
        let _ = guard.put_full_hash(&rec, mtime_nanos);
    }

    fn get_record(&self, path: &Path) -> Option<ferail_fs_native::CachedHash> {
        let path_str = path.to_str()?;
        let guard = self.db.lock().ok()?;
        let stored = guard.get_full_hash(path_str).ok().flatten()?;
        Some(ferail_fs_native::CachedHash {
            size: stored.size,
            mtime_unix: stored.mtime_unix,
            mtime_nanos: stored.mtime_nanos,
            hash: stored.hash,
        })
    }

    fn records(&self, path: &Path) -> bool {
        let Some(path_str) = path.to_str() else {
            return false;
        };
        self.db
            .lock()
            .map(|guard| !guard.is_excluded(path_str))
            .unwrap_or(false)
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
        assert!(cache.get_full(p, 10, 6).is_none(), "mtime changed → miss");
    }

    #[test]
    fn record_keeps_its_key_whatever_the_file_is_now() {
        let cache = DbHashCache::new(db(), 100);
        let p = Path::new("/tmp/z.bin");
        assert!(cache.get_record(p).is_none());
        cache.put_full(p, 10, 5, "ABC");
        let rec = cache.get_record(p).unwrap();
        assert_eq!((rec.size, rec.mtime_unix, rec.hash.as_str()), (10, 5, "ABC"));
        assert!(cache.records(p));
    }

    /// The bug this guards: an edited file was seen by the type prefetch
    /// (which rewrites the row with the new mtime and no hash) before
    /// Verify Integrity ran, and the hash was gone, so the edit was
    /// recorded as a new baseline instead of reported as modified.
    #[test]
    fn an_edit_seen_by_the_prefetch_keeps_the_old_hash_and_its_key() {
        let db = db();
        let cache = DbHashCache::new(db.clone(), 100);
        let p = Path::new("/tmp/edited.txt");
        cache.put_full_keyed(p, 10, 5, Some(7), "OLD");
        db.lock()
            .unwrap()
            .upsert_file(&FileMetaRecord {
                path: p.to_str().unwrap().into(),
                mtime_unix: 9,
                size: 12,
                magic_label: Some("Plain text".into()),
                description: None,
                partial_hash: None,
                full_hash: None,
                mime: None,
                quarantined: Some(false),
                quarantine_agent: None,
                quarantine_iso: None,
                quarantine_where_from: None,
                indexed_at_unix: 200,
            })
            .unwrap();
        let rec = cache.get_record(p).unwrap();
        assert_eq!(
            (rec.size, rec.mtime_unix, rec.mtime_nanos, rec.hash.as_str()),
            (10, 5, Some(7), "OLD")
        );
        assert_eq!(
            ferail_fs_native::integrity::classify(Some(&rec), 12, 9, Some(0), "NEW"),
            ferail_fs_native::integrity::IntegrityVerdict::Modified
        );
        assert!(cache.get_full(p, 12, 9).is_none(), "no hash for the new content");
    }
}
