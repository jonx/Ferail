//! Thin in-memory cache that sits in front of [`crate::MetadataDb`]
//! for hot data. Read-through, write-through. The DB handles
//! durability; the cache absorbs read-amplification when a hot
//! folder is repainted many times per second.
//!
//! Eviction is bounded-size FIFO-ish — we drop half the entries
//! when capacity is hit, same approach Ferail used. Real LRU is
//! deferred until profiling says it matters.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::db::FileMetaRecord;

#[derive(Clone)]
pub struct MetadataCache {
    inner: Arc<RwLock<Inner>>,
}

struct Inner {
    map: HashMap<String, FileMetaRecord>,
    cap: usize,
}

impl MetadataCache {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                map: HashMap::new(),
                cap: cap.max(16),
            })),
        }
    }

    pub fn get(&self, path: &str) -> Option<FileMetaRecord> {
        self.inner.read().ok()?.map.get(path).cloned()
    }

    pub fn insert(&self, rec: FileMetaRecord) {
        let Ok(mut inner) = self.inner.write() else {
            return;
        };
        if inner.map.len() >= inner.cap {
            // Drop half — cheap, keeps cap tight, no LRU bookkeeping.
            let drop_to = inner.cap / 2;
            let to_drop: Vec<String> = inner
                .map
                .keys()
                .take(inner.map.len().saturating_sub(drop_to))
                .cloned()
                .collect();
            for k in to_drop {
                inner.map.remove(&k);
            }
        }
        inner.map.insert(rec.path.clone(), rec);
    }

    pub fn invalidate(&self, path: &str) {
        if let Ok(mut inner) = self.inner.write() {
            inner.map.remove(path);
        }
    }

    pub fn clear(&self) {
        if let Ok(mut inner) = self.inner.write() {
            inner.map.clear();
        }
    }

    pub fn len(&self) -> usize {
        self.inner.read().map(|i| i.map.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for MetadataCache {
    fn default() -> Self {
        Self::new(8_192)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(path: &str, mtime: i64) -> FileMetaRecord {
        FileMetaRecord {
            path: path.into(),
            mtime_unix: mtime,
            size: 0,
            magic_label: None,
            partial_hash: None,
            full_hash: None,
            mime: None,
            quarantined: None,
            quarantine_agent: None,
            quarantine_iso: None,
            quarantine_where_from: None,
            indexed_at_unix: mtime,
        }
    }

    #[test]
    fn get_after_insert() {
        let c = MetadataCache::new(64);
        c.insert(rec("/a", 100));
        assert_eq!(c.get("/a").unwrap().mtime_unix, 100);
        assert!(c.get("/missing").is_none());
    }

    #[test]
    fn invalidate_removes_entry() {
        let c = MetadataCache::new(64);
        c.insert(rec("/a", 100));
        c.invalidate("/a");
        assert!(c.get("/a").is_none());
    }

    #[test]
    fn eviction_drops_to_half_when_full() {
        let c = MetadataCache::new(16);
        for i in 0..16 {
            c.insert(rec(&format!("/p{i}"), i as i64));
        }
        assert_eq!(c.len(), 16);
        // Inserting one more should cap-evict half.
        c.insert(rec("/new", 999));
        assert!(c.len() <= 16 / 2 + 1);
    }
}
