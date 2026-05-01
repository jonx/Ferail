//! Selection model — shared by VirtualizedList, FileTree, and any other
//! row-keyed control. Indexed by `usize` (row index) for iter-2; will
//! gain a NodeId-keyed variant when streaming enumeration lands in iter-3.
//!
//! Spec: `specs/ux/02-selection.md`. Iter-2 covers the keyboard subset:
//! single-click replaces, arrow keys move cursor, shift+arrows extend,
//! Ctrl+A select-all. No mouse range/marquee/Ctrl-toggle yet.

use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SelectionSet {
    #[default]
    None,
    Single(usize),
    /// Inclusive on both ends; invariant `from <= to`.
    Range { from: usize, to: usize },
    Discrete(BTreeSet<usize>),
}

impl SelectionSet {
    pub fn contains(&self, idx: usize) -> bool {
        match self {
            SelectionSet::None => false,
            SelectionSet::Single(i) => *i == idx,
            SelectionSet::Range { from, to } => idx >= *from && idx <= *to,
            SelectionSet::Discrete(set) => set.contains(&idx),
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, SelectionSet::None)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub anchor: Option<usize>,
    pub cursor: Option<usize>,
    pub set: SelectionSet,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// Replace selection with a single index; reset anchor to it.
    pub fn set_cursor(&mut self, idx: usize) {
        self.cursor = Some(idx);
        self.anchor = Some(idx);
        self.set = SelectionSet::Single(idx);
    }

    /// Drop the selection set but keep cursor (focus stays put).
    pub fn clear(&mut self) {
        self.set = SelectionSet::None;
        self.anchor = None;
    }

    pub fn select_all(&mut self, count: usize) {
        if count == 0 {
            self.clear();
            return;
        }
        self.set = SelectionSet::Range { from: 0, to: count - 1 };
        self.anchor = Some(0);
        self.cursor = Some(count - 1);
    }

    /// Move cursor by delta in single-select mode (replaces selection).
    pub fn move_cursor(&mut self, delta: i64, count: usize) {
        if count == 0 {
            return;
        }
        let cur = self.cursor.unwrap_or(0) as i64;
        let next = (cur + delta).clamp(0, (count as i64) - 1) as usize;
        self.set_cursor(next);
    }

    /// Extend selection to `idx`, anchored at `self.anchor`. If no anchor,
    /// behaves as `set_cursor`.
    pub fn extend_to(&mut self, idx: usize) {
        let Some(anchor) = self.anchor else {
            self.set_cursor(idx);
            return;
        };
        self.cursor = Some(idx);
        let from = anchor.min(idx);
        let to = anchor.max(idx);
        self.set = if from == to {
            SelectionSet::Single(from)
        } else {
            SelectionSet::Range { from, to }
        };
    }

    /// Shift+arrow: move cursor and extend selection to it.
    pub fn move_cursor_extending(&mut self, delta: i64, count: usize) {
        if count == 0 {
            return;
        }
        let cur = self.cursor.unwrap_or(0) as i64;
        let next = (cur + delta).clamp(0, (count as i64) - 1) as usize;
        self.extend_to(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_cursor_replaces_selection() {
        let mut sel = Selection::new();
        sel.set_cursor(30);
        assert_eq!(sel.cursor, Some(30));
        assert_eq!(sel.set, SelectionSet::Single(30));
        sel.move_cursor(1, 100);
        assert_eq!(sel.cursor, Some(31));
        assert_eq!(sel.set, SelectionSet::Single(31));
    }

    #[test]
    fn extend_to_creates_range() {
        let mut sel = Selection::new();
        sel.set_cursor(30);
        sel.extend_to(35);
        assert_eq!(sel.anchor, Some(30));
        assert_eq!(sel.cursor, Some(35));
        assert_eq!(sel.set, SelectionSet::Range { from: 30, to: 35 });
    }

    #[test]
    fn extend_to_below_anchor_inverts() {
        let mut sel = Selection::new();
        sel.set_cursor(30);
        sel.extend_to(25);
        assert_eq!(sel.anchor, Some(30));
        assert_eq!(sel.cursor, Some(25));
        assert_eq!(sel.set, SelectionSet::Range { from: 25, to: 30 });
    }

    #[test]
    fn select_all_full_range() {
        let mut sel = Selection::new();
        sel.select_all(100);
        assert_eq!(sel.set, SelectionSet::Range { from: 0, to: 99 });
        assert_eq!(sel.anchor, Some(0));
        assert_eq!(sel.cursor, Some(99));
    }

    #[test]
    fn move_cursor_clamps() {
        let mut sel = Selection::new();
        sel.set_cursor(0);
        sel.move_cursor(-5, 10);
        assert_eq!(sel.cursor, Some(0));
        sel.move_cursor(100, 10);
        assert_eq!(sel.cursor, Some(9));
    }

    #[test]
    fn set_contains_membership() {
        let s = SelectionSet::Range { from: 5, to: 10 };
        assert!(s.contains(7));
        assert!(!s.contains(11));
        assert!(!s.contains(4));
    }

    #[test]
    fn extend_with_no_anchor_is_set_cursor() {
        let mut sel = Selection::new();
        sel.extend_to(7);
        assert_eq!(sel.anchor, Some(7));
        assert_eq!(sel.cursor, Some(7));
        assert_eq!(sel.set, SelectionSet::Single(7));
    }
}
