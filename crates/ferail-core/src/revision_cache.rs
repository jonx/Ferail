//! Small process-memory caches for derived file/provider records.
//!
//! Keys are compact identities supplied by the caller, never paths. Revisions
//! are compared on lookup so a rewritten file or refreshed provider item
//! cannot return stale personal data. Values are deliberately absent from the
//! cache's `Debug` representation.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::hash::Hash;
use std::sync::Arc;

/// Revision obtainable from the stat which created a filesystem row. Reading
/// it requires no additional file open on the UI thread.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileRevision {
    pub byte_len: u64,
    pub modified_ns: Option<i128>,
}

/// Bounded FIFO cache keyed by an existing compact identity and its revision.
/// It never owns a source path and is dropped with the process or owning
/// surface. `Debug` reports only capacity/size, regardless of value type.
pub struct RevisionCache<K, R, V> {
    capacity: usize,
    order: VecDeque<K>,
    entries: HashMap<K, (R, Arc<V>)>,
}

impl<K, R, V> RevisionCache<K, R, V>
where
    K: Copy + Eq + Hash,
    R: Copy + Eq,
{
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            order: VecDeque::new(),
            entries: HashMap::new(),
        }
    }

    pub fn get(&mut self, key: K, revision: R) -> Option<Arc<V>> {
        let (cached_revision, value) = self.entries.get(&key)?;
        if *cached_revision == revision {
            return Some(value.clone());
        }
        self.entries.remove(&key);
        self.order.retain(|cached| *cached != key);
        None
    }

    pub fn insert(&mut self, key: K, revision: R, value: V) {
        if self.capacity == 0 {
            return;
        }
        if self.entries.contains_key(&key) {
            self.order.retain(|cached| *cached != key);
        }
        self.entries.insert(key, (revision, Arc::new(value)));
        self.order.push_back(key);
        while self.entries.len() > self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.entries.remove(&evicted);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.order.clear();
        self.entries.clear();
    }
}

impl<K, R, V> fmt::Debug for RevisionCache<K, R, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RevisionCache")
            .field("capacity", &self.capacity)
            .field("len", &self.entries.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_revision_cache_evicts_stale_values_without_debugging_them() {
        let mut cache = RevisionCache::new(2);
        cache.insert(1_u64, 1_u64, "private-one".to_string());
        cache.insert(2, 1, "private-two".to_string());
        cache.insert(3, 1, "private-three".to_string());
        assert_eq!(cache.len(), 2);
        assert!(cache.get(1, 1).is_none());
        assert!(cache.get(2, 2).is_none());
        assert_eq!(cache.len(), 1);
        let debug = format!("{cache:?}");
        assert!(!debug.contains("private"));
    }

    #[test]
    fn zero_capacity_and_clear_release_values() {
        let mut cache = RevisionCache::new(0);
        cache.insert(1_u64, 1_u64, "private".to_string());
        assert!(cache.is_empty());

        let mut cache = RevisionCache::new(1);
        cache.insert(1_u64, 1_u64, "private".to_string());
        cache.clear();
        assert!(cache.is_empty());
    }
}
