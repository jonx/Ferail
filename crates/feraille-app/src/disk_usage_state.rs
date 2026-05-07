//! Per-window state for the Disk Usage window — the tree, scan
//! generation/cancel, layout cache, zoom path, hover/selection.
//!
//! Lives in its own struct (not on `App`) so the window can be created
//! and torn down independently of the main window's lifecycle, and so
//! `App` doesn't grow another dozen fields it would otherwise carry
//! around even when the window is closed.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use feraille_controls::primitives::toast::ToastStack;
use feraille_controls::TreemapColoring;
use feraille_core::{EnumerationError, NodeId};
use feraille_disk_usage::{
    DiskUsageLayoutNode, DiskUsageStats, DiskUsageTree, FileCategory, NodeKind, SizeMode,
    TreemapRect,
};

use crate::tasks::TaskId;

/// Snapshot of volume capacity for the header. Mirrors the macOS
/// `volume_info_for_path` shape but kept locally so this module can
/// stay platform-neutral.
#[derive(Clone, Debug)]
#[allow(dead_code)] // is_local / is_removable surface in iter-6.4 chrome (volume icon glyph)
pub struct VolumeSnapshot {
    pub name: String,
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub is_local: bool,
    pub is_removable: bool,
}

/// Top-N largest individual files in the scanned tree. Sorted by
/// `size_bytes` descending. Capped at [`TOPN_CAP`] to keep rebuild
/// cost flat for huge trees.
pub const TOPN_CAP: usize = 50;

#[derive(Clone, Debug)]
pub struct TopFileEntry {
    pub node_id: NodeId,
    pub size_bytes: u64,
    pub display_name: String,
    /// Display name of the file's first containing folder, or empty
    /// when the file is at the focus root with no parent recorded
    /// in the tree.
    pub parent_display_name: String,
    /// Last-modified time, when known. Used by the Age sort.
    pub mtime: Option<std::time::SystemTime>,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum TopNSort {
    #[default]
    Size,
    Name,
    Age,
}

/// Treemap recursion depth used by the DU window. Matches the iter-6.1
/// CLI default; a future setting can let the user override it.
pub const DU_LAYOUT_DEPTH: u32 = 4;

/// Maximum interval between layout rebuilds while a scan is streaming
/// in. Below this we accumulate facts; above it we rebuild + repaint
/// even if more facts are coming. Chosen to keep first-paint snappy
/// without thrashing layout on every batch.
pub const DU_LAYOUT_DEBOUNCE_MS: u128 = 80;

/// Everything the DU window needs to paint and respond. Owned by the
/// window struct; the window's winit/softbuffer/renderer fields hang
/// off it separately so this type stays renderer-agnostic and
/// testable on its own.
pub struct DiskUsageState {
    pub root_path: PathBuf,
    pub root_id: NodeId,

    pub tree: DiskUsageTree,
    pub stats: DiskUsageStats,
    pub generation: u64,
    pub cancel: Arc<AtomicBool>,
    pub task_id: Option<TaskId>,
    pub scan_complete: bool,
    pub error: Option<EnumerationError>,

    /// `.last()` is the current focus container. Empty = root.
    pub zoom_path: Vec<NodeId>,

    pub layout_cache: Option<DiskUsageLayoutNode>,
    pub rects_cache: Vec<TreemapRect>,
    /// Rect bounds the cache was computed for; mismatch triggers a
    /// recompute on next paint.
    pub rects_bounds: Option<(f32, f32, f32, f32)>,

    pub selection: HashSet<NodeId>,
    pub hovered: Option<NodeId>,
    pub coloring: TreemapColoring,

    /// When `Some`, files of other categories are dimmed in the
    /// treemap. Click a chip in the legend to set; click the same
    /// chip again or "All" to clear.
    pub category_filter: Option<FileCategory>,

    /// Whether to descend into macOS package directories (`.app`,
    /// `.bundle`, `.framework`, etc.) instead of treating them as
    /// opaque leaves. Default `false` — matches Finder's behaviour
    /// where a `.app` shows up as a single object with its rolled-up
    /// size. Toggleable via `disk_usage.toggle_packages`.
    pub descend_packages: bool,

    /// When the active tab navigates to a different folder while the
    /// DU window is open, automatically restart the scan rooted at
    /// the new path. Default `true`. Toggleable via
    /// `disk_usage.toggle_follow_navigation`.
    pub follow_navigation: bool,

    /// Whether to aggregate apparent (logical) or allocated
    /// (block-aligned, on-disk) bytes. Apparent matches Finder.
    pub size_mode: SizeMode,

    /// Bottom-right toast stack for trash failures and other
    /// transient messages. Mirrors the main window's pattern.
    pub toasts: ToastStack,

    /// Volume capacity snapshot (free / total) — populated once when
    /// the window opens via the platform's volume-info call. None on
    /// non-macOS or when the lookup failed.
    pub volume: Option<VolumeSnapshot>,

    /// Top-N largest files anywhere in the tree, sorted by
    /// `topn_sort`. Rebuilt on layout invalidation.
    pub topn_files: Vec<TopFileEntry>,
    /// Show the Top-N side panel? Toggled by `disk_usage.toggle_topn`.
    pub topn_visible: bool,
    /// Width of the Top-N panel in DIPs, measured from the right edge.
    /// Splitter drag updates this; a future "remember" setting can
    /// persist it.
    pub topn_width_dips: f32,
    /// Vertical scroll offset (DIPs) into the Top-N row list when it
    /// overflows the pane height.
    pub topn_scroll_offset: f32,
    /// Active sort key. Click a column header in the Top-N panel to
    /// cycle.
    pub topn_sort: TopNSort,

    /// Wall-clock instant of the last layout rebuild — used to debounce
    /// rebuilds while batches stream in.
    pub last_rebuild: std::time::Instant,
}

impl DiskUsageState {
    pub fn new(root_path: PathBuf, root_id: NodeId, generation: u64) -> Self {
        Self {
            root_path,
            root_id,
            tree: DiskUsageTree::new(root_id),
            stats: DiskUsageStats::default(),
            generation,
            cancel: Arc::new(AtomicBool::new(false)),
            task_id: None,
            scan_complete: false,
            error: None,
            zoom_path: Vec::new(),
            layout_cache: None,
            rects_cache: Vec::new(),
            rects_bounds: None,
            selection: HashSet::new(),
            hovered: None,
            coloring: TreemapColoring::Category,
            category_filter: None,
            volume: None,
            topn_files: Vec::new(),
            topn_visible: true,
            topn_width_dips: 280.0,
            topn_scroll_offset: 0.0,
            topn_sort: TopNSort::Size,
            descend_packages: false,
            follow_navigation: true,
            size_mode: SizeMode::Apparent,
            toasts: ToastStack::default(),
            last_rebuild: std::time::Instant::now(),
        }
    }

    /// Recompute [`Self::topn_files`] from the current tree. Cheap on
    /// trees of any size: one pass to collect file (id, size, name)
    /// and a partial sort to keep the top [`TOPN_CAP`].
    pub fn rebuild_topn(&mut self) {
        // Build a reverse index: node -> first containing container
        // (display_name). DAG-safe — first wins. Single pass over
        // `containers` keeps the cost O(total members).
        let mut parent_of: std::collections::HashMap<NodeId, NodeId> =
            std::collections::HashMap::new();
        for (container, members) in &self.tree.containers {
            for m in members {
                parent_of.entry(*m).or_insert(*container);
            }
        }

        let mut all: Vec<TopFileEntry> = self
            .tree
            .nodes
            .iter()
            .filter(|(_, n)| matches!(n.kind, NodeKind::File))
            .filter(|(_, n)| n.size_bytes > 0)
            .map(|(id, n)| {
                let parent_name = parent_of
                    .get(id)
                    .and_then(|pid| self.tree.nodes.get(pid))
                    .map(|p| p.display_name.clone())
                    .unwrap_or_default();
                TopFileEntry {
                    node_id: *id,
                    size_bytes: n.size_bytes,
                    display_name: n.display_name.clone(),
                    parent_display_name: parent_name,
                    mtime: n.mtime,
                }
            })
            .collect();

        match self.topn_sort {
            TopNSort::Size => all.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes)),
            TopNSort::Name => all.sort_by(|a, b| {
                a.display_name
                    .to_ascii_lowercase()
                    .cmp(&b.display_name.to_ascii_lowercase())
            }),
            TopNSort::Age => all.sort_by(|a, b| {
                // Oldest first.
                let ka = a.mtime.unwrap_or(std::time::UNIX_EPOCH);
                let kb = b.mtime.unwrap_or(std::time::UNIX_EPOCH);
                ka.cmp(&kb)
            }),
        }
        all.truncate(TOPN_CAP);
        self.topn_files = all;
    }

    /// Current zoom focus — the deepest entry on the zoom path, or the
    /// root if the user hasn't drilled into anything.
    pub fn focus_id(&self) -> NodeId {
        self.zoom_path.last().copied().unwrap_or(self.root_id)
    }

    /// Drop layout caches; called when zoom changes, bounds change, or
    /// a fresh scan starts.
    pub fn invalidate_layout(&mut self) {
        self.layout_cache = None;
        self.rects_cache.clear();
        self.rects_bounds = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(raw: u64) -> NodeId {
        NodeId::from_raw(raw).expect("nonzero")
    }

    #[test]
    fn focus_id_is_root_when_zoom_path_empty() {
        let s = DiskUsageState::new(PathBuf::from("/"), nid(1), 0);
        assert_eq!(s.focus_id(), nid(1));
    }

    #[test]
    fn focus_id_is_last_zoom_path_entry() {
        let mut s = DiskUsageState::new(PathBuf::from("/"), nid(1), 0);
        s.zoom_path.push(nid(2));
        s.zoom_path.push(nid(3));
        assert_eq!(s.focus_id(), nid(3));
    }

    #[test]
    fn invalidate_layout_drops_caches() {
        use feraille_disk_usage::{NodeKind, ScanState, FileCategory};
        let mut s = DiskUsageState::new(PathBuf::from("/"), nid(1), 0);
        s.layout_cache = Some(DiskUsageLayoutNode::new(
            nid(1), 0, ScanState::Complete,
            NodeKind::Container, FileCategory::Other, vec![],
        ));
        s.rects_bounds = Some((0.0, 0.0, 100.0, 100.0));
        s.invalidate_layout();
        assert!(s.layout_cache.is_none());
        assert!(s.rects_cache.is_empty());
        assert!(s.rects_bounds.is_none());
    }
}
