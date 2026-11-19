//! Recent copies and cuts, for Paste from History (Cmd+Shift+V).
//!
//! An entry is a list of paths and whether they were cut: the files
//! themselves stay where they are, so the history costs a few paths per
//! entry and pasting an old entry is an ordinary copy or move from those
//! paths. It lives in memory only, is shared by every window of the
//! process, and records nothing while Private Mode is on. See
//! docs/features/FILE_OPS.md#paste-from-history.

use std::collections::VecDeque;
use std::path::PathBuf;

/// How many copies and cuts the history keeps.
pub const CAPACITY: usize = 10;

#[derive(Clone, Debug)]
pub struct ClipEntry {
    /// What was copied or cut, in selection order.
    pub paths: Vec<PathBuf>,
    /// A cut moves on paste and leaves the history once pasted.
    pub cut: bool,
    /// When it was copied, in Unix seconds (for "5 minutes ago").
    pub at: i64,
}

impl ClipEntry {
    /// The folder the items were taken from (the first item's parent).
    pub fn source_folder(&self) -> Option<&std::path::Path> {
        self.paths.first().and_then(|p| p.parent())
    }
}

#[derive(Default, Debug)]
pub struct ClipboardHistory {
    entries: VecDeque<ClipEntry>,
}

impl ClipboardHistory {
    /// Newest first.
    pub fn entries(&self) -> impl Iterator<Item = &ClipEntry> {
        self.entries.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Record a copy or cut. Copying the same items again moves their
    /// entry to the top instead of adding a duplicate.
    pub fn record(&mut self, paths: Vec<PathBuf>, cut: bool, at: i64) {
        if paths.is_empty() {
            return;
        }
        self.entries.retain(|e| e.paths != paths);
        self.entries.push_front(ClipEntry { paths, cut, at });
        self.entries.truncate(CAPACITY);
    }

    /// The entry at `index` (0 = newest), for pasting.
    pub fn get(&self, index: usize) -> Option<&ClipEntry> {
        self.entries.get(index)
    }

    /// Forget a cut once it has been pasted: its items are no longer at
    /// those paths. Matched by content, since the index may have moved.
    pub fn consume_cut(&mut self, paths: &[PathBuf]) {
        self.entries.retain(|e| !(e.cut && e.paths == paths));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn newest_first_deduplicated_and_bounded() {
        let mut h = ClipboardHistory::default();
        let now = 0;
        h.record(vec![p("/a")], false, now);
        h.record(vec![p("/b")], false, now);
        h.record(vec![p("/a")], true, now);
        let order: Vec<_> = h.entries().map(|e| (e.paths[0].clone(), e.cut)).collect();
        assert_eq!(order, vec![(p("/a"), true), (p("/b"), false)]);

        for i in 0..CAPACITY + 5 {
            h.record(vec![p(&format!("/x{i}"))], false, now);
        }
        assert_eq!(h.entries().count(), CAPACITY);
        assert_eq!(h.get(0).unwrap().paths[0], p(&format!("/x{}", CAPACITY + 4)));
    }

    #[test]
    fn a_pasted_cut_leaves_and_a_copy_stays() {
        let mut h = ClipboardHistory::default();
        let now = 0;
        h.record(vec![p("/dir/a"), p("/dir/b")], true, now);
        h.record(vec![p("/dir/c")], false, now);
        h.consume_cut(&[p("/dir/c")]);
        assert_eq!(h.entries().count(), 2, "a copy is not consumed");
        h.consume_cut(&[p("/dir/a"), p("/dir/b")]);
        assert_eq!(h.entries().count(), 1);
        assert_eq!(h.get(0).unwrap().source_folder(), Some(std::path::Path::new("/dir")));
        h.record(vec![], false, now);
        assert_eq!(h.entries().count(), 1, "an empty copy is not recorded");
    }
}
