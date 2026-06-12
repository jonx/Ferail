use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicBool};

use feraille_core::{EnumerationError, FileEntry, NodeId, navigation::NavigationState};
use gpui::{Entity, Subscription};
use gpui_component::input::InputState;

use crate::file_list::FileListDelegate;
use crate::multi_table::TableState;
use crate::tasks::TaskId;

/// One entry in a tab's back/forward history. Spec §2.6 history
/// exception: a back-navigation should restore the selection state
/// that tab had for the destination directory, reconciled against
/// the freshly streamed model on `Done`. Each push records the
/// selection that was live at the moment of leaving.
#[derive(Clone)]
pub struct HistoryEntry {
    pub path: PathBuf,
    pub selection: HashSet<NodeId>,
    pub anchor: Option<NodeId>,
    pub lead: Option<NodeId>,
}

impl HistoryEntry {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            selection: HashSet::new(),
            anchor: None,
            lead: None,
        }
    }
}

/// Process-local stable identifier for a tab. Minted from
/// `ProcessState::mint_tab_id`. Survives reorder; survives tear-off
/// to a different window (Phase F). Cheaper than path equality for
/// "is this still the same tab" checks across the streaming pipeline.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TabId(pub u64);

pub struct Tab {
    /// Process-local stable identity. Stays the same through
    /// reorder, history navigation, and (later) tear-off.
    pub id: TabId,
    /// Authoritative location identity. `current_dir` remains as a
    /// display/job snapshot during the migration, but navigation logic
    /// moves through this NodeId state first.
    pub nav: NavigationState,
    pub current_dir: PathBuf,
    pub history: Vec<HistoryEntry>,
    pub history_index: usize,
    /// Multi-selection set keyed by `NodeId`. Per spec §2.2 this is
    /// in-memory interaction state, not persisted truth; reconciled
    /// against the model on streaming `Done` and on every model
    /// change. Empty is a valid common state.
    pub selection: HashSet<NodeId>,
    /// Anchor — fixed end of a Shift-range; set on plain click or
    /// the first Cmd-click into an empty selection. (Spec §2.3.)
    pub anchor: Option<NodeId>,
    /// Lead / cursor — moving end of a range, target of keyboard
    /// navigation, focus-ring row. Mirrored to the underlying
    /// `TableState::selected_row` so the primitive's native focus
    /// overlay marks it.
    pub lead: Option<NodeId>,
    /// Spec §2.6 filter holding set: NodeIds that were in
    /// `selection` but got filtered out of the visible model. When
    /// the filter loosens or clears, members whose rows reappear
    /// move back to `selection`. Dropped entirely on navigation.
    pub filtered_out: HashSet<NodeId>,
    /// Spec §2.6 live Shift-range marker: when true the current
    /// selection is the anchor→lead inclusive span and should be
    /// recomputed against the model on every batch arrival, so
    /// rows streaming in between the endpoints join the selection
    /// automatically. Set by Shift-click / Cmd+Shift-click / any
    /// Shift-extend keyboard nav; cleared on any non-range gesture
    /// (plain click, Cmd-click, plain kbd nav, Cmd+A, Esc, sort).
    pub range_live: bool,
    /// gpui-component's virtualized Table state for *this* tab.
    /// Per-tab so tab-switching doesn't re-enumerate — inactive
    /// tabs' enumerations keep streaming into their own table and
    /// the result is ready when the user switches back.
    pub table: Entity<TableState<FileListDelegate>>,
    /// Monotonic generation for *this* tab's directory loads.
    /// Background enumeration results apply only if their generation
    /// still matches the tab they were spawned for.
    pub load_generation: u64,
    /// Cooperative cancel flag for the active directory enumeration
    /// on this tab. Replaced on every navigation/filter/show-hidden
    /// reload that targets this tab.
    pub load_cancel: Option<Arc<AtomicBool>>,
    /// Task-registry row for this tab's in-flight enumeration.
    pub load_task: Option<TaskId>,
    /// True after a navigation starts and before the first batch
    /// lands for this tab. Keeps the old rows visible during the
    /// gap instead of flashing empty.
    pub load_pending_first_batch: bool,
    /// `Some(err)` when this tab's last enumerate returned an OS
    /// error (most commonly macOS TCC denial). Drives an empty-
    /// state in the file pane when the tab is active.
    pub last_error: Option<EnumerationError>,
    /// Screenshot-driver row index queued for selection once a
    /// streaming batch lands for this tab. Cleared on apply or on
    /// navigation. Internal/CLI use only.
    pub pending_select_row: Option<usize>,
    /// Same as `pending_select_row` but for the multi-row
    /// screenshot seed (`--select-rows`).
    pub pending_select_rows: Vec<usize>,
    /// Live filter text for *this* tab. Spec §3.1: each tab owns its
    /// own filter. Switching tabs swaps the visible filter input
    /// to this tab's `filter_input`; typing only re-enumerates
    /// this tab.
    pub filter_text: String,
    /// gpui-component `Input` state for the filter field, scoped to
    /// this tab. The title-bar render mounts the active tab's input
    /// directly so cursor / focus / value are naturally per-tab.
    pub filter_input: Entity<InputState>,
    /// Subscription handle for this tab's table-event bridge into
    /// `Shell`. Owned by the tab so dropping the tab drops the
    /// subscription — important for Phase D's tab-close path.
    #[allow(dead_code)]
    pub(crate) _table_subscription: Subscription,
    /// Subscription for this tab's filter `Input`. Same lifecycle as
    /// `_table_subscription`.
    #[allow(dead_code)]
    pub(crate) _filter_subscription: Subscription,
}

impl Tab {
    /// Internal constructor. Use `Shell::make_tab` from view code so
    /// the new tab is correctly wired to a per-tab `TableState`
    /// subscription and filter `Input`.
    pub fn new_internal(
        id: TabId,
        at: PathBuf,
        node_id: NodeId,
        table: Entity<TableState<FileListDelegate>>,
        table_subscription: Subscription,
        filter_input: Entity<InputState>,
        filter_subscription: Subscription,
    ) -> Self {
        Self {
            id,
            nav: NavigationState::new(node_id),
            current_dir: at.clone(),
            history: vec![HistoryEntry::new(at)],
            history_index: 0,
            selection: HashSet::new(),
            anchor: None,
            lead: None,
            filtered_out: HashSet::new(),
            range_live: false,
            table,
            load_generation: 0,
            load_cancel: None,
            load_task: None,
            load_pending_first_batch: false,
            last_error: None,
            pending_select_row: None,
            pending_select_rows: Vec::new(),
            filter_text: String::new(),
            filter_input,
            _table_subscription: table_subscription,
            _filter_subscription: filter_subscription,
        }
    }

    /// Row index of the lead within `entries`, or `None` if no lead
    /// is set or the lead is not currently in the view's model.
    /// Used by every site that previously read `Tab::selected` as a
    /// row index — preview pane, status bar, screenshot driver,
    /// activate_row's keyboard fallback.
    pub fn lead_row(&self, entries: &[FileEntry]) -> Option<usize> {
        let lead = self.lead?;
        entries.iter().position(|e| e.id == lead)
    }

    /// Short label for the tabstrip. Last path component, or "/" for
    /// the filesystem root.
    pub fn label(&self) -> String {
        self.current_dir
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| self.current_dir.to_string_lossy().into_owned())
    }

    /// Snapshot the parts of this tab the closed-tab stack needs to
    /// reopen it later (Phase D, spec §3.3 "Reopen closed tab").
    ///
    /// Captures directory, history, filter, and selection. Drops
    /// load-in-flight bookkeeping, the gpui-component `TableState`,
    /// and the filter `Input` entity — those are remade fresh on
    /// reopen, and the reopen path re-issues a streaming enumeration
    /// like any new tab. Sort restore is deferred (TableState's
    /// current sort isn't exposed on the public surface today); spec
    /// acceptance lists sort under "restore on reopen" but it's a
    /// follow-on polish item.
    pub fn snapshot_for_close(&self) -> ClosedTab {
        ClosedTab {
            current_dir: self.current_dir.clone(),
            history: self.history.clone(),
            history_index: self.history_index,
            filter_text: self.filter_text.clone(),
            selection: self.selection.clone(),
            anchor: self.anchor,
            lead: self.lead,
        }
    }
}

/// Pure index math for within-strip tab drag-reorder (spec §3.3).
/// Gap positions number `0..=len`: gap 0 before the first tab, gap
/// `len` after the last. Returns the index to `insert` at AFTER
/// `remove(from_idx)`, or `None` when the drop is invalid or a no-op
/// (dropping into the gap on either side of the dragged tab itself).
///
/// Extracted from `Shell::reorder_tab` so the arithmetic is testable
/// without a gpui harness — the active-index bookkeeping stays in
/// the Shell method.
pub fn reorder_insert_index(from_idx: usize, to_pos: usize, len: usize) -> Option<usize> {
    if from_idx >= len || to_pos > len {
        return None;
    }
    if to_pos == from_idx || to_pos == from_idx + 1 {
        return None;
    }
    // After removal, indices > from_idx shift down by one: a gap to
    // the RIGHT of the dragged tab maps to `to_pos - 1` in the
    // post-remove list; a gap to the left is unchanged.
    Some(if from_idx < to_pos { to_pos - 1 } else { to_pos })
}

/// Map a drop ON a tab chip (rather than into a between-chip gap) to
/// the gap position that puts the dragged tab at the target chip's
/// current slot: dragging rightward inserts after the target, leftward
/// inserts before it — in both cases the dragged tab ends up exactly
/// where the target chip was. Dropping a chip on itself maps to its
/// own gap, which `reorder_insert_index` rejects as a no-op.
pub fn chip_drop_gap_index(from_idx: usize, chip_idx: usize) -> usize {
    if from_idx < chip_idx {
        chip_idx + 1
    } else {
        chip_idx
    }
}

#[cfg(test)]
mod reorder_tests {
    use super::reorder_insert_index;

    #[test]
    fn forward_move_adjusts_for_removal_shift() {
        // [A B C D], drag A (0) to gap 2 (between B and C) → remove A,
        // insert at 1 → [B A C D].
        assert_eq!(reorder_insert_index(0, 2, 4), Some(1));
        // Drag A to the far end (gap 4) → insert at 3 → [B C D A].
        assert_eq!(reorder_insert_index(0, 4, 4), Some(3));
    }

    #[test]
    fn backward_move_keeps_position() {
        // [A B C D], drag D (3) to gap 0 → insert at 0 → [D A B C].
        assert_eq!(reorder_insert_index(3, 0, 4), Some(0));
        // Drag C (2) to gap 1 → insert at 1 → [A C B D].
        assert_eq!(reorder_insert_index(2, 1, 4), Some(1));
    }

    #[test]
    fn adjacent_gaps_are_noops() {
        // Gap to the immediate left and right of the dragged tab.
        assert_eq!(reorder_insert_index(1, 1, 4), None);
        assert_eq!(reorder_insert_index(1, 2, 4), None);
        // First and last tabs' own gaps.
        assert_eq!(reorder_insert_index(0, 0, 4), None);
        assert_eq!(reorder_insert_index(0, 1, 4), None);
        assert_eq!(reorder_insert_index(3, 3, 4), None);
        assert_eq!(reorder_insert_index(3, 4, 4), None);
    }

    #[test]
    fn out_of_range_is_rejected() {
        assert_eq!(reorder_insert_index(4, 0, 4), None); // from beyond end
        assert_eq!(reorder_insert_index(0, 5, 4), None); // gap beyond end
        assert_eq!(reorder_insert_index(0, 0, 0), None); // empty strip
    }

    #[test]
    fn single_tab_strip_has_no_valid_moves() {
        for gap in 0..=1 {
            assert_eq!(reorder_insert_index(0, gap, 1), None);
        }
    }

    #[test]
    fn chip_drop_lands_on_target_slot() {
        use super::{chip_drop_gap_index, reorder_insert_index};
        // [A B C D]: drag A (0) onto C (2) → gap 3 → insert at 2 →
        // [B C A D]; A sits where C was.
        assert_eq!(chip_drop_gap_index(0, 2), 3);
        assert_eq!(reorder_insert_index(0, 3, 4), Some(2));
        // Drag D (3) onto B (1) → gap 1 → insert at 1 → [A D B C].
        assert_eq!(chip_drop_gap_index(3, 1), 1);
        assert_eq!(reorder_insert_index(3, 1, 4), Some(1));
        // Adjacent neighbors still reorder (unlike gap drops, a drop
        // ON the neighbor chip is an unambiguous swap intent):
        // drag A (0) onto B (1) → gap 2 → insert at 1 → [B A C D].
        assert_eq!(chip_drop_gap_index(0, 1), 2);
        assert_eq!(reorder_insert_index(0, 2, 4), Some(1));
    }

    #[test]
    fn chip_drop_on_self_is_noop() {
        use super::{chip_drop_gap_index, reorder_insert_index};
        for idx in 0..4 {
            let gap = chip_drop_gap_index(idx, idx);
            assert_eq!(reorder_insert_index(idx, gap, 4), None);
        }
    }
}

/// Per-tab state captured at close time for `Cmd+Shift+T`. Lives on
/// `ProcessState::closed_tabs` so it survives the tab's owning window
/// closing — a closed window's tabs can still be reopened from any
/// other window. Spec §3.3 + §3.4.
///
/// `ClosedTab` is intentionally plain data (no gpui entities, no
/// `Subscription`s) so it can sit in a `VecDeque` indefinitely without
/// pinning view-tree resources. The reopen path in `Shell` rebuilds
/// the live `Tab` via `Shell::make_tab` and then applies these fields
/// onto the fresh tab before scheduling its initial load.
#[derive(Clone)]
pub struct ClosedTab {
    pub current_dir: PathBuf,
    pub history: Vec<HistoryEntry>,
    pub history_index: usize,
    pub filter_text: String,
    pub selection: HashSet<NodeId>,
    pub anchor: Option<NodeId>,
    pub lead: Option<NodeId>,
}
