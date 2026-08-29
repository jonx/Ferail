//! Disk Usage window (Harvest Stage 7).
//!
//! A second native window showing a squarified treemap of the scanned
//! folder's contents. Reuses every piece of the existing
//! `ferail-disk-usage` crate (scan tree, layout, classification) and
//! `ferail-fs-native::scan_disk_usage` for the walker — the new
//! code is just orchestration + GPUI rendering.
//!
//! Streaming pattern: the BG scan pushes fact batches into an
//! `Arc<Mutex<VecDeque<ScanMsg>>>` queue; a FG timer drains bounded
//! FIFO chunks on a dynamic cadence and applies them to the tree,
//! debouncing layout rebuilds the same way the old
//! `disk_usage_state` did. The queue is bounded ([`DU_QUEUE_CAP`]):
//! when the drain falls behind, the scanner thread parks briefly
//! instead of growing the backlog. Cancellation is cooperative via
//! `AtomicBool` (also checked inside the backpressure wait).

use crate::text::{TextScale as _, TruncateMiddle as _};
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use std::time::SystemTime;

#[cfg(target_os = "windows")]
use std::ffi::OsString;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStringExt as _;

/// Key context for the Disk Usage pane — keymap.rs binds the treemap
/// keys (Enter/Backspace/Escape, Cmd+C/I/Backspace) against it.
pub const DISK_USAGE_CONTEXT: &str = "DiskUsage";

gpui::actions!(
    disk_usage,
    [
        DuClearSelection,
        DuOpen,
        DuOpenInNewTab,
        DuReveal,
        DuGetInfo,
        DuCopyFiles,
        DuCopyPaths,
        DuTrash,
        DuZoomIn,
        DuZoomOut,
        DuCopyHtml,
        DuSaveHtml,
        DuCopyViewHtml,
        DuSaveViewHtml,
    ]
);

use ferail_core::counts::format_count;
use ferail_core::{EnumerationError, NodeId};
#[cfg(target_os = "windows")]
use ferail_disk_usage::classify_extension;
use ferail_disk_usage::{
    DiskUsageFact, DiskUsageLayoutNode, DiskUsageStats, DiskUsageTree, FileCategory, SizeMode,
    TreemapRect, build_filtered_layout_node_with_mode, build_layout_node_with_mode,
    compute_treemap,
};
use ferail_fs_native::NativeFs;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, ElementExt, Root, Selectable, Sizable, WindowExt as _,
    button::{Button, ButtonGroup},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use crate::tasks::{TaskId, TaskKind, TaskRegistry};
use crate::tool_results::{ToolHostContext, ToolHostEvent};

/// Callback invoked after a task-registry mutation so the owning
/// Shell can `cx.notify` itself (the registry is plain `Rc<RefCell>`
/// with no built-in observers).
pub type NotifyOwner = Rc<dyn Fn(&mut App)>;
/// Callback used by a standalone Disk Usage window to dock itself back
/// into the owning Shell. The shell decides which tab receives it.
pub type DockOwner = Rc<dyn Fn(PathBuf, Entity<DiskUsageView>, &mut App)>;

/// Treemap recursion depth used by the DU window. Matches the old
/// app's DU_LAYOUT_DEPTH.
const DU_LAYOUT_DEPTH: u32 = 4;
/// Foreground drain cadence while the backlog is small.
const DU_DRAIN_INTERVAL_IDLE: Duration = Duration::from_millis(80);
/// When the worker gets ahead, keep draining more often, but still in
/// bounded chunks so the UI thread gets to breathe between updates.
const DU_DRAIN_INTERVAL_BUSY: Duration = Duration::from_millis(16);
/// Hard cap on how many queue messages one foreground drain tick may
/// apply. Prevents a large scan backlog from collapsing into one giant
/// main-thread update.
const DU_MAX_MSGS_PER_TICK: usize = 12;
/// Top-N rebuild is O(n) in tree size + O(n log n) for the sort. At
/// large folders it dominates each drain tick, so we throttle it to a
/// human-scale refresh rate. The Done message always forces a final
/// rebuild regardless.
const DU_TOPN_REBUILD_INTERVAL: Duration = Duration::from_millis(500);
/// Treemap layout rebuild (aggregate recursion over every node in the
/// tree) is the other per-tick cost that scales with tree size. While
/// facts are streaming, rebuild at most this often; progress counters
/// keep updating every tick, and Done always forces a final rebuild so
/// the last frame is exact. User-driven invalidations (zoom, filter,
/// size-mode, resize) bypass this and stay immediate.
const DU_LAYOUT_REBUILD_INTERVAL: Duration = Duration::from_millis(250);
const DU_LARGE_TREE_NODES: usize = 100_000;
const DU_HUGE_TREE_NODES: usize = 500_000;

fn layout_rebuild_interval(nodes: usize) -> Duration {
    if nodes >= DU_HUGE_TREE_NODES {
        Duration::from_secs(2)
    } else if nodes >= DU_LARGE_TREE_NODES {
        Duration::from_secs(1)
    } else {
        DU_LAYOUT_REBUILD_INTERVAL
    }
}

fn topn_rebuild_interval(nodes: usize) -> Duration {
    if nodes >= DU_HUGE_TREE_NODES {
        Duration::from_secs(3)
    } else if nodes >= DU_LARGE_TREE_NODES {
        Duration::from_secs(1)
    } else {
        DU_TOPN_REBUILD_INTERVAL
    }
}
/// Backpressure cap on the BG→FG queue. The drain applies at most
/// `DU_MAX_MSGS_PER_TICK` per busy tick, so a warm-cache scan of a huge
/// volume can outrun it indefinitely — without a cap the backlog grows
/// to hundreds of MB of fact batches. When the queue is full the
/// scanner thread naps ([`DU_BACKPRESSURE_NAP`]) until the drain
/// catches up, re-checking `cancel` each lap so a cancelled scan (or a
/// closed window) never hangs in the wait.
const DU_QUEUE_CAP: usize = 256;
/// How long the scanner sleeps per backpressure lap. Short enough that
/// the walker resumes promptly once the drain frees a slot; the
/// scanner runs on a background pool thread, so blocking it is fine.
const DU_BACKPRESSURE_NAP: Duration = Duration::from_millis(8);

// Disk Usage identities are scan-local. Bit 62 keeps them disjoint from
// ordinary NativeFs ids; Flat View owns bit 63. Forty low bits allow more than
// one trillion nodes per scan and the remaining bits distinguish concurrent
// or repeated scans.
const DU_ID_MARKER: u64 = 1 << 62;
const DU_ROW_BITS: u32 = 40;
static NEXT_DU_SCAN: AtomicU64 = AtomicU64::new(1);

fn next_du_id_base() -> u64 {
    let scan_mask = (1_u64 << (62 - DU_ROW_BITS)) - 1;
    let scan = NEXT_DU_SCAN.fetch_add(1, Ordering::Relaxed) & scan_mask;
    DU_ID_MARKER | (scan << DU_ROW_BITS)
}

/// Minimal, surface-owned path index. Node names already live in
/// `DiskUsageTree`; retaining one parent id per node is enough to reconstruct
/// an absolute path when an action is invoked. Closing the DU surface drops
/// this vector and the tree together, unlike NativeFs's process-wide path map.
struct DiskUsagePathArena {
    root: PathBuf,
    id_base: u64,
    parents: Vec<Option<NodeId>>,
    /// Fast NTFS filenames are opaque UTF-16. Keep them once in a compact
    /// arena so actions never round-trip through the lossy display label.
    #[cfg(target_os = "windows")]
    raw_names: Vec<u16>,
    #[cfg(target_os = "windows")]
    raw_ranges: Vec<Option<RawNameRange>>,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy)]
struct RawNameRange {
    start: u32,
    len: u16,
}

impl DiskUsagePathArena {
    fn new(root: PathBuf, id_base: u64) -> Self {
        Self {
            root,
            id_base,
            parents: vec![None], // id_base + 1 is always the scan root.
            #[cfg(target_os = "windows")]
            raw_names: Vec::new(),
            #[cfg(target_os = "windows")]
            raw_ranges: vec![None],
        }
    }

    fn root_id(&self) -> NodeId {
        NodeId::from_raw(self.id_base + 1).expect("disk-usage root id is nonzero")
    }

    fn row_index(&self, id: NodeId) -> Option<usize> {
        let offset = id.as_raw().checked_sub(self.id_base)?.checked_sub(1)?;
        usize::try_from(offset).ok()
    }

    fn ensure(&mut self, id: NodeId) -> Option<usize> {
        let row = self.row_index(id)?;
        if self.parents.len() <= row {
            self.parents.resize(row + 1, None);
            #[cfg(target_os = "windows")]
            self.raw_ranges.resize(row + 1, None);
        }
        Some(row)
    }

    fn parent_for(&self, id: NodeId) -> Option<NodeId> {
        let row = self.row_index(id)?;
        self.parents.get(row).copied().flatten()
    }

    fn nearest_visible_ancestor(&self, id: NodeId, visible: &HashSet<NodeId>) -> Option<NodeId> {
        let mut current = id;
        for _ in 0..=self.parents.len() {
            if visible.contains(&current) {
                return Some(current);
            }
            current = self.parent_for(current)?;
        }
        None
    }

    #[cfg(target_os = "windows")]
    fn set_raw_name(&mut self, id: NodeId, raw_name: &[u16]) {
        let Ok(start) = u32::try_from(self.raw_names.len()) else {
            return;
        };
        let Ok(len) = u16::try_from(raw_name.len()) else {
            return;
        };
        let Some(row) = self.ensure(id) else { return };
        self.raw_names.extend_from_slice(raw_name);
        self.raw_ranges[row] = Some(RawNameRange { start, len });
    }

    fn apply_facts(&mut self, facts: &[DiskUsageFact]) {
        for fact in facts {
            match fact {
                DiskUsageFact::NodeDiscovered { node, .. } => {
                    self.ensure(*node);
                }
                DiskUsageFact::NodeLinked { container, node } => {
                    self.ensure(*container);
                    if let Some(row) = self.ensure(*node) {
                        self.parents[row] = Some(*container);
                    }
                }
                _ => {}
            }
        }
    }

    fn path_for(&self, id: NodeId, tree: &DiskUsageTree) -> Option<PathBuf> {
        if id == self.root_id() {
            return Some(self.root.clone());
        }
        let mut current = id;
        let mut components = Vec::new();
        // The bound turns corrupt/cyclic fact input into a missing action
        // target instead of an infinite loop.
        for _ in 0..self.parents.len() {
            if current == self.root_id() {
                let mut path = self.root.clone();
                for component in components.iter().rev() {
                    path.push(component);
                }
                return Some(path);
            }
            let row = self.row_index(current)?;
            #[cfg(target_os = "windows")]
            let component = self
                .raw_ranges
                .get(row)
                .copied()
                .flatten()
                .and_then(|range| {
                    let start = range.start as usize;
                    let end = start.checked_add(range.len as usize)?;
                    Some(OsString::from_wide(self.raw_names.get(start..end)?))
                });
            #[cfg(target_os = "windows")]
            let component = match component {
                Some(component) => component,
                None => OsString::from(&tree.nodes.get(&current)?.display_name),
            };
            #[cfg(not(target_os = "windows"))]
            let component = tree.nodes.get(&current)?.display_name.clone();
            components.push(component);
            current = self.parents.get(row).copied().flatten()?;
        }
        None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DuEngine {
    Portable,
    FastNtfs,
}

impl DuEngine {
    #[cfg(target_os = "windows")]
    fn from_preference() -> Self {
        match crate::app_state::load().disk_usage_engine.as_deref() {
            Some("fast-ntfs") => Self::FastNtfs,
            _ => Self::Portable,
        }
    }

    #[cfg(not(target_os = "windows"))]
    const fn from_preference() -> Self {
        Self::Portable
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
enum FastEligibility {
    Checking,
    Eligible,
    Ineligible,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FastFallbackReason {
    ElevationDeclined,
    HelperMissing,
    /// The helper is there but is not the binary this build shipped with.
    /// Kept distinct from `Failed` because it is the one fallback that may
    /// mean something tampered with the installation.
    HelperUntrusted,
    Unsupported,
    Failed,
}

#[cfg(target_os = "windows")]
struct FastBatch {
    facts: Vec<DiskUsageFact>,
    raw_names: Vec<(NodeId, Vec<u16>)>,
    stats: DiskUsageStats,
}

pub struct DiskUsageView {
    root_path: PathBuf,
    root_id: NodeId,
    fs: Arc<NativeFs>,
    path_arena: DiskUsagePathArena,

    /// Immutable-by-default after the scan completes. Filter projections keep
    /// an `Arc` snapshot and walk it off-thread without cloning millions of
    /// nodes; streaming mutation uses `Arc::make_mut` while no projection is
    /// allowed to retain a snapshot.
    tree: Arc<DiskUsageTree>,
    stats: DiskUsageStats,
    scan_complete: bool,
    error: Option<EnumerationError>,
    cancel: Arc<AtomicBool>,
    engine: DuEngine,
    active_engine: DuEngine,
    fast_eligibility: FastEligibility,
    #[cfg(target_os = "windows")]
    fast_fallback: Option<FastFallbackReason>,
    #[cfg(target_os = "windows")]
    fast_best_effort_live: bool,
    #[cfg(target_os = "windows")]
    fast_progress: Option<ferail_ntfs::Progress>,
    #[cfg(target_os = "windows")]
    fast_scan_elapsed: Option<Duration>,

    /// Queue of messages produced by the BG scanner; drained by the
    /// FG timer task. `Arc<Mutex<_>>` for cross-thread share.
    msg_queue: Arc<Mutex<VecDeque<ScanMsg>>>,

    /// Cached layout for the current scan + treemap size; invalidated
    /// when new facts come in or the user clicks a rect to zoom.
    layout_cache: Option<DiskUsageLayoutNode>,
    rects_cache: Vec<TreemapRect>,
    treemap_size: Option<(f32, f32)>,
    scan_generation: u64,

    /// `last() == focus` — the deepest folder the user has clicked
    /// into. Empty == root.
    zoom_path: Vec<NodeId>,
    /// Multi-selection over treemap rects / Top-N rows, spec-matching
    /// the file list: plain click selects one, Cmd-click toggles.
    /// In-memory interaction state only.
    selected: HashSet<NodeId>,
    /// Most recently selected node — drives the status line's
    /// single-item detail and Zoom In targeting.
    lead: Option<NodeId>,
    category_filter: Option<FileCategory>,
    /// Text filter over the scanned tree, in the same query language as
    /// the file list's filter box (docs/features/DISK_USAGE.md). Parsed
    /// once per keystroke, then evaluated per node with no allocation.
    text_filter: String,
    text_filter_expr: ferail_core::filter_expr::FilterExpr,
    /// Context mode preserves the complete size map and dims misses. Results
    /// mode asynchronously projects only matches plus their ancestor chain.
    filter_results_only: bool,
    filtered_layout: Option<DiskUsageLayoutNode>,
    filter_generation: u64,
    filter_pending: bool,
    filter_cancel: Option<Arc<AtomicBool>>,
    /// Non-zero while a background projection holds an immutable snapshot of
    /// the streaming tree. The queue drain pauses mutations for that short
    /// window, avoiding a huge `Arc::make_mut` clone on the UI thread.
    filter_snapshot_gate: Arc<AtomicU64>,
    /// The filter field, only when this view owns its window; docked in
    /// a tab the shell's own toolbar filter drives us instead.
    filter_input: Option<Entity<InputState>>,
    /// Weak handle to the owning Shell, when opened from one (always,
    /// in the real app; `None` in the screenshot harness). Lets the
    /// context menu open Get Info windows and reload affected tabs
    /// after a trash.
    pub(crate) shell: Option<gpui::WeakEntity<crate::shell::Shell>>,
    /// `(node, is_container)` recorded by a rect's right-mouse-down,
    /// consumed by the treemap's ONE context-menu builder to route
    /// between the rect menu and the background menu. One menu layer
    /// by design: gpui-component's ContextMenu overlay hitbox paints
    /// above the rects, so stacking a menu per rect made the
    /// container's menu open too (and its background handling wiped
    /// the selection).
    menu_rect_target: Option<(NodeId, bool)>,

    size_mode: SizeMode,
    descend_packages: bool,

    /// Capacity of the volume containing `root_path`, when known.
    /// Renders as a Finder-style "X.X GB free of Y.Y GB" capacity
    /// bar in the header.
    volume: Option<ferail_fs_native::VolumeInfo>,

    /// Top-N largest files in the scanned tree, recomputed when a
    /// new fact batch lands. Capped at 50 entries.
    top_files: Vec<TopFileEntry>,
    /// Show the Top-N panel? Toggleable via the header chip.
    topn_visible: bool,

    /// Shared task registry from the parent Shell. The DU view
    /// `begin`s a task at scan start, optionally updates progress, and
    /// `end`s it when the scan finishes — so the status bar's progress
    /// strip stays live while the DU view scans.
    tasks: Rc<RefCell<TaskRegistry>>,
    /// Active task id while the scan is in flight. `None` after Done.
    task_id: Option<TaskId>,
    /// Optional callback invoked after a `tasks` mutation so the
    /// owning Shell can `cx.notify` itself (the registry is plain
    /// `Rc<RefCell>` so it has no built-in observers).
    notify_owner: Option<NotifyOwner>,
    /// Optional callback for standalone windows that can return to a
    /// shell tab. `None` when already docked or opened without an owner.
    dock_owner: Option<DockOwner>,
    /// Current host placement. Docked DU can rely on the shell breadcrumb for
    /// the root path; windowed DU must show the path itself.
    host: ToolHostContext,

    /// Last measured size of the host element. A standalone DU window and
    /// a docked shell pane use the same view; render falls back to the
    /// native window viewport on the first frame, then sizes from this
    /// measured container so docked DU does not assume it owns the whole
    /// shell window.
    host_size: Option<(f32, f32)>,

    focus_handle: FocusHandle,
}

/// One entry in the Top-N largest-files panel.
#[derive(Clone, Debug)]
struct TopFileEntry {
    node_id: NodeId,
    category: FileCategory,
    name: String,
    size_bytes: u64,
}

struct FilterProjection {
    layout: Option<DiskUsageLayoutNode>,
    top_files: Vec<TopFileEntry>,
}

const TOPN_CAP: usize = 50;
const TOPN_PANEL_WIDTH: f32 = 240.0;

fn node_matches_text_filter(
    expr: &ferail_core::filter_expr::FilterExpr,
    node: &ferail_disk_usage::DiskUsageNode,
    size_mode: SizeMode,
) -> bool {
    if expr.is_empty() {
        return true;
    }
    let kind = match node.kind {
        ferail_disk_usage::NodeKind::Container => ferail_core::EntryKind::Directory,
        ferail_disk_usage::NodeKind::File => ferail_core::EntryKind::File,
    };
    let size = if matches!(node.kind, ferail_disk_usage::NodeKind::Container) {
        match size_mode {
            SizeMode::Apparent => node.descendant_size_bytes.max(node.size_bytes),
            SizeMode::Allocated => node
                .descendant_effective_allocated_bytes
                .max(node.allocated_bytes)
                .max(node.size_bytes),
        }
    } else {
        size_for_mode(node.size_bytes, node.allocated_bytes, size_mode)
    };
    let parts = ferail_core::filter_expr::FilterParts {
        name: &node.display_name,
        kind: Some(kind),
        size: Some(size),
        mtime_unix: node.mtime.and_then(|mtime| {
            mtime
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|duration| duration.as_secs() as i64)
        }),
        // The DU scanner does not retain these attributes. An unsupported
        // predicate honestly matches nothing, just like FilterExpr promises.
        created_unix: None,
        locked: None,
    };
    let haystack = format!(
        "{} {}",
        node.display_name.to_lowercase(),
        category_label(node.file_category).to_lowercase()
    );
    expr.text_matches(&haystack) && expr.metadata_matches_parts(&parts)
}

fn node_matches_projection(
    expr: &ferail_core::filter_expr::FilterExpr,
    category: Option<FileCategory>,
    node: &ferail_disk_usage::DiskUsageNode,
    size_mode: SizeMode,
) -> bool {
    if let Some(category) = category {
        // Folders remain structural ancestors. Category chips describe file
        // content, so an "Other" chip must not accidentally make every
        // directory a direct match and restore its complete subtree.
        if node.kind != ferail_disk_usage::NodeKind::File || node.file_category != category {
            return false;
        }
    }
    node_matches_text_filter(expr, node, size_mode)
}

fn build_filter_projection(
    tree: &DiskUsageTree,
    focus: NodeId,
    expr: ferail_core::filter_expr::FilterExpr,
    category: Option<FileCategory>,
    size_mode: SizeMode,
    cancel: &AtomicBool,
) -> FilterProjection {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    #[derive(Clone, Copy, Eq, PartialEq)]
    struct Candidate {
        node_id: NodeId,
        category: FileCategory,
        size_bytes: u64,
    }
    impl Ord for Candidate {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            (self.size_bytes, self.node_id).cmp(&(other.size_bytes, other.node_id))
        }
    }
    impl PartialOrd for Candidate {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    let layout = build_filtered_layout_node_with_mode(
        tree,
        focus,
        DU_LAYOUT_DEPTH,
        size_mode,
        |node| node_matches_projection(&expr, category, node, size_mode),
        || cancel.load(Ordering::Acquire),
    );
    let mut top = BinaryHeap::with_capacity(TOPN_CAP + 1);
    if !cancel.load(Ordering::Acquire) {
        for (&node_id, node) in &tree.nodes {
            if cancel.load(Ordering::Acquire) {
                break;
            }
            if node.kind != ferail_disk_usage::NodeKind::File
                || !node_matches_projection(&expr, category, node, size_mode)
            {
                continue;
            }
            let size_bytes = size_for_mode(node.size_bytes, node.allocated_bytes, size_mode);
            if size_bytes == 0 {
                continue;
            }
            let candidate = Candidate {
                node_id,
                category: node.file_category,
                size_bytes,
            };
            if top.len() < TOPN_CAP {
                top.push(Reverse(candidate));
            } else if top.peek().is_some_and(|smallest| candidate > smallest.0) {
                top.pop();
                top.push(Reverse(candidate));
            }
        }
    }
    let mut top: Vec<_> = top.into_iter().map(|entry| entry.0).collect();
    top.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.size_bytes));
    let top_files = top
        .into_iter()
        .filter_map(|entry| {
            tree.nodes.get(&entry.node_id).map(|node| TopFileEntry {
                node_id: entry.node_id,
                category: entry.category,
                name: node.display_name.clone(),
                size_bytes: entry.size_bytes,
            })
        })
        .collect();
    FilterProjection { layout, top_files }
}

impl DiskUsageView {
    pub fn new(
        root_path: PathBuf,
        fs: Arc<NativeFs>,
        tasks: Rc<RefCell<TaskRegistry>>,
        notify_owner: Option<NotifyOwner>,
        dock_owner: Option<DockOwner>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_engine(root_path, fs, tasks, notify_owner, dock_owner, None, cx)
    }

    /// Deterministic constructor for the hidden screenshot harness. A visual
    /// smoke test must not request elevation or depend on an out-of-process
    /// helper completing inside the fixed capture settle delay.
    pub(crate) fn new_for_screenshot(
        root_path: PathBuf,
        fs: Arc<NativeFs>,
        tasks: Rc<RefCell<TaskRegistry>>,
        notify_owner: Option<NotifyOwner>,
        dock_owner: Option<DockOwner>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_engine(
            root_path,
            fs,
            tasks,
            notify_owner,
            dock_owner,
            Some(DuEngine::Portable),
            cx,
        )
    }

    fn new_with_engine(
        root_path: PathBuf,
        fs: Arc<NativeFs>,
        tasks: Rc<RefCell<TaskRegistry>>,
        notify_owner: Option<NotifyOwner>,
        dock_owner: Option<DockOwner>,
        engine_override: Option<DuEngine>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Keep construction UI-cheap. The background scanner performs
        // canonicalisation before walking; opening the window should
        // not wait on filesystem resolution.
        let canonical = root_path.clone();
        let id_base = next_du_id_base();
        let path_arena = DiskUsagePathArena::new(canonical.clone(), id_base);
        let root_id = path_arena.root_id();
        let cancel = Arc::new(AtomicBool::new(false));
        let msg_queue = Arc::new(Mutex::new(VecDeque::new()));
        // Volume capacity for the header bar arrives off-thread below —
        // the NSURL/statfs lookup can round-trip to a network mount,
        // and this constructor runs on the UI thread.
        let volume = None;
        let engine = engine_override.unwrap_or_else(DuEngine::from_preference);
        #[cfg(target_os = "windows")]
        let fast_eligibility = FastEligibility::Checking;
        #[cfg(not(target_os = "windows"))]
        let fast_eligibility = FastEligibility::Ineligible;
        let mut view = Self {
            root_path: canonical.clone(),
            root_id,
            fs: fs.clone(),
            path_arena,
            tree: Arc::new(DiskUsageTree::new(root_id)),
            stats: DiskUsageStats::default(),
            scan_complete: false,
            error: None,
            cancel: cancel.clone(),
            engine,
            active_engine: engine,
            fast_eligibility,
            #[cfg(target_os = "windows")]
            fast_fallback: None,
            #[cfg(target_os = "windows")]
            fast_best_effort_live: false,
            #[cfg(target_os = "windows")]
            fast_progress: None,
            #[cfg(target_os = "windows")]
            fast_scan_elapsed: None,
            msg_queue: msg_queue.clone(),
            layout_cache: None,
            rects_cache: Vec::new(),
            treemap_size: None,
            scan_generation: 0,
            zoom_path: Vec::new(),
            selected: HashSet::new(),
            lead: None,
            category_filter: None,
            text_filter: String::new(),
            text_filter_expr: ferail_core::filter_expr::FilterExpr::default(),
            filter_results_only: false,
            filtered_layout: None,
            filter_generation: 0,
            filter_pending: false,
            filter_cancel: None,
            filter_snapshot_gate: Arc::new(AtomicU64::new(0)),
            filter_input: None,
            shell: None,
            menu_rect_target: None,
            size_mode: SizeMode::Apparent,
            descend_packages: false,
            volume,
            top_files: Vec::new(),
            topn_visible: true,
            tasks,
            task_id: None,
            notify_owner,
            dock_owner,
            host: ToolHostContext::Windowed,
            host_size: None,
            focus_handle: cx.focus_handle(),
        };
        view.start_scan(fs, cx);
        #[cfg(target_os = "windows")]
        {
            let probe_path = canonical.clone();
            cx.spawn(async move |this, cx| {
                let eligible = cx
                    .background_executor()
                    .spawn(async move { ferail_ntfs_win32::probe_fast_ntfs(&probe_path).is_ok() })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.fast_eligibility = if eligible {
                        FastEligibility::Eligible
                    } else {
                        FastEligibility::Ineligible
                    };
                    cx.notify();
                });
            })
            .detach();
        }
        // Fetch the header capacity-bar volume info off-thread. The
        // root never changes for a DU view, so no staleness guard is
        // needed beyond the entity being alive.
        let vol_path = canonical;
        cx.spawn(async move |this, cx| {
            let volume = cx
                .background_executor()
                .spawn(async move { ferail_fs_native::volume_info_for_path(&vol_path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.volume = volume;
                cx.notify();
            });
        })
        .detach();
        view
    }

    pub fn set_dock_owner(&mut self, dock_owner: Option<DockOwner>, cx: &mut Context<Self>) {
        self.dock_owner = dock_owner;
        cx.notify();
    }

    pub fn handle_host_event(&mut self, event: ToolHostEvent, cx: &mut Context<Self>) {
        match event {
            ToolHostEvent::HostChanged(context) => self.host = context,
        }
        cx.notify();
    }

    /// Mutate the shared task registry and nudge the owner Shell to
    /// repaint its status bar. Called at scan-begin and scan-end.
    fn with_tasks<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut TaskRegistry) -> R,
    ) -> R {
        let result = {
            let mut reg = self.tasks.borrow_mut();
            f(&mut reg)
        };
        if let Some(n) = self.notify_owner.clone() {
            let app: &mut App = std::borrow::BorrowMut::borrow_mut(cx);
            app.defer(move |cx| n(cx));
        }
        result
    }

    /// Recompute the Top-N largest-files list from the current tree.
    /// Single pass + partial sort, capped at `TOPN_CAP`.
    fn rebuild_top_files(&mut self) {
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;

        let node_count = self.tree.nodes.len();
        let started = Instant::now();
        if node_count >= DU_LARGE_TREE_NODES {
            crate::obs::breadcrumb(format_args!("du/top-n begin nodes={node_count}"));
        }

        #[derive(Clone, Copy)]
        struct TopFileCandidate {
            node_id: NodeId,
            category: FileCategory,
            size_bytes: u64,
        }

        impl PartialEq for TopFileCandidate {
            fn eq(&self, other: &Self) -> bool {
                (self.size_bytes, self.node_id) == (other.size_bytes, other.node_id)
            }
        }
        impl Eq for TopFileCandidate {}
        impl PartialOrd for TopFileCandidate {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }
        impl Ord for TopFileCandidate {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                (self.size_bytes, self.node_id).cmp(&(other.size_bytes, other.node_id))
            }
        }

        // Keep only fifty candidates while scanning the tree. The old path
        // allocated one candidate per file and then partitioned the million-
        // item Vec on the UI thread every 500 ms.
        let mut top: BinaryHeap<Reverse<TopFileCandidate>> =
            BinaryHeap::with_capacity(TOPN_CAP + 1);
        for (id, node) in &self.tree.nodes {
            if node.kind != ferail_disk_usage::NodeKind::File
                || self
                    .category_filter
                    .is_some_and(|cat| cat != node.file_category)
                || !self.passes_text_filter(node)
            {
                continue;
            }
            let size_bytes = size_for_mode(node.size_bytes, node.allocated_bytes, self.size_mode);
            if size_bytes == 0 {
                continue;
            }
            let candidate = TopFileCandidate {
                node_id: *id,
                category: node.file_category,
                size_bytes,
            };
            if top.len() < TOPN_CAP {
                top.push(Reverse(candidate));
            } else if top.peek().is_some_and(|smallest| candidate > smallest.0) {
                top.pop();
                top.push(Reverse(candidate));
            }
        }
        let mut all: Vec<_> = top.into_iter().map(|entry| entry.0).collect();
        all.sort_unstable_by_key(|e| std::cmp::Reverse(e.size_bytes));
        self.top_files = all
            .into_iter()
            .map(|e| TopFileEntry {
                node_id: e.node_id,
                category: e.category,
                name: self
                    .tree
                    .nodes
                    .get(&e.node_id)
                    .map(|n| n.display_name.clone())
                    .unwrap_or_default(),
                size_bytes: e.size_bytes,
            })
            .collect();
        let elapsed = started.elapsed();
        if node_count >= DU_LARGE_TREE_NODES {
            crate::obs::breadcrumb(format_args!(
                "du/top-n end nodes={node_count} elapsed_ms={}",
                elapsed.as_millis()
            ));
        }
        if elapsed >= Duration::from_millis(250) {
            crate::log_warn!(
                90,
                "disk-usage: top-n refresh took {} ms for {node_count} nodes",
                elapsed.as_millis()
            );
        }
    }

    /// Spawn the disk-usage scan on the background executor + start
    /// the FG drain timer.
    fn start_scan(&mut self, fs: Arc<NativeFs>, cx: &mut Context<Self>) {
        self.scan_generation = self.scan_generation.wrapping_add(1);
        let generation = self.scan_generation;
        let engine = self.engine;
        self.active_engine = engine;
        #[cfg(target_os = "windows")]
        {
            self.fast_fallback = None;
            self.fast_best_effort_live = false;
            self.fast_progress = None;
            self.fast_scan_elapsed = None;
        }
        let request = DuScanRequest {
            root: self.root_path.clone(),
            cancel: self.cancel.clone(),
            descend_packages: self.descend_packages,
            #[cfg(target_os = "windows")]
            size_mode: self.size_mode,
            id_base: self.path_arena.id_base,
            #[cfg(target_os = "windows")]
            request_id: generation.max(1),
        };
        let queue_for_scan = self.msg_queue.clone();

        // Register the scan with the shared task registry so the
        // owning Shell's status-bar progress strip shows indeterminate
        // motion for the duration. Cancellable: the status bar / task
        // panel will pick that up when we wire the cancel button.
        let task_label = tr!("Scanning {path}", path = short_path(&self.root_path)).to_string();
        let task_id = self.with_tasks(cx, |reg| reg.begin(TaskKind::DiskUsage, task_label, true));
        self.task_id = Some(task_id);

        // BG: run the scan. Synchronous I/O on the executor's pool.
        cx.background_executor()
            .spawn(async move { run_scan_worker(engine, fs, request, queue_for_scan) })
            .detach();

        // FG: drain the queue periodically + apply on the view.
        // CRITICAL: expensive work (layout invalidation, top-N rebuild,
        // cx.notify) runs ONCE per drain tick — not once per message —
        // because at peak scan rate dozens of batches accumulate
        // between drains. Doing 50× sorts of a million-node tree in a
        // single main-thread update is what was freezing the UI.
        let queue_for_drain = self.msg_queue.clone();
        let filter_snapshot_gate = self.filter_snapshot_gate.clone();
        cx.spawn(async move |this, cx| {
            let mut last_topn_rebuild = Instant::now() - DU_TOPN_REBUILD_INTERVAL;
            let mut last_layout_rebuild = Instant::now() - DU_LAYOUT_REBUILD_INTERVAL;
            let mut interval = DU_DRAIN_INTERVAL_IDLE;
            loop {
                cx.background_executor().timer(interval).await;
                // A results-only worker briefly owns an immutable `Arc`
                // snapshot. Leave incoming batches in the bounded queue until
                // it releases that snapshot; mutating now would make
                // `Arc::make_mut` clone the entire tree on this UI task.
                if filter_snapshot_gate.load(Ordering::Acquire) != 0 {
                    interval = DU_DRAIN_INTERVAL_BUSY;
                    continue;
                }
                let (msgs, more_pending): (Vec<ScanMsg>, bool) = match queue_for_drain.lock() {
                    Ok(mut q) => {
                        let take = q.len().min(DU_MAX_MSGS_PER_TICK);
                        let mut msgs = Vec::with_capacity(take);
                        for _ in 0..take {
                            if let Some(msg) = q.pop_front() {
                                msgs.push(msg);
                            }
                        }
                        (msgs, !q.is_empty())
                    }
                    Err(_) => break,
                };
                if msgs.is_empty() {
                    interval = DU_DRAIN_INTERVAL_IDLE;
                    continue;
                }
                interval = if more_pending {
                    DU_DRAIN_INTERVAL_BUSY
                } else {
                    DU_DRAIN_INTERVAL_IDLE
                };
                let mut done = false;
                let mut had_batch = false;
                let mut had_progress = false;
                let mut stale = false;
                let update_result = this.update(cx, |v, cx| {
                    if v.scan_generation != generation {
                        stale = true;
                        return;
                    }
                    for msg in msgs {
                        match &msg {
                            ScanMsg::Batch(_) => had_batch = true,
                            #[cfg(target_os = "windows")]
                            ScanMsg::ResetForFallback(_) | ScanMsg::FastComplete { .. } => {
                                had_batch = true
                            }
                            ScanMsg::Done(_) => done = true,
                            ScanMsg::Progress(_) => had_progress = true,
                            #[cfg(target_os = "windows")]
                            ScanMsg::FastProgress(_) => had_progress = true,
                        }
                        v.apply_scan_msg(msg);
                    }
                    if had_batch || done {
                        // Streaming layout throttle: invalidate + full-tree
                        // rebuild is the expensive half of a drain tick, so
                        // while batches are streaming it runs at most every
                        // DU_LAYOUT_REBUILD_INTERVAL. Done always rebuilds so
                        // the final frame is exact; ticks in between still
                        // notify so the header counters stay live against
                        // the last-built treemap.
                        let rebuild_layout = done
                            || last_layout_rebuild.elapsed()
                                >= layout_rebuild_interval(v.tree.nodes.len());
                        if rebuild_layout {
                            if v.projection_filter_active() {
                                // Preserve the last matching projection while
                                // a same-query refresh incorporates newly
                                // scanned facts. Only the ordinary fallback is
                                // stale; replacing a useful filtered map with
                                // the full dimmed map every few seconds would
                                // visibly pulse throughout a long scan.
                                v.layout_cache = None;
                                v.schedule_filter_projection(cx);
                            } else {
                                v.invalidate_layout();
                                v.rebuild_layout_if_ready();
                            }
                            last_layout_rebuild = Instant::now();
                        }
                        let rebuild_topn = done
                            || last_topn_rebuild.elapsed()
                                >= topn_rebuild_interval(v.tree.nodes.len());
                        if rebuild_topn && !v.projection_filter_active() {
                            v.rebuild_top_files();
                            last_topn_rebuild = Instant::now();
                        }
                    }
                    if had_batch || had_progress || done {
                        cx.notify();
                    }
                    if done {
                        if let Some(id) = v.task_id.take() {
                            v.with_tasks(cx, |reg| reg.end(id));
                        }
                    }
                });
                if update_result.is_err() {
                    break;
                }
                if stale || done {
                    break;
                }
            }
        })
        .detach();
    }

    /// Pure data application — no cache invalidation, no notify. The
    /// drain loop batches those so they happen once per tick.
    fn apply_scan_msg(&mut self, msg: ScanMsg) {
        match msg {
            ScanMsg::Batch(batch) => {
                #[cfg(not(target_os = "windows"))]
                let ScanBatch::Portable(facts) = batch;
                #[cfg(target_os = "windows")]
                let facts = match batch {
                    ScanBatch::Portable(facts) => facts,
                    ScanBatch::Fast(batch) => {
                        for (node, raw_name) in &batch.raw_names {
                            self.path_arena.set_raw_name(*node, raw_name);
                        }
                        self.stats = batch.stats;
                        batch.facts
                    }
                };
                self.path_arena.apply_facts(&facts);
                Arc::make_mut(&mut self.tree).apply_facts(&facts);
            }
            ScanMsg::Progress(p) => self.stats = p,
            #[cfg(target_os = "windows")]
            ScanMsg::FastProgress(progress) => self.fast_progress = Some(progress),
            #[cfg(target_os = "windows")]
            ScanMsg::ResetForFallback(reason) => {
                let id_base = self.path_arena.id_base;
                self.path_arena = DiskUsagePathArena::new(self.root_path.clone(), id_base);
                self.root_id = self.path_arena.root_id();
                self.tree = Arc::new(DiskUsageTree::new(self.root_id));
                self.stats = DiskUsageStats::default();
                self.error = None;
                self.active_engine = DuEngine::Portable;
                self.fast_fallback = Some(reason);
                self.fast_best_effort_live = false;
                self.fast_progress = None;
                self.fast_scan_elapsed = None;
                self.zoom_path.clear();
                self.selected.clear();
                self.lead = None;
                self.top_files.clear();
            }
            #[cfg(target_os = "windows")]
            ScanMsg::FastComplete {
                best_effort_live,
                elapsed,
            } => {
                self.fast_best_effort_live = best_effort_live;
                self.fast_progress = None;
                self.fast_scan_elapsed = Some(elapsed);
            }
            ScanMsg::Done(err) => {
                self.scan_complete = true;
                self.error = err;
                Arc::make_mut(&mut self.tree).complete = self.error.is_none();
            }
        }
    }

    fn invalidate_layout(&mut self) {
        self.layout_cache = None;
        self.filtered_layout = None;
        self.rects_cache.clear();
    }

    fn projection_filter_active(&self) -> bool {
        self.filter_results_only
            && (!self.text_filter_expr.is_empty() || self.category_filter.is_some())
    }

    /// Cancel any results-only projection without touching the scan. The
    /// ordinary treemap remains backed by `layout_cache`; its tiles are
    /// dimmed directly from the current predicate and Top-N is rebuilt with
    /// that same predicate.
    fn leave_filter_projection(&mut self) {
        if let Some(cancel) = self.filter_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.filter_generation = self.filter_generation.wrapping_add(1).max(1);
        self.filter_pending = false;
        self.filtered_layout = None;
        self.rebuild_layout_if_ready();
        self.rebuild_top_files();
    }

    fn rebuild_layout_if_ready(&mut self) {
        let Some((w, h)) = self.treemap_size else {
            return;
        };
        let node_count = self.tree.nodes.len();
        let started = Instant::now();
        if node_count >= DU_LARGE_TREE_NODES {
            crate::obs::breadcrumb(format_args!("du/layout begin nodes={node_count}"));
        }
        // While a new projection is pending, keep the ordinary map visible
        // and dim it with the current predicate. This is both useful feedback
        // and a safe fallback during a still-running scan.
        let use_projection = self.projection_filter_active()
            && (!self.filter_pending || self.filtered_layout.is_some());
        if !use_projection && self.layout_cache.is_none() {
            self.layout_cache = Some(build_layout_node_with_mode(
                &self.tree,
                self.focus_id(),
                DU_LAYOUT_DEPTH,
                self.size_mode,
            ));
        }
        let layout = if use_projection {
            self.filtered_layout.as_ref()
        } else {
            self.layout_cache.as_ref()
        };
        self.rects_cache.clear();
        if let Some(layout) = layout {
            self.rects_cache = compute_treemap(layout, (0.0, 0.0, w, h), DU_LAYOUT_DEPTH);
        }
        let elapsed = started.elapsed();
        if node_count >= DU_LARGE_TREE_NODES {
            crate::obs::breadcrumb(format_args!(
                "du/layout end nodes={node_count} rects={} elapsed_ms={}",
                self.rects_cache.len(),
                elapsed.as_millis()
            ));
        }
        if elapsed >= Duration::from_millis(250) {
            crate::log_warn!(
                90,
                "disk-usage: layout refresh took {} ms for {node_count} nodes and {} rects",
                elapsed.as_millis(),
                self.rects_cache.len()
            );
        }
    }

    fn update_treemap_size(&mut self, width: f32, height: f32, cx: &mut Context<Self>) {
        let next = (width.max(260.0).round(), height.max(220.0).round());
        if self.treemap_size == Some(next) {
            return;
        }
        self.treemap_size = Some(next);
        self.rebuild_layout_if_ready();
        cx.notify();
    }

    fn update_host_size(&mut self, width: f32, height: f32, cx: &mut Context<Self>) {
        let next = (width.max(260.0).round(), height.max(220.0).round());
        if self.host_size == Some(next) {
            return;
        }
        self.host_size = Some(next);
        cx.notify();
    }

    fn restart_scan(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(cancel) = self.filter_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.filter_generation = self.filter_generation.wrapping_add(1).max(1);
        self.filter_snapshot_gate.store(0, Ordering::Release);
        self.filter_pending = self.projection_filter_active();
        if let Some(id) = self.task_id.take() {
            self.with_tasks(cx, |reg| reg.end(id));
        }
        let id_base = next_du_id_base();
        self.path_arena = DiskUsagePathArena::new(self.root_path.clone(), id_base);
        self.root_id = self.path_arena.root_id();
        self.tree = Arc::new(DiskUsageTree::new(self.root_id));
        self.stats = DiskUsageStats::default();
        self.scan_complete = false;
        self.error = None;
        self.active_engine = self.engine;
        #[cfg(target_os = "windows")]
        {
            self.fast_fallback = None;
            self.fast_best_effort_live = false;
            self.fast_progress = None;
            self.fast_scan_elapsed = None;
        }
        self.cancel = Arc::new(AtomicBool::new(false));
        self.msg_queue = Arc::new(Mutex::new(VecDeque::new()));
        self.zoom_path.clear();
        self.selected.clear();
        self.lead = None;
        self.top_files.clear();
        self.invalidate_layout();
        self.rebuild_layout_if_ready();
        self.start_scan(self.fs.clone(), cx);
        cx.notify();
    }

    /// User clicked the header's stop button. Three things happen
    /// together so the UI feedback is instant rather than waiting on
    /// the cooperative cancel to actually unwind:
    ///   1. Tell the worker to stop (it'll exit at the next dirent
    ///      or directory boundary; harmless if it finishes naturally
    ///      first).
    ///   2. Bump `scan_generation` so the drain task sees a stale
    ///      generation at its next tick and breaks. Late
    ///      `ScanMsg::Batch`/`Done` from the dying worker land in the
    ///      orphan queue and are never applied — accumulated tree
    ///      data stays exactly where it was at click time.
    ///   3. Flip `scan_complete = true` locally so the header swaps
    ///      from "Scanning…" / Stop button to the final summary +
    ///      Refresh button immediately.
    ///
    /// Also ends the registry task entry so the parent Shell's
    /// status-bar progress strip stops showing this scan as in
    /// flight.
    fn cancel_scan(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.scan_complete = true;
        if self.projection_filter_active() {
            self.schedule_filter_projection(cx);
        }
        if let Some(id) = self.task_id.take() {
            self.with_tasks(cx, |reg| reg.end(id));
        }
        // The drain loop breaks on the stale generation without a final
        // pass, and streaming ticks throttle layout rebuilds — so rebuild
        // here (user-driven, immediate) to show every fact applied so far.
        self.invalidate_layout();
        self.rebuild_layout_if_ready();
        self.rebuild_top_files();
        cx.notify();
    }

    fn focus_id(&self) -> NodeId {
        self.zoom_path.last().copied().unwrap_or(self.root_id)
    }

    /// Plain click: this node becomes the whole selection.
    fn select_only(&mut self, target: NodeId, cx: &mut Context<Self>) {
        self.selected.clear();
        self.selected.insert(target);
        self.lead = Some(target);
        cx.notify();
    }

    /// Cmd-click: toggle membership, keeping `lead` on a live member.
    fn toggle_select(&mut self, target: NodeId, cx: &mut Context<Self>) {
        if self.selected.remove(&target) {
            if self.lead == Some(target) {
                self.lead = self.selected.iter().next().copied();
            }
        } else {
            self.selected.insert(target);
            self.lead = Some(target);
        }
        cx.notify();
    }

    /// Right-click target rule (Finder-style): clicking a node outside
    /// the current selection retargets the selection to just it;
    /// clicking a member keeps the whole selection as the menu target.
    fn ensure_selected_for_menu(&mut self, target: NodeId, cx: &mut Context<Self>) {
        if !self.selected.contains(&target) {
            self.select_only(target, cx);
        } else {
            self.lead = Some(target);
        }
    }

    /// The selection resolved to `(path, is_dir, NodeId)` triples via
    /// the id map — action handlers only (never render).
    fn selected_paths(&self) -> Vec<(PathBuf, bool, NodeId)> {
        let mut out = Vec::with_capacity(self.selected.len());
        for id in &self.selected {
            let Some(path) = self.path_arena.path_for(*id, &self.tree) else {
                continue;
            };
            let is_dir = self
                .tree
                .nodes
                .get(id)
                .map(|n| matches!(n.kind, ferail_disk_usage::NodeKind::Container))
                .unwrap_or(false);
            out.push((path, is_dir, *id));
        }
        // Stable order for path lists / fanout.
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn toggle_category_filter(&mut self, category: FileCategory, cx: &mut Context<Self>) {
        self.category_filter = if self.category_filter == Some(category) {
            None
        } else {
            Some(category)
        };
        if self.projection_filter_active() {
            self.filtered_layout = None;
            self.schedule_filter_projection(cx);
        } else {
            self.leave_filter_projection();
        }
        cx.notify();
    }

    /// Apply a text filter in the shared filter language. Called from the
    /// view's own field when windowed, and forwarded from the shell's
    /// toolbar filter when docked in a tab, so one code path serves both
    /// hosts. Pure in-memory work over the already-scanned tree: no
    /// rescan, no I/O.
    pub fn apply_filter(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.text_filter == text {
            return;
        }
        self.text_filter = text.to_string();
        self.text_filter_expr = ferail_core::filter_expr::FilterExpr::parse(
            text.trim(),
            ferail_core::filter_expr::DateCtx {
                now_unix: ferail_core::now_unix(),
                tz_offset_secs: ferail_fs_native::stat_info::local_tz_offset_secs(),
            },
        );
        if self.projection_filter_active() {
            self.filtered_layout = None;
            self.schedule_filter_projection(cx);
        } else {
            self.leave_filter_projection();
        }
        cx.notify();
    }

    fn toggle_filter_results_only(&mut self, cx: &mut Context<Self>) {
        self.filter_results_only = !self.filter_results_only;
        if self.projection_filter_active() {
            self.schedule_filter_projection(cx);
        } else {
            self.leave_filter_projection();
        }
        cx.notify();
    }

    /// Deterministic hook for the headless visual-regression harness. Keeping
    /// it here exercises the same toggle path as the real toolbar button.
    pub(crate) fn set_filter_results_only_for_screenshot(
        &mut self,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        if self.filter_results_only != enabled {
            self.toggle_filter_results_only(cx);
        }
    }

    /// Debounced, cancellable projection for results-only mode. During a live
    /// scan, the bounded queue drain pauses only while this worker owns its
    /// immutable `Arc` snapshot. That keeps results live without restarting
    /// the scan or triggering a multi-million-node copy-on-write clone on the
    /// UI thread.
    fn schedule_filter_projection(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.filter_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.filter_generation = self.filter_generation.wrapping_add(1).max(1);
        let generation = self.filter_generation;
        if !self.projection_filter_active() {
            self.filter_pending = false;
            self.filtered_layout = None;
            self.rebuild_layout_if_ready();
            return;
        }
        self.filter_pending = true;
        // Show the current full map, dimmed with the new predicate, throughout
        // the debounce/projection rather than flashing an empty surface.
        self.rebuild_layout_if_ready();

        let cancel = Arc::new(AtomicBool::new(false));
        self.filter_cancel = Some(cancel.clone());
        let expr = self.text_filter_expr.clone();
        let category = self.category_filter;
        let mode = self.size_mode;
        let gate = self.filter_snapshot_gate.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            if cancel.load(Ordering::Acquire) {
                return;
            }
            // Gate and snapshot in one foreground update. No queue mutation can
            // interleave between the two operations.
            let snapshot = this
                .update(cx, |view, _| {
                    if view.filter_generation != generation
                        || !view.projection_filter_active()
                        || cancel.load(Ordering::Acquire)
                    {
                        return None;
                    }
                    gate.store(generation, Ordering::Release);
                    Some((view.tree.clone(), view.focus_id()))
                })
                .ok()
                .flatten();
            let Some((tree, focus)) = snapshot else {
                return;
            };
            let worker_cancel = cancel.clone();
            let projection = cx
                .background_executor()
                .spawn(async move {
                    build_filter_projection(&tree, focus, expr, category, mode, &worker_cancel)
                })
                .await;
            if cancel.load(Ordering::Acquire) {
                let _ = gate.compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire);
                return;
            }
            let _ = this.update(cx, |view, cx| {
                if view.filter_generation != generation || !view.projection_filter_active() {
                    return;
                }
                view.filtered_layout = projection.layout;
                view.top_files = projection.top_files;
                view.filter_pending = false;
                view.rebuild_layout_if_ready();
                cx.notify();
            });
            // An older cancelled generation must never reopen a newer job's
            // gate, hence compare-exchange rather than an unconditional store.
            let _ = gate.compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire);
        })
        .detach();
    }

    /// Current filter text, so a docked host can seed its own field.
    pub fn filter_text(&self) -> &str {
        &self.text_filter
    }

    /// Does this node pass the text filter? Cheap and allocation-free
    /// apart from the lowercased name, evaluated per visible tile and
    /// per Top-N candidate.
    fn passes_text_filter(&self, node: &ferail_disk_usage::DiskUsageNode) -> bool {
        node_matches_text_filter(&self.text_filter_expr, node, self.size_mode)
    }

    fn toggle_size_mode(&mut self, mode: SizeMode, cx: &mut Context<Self>) {
        if self.size_mode == mode {
            return;
        }
        self.size_mode = mode;
        self.invalidate_layout();
        self.rebuild_layout_if_ready();
        if self.projection_filter_active() {
            self.schedule_filter_projection(cx);
        } else {
            self.rebuild_top_files();
        }
        cx.notify();
    }

    fn toggle_packages(&mut self, cx: &mut Context<Self>) {
        if !self.scan_complete {
            return;
        }
        self.descend_packages = !self.descend_packages;
        self.restart_scan(cx);
    }

    fn select_engine(&mut self, engine: DuEngine, cx: &mut Context<Self>) {
        if engine == DuEngine::FastNtfs && self.fast_eligibility != FastEligibility::Eligible {
            return;
        }
        if self.engine == engine {
            return;
        }
        self.engine = engine;
        #[cfg(target_os = "windows")]
        {
            let existing = crate::app_state::load();
            crate::app_state::save(&crate::app_state::AppState {
                disk_usage_engine: Some(
                    match engine {
                        DuEngine::Portable => "portable",
                        DuEngine::FastNtfs => "fast-ntfs",
                    }
                    .to_owned(),
                ),
                ..existing
            });
        }
        self.restart_scan(cx);
    }

    fn engine_summary(&self) -> Option<SharedString> {
        #[cfg(target_os = "windows")]
        {
            if let Some(reason) = self.fast_fallback {
                return Some(match reason {
                    FastFallbackReason::ElevationDeclined => {
                        tr!("Portable fallback — administrator access was declined")
                    }
                    FastFallbackReason::HelperMissing => {
                        tr!("Portable fallback — the Fast NTFS helper is missing")
                    }
                    FastFallbackReason::HelperUntrusted => {
                        tr!("Portable fallback — the Fast NTFS helper does not match this build")
                    }
                    FastFallbackReason::Unsupported => {
                        tr!("Portable fallback — Fast NTFS is unavailable here")
                    }
                    FastFallbackReason::Failed => {
                        tr!("Portable fallback — Fast NTFS could not finish safely")
                    }
                });
            }
            if self.active_engine == DuEngine::FastNtfs {
                return Some(if self.fast_best_effort_live {
                    tr!("Fast NTFS — best effort because files changed during the scan")
                } else {
                    tr!("Fast NTFS engine")
                });
            }
            Some(tr!("Portable engine"))
        }
        #[cfg(not(target_os = "windows"))]
        None
    }

    #[cfg(target_os = "windows")]
    fn fast_scan_summary(&self) -> Option<String> {
        if self.scan_complete || self.active_engine != DuEngine::FastNtfs {
            return None;
        }
        let progress = self.fast_progress?;
        Some(match progress.phase {
            ferail_ntfs::ScanPhase::Opening | ferail_ntfs::ScanPhase::MappingMft => {
                tr!("Preparing NTFS metadata…").to_string()
            }
            ferail_ntfs::ScanPhase::ReadingRecords => {
                let percent = progress
                    .completed
                    .saturating_mul(100)
                    .checked_div(progress.total)
                    .unwrap_or(0);
                tr!(
                    "Reading NTFS metadata… {percent}% · {completed} / {total} records",
                    percent = percent,
                    completed = format_count(progress.completed),
                    total = format_count(progress.total)
                )
                .to_string()
            }
            ferail_ntfs::ScanPhase::BuildingIndex => tr!(
                "Building NTFS index… {live} live records",
                live = format_count(progress.live_records)
            )
            .to_string(),
            ferail_ntfs::ScanPhase::Traversing => tr!("Reading the selected folder…").to_string(),
        })
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let title = if self.host == ToolHostContext::Docked {
            tr!("Disk Usage").to_string()
        } else {
            crate::private_mode::present_path(&self.root_path)
        };
        let scanned = humanize_bytes(crate::private_mode::present_bytes(
            0x4455_5343,
            self.stats.bytes_scanned,
        ));
        // Counts a scan reports run into the millions; group every one of
        // them the way the status bar does (`ferail_core::counts`).
        let files = format_count(self.stats.files_scanned);
        let folders = format_count(self.stats.dirs_scanned);
        let skipped = self.stats.dirs_skipped;
        let scanning = !self.scan_complete;
        // A failed scan must say so — it used to store the error and
        // render "0 files, 0 folders, 0 B", indistinguishable from an
        // empty folder (this hid "canonicalize unsupported" on AROS).
        let mut summary = if let Some(err) = &self.error {
            let why = match err {
                ferail_core::EnumerationError::PermissionDenied => {
                    tr!("permission denied").to_string()
                }
                ferail_core::EnumerationError::NotFound => tr!("folder not found").to_string(),
                ferail_core::EnumerationError::Other(msg) => {
                    crate::private_mode::present_label(msg)
                }
            };
            tr!("Scan failed \u{2014} {detail}", detail = why).to_string()
        } else if self.scan_complete && skipped > 0 {
            trn!(
                "{files} files, {folders} folders, {scanned} · {n} folder skipped",
                "{files} files, {folders} folders, {scanned} · {n} folders skipped",
                skipped as usize,
                files = files,
                folders = folders,
                scanned = scanned
            )
            .to_string()
        } else if self.scan_complete {
            tr!(
                "{files} files, {folders} folders, {scanned}",
                files = files,
                folders = folders,
                scanned = scanned
            )
            .to_string()
        } else if cfg!(target_os = "windows") {
            #[cfg(target_os = "windows")]
            {
                self.fast_scan_summary().unwrap_or_else(|| {
                    tr!(
                        "Scanning… {files} files, {scanned}",
                        files = files,
                        scanned = scanned
                    )
                    .to_string()
                })
            }
            #[cfg(not(target_os = "windows"))]
            unreachable!()
        } else {
            tr!(
                "Scanning\u{2026} {files} files, {scanned}",
                files = files,
                scanned = scanned
            )
            .to_string()
        };
        if let Some(engine) = self.engine_summary() {
            summary.push_str(" · ");
            summary.push_str(&engine);
        }
        #[cfg(target_os = "windows")]
        if self.scan_complete
            && self.active_engine == DuEngine::FastNtfs
            && let Some(elapsed) = self.fast_scan_elapsed
        {
            summary.push_str(" · ");
            summary.push_str(
                tr!(
                    "Scan completed in {elapsed}",
                    elapsed = humanize_duration(elapsed)
                )
                .as_ref(),
            );
        }
        let summary_color = if self.error.is_some() {
            theme.danger
        } else if skipped > 0 {
            theme.warning
        } else {
            theme.muted_foreground
        };
        // Phase 6 follow-on: header action buttons are icon-only.
        // Each one carries a tooltip with the human-readable name
        // so the affordance is recoverable on hover.
        use gpui_component::Icon;
        let scan_button = if scanning {
            Button::new("du-cancel")
                .small()
                .icon(Icon::empty().path("icons/close.svg"))
                .tooltip(tr!("Cancel scan"))
                .on_click(cx.listener(|this, _, _, cx| this.cancel_scan(cx)))
        } else {
            Button::new("du-refresh")
                .small()
                .icon(Icon::empty().path("icons/nav/refresh.svg"))
                .tooltip(tr!("Refresh"))
                .on_click(cx.listener(|this, _, _, cx| this.restart_scan(cx)))
        };
        let dock_button = self.dock_owner.as_ref().map(|dock| {
            let dock = dock.clone();
            let root = self.root_path.clone();
            Button::new("du-dock")
                .small()
                .icon(Icon::empty().path("icons/minimize.svg"))
                .tooltip(tr!("Dock in tab"))
                .on_click(cx.listener(move |_, _, window, cx| {
                    let view = cx.entity().clone();
                    let app: &mut App = std::borrow::BorrowMut::borrow_mut(cx);
                    let dock = dock.clone();
                    let root = root.clone();
                    app.defer(move |cx| dock(root, view, cx));
                    window.remove_window();
                }))
        });
        // Keep the scan identity, progress and controls on one compact row at
        // normal window widths. The row wraps deliberately on small windows;
        // no action disappears and the treemap keeps the remaining height.
        let row = h_flex()
            .w_full()
            .items_center()
            .flex_wrap()
            .gap_2()
            .child(
                div()
                    .min_w(px(120.0))
                    .truncate()
                    .text_scale_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(220.0))
                    .truncate()
                    .text_scale_xs()
                    .text_color(summary_color)
                    .child(SharedString::from(summary)),
            )
            .when(
                cfg!(target_os = "macos") && self.stats.permission_denied_dirs > 0,
                |this| {
                    this.child(
                        Button::new("du-full-disk-access")
                            .small()
                            .icon(Icon::empty().path("icons/lock.svg"))
                            .tooltip(tr!("Include folders protected by macOS in future scans"))
                            .on_click(cx.listener(|_, _, window, cx| {
                                use gpui_component::notification::Notification;
                                if let Some(path) = crate::platform_shell::app_bundle_path() {
                                    cx.write_to_clipboard(ClipboardItem::new_string(path));
                                    window.push_notification(
                                        Notification::info(tr!(
                                            "Ferail's path is copied. Add it in Full Disk Access, then relaunch Ferail."
                                        ))
                                        .autohide(false),
                                        cx,
                                    );
                                }
                                cx.background_spawn(async move {
                                    crate::platform_shell::open_url(
                                        crate::shell::FULL_DISK_ACCESS_SETTINGS_URL,
                                    );
                                })
                                .detach();
                            })),
                    )
                },
            )
            .when(cfg!(target_os = "windows"), |this| {
                let fast_tooltip = match self.fast_eligibility {
                    FastEligibility::Checking => tr!("Checking Fast NTFS availability…"),
                    FastEligibility::Eligible => {
                        tr!("Read NTFS metadata with a temporary administrator helper")
                    }
                    FastEligibility::Ineligible => {
                        tr!("Fast NTFS requires a local fixed NTFS volume")
                    }
                };
                this.child(
                    ButtonGroup::new("du-engine")
                        .small()
                        .outline()
                        .compact()
                        .child(
                            Button::new("du-engine-portable")
                                .label(tr!("Portable"))
                                .selected(self.engine == DuEngine::Portable),
                        )
                        .child(
                            Button::new("du-engine-fast-ntfs")
                                .label(tr!("Fast NTFS"))
                                .tooltip(fast_tooltip)
                                .disabled(self.fast_eligibility != FastEligibility::Eligible)
                                .selected(self.engine == DuEngine::FastNtfs),
                        )
                        .on_click(cx.listener(|this, clicks: &Vec<usize>, _, cx| {
                            match clicks.first().copied() {
                                Some(0) => this.select_engine(DuEngine::Portable, cx),
                                Some(1) => this.select_engine(DuEngine::FastNtfs, cx),
                                _ => {}
                            }
                        })),
                )
            })
            // Filter over the scanned tree, same query language as the file
            // list's filter box. Windowed only: docked in a tab, the shell's
            // toolbar filter drives `apply_filter` instead of a second field.
            .when_some(self.filter_input.clone(), |this, input| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .w(px(220.0))
                        .child(Input::new(&input).xsmall()),
                )
            })
            .child(
                Button::new("du-filter-results-only")
                    .small()
                    .icon(Icon::empty().path("icons/view-list.svg"))
                    .selected(self.filter_results_only)
                    .tooltip(if self.filter_results_only {
                        tr!("Show the full treemap and dim non-matches")
                    } else {
                        tr!("Show matching files only")
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_filter_results_only(cx)
                    })),
            )
            .child(
                Button::new("du-size-apparent")
                    .small()
                    .icon(Icon::empty().path("icons/file/generic.svg"))
                    .selected(self.size_mode == SizeMode::Apparent)
                    .tooltip(tr!("Apparent size"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_size_mode(SizeMode::Apparent, cx)
                    })),
            )
            .child(
                Button::new("du-size-allocated")
                    .small()
                    .icon(Icon::empty().path("icons/file/disk.svg"))
                    .selected(self.size_mode == SizeMode::Allocated)
                    .tooltip(tr!("Allocated size on disk"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_size_mode(SizeMode::Allocated, cx)
                    })),
            )
            .when(cfg!(target_os = "macos"), |this| {
                this.child(
                    Button::new("du-packages")
                        .small()
                        .icon(Icon::empty().path("icons/nav/package.svg"))
                        .selected(self.descend_packages)
                        .disabled(scanning)
                        .tooltip(tr!("Scan package folders as containers"))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_packages(cx))),
                )
            })
            .when_some(dock_button, |this, button| this.child(button))
            .child(
                Button::new("du-up")
                    .small()
                    .icon(Icon::empty().path("icons/arrow-up.svg"))
                    .tooltip(tr!("Zoom out"))
                    .disabled(self.zoom_path.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_out(cx))),
            )
            .child(scan_button)
            .child(
                Button::new("du-topn")
                    .small()
                    .icon(Icon::empty().path(if self.topn_visible {
                        "icons/panel-right-close.svg"
                    } else {
                        "icons/panel-right-open.svg"
                    }))
                    .tooltip(if self.topn_visible {
                        tr!("Hide largest-files panel")
                    } else {
                        tr!("Show largest-files panel")
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.topn_visible = !this.topn_visible;
                        cx.notify();
                    })),
            );

        v_flex()
            .w_full()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(row)
    }

    /// Right-side Top-N panel: scrollable list of the largest files
    /// in the scanned tree. Updates live as new facts arrive.
    fn top_panel(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let rows: Vec<AnyElement> = self
            .top_files
            .iter()
            .enumerate()
            .map(|(ix, e)| {
                let selected = self.selected.contains(&e.node_id);
                let node_id = e.node_id;
                h_flex()
                    .id(("du-top-file", ix))
                    .w_full()
                    .gap_2()
                    .items_center()
                    .py_1()
                    .px_2()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .when(ix % 2 == 0, |this| {
                        this.bg(theme.muted_foreground.opacity(0.04))
                    })
                    .when(selected, |this| this.bg(theme.accent))
                    .hover(|style| style.bg(theme.accent.opacity(0.65)))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.modifiers().platform {
                            this.toggle_select(node_id, cx);
                        } else {
                            this.select_only(node_id, cx);
                        }
                    }))
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(8.0))
                            .h(px(8.0))
                            .rounded(px(2.0))
                            .bg(category_color(e.category)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_scale_xs()
                            .text_color(if selected {
                                theme.accent_foreground
                            } else {
                                theme.foreground
                            })
                            .child(SharedString::from(crate::private_mode::present_leaf_str(
                                &e.name, false,
                            ))),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_scale_xs()
                            .text_color(if selected {
                                theme.accent_foreground.opacity(0.82)
                            } else {
                                theme.muted_foreground
                            })
                            .child(SharedString::from(humanize_bytes(
                                crate::private_mode::present_bytes(
                                    e.node_id.as_raw(),
                                    e.size_bytes,
                                ),
                            ))),
                    )
                    .into_any_element()
            })
            .collect();
        v_flex()
            .w(px(TOPN_PANEL_WIDTH))
            .h_full()
            .flex_shrink_0()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                div()
                    .id("du-topn-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_1()
                    .py_1()
                    .child(if rows.is_empty() {
                        v_flex().w_full().child(
                            div()
                                .px_2()
                                .py_2()
                                .text_scale_xs()
                                .text_color(theme.muted_foreground)
                                .child(tr!("No matching files yet")),
                        )
                    } else {
                        v_flex().w_full().children(rows)
                    }),
            )
    }

    /// Color legend at the bottom. Chips toggle a lightweight category
    /// filter for the Top-N list and dim non-matching treemap tiles.
    fn legend(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let entries = [
            FileCategory::Image,
            FileCategory::Video,
            FileCategory::Audio,
            FileCategory::Document,
            FileCategory::Archive,
            FileCategory::Executable,
            FileCategory::Other,
        ];
        let chips = entries.iter().enumerate().map(|(ix, cat)| {
            let cat = *cat;
            let label = crate::i18n::tr_static(category_label(cat));
            let selected = self.category_filter == Some(cat);
            h_flex()
                .id(("du-category", ix))
                .gap_1()
                .items_center()
                .px_2()
                .py_1()
                .rounded(px(4.0))
                .cursor_pointer()
                .when(selected, |this| {
                    this.bg(theme.accent).border_1().border_color(theme.border)
                })
                .hover(|this| this.bg(theme.accent.opacity(0.55)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_category_filter(cat, cx);
                }))
                .child(
                    div()
                        .w(px(10.0))
                        .h(px(10.0))
                        .rounded(px(2.0))
                        .bg(category_color(cat)),
                )
                .child(
                    div()
                        .text_scale_xs()
                        .text_color(if selected {
                            theme.accent_foreground
                        } else {
                            theme.muted_foreground
                        })
                        .child(label),
                )
        });
        let selection = match self.selected.len() {
            0 => None,
            1 => self
                .lead
                .or_else(|| self.selected.iter().next().copied())
                .and_then(|id| self.tree.nodes.get(&id))
                .map(|n| {
                    let size = size_for_mode(n.size_bytes, n.allocated_bytes, self.size_mode);
                    format!(
                        "{}  {}",
                        crate::private_mode::present_leaf_str(
                            &n.display_name,
                            matches!(n.kind, ferail_disk_usage::NodeKind::Container)
                        ),
                        humanize_bytes(crate::private_mode::present_bytes(0x4455_5345, size))
                    )
                }),
            n => {
                let total: u64 = self
                    .selected
                    .iter()
                    .filter_map(|id| self.tree.nodes.get(id))
                    .map(|node| {
                        size_for_mode(node.size_bytes, node.allocated_bytes, self.size_mode)
                    })
                    .sum();
                Some(
                    tr!(
                        "{n} selected  {size}",
                        n = format_count(n as u64),
                        size =
                            humanize_bytes(crate::private_mode::present_bytes(0x4455_4d55, total))
                    )
                    .to_string(),
                )
            }
        }
        .or_else(|| {
            self.category_filter.map(|cat| {
                tr!(
                    "Filtering {category}",
                    category = crate::i18n::tr_static(category_label(cat))
                )
                .to_string()
            })
        })
        .unwrap_or_else(|| tr!("All categories").to_string());
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_4()
            .py_1()
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .child(h_flex().items_center().gap_1().flex_wrap().children(chips))
            .child(div().flex_1())
            .child(
                div()
                    .max_w(px(360.0))
                    .truncate()
                    .text_scale_xs()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(selection)),
            )
    }

    fn treemap(
        &self,
        w: f32,
        h: f32,
        cx: &mut Context<Self>,
    ) -> gpui_component::menu::ContextMenu<gpui::Stateful<Div>> {
        use gpui_component::menu::ContextMenuExt as _;
        let (w, h) = (w.max(260.0), h.max(220.0));
        let view = cx.entity().clone();
        let weak_bg_menu = cx.weak_entity();
        let zoomed = !self.zoom_path.is_empty();
        let bg_focus = self.focus_handle.clone();
        let mut container = div()
            .id("du-treemap")
            .relative()
            .w(px(w))
            .h(px(h))
            .bg(cx.theme().background)
            .on_prepaint(move |bounds, _, cx| {
                view.update(cx, |this, cx| {
                    this.update_treemap_size(
                        f32::from(bounds.size.width),
                        f32::from(bounds.size.height),
                        cx,
                    );
                });
            })
            // THE one context menu for the whole treemap, routed by
            // what the right-click landed on. Each rect records itself
            // in `menu_rect_target` on right-mouse-down (which runs
            // before the deferred menu build); no per-rect ContextMenu
            // layers — the overlay hitboxes paint above the rects, so
            // stacked layers all fired at once (two menus colliding on
            // screen, and the background arm wiping the selection).
            .context_menu(move |menu, window, cx| {
                let Some(v) = weak_bg_menu.upgrade() else {
                    return menu;
                };
                let (target, count, has_shell) = v.update(cx, |this, cx| {
                    let target = this.menu_rect_target.take();
                    if target.is_none() {
                        // True background click: the view is the
                        // target — drop the rect selection so the
                        // status line and the menu agree.
                        this.selected.clear();
                        this.lead = None;
                        cx.notify();
                    }
                    (target, this.selected.len(), this.shell.is_some())
                });
                let menu = menu.action_context(bg_focus.clone());
                match target {
                    None => menu
                        .menu_with_disabled(tr!("Zoom Out"), Box::new(DuZoomOut), !zoomed)
                        .separator()
                        .menu(tr!("Copy View as HTML"), Box::new(DuCopyViewHtml))
                        .menu(tr!("Save View as HTML\u{2026}"), Box::new(DuSaveViewHtml)),
                    Some((_node, is_container)) => {
                        let single = count == 1;
                        let mut menu = menu
                            .menu(tr!("Open"), Box::new(DuOpen))
                            .menu_with_disabled(
                                tr!("Open in New Tab"),
                                Box::new(DuOpenInNewTab),
                                !(has_shell && single),
                            )
                            .menu(
                                if cfg!(target_os = "macos") {
                                    tr!("Reveal in Finder")
                                } else {
                                    tr!("Reveal in File Manager")
                                },
                                Box::new(DuReveal),
                            );
                        if has_shell {
                            menu = menu.menu(tr!("Get Info"), Box::new(DuGetInfo));
                        }
                        menu = menu
                            .separator()
                            .menu(tr!("Copy"), Box::new(DuCopyFiles))
                            .menu(
                                if single {
                                    tr!("Copy Path")
                                } else {
                                    tr!("Copy Paths")
                                },
                                Box::new(DuCopyPaths),
                            )
                            .separator()
                            .submenu(tr!("Export as HTML"), window, cx, move |menu, _, _| {
                                menu.menu_with_disabled(
                                    tr!("Copy This Folder"),
                                    Box::new(DuCopyHtml),
                                    !(single && is_container),
                                )
                                .menu_with_disabled(
                                    tr!("Save This Folder\u{2026}"),
                                    Box::new(DuSaveHtml),
                                    !(single && is_container),
                                )
                                .separator()
                                .menu(tr!("Copy Whole View"), Box::new(DuCopyViewHtml))
                                .menu(tr!("Save Whole View\u{2026}"), Box::new(DuSaveViewHtml))
                            })
                            .separator()
                            .menu_with_disabled(
                                tr!("Zoom In"),
                                Box::new(DuZoomIn),
                                !(single && is_container),
                            )
                            .menu_with_disabled(tr!("Zoom Out"), Box::new(DuZoomOut), !zoomed)
                            .separator()
                            .menu(tr!("Move to Trash"), Box::new(DuTrash));
                        menu
                    }
                }
            });
        // A Top-N file can live deeper than the treemap's drawing depth. In
        // that case highlight its nearest drawn ancestor instead of leaving
        // the selection invisible.
        let visible_nodes: HashSet<NodeId> =
            self.rects_cache.iter().map(|rect| rect.node_id).collect();
        let visible_selection: HashSet<NodeId> = self
            .selected
            .iter()
            .filter_map(|id| {
                self.path_arena
                    .nearest_visible_ancestor(*id, &visible_nodes)
            })
            .collect();
        for (ix, r) in self.rects_cache.iter().enumerate() {
            if r.width < 1.0 || r.height < 1.0 {
                continue;
            }
            let color = category_color(r.file_category);
            let node_id = r.node_id;
            let has_children = r.has_children;
            let name = self
                .tree
                .nodes
                .get(&r.node_id)
                .map_or_else(String::new, |n| {
                    crate::private_mode::present_leaf_str(
                        &n.display_name,
                        matches!(n.kind, ferail_disk_usage::NodeKind::Container),
                    )
                });
            let size = humanize_bytes(crate::private_mode::present_bytes(
                r.node_id.as_raw(),
                r.size_bytes,
            ));
            let show_label = if r.lays_out_children {
                r.label_strip_height > 0.0
            } else {
                r.width >= 60.0 && r.height >= 24.0
            };
            let show_size = !r.lays_out_children && r.width >= 80.0 && r.height >= 40.0;
            let selected = visible_selection.contains(&node_id);
            let dimmed = (!self.filter_results_only
                || (self.filter_pending && self.filtered_layout.is_none()))
                && (self
                    .category_filter
                    .is_some_and(|category| category != r.file_category)
                    || !self
                        .tree
                        .nodes
                        .get(&r.node_id)
                        .is_none_or(|node| self.passes_text_filter(node)));
            let mut rect = div()
                .absolute()
                .top(px(r.y))
                .left(px(r.x))
                .w(px(r.width))
                .h(px(r.height))
                .bg(color)
                // Selected: the app accent survives both light and dark
                // themes and links Top-N rows to their treemap tile.
                .map(|this| {
                    if selected {
                        this.border_2().border_color(cx.theme().primary)
                    } else {
                        this.border_1().border_color(rgba(0x00000033))
                    }
                })
                .id(("du-rect", ix))
                .cursor_pointer()
                // Topmost-rect-wins for right-clicks: without occlusion
                // every ancestor rect's context menu (and the container's
                // background menu) would ALSO fire — gpui-component's
                // ContextMenu tests `hitbox.is_hovered` without stopping
                // propagation.
                .occlude()
                .when(dimmed, |this| this.opacity(0.26))
                // A label must never escape its tile: without this, a long
                // name wraps, pushes the size line past the bottom edge, and
                // paints over the neighbouring tiles.
                .overflow_hidden()
                .hover(|this| this.border_color(cx.theme().selection));
            // The full name and size, for the tooltip: a truncated tile
            // label is only useful if the whole thing is one hover away.
            let tooltip_text = SharedString::from(if show_size {
                format!("{name}\n{size}")
            } else {
                format!("{name} · {size}")
            });
            rect = rect.tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(tooltip_text.clone()).build(window, cx)
            });
            if show_label {
                let name_label = div()
                    .w_full()
                    .min_w_0()
                    // Middle ellipsis keeps the extension visible,
                    // same treatment as the file list's name cell.
                    .truncate_middle()
                    .text_scale_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgba(0xFFFFFFEE))
                    .child(SharedString::from(name));
                let inner = if r.lays_out_children {
                    // Match the layout's reserved strip exactly. Tile fills
                    // are translucent, so allowing this label surface to
                    // extend into the child area makes the parent's text show
                    // through its descendants.
                    h_flex()
                        .w_full()
                        .h(px(r.label_strip_height))
                        .min_w_0()
                        .items_center()
                        .overflow_hidden()
                        .px_1()
                        .child(name_label)
                        .into_any_element()
                } else {
                    v_flex()
                        .size_full()
                        .min_w_0()
                        .px_1()
                        .py_1()
                        .child(name_label)
                        .when(show_size, |this| {
                            this.child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .truncate()
                                    .text_scale_xs()
                                    .text_color(rgba(0xFFFFFFAA))
                                    .child(SharedString::from(size)),
                            )
                        })
                        .into_any_element()
                };
                rect = rect.child(inner);
            }
            // Single click selects (Cmd-click toggles, file-list
            // parity); double click on a container zooms in. Right
            // click opens the context menu below (zoom-out moved to
            // the background menu / the menu itself).
            rect = rect.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.is_right_click() {
                    return; // handled by the context menu
                }
                // Clicking the treemap claims keyboard focus so the
                // DiskUsage key context (Enter/Backspace/Escape/...)
                // applies immediately.
                this.focus_handle.focus(window, cx);
                if event.click_count() >= 2 && has_children {
                    this.select_only(node_id, cx);
                    this.zoom_into(node_id, cx);
                } else if event.modifiers().platform {
                    this.toggle_select(node_id, cx);
                } else {
                    this.select_only(node_id, cx);
                }
            }));
            // Route the container's single context menu to this rect:
            // record the target and apply the Finder rule (a member
            // keeps the whole selection, an outsider retargets to just
            // itself) BEFORE the deferred menu build reads the state.
            let is_container_node = has_children
                || self
                    .tree
                    .nodes
                    .get(&node_id)
                    .map(|n| matches!(n.kind, ferail_disk_usage::NodeKind::Container))
                    .unwrap_or(false);
            rect = rect.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    this.ensure_selected_for_menu(node_id, cx);
                    this.menu_rect_target = Some((node_id, is_container_node));
                }),
            );
            container = container.child(rect);
        }
        container
    }

    /// Zoom into `target` (clicked container rect). Pushes onto the
    /// zoom path and rebuilds the cached layout against the current
    /// treemap size.
    pub fn zoom_into(&mut self, target: NodeId, cx: &mut Context<Self>) {
        // Ignore the root — already focused.
        if target == self.focus_id() {
            return;
        }
        self.zoom_path.push(target);
        self.invalidate_layout();
        self.rebuild_layout_if_ready();
        if self.projection_filter_active() {
            self.schedule_filter_projection(cx);
        }
        cx.notify();
    }

    /// Pop one level of zoom (Cmd+Up or backspace-like) and rebuild
    /// the cached layout against the current treemap size.
    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        if self.zoom_path.pop().is_some() {
            self.invalidate_layout();
            self.rebuild_layout_if_ready();
            if self.projection_filter_active() {
                self.schedule_filter_projection(cx);
            }
            cx.notify();
        }
    }
}

// ---- Context-menu operations ----------------------------------------
//
// The same core verbs the list/grid rows offer, implemented directly on
// the view (a Disk Usage window may float independently of any shell
// window, so nothing here routes through Shell state except Get Info's
// window opener and the post-trash tab reload, which use the weak shell
// handle when present). All filesystem work runs on the background
// executor (Prime Directive); path resolution happens in the handlers,
// never at paint time.
impl DiskUsageView {
    fn on_du_open(&mut self, _: &DuOpen, _: &mut Window, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        cx.background_spawn(async move {
            for (path, _, _) in &paths {
                if let Err(e) = crate::platform_shell::open_with_default(path) {
                    crate::log_warn!(90, "du open {}: {e}", path.display());
                }
            }
        })
        .detach();
    }

    fn on_du_open_in_new_tab(
        &mut self,
        _: &DuOpenInNewTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shell) = self.shell.as_ref().and_then(gpui::WeakEntity::upgrade) else {
            return;
        };
        let mut selected = self.selected_paths().into_iter();
        let Some((path, is_dir, _)) = selected.next() else {
            return;
        };
        // The menu permits exactly one item. Keep that invariant in the
        // handler too so a synthetic action cannot fan out millions of tabs.
        if selected.next().is_some() {
            return;
        }
        shell.update(cx, |shell, cx| {
            if is_dir {
                shell.open_path_in_new_tab(path, window, cx);
            } else if let Some(parent) = path.parent().map(PathBuf::from) {
                let names = path
                    .file_name()
                    .map(|name| vec![name.to_string_lossy().into_owned()])
                    .unwrap_or_default();
                shell.reveal_in_new_tab(parent, names, window, cx);
            }
        });
    }

    fn on_du_reveal(&mut self, _: &DuReveal, _: &mut Window, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        cx.background_spawn(async move {
            for (path, _, _) in &paths {
                crate::platform_shell::reveal_in_finder(path);
            }
        })
        .detach();
    }

    fn on_du_get_info(&mut self, _: &DuGetInfo, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::notification::Notification;
        let Some(shell) = self.shell.clone() else {
            return;
        };
        // One window per item, capped so a huge selection can't carpet
        // the screen (the shell's own Get Info uses a fanout confirm;
        // a hard cap keeps this dependency-free here).
        const GET_INFO_CAP: usize = 8;
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        if paths.len() > GET_INFO_CAP {
            window.push_notification(
                Notification::info(tr!(
                    "Showing info for the first {cap} of {total} items.",
                    cap = GET_INFO_CAP,
                    total = format_count(paths.len() as u64)
                )),
                cx,
            );
        }
        for (path, is_dir, _) in paths.into_iter().take(GET_INFO_CAP) {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let target = if is_dir {
                ferail_core::entry_info::InfoTarget::Folder
            } else {
                ferail_core::entry_info::InfoTarget::File
            };
            crate::entry_info::open(path, name, target, None, shell.clone(), cx);
        }
    }

    fn on_du_copy_files(&mut self, _: &DuCopyFiles, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::notification::Notification;
        let items = self.selected_paths();
        if items.is_empty() {
            return;
        }
        let refs: Vec<(&std::path::Path, bool)> =
            items.iter().map(|(p, d, _)| (p.as_path(), *d)).collect();
        if !crate::platform_shell::clipboard_copy_file_urls(&refs) {
            window.push_notification(
                Notification::error(tr!("File clipboard isn't available on this platform yet.")),
                cx,
            );
            return;
        }
        let msg = if items.len() == 1 {
            tr!(
                "Copied \u{201C}{name}\u{201D}",
                name = items[0].0.file_name().unwrap_or_default().to_string_lossy()
            )
        } else {
            trn!("Copied {n} item", "Copied {n} items", items.len())
        };
        window.push_notification(Notification::success(msg), cx);
    }

    fn on_du_copy_paths(&mut self, _: &DuCopyPaths, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::notification::Notification;
        let items = self.selected_paths();
        if items.is_empty() {
            return;
        }
        let text = items
            .iter()
            .map(|(p, _, _)| ferail_fs_native::paths::display_path(p))
            .collect::<Vec<_>>()
            .join("\n");
        crate::platform_shell::copy_to_clipboard(&text);
        let msg = trn!("Copied path", "Copied {n} paths", items.len());
        window.push_notification(Notification::success(msg), cx);
    }

    fn on_du_trash(&mut self, _: &DuTrash, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::notification::Notification;
        let items = self.selected_paths();
        if items.is_empty() {
            return;
        }
        let win = window.window_handle();
        let shell = self.shell.clone();
        cx.spawn(async move |this, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    let mut ok = 0usize;
                    let mut failed: Vec<String> = Vec::new();
                    let mut parents: Vec<PathBuf> = Vec::new();
                    for (path, _, _) in &items {
                        match ferail_fs_native::move_to_trash(path) {
                            Ok(_) => {
                                ok += 1;
                                if let Some(parent) = path.parent() {
                                    let parent = parent.to_path_buf();
                                    if !parents.contains(&parent) {
                                        parents.push(parent);
                                    }
                                }
                            }
                            Err(e) => failed.push(format!("{}: {e}", path.display())),
                        }
                    }
                    (ok, failed, parents)
                })
                .await;
            let (ok, failed, parents) = results;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    // The scan tree still counts the trashed bytes —
                    // rescan for an honest picture.
                    if ok > 0 {
                        this.restart_scan(cx);
                    }
                });
            }
            if let (Some(shell), false) = (shell, parents.is_empty()) {
                if let Some(shell) = shell.upgrade() {
                    shell.update(cx, |shell, cx| {
                        shell.reload_tabs_matching_paths(&parents, cx);
                    });
                }
            }
            let _ = win.update(cx, |_, window, cx| {
                if failed.is_empty() {
                    let msg = trn!("Moved {n} item to Trash", "Moved {n} items to Trash", ok);
                    window.push_notification(Notification::success(msg), cx);
                } else {
                    window.push_notification(
                        crate::shell::error_notification(
                            tr!(
                                "Trashed {ok}, {failed} failed \u{2014} {detail}",
                                ok = format_count(ok as u64),
                                failed = format_count(failed.len() as u64),
                                detail = failed.first().cloned().unwrap_or_default()
                            )
                            .to_string(),
                        ),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    fn on_du_zoom_in(&mut self, _: &DuZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.single_selected_container() {
            self.zoom_into(target, cx);
        }
    }

    fn on_du_zoom_out(&mut self, _: &DuZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_out(cx);
    }

    fn on_du_clear_selection(
        &mut self,
        _: &DuClearSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.selected.is_empty() || self.lead.is_some() {
            self.selected.clear();
            self.lead = None;
            cx.notify();
            return;
        }

        match self.host {
            ToolHostContext::Docked => {
                if let Some(shell) = self.shell.clone() {
                    let _ = shell.update(cx, |shell, cx| shell.close_active_tool_result(cx));
                }
            }
            ToolHostContext::Windowed => window.remove_window(),
        }
    }

    /// The single selected container, when the selection is exactly one
    /// folder — the target rule for Zoom In and the subtree HTML export.
    fn single_selected_container(&self) -> Option<NodeId> {
        if self.selected.len() != 1 {
            return None;
        }
        let id = self.selected.iter().next().copied()?;
        let node = self.tree.nodes.get(&id)?;
        matches!(node.kind, ferail_disk_usage::NodeKind::Container).then_some(id)
    }

    /// Export dimensions: what the user is looking at, with a floor so
    /// a squeezed docked pane still exports a readable picture.
    fn export_dims(&self) -> (f32, f32) {
        let (w, h) = self.treemap_size.unwrap_or((1200.0, 800.0));
        (w.max(800.0), h.max(560.0))
    }

    /// Build the HTML for `root` at the current size mode. Runs inline
    /// on a semantic user action — same order of work as one streaming
    /// layout rebuild tick.
    fn export_html(&self, root: NodeId, document: bool) -> String {
        let (w, h) = self.export_dims();
        if document {
            ferail_disk_usage::treemap_html_document(
                &self.tree,
                root,
                self.size_mode,
                w,
                h,
                DU_LAYOUT_DEPTH,
            )
        } else {
            ferail_disk_usage::treemap_html_fragment(
                &self.tree,
                root,
                self.size_mode,
                w,
                h,
                DU_LAYOUT_DEPTH,
            )
        }
    }

    /// Subtree export target: the single selected folder, else the
    /// current zoom focus.
    fn html_target(&self) -> NodeId {
        self.single_selected_container().unwrap_or(self.focus_id())
    }

    fn on_du_copy_html(&mut self, _: &DuCopyHtml, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.html_target();
        self.copy_html_for(target, window, cx);
    }

    fn on_du_copy_view_html(
        &mut self,
        _: &DuCopyViewHtml,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.focus_id();
        self.copy_html_for(target, window, cx);
    }

    fn copy_html_for(&mut self, target: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::notification::Notification;
        // Fragment on the clipboard: pasteable straight into an
        // existing document/page (a full <!DOCTYPE> is for files).
        let html = self.export_html(target, false);
        crate::platform_shell::copy_to_clipboard(&html);
        window.push_notification(
            Notification::success(tr!(
                "Treemap HTML copied \u{2014} paste into any page or document."
            )),
            cx,
        );
    }

    fn on_du_save_html(&mut self, _: &DuSaveHtml, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.html_target();
        self.save_html_for(target, window, cx);
    }

    fn on_du_save_view_html(
        &mut self,
        _: &DuSaveViewHtml,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.focus_id();
        self.save_html_for(target, window, cx);
    }

    fn save_html_for(&mut self, target: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::notification::Notification;
        let html = self.export_html(target, true);
        let name = self
            .tree
            .nodes
            .get(&target)
            .map(|n| n.display_name.clone())
            .unwrap_or_else(|| "treemap".to_owned());
        // Sanitized leaf for the file name (path separators and other
        // hostile characters out).
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let win = window.window_handle();
        cx.spawn(async move |_this, cx| {
            let (path, result) = cx
                .background_executor()
                .spawn(async move {
                    // Downloads when it exists (the natural "hand me
                    // the file" place), else the temp dir.
                    let downloads = ferail_fs_native::home_dir().join("Downloads");
                    let dir = if downloads.is_dir() {
                        downloads
                    } else {
                        std::env::temp_dir()
                    };
                    let path = dir.join(format!("ferail-treemap-{safe}-{stamp}.html"));
                    let result = std::fs::write(&path, html.as_bytes());
                    (path, result)
                })
                .await;
            let ok = result.is_ok();
            let _ = win.update(cx, |_, window, cx| match &result {
                Ok(()) => {
                    window.push_notification(
                        Notification::success(tr!(
                            "Saved {name}",
                            name = path.file_name().unwrap_or_default().to_string_lossy()
                        )),
                        cx,
                    );
                }
                Err(e) => {
                    window.push_notification(
                        crate::shell::error_notification(
                            tr!("Save failed: {detail}", detail = e).to_string(),
                        ),
                        cx,
                    );
                }
            });
            if ok {
                // Show the file so it's one drag away from the document.
                cx.background_executor()
                    .spawn(async move {
                        crate::platform_shell::reveal_in_finder(&path);
                    })
                    .detach();
            }
        })
        .detach();
    }
}

impl Focusable for DiskUsageView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Drop for DiskUsageView {
    /// Closing the Disk Usage window must stop the still-running
    /// scanner — without this, the worker keeps walking the volume
    /// in the background long after the user has dismissed the
    /// window. The scanner checks `cancel` at every dirent boundary
    /// and exits cleanly once it sees the flag flip; the drain task
    /// is already gone by the time we get here (it broke out of its
    /// loop when `this.update` started returning `Err` on the dead
    /// entity), so the worker's final messages land in an orphan
    /// queue that's dropped with the rest of `self`.
    ///
    /// Also ends the registry task entry so the parent Shell's
    /// status-bar progress strip doesn't show a phantom in-flight
    /// scan. The owner-notify callback isn't reachable from `drop`
    /// (no `&mut App`), but next paint of the Shell picks up the
    /// missing task naturally.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(cancel) = self.filter_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.filter_snapshot_gate.store(0, Ordering::Release);
        if let Some(id) = self.task_id.take() {
            if let Ok(mut reg) = self.tasks.try_borrow_mut() {
                reg.end(id);
            }
        }
    }
}

impl Render for DiskUsageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.host == ToolHostContext::Windowed {
            window.set_window_title(&tr!(
                "Disk Usage — {path}",
                path = crate::private_mode::present_path(&self.root_path)
            ));
            // The filter field needs a `Window`, which the constructor
            // doesn't get; build it on first paint of a windowed view.
            // Docked, the shell's toolbar filter drives `apply_filter`
            // and this stays `None` so there is only ever one field.
            if self.filter_input.is_none() {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("Filter…")));
                if !self.text_filter.is_empty() {
                    let value = self.text_filter.clone();
                    input.update(cx, |state, cx| state.set_value(value, window, cx));
                }
                cx.subscribe(&input, |this: &mut Self, input, ev, cx| {
                    if matches!(ev, InputEvent::Change) {
                        let value = input.read(cx).value().to_string();
                        this.apply_filter(&value, cx);
                    }
                })
                .detach();
                self.filter_input = Some(input);
            }
        }
        let topn_visible = self.topn_visible;
        let viewport = window.viewport_size();
        let (host_w, host_h) = self
            .host_size
            .unwrap_or((viewport.width.as_f32(), viewport.height.as_f32()));
        let side_width = if topn_visible { TOPN_PANEL_WIDTH } else { 0.0 };
        // One row at normal widths, with room for its deliberate wrap on a
        // compact window. This is only the treemap's layout estimate.
        let header_height = if host_w < 900.0 { 78.0 } else { 52.0 };
        let footer_height = 34.0;
        let treemap_width = (host_w - side_width - 32.0).max(260.0);
        let treemap_height = (host_h - header_height - footer_height).max(220.0);
        let header = self.header(cx);
        let treemap = self.treemap(treemap_width, treemap_height, cx);
        let topn = if topn_visible {
            Some(self.top_panel(cx))
        } else {
            None
        };
        let legend = self.legend(cx);
        let view = cx.entity().clone();
        let content = v_flex()
            .track_focus(&self.focus_handle)
            .key_context(DISK_USAGE_CONTEXT)
            // Context-menu verbs (rect + background menus dispatch
            // these through the view's focus context).
            .on_action(cx.listener(Self::on_du_clear_selection))
            .on_action(cx.listener(Self::on_du_open))
            .on_action(cx.listener(Self::on_du_open_in_new_tab))
            .on_action(cx.listener(Self::on_du_reveal))
            .on_action(cx.listener(Self::on_du_get_info))
            .on_action(cx.listener(Self::on_du_copy_files))
            .on_action(cx.listener(Self::on_du_copy_paths))
            .on_action(cx.listener(Self::on_du_trash))
            .on_action(cx.listener(Self::on_du_zoom_in))
            .on_action(cx.listener(Self::on_du_zoom_out))
            .on_action(cx.listener(Self::on_du_copy_html))
            .on_action(cx.listener(Self::on_du_save_html))
            .on_action(cx.listener(Self::on_du_copy_view_html))
            .on_action(cx.listener(Self::on_du_save_view_html))
            .size_full()
            .bg(cx.theme().background)
            .on_prepaint(move |bounds, _, cx| {
                view.update(cx, |this, cx| {
                    this.update_host_size(
                        f32::from(bounds.size.width),
                        f32::from(bounds.size.height),
                        cx,
                    );
                });
            })
            .child(header)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(div().flex_1().min_w_0().p_2().child(treemap))
                    .when_some(topn, |this, panel| this.child(panel)),
            )
            .child(legend)
            .into_any_element();
        crate::private_mode::protect(content, cx)
    }
}

enum ScanBatch {
    Portable(Vec<DiskUsageFact>),
    #[cfg(target_os = "windows")]
    Fast(FastBatch),
}

enum ScanMsg {
    Batch(ScanBatch),
    Progress(DiskUsageStats),
    #[cfg(target_os = "windows")]
    FastProgress(ferail_ntfs::Progress),
    #[cfg(target_os = "windows")]
    ResetForFallback(FastFallbackReason),
    #[cfg(target_os = "windows")]
    FastComplete {
        best_effort_live: bool,
        elapsed: Duration,
    },
    Done(Option<EnumerationError>),
}

struct DuScanRequest {
    root: PathBuf,
    cancel: Arc<AtomicBool>,
    descend_packages: bool,
    #[cfg(target_os = "windows")]
    size_mode: SizeMode,
    id_base: u64,
    #[cfg(target_os = "windows")]
    request_id: u64,
}

fn run_scan_worker(
    engine: DuEngine,
    fs: Arc<NativeFs>,
    request: DuScanRequest,
    queue: Arc<Mutex<VecDeque<ScanMsg>>>,
) {
    #[cfg(not(target_os = "windows"))]
    let _ = engine;
    #[cfg(target_os = "windows")]
    if engine == DuEngine::FastNtfs {
        run_fast_scan_worker(&fs, &request, &queue);
        return;
    }
    run_portable_scan(
        &fs,
        &request.root,
        &request.cancel,
        request.descend_packages,
        request.id_base,
        &queue,
    );
}

#[cfg(target_os = "windows")]
fn run_fast_scan_worker(
    fs: &NativeFs,
    request: &DuScanRequest,
    queue: &Arc<Mutex<VecDeque<ScanMsg>>>,
) {
    let fast_request = ferail_ntfs_win32::FastNtfsRequest {
        root: request.root.clone(),
        sizing_mode: match request.size_mode {
            SizeMode::Apparent => ferail_ntfs::SizingMode::Apparent,
            SizeMode::Allocated => ferail_ntfs::SizingMode::Allocated,
        },
        descend_packages: request.descend_packages,
        root_id: request.id_base + 1,
        first_child_id: request.id_base + 2,
        request_id: request.request_id,
    };
    let mut stats = DiskUsageStats {
        dirs_scanned: 1,
        ..DiskUsageStats::default()
    };
    let mut best_effort_live = false;
    // Ready is emitted only after ShellExecute/UAC, pipe authentication and
    // opening the raw volume have completed. Starting here deliberately keeps
    // the user's credential-entry time out of the reported scan duration.
    let mut fast_started = None;
    let mut fast_elapsed = None;
    install_helper_attestation();
    let result =
        ferail_ntfs_win32::run_fast_ntfs(fast_request, &request.cancel, |event| match event {
            ferail_ntfs_win32::FastNtfsEvent::Ready => {
                fast_started = Some(Instant::now());
                let _ = push_scan_msg(
                    queue,
                    &request.cancel,
                    ScanMsg::Batch(ScanBatch::Fast(fast_root_batch(
                        &request.root,
                        request.id_base,
                    ))),
                );
            }
            ferail_ntfs_win32::FastNtfsEvent::Batch(rows) => {
                let batch = fast_rows_to_batch(rows, &mut stats);
                let _ = push_scan_msg(
                    queue,
                    &request.cancel,
                    ScanMsg::Batch(ScanBatch::Fast(batch)),
                );
            }
            ferail_ntfs_win32::FastNtfsEvent::Progress(progress) => {
                push_fast_progress(queue, progress);
            }
            ferail_ntfs_win32::FastNtfsEvent::Complete(complete) => {
                best_effort_live = complete.best_effort_live;
                fast_elapsed = fast_started.map(|started| started.elapsed());
            }
        });
    match result {
        Ok(()) => {
            // A successful protocol always contains Ready then Complete. Keep
            // the UI robust if a future helper violates that contract.
            let elapsed = fast_elapsed.unwrap_or_default();
            let _ = push_scan_msg(
                queue,
                &request.cancel,
                ScanMsg::FastComplete {
                    best_effort_live,
                    elapsed,
                },
            );
            let _ = push_scan_msg(queue, &request.cancel, ScanMsg::Done(None));
        }
        Err(error) if !request.cancel.load(Ordering::Acquire) => {
            let reason = fast_fallback_reason(&error);
            if push_scan_msg(queue, &request.cancel, ScanMsg::ResetForFallback(reason)) {
                run_portable_scan(
                    fs,
                    &request.root,
                    &request.cancel,
                    request.descend_packages,
                    request.id_base,
                    queue,
                );
            }
        }
        Err(_) => {}
    }
}

fn push_scan_msg(
    queue: &Arc<Mutex<VecDeque<ScanMsg>>>,
    cancel: &AtomicBool,
    message: ScanMsg,
) -> bool {
    let mut pending = Some(message);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        match queue.lock() {
            Ok(mut queue) if queue.len() < DU_QUEUE_CAP => {
                queue.push_back(pending.take().expect("scan message is queued once"));
                return true;
            }
            Ok(_) => std::thread::sleep(DU_BACKPRESSURE_NAP),
            Err(_) => return false,
        }
    }
}

fn push_scan_progress(queue: &Arc<Mutex<VecDeque<ScanMsg>>>, progress: DiskUsageStats) {
    let Ok(mut queue) = queue.lock() else { return };
    if let Some(ScanMsg::Progress(last)) = queue.back_mut() {
        *last = progress;
    } else if queue.len() < DU_QUEUE_CAP {
        queue.push_back(ScanMsg::Progress(progress));
    }
}

#[cfg(target_os = "windows")]
fn push_fast_progress(queue: &Arc<Mutex<VecDeque<ScanMsg>>>, progress: ferail_ntfs::Progress) {
    let Ok(mut queue) = queue.lock() else { return };
    if let Some(ScanMsg::FastProgress(last)) = queue.back_mut() {
        *last = progress;
    } else if queue.len() < DU_QUEUE_CAP {
        queue.push_back(ScanMsg::FastProgress(progress));
    }
}

fn run_portable_scan(
    fs: &NativeFs,
    root: &std::path::Path,
    cancel: &AtomicBool,
    descend_packages: bool,
    id_base: u64,
    queue: &Arc<Mutex<VecDeque<ScanMsg>>>,
) {
    let err = fs.scan_disk_usage_local(
        root,
        ferail_fs_native::DEFAULT_DU_BATCH,
        cancel,
        descend_packages,
        id_base,
        |batch| {
            let _ = push_scan_msg(queue, cancel, ScanMsg::Batch(ScanBatch::Portable(batch)));
        },
        |progress| push_scan_progress(queue, progress),
    );
    let _ = push_scan_msg(queue, cancel, ScanMsg::Done(err));
}

/// The Fast NTFS helper identity baked in by `build.rs`, as
/// `Some((salt_hex, digest_hex))` in a packaged release and `None` in a
/// development tree. Wrapped in a module because `include!` expands to items
/// and so cannot appear inside a function body.
// Read only by the Windows launch path and by the well-formedness test, so a
// non-Windows release build legitimately has no consumer.
#[allow(dead_code)]
mod helper_attestation {
    include!(concat!(env!("OUT_DIR"), "/helper_attestation.rs"));
}

/// Hand `ferail-ntfs-win32` the helper identity this build was packaged with,
/// so it can verify the binary before elevating it. The values come from
/// `build.rs`; an unpackaged development tree has none, and the launch path
/// then runs unattested rather than refusing to work.
///
/// Called immediately before each scan rather than at boot: the underlying
/// `set` is idempotent, and this way the check cannot be defeated by a change
/// to startup ordering.
#[cfg(target_os = "windows")]
fn install_helper_attestation() {
    let Some((salt, digest)) = helper_attestation::HELPER_ATTESTATION else {
        return;
    };
    // build.rs already validated the shape, so a parse failure here means the
    // generated constant was corrupted. Install an attestation that cannot
    // match rather than skipping installation: skipping would downgrade to
    // "unattested" and launch the helper anyway, which is precisely backwards.
    let attestation = match (
        ferail_ntfs_win32::parse_hex32(salt),
        ferail_ntfs_win32::parse_hex32(digest),
    ) {
        (Some(salt), Some(digest)) => ferail_ntfs_win32::HelperAttestation::new(salt, digest),
        _ => ferail_ntfs_win32::HelperAttestation::UNMATCHABLE,
    };
    ferail_ntfs_win32::set_helper_attestation(attestation);
}

#[cfg(target_os = "windows")]
fn fast_fallback_reason(error: &ferail_ntfs_win32::ClientError) -> FastFallbackReason {
    match error {
        ferail_ntfs_win32::ClientError::UacCancelled => FastFallbackReason::ElevationDeclined,
        ferail_ntfs_win32::ClientError::HelperMissing => FastFallbackReason::HelperMissing,
        ferail_ntfs_win32::ClientError::HelperUntrusted
        | ferail_ntfs_win32::ClientError::HelperUnreadable => FastFallbackReason::HelperUntrusted,
        ferail_ntfs_win32::ClientError::Helper(ferail_ntfs::FailureCode::Unsupported)
        | ferail_ntfs_win32::ClientError::Platform("probe") => FastFallbackReason::Unsupported,
        _ => FastFallbackReason::Failed,
    }
}

#[cfg(target_os = "windows")]
fn fast_root_batch(root: &std::path::Path, id_base: u64) -> FastBatch {
    let root_id = NodeId::from_raw(id_base + 1).expect("disk-usage root id is nonzero");
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| ferail_fs_native::paths::display_path(root));
    FastBatch {
        facts: vec![
            DiskUsageFact::NodeDiscovered {
                node: root_id,
                kind: ferail_disk_usage::NodeKind::Container,
                file_category: FileCategory::Other,
                mtime: None,
                name,
                is_cloud: false,
            },
            DiskUsageFact::ContainerScanStarted { container: root_id },
        ],
        raw_names: Vec::new(),
        stats: DiskUsageStats {
            dirs_scanned: 1,
            ..DiskUsageStats::default()
        },
    }
}

#[cfg(target_os = "windows")]
fn fast_rows_to_batch(rows: Vec<ferail_ntfs::NeutralRow>, stats: &mut DiskUsageStats) -> FastBatch {
    let mut facts = Vec::with_capacity(rows.len().saturating_mul(5));
    let mut raw_names = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(node) = NodeId::from_raw(row.id) else {
            continue;
        };
        let Some(parent) = NodeId::from_raw(row.parent_id) else {
            continue;
        };
        let is_directory = matches!(
            row.kind,
            ferail_ntfs::NeutralNodeKind::Directory
                | ferail_ntfs::NeutralNodeKind::ReparseDirectory
                | ferail_ntfs::NeutralNodeKind::OpaquePackage
        );
        let kind = if row.kind == ferail_ntfs::NeutralNodeKind::Directory {
            ferail_disk_usage::NodeKind::Container
        } else {
            ferail_disk_usage::NodeKind::File
        };
        let category = row
            .display_name
            .rsplit_once('.')
            .map_or(FileCategory::Other, |(_, ext)| classify_extension(ext));
        facts.push(DiskUsageFact::NodeDiscovered {
            node,
            kind,
            file_category: category,
            mtime: ntfs_ticks_to_system_time(row.modified_ticks),
            name: row.display_name,
            is_cloud: false,
        });
        facts.push(DiskUsageFact::NodeLinked {
            container: parent,
            node,
        });
        if kind == ferail_disk_usage::NodeKind::Container {
            facts.push(DiskUsageFact::ContainerScanStarted { container: node });
        }
        if row.logical_bytes != 0 {
            facts.push(DiskUsageFact::NodeSizeAdded {
                node,
                size_bytes: row.logical_bytes,
            });
        }
        if row.allocated_bytes != 0 {
            facts.push(DiskUsageFact::NodeAllocatedAdded {
                node,
                bytes: row.allocated_bytes,
            });
        }
        raw_names.push((node, row.raw_name));
        if is_directory {
            stats.dirs_scanned = stats.dirs_scanned.saturating_add(1);
        } else {
            stats.files_scanned = stats.files_scanned.saturating_add(1);
        }
        stats.bytes_scanned = stats.bytes_scanned.saturating_add(row.logical_bytes);
    }
    FastBatch {
        facts,
        raw_names,
        stats: *stats,
    }
}

#[cfg(target_os = "windows")]
fn ntfs_ticks_to_system_time(ticks: u64) -> Option<SystemTime> {
    const TICKS_PER_SECOND: u64 = 10_000_000;
    const WINDOWS_TO_UNIX_SECONDS: u64 = 11_644_473_600;
    let seconds = ticks / TICKS_PER_SECOND;
    let nanos = (ticks % TICKS_PER_SECOND).checked_mul(100)?;
    if seconds >= WINDOWS_TO_UNIX_SECONDS {
        SystemTime::UNIX_EPOCH.checked_add(Duration::new(
            seconds - WINDOWS_TO_UNIX_SECONDS,
            nanos as u32,
        ))
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(Duration::new(
            WINDOWS_TO_UNIX_SECONDS - seconds,
            nanos as u32,
        ))
    }
}

/// Category fill, from the canonical palette in `ferail-disk-usage`
/// (`category_color_rgba`) — one source shared with the HTML export so
/// the window and an exported page can't drift apart.
fn category_color(cat: FileCategory) -> Rgba {
    let (r, g, b, a) = ferail_disk_usage::category_color_rgba(cat);
    Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    }
}

fn humanize_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut s = b as f64;
    let mut u = 0;
    while s >= 1024.0 && u + 1 < UNITS.len() {
        s /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} {}", b, UNITS[u])
    } else {
        format!("{:.1} {}", s, UNITS[u])
    }
}

#[cfg(any(target_os = "windows", test))]
fn humanize_duration(duration: Duration) -> String {
    if duration < Duration::from_secs(1) {
        return format!("{} ms", duration.as_millis());
    }
    if duration < Duration::from_secs(60) {
        return format!("{:.1} s", duration.as_secs_f64());
    }
    let minutes = duration.as_secs() / 60;
    let seconds = duration.as_secs() % 60;
    format!("{minutes} min {seconds} s")
}

fn size_for_mode(apparent: u64, allocated: u64, mode: SizeMode) -> u64 {
    match mode {
        SizeMode::Apparent => apparent,
        SizeMode::Allocated => {
            if allocated == 0 {
                apparent
            } else {
                allocated
            }
        }
    }
}

/// Legend / filter label for a category, as a msgid — translate at the
/// display site with `crate::i18n::tr_static`.
fn category_label(cat: FileCategory) -> &'static str {
    use ferail_core::msgid;
    match cat {
        FileCategory::Image => msgid!("Image"),
        FileCategory::Video => msgid!("Video"),
        FileCategory::Audio => msgid!("Audio"),
        FileCategory::Archive => msgid!("Archive"),
        FileCategory::Document => msgid!("Document"),
        FileCategory::Executable => msgid!("Executable"),
        FileCategory::Other => msgid!("Other"),
    }
}

/// Open the Disk Usage window for `root`. Independent of the main
/// shell — closing one doesn't affect the other. The shared `tasks`
/// registry lets the DU scan register a task so the owner Shell's
/// status bar shows progress, and `notify_owner` is invoked after each
/// task mutation so the Shell's status bar repaints promptly.
pub fn open_window(
    root: PathBuf,
    fs: Arc<NativeFs>,
    tasks: Rc<RefCell<TaskRegistry>>,
    notify_owner: Option<NotifyOwner>,
    dock_owner: Option<DockOwner>,
    cx: &mut App,
) -> Result<WindowHandle<Root>, anyhow::Error> {
    let view = cx.new(|cx| {
        DiskUsageView::new(
            root.clone(),
            fs.clone(),
            tasks.clone(),
            notify_owner.clone(),
            dock_owner.clone(),
            cx,
        )
    });
    open_existing_window(root, view, dock_owner, cx)
}

/// Open a standalone Disk Usage window around an existing view entity.
/// Used for pop-out so the scan tree, progress, zoom, selection, and
/// queues survive the host move.
pub fn open_existing_window(
    root: PathBuf,
    view: Entity<DiskUsageView>,
    dock_owner: Option<DockOwner>,
    cx: &mut App,
) -> Result<WindowHandle<Root>, anyhow::Error> {
    view.update(cx, |view, cx| view.set_dock_owner(dock_owner, cx));
    let menu_label = tr!(
        "Disk Usage \u{2014} {path}",
        path = crate::private_mode::present_path(&root)
    );
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(960.0), px(720.0)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(menu_label.clone()),
            ..Default::default()
        }),
        ..crate::base_window_options()
    };
    let handle = cx.open_window(opts, |window, cx| cx.new(|cx| Root::new(view, window, cx)))?;
    crate::process_state::process_state(cx)
        .register_aux_window(handle.into(), menu_label.to_string());
    crate::boot::refresh_window_menu(cx);
    Ok(handle)
}

/// Format a path for inclusion in the task label: file name plus one
/// parent component, falling back to the full path when shorter than
/// 40 chars. Keeps the status-bar text from being dominated by long
/// absolute paths.
fn short_path(p: &std::path::Path) -> String {
    let full = p.to_string_lossy().to_string();
    if full.len() <= 40 {
        return full;
    }
    let mut comps: Vec<_> = p
        .components()
        .rev()
        .take(2)
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    comps.reverse();
    let tail = comps.join("/");
    if tail.is_empty() {
        full
    } else {
        format!("\u{2026}/{}", tail)
    }
}

#[cfg(test)]
mod path_arena_tests {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    use ferail_core::NodeId;
    use ferail_disk_usage::{
        DiskUsageFact, DiskUsageNode, DiskUsageTree, FileCategory, NodeKind, SizeMode,
    };

    use super::{
        DiskUsagePathArena, layout_rebuild_interval, node_matches_projection,
        node_matches_text_filter, topn_rebuild_interval,
    };

    const FILTER_CTX: ferail_core::filter_expr::DateCtx = ferail_core::filter_expr::DateCtx {
        now_unix: 1_800_000_000,
        tz_offset_secs: 0,
    };

    #[test]
    fn million_node_refreshes_are_human_scale_not_frame_scale() {
        assert_eq!(layout_rebuild_interval(1_000_000), Duration::from_secs(2));
        assert_eq!(topn_rebuild_interval(1_000_000), Duration::from_secs(3));
        assert!(layout_rebuild_interval(10_000) < Duration::from_secs(1));
    }

    #[test]
    fn scan_local_parent_index_reconstructs_paths_without_native_fs() {
        let base = 1_u64 << 62;
        let mut arena = DiskUsagePathArena::new(PathBuf::from("/scan-root"), base);
        let root = arena.root_id();
        let folder = NodeId::from_raw(base + 2).unwrap();
        let file = NodeId::from_raw(base + 3).unwrap();
        let facts = vec![
            DiskUsageFact::NodeDiscovered {
                node: root,
                kind: ferail_disk_usage::NodeKind::Container,
                file_category: FileCategory::Other,
                mtime: None,
                name: "scan-root".into(),
                is_cloud: false,
            },
            DiskUsageFact::NodeDiscovered {
                node: folder,
                kind: ferail_disk_usage::NodeKind::Container,
                file_category: FileCategory::Other,
                mtime: None,
                name: "nested".into(),
                is_cloud: false,
            },
            DiskUsageFact::NodeLinked {
                container: root,
                node: folder,
            },
            DiskUsageFact::NodeDiscovered {
                node: file,
                kind: ferail_disk_usage::NodeKind::File,
                file_category: FileCategory::Document,
                mtime: None,
                name: "report.txt".into(),
                is_cloud: false,
            },
            DiskUsageFact::NodeLinked {
                container: folder,
                node: file,
            },
        ];
        let mut tree = DiskUsageTree::new(root);
        arena.apply_facts(&facts);
        tree.apply_facts(&facts);

        assert_eq!(
            arena.path_for(file, &tree),
            Some(PathBuf::from("/scan-root/nested/report.txt"))
        );
        assert_eq!(arena.parents.len(), 3);
    }

    #[test]
    fn deep_selection_resolves_to_the_nearest_drawn_treemap_node() {
        let base = 1_u64 << 62;
        let mut arena = DiskUsagePathArena::new(PathBuf::from("/scan-root"), base);
        let root = arena.root_id();
        let folder = NodeId::from_raw(base + 2).unwrap();
        let file = NodeId::from_raw(base + 3).unwrap();
        arena.apply_facts(&[
            DiskUsageFact::NodeLinked {
                container: root,
                node: folder,
            },
            DiskUsageFact::NodeLinked {
                container: folder,
                node: file,
            },
        ]);

        let visible = HashSet::from([root, folder]);
        assert_eq!(arena.nearest_visible_ancestor(file, &visible), Some(folder));
        assert_eq!(
            arena.nearest_visible_ancestor(folder, &visible),
            Some(folder)
        );
    }

    #[test]
    fn du_filter_uses_the_shared_structured_expression_semantics() {
        let mut node = DiskUsageNode::new(NodeId::from_raw(9).unwrap());
        node.display_name = "Quarterly Report.PDF".into();
        node.kind = NodeKind::File;
        node.file_category = FileCategory::Document;
        node.size_bytes = 2 * 1024 * 1024;
        node.mtime = Some(UNIX_EPOCH + Duration::from_secs(1_799_999_000));

        let expr = ferail_core::filter_expr::FilterExpr::parse(
            "quarterly ext:pdf kind:file size:>1mb mod:week",
            FILTER_CTX,
        );
        assert!(node_matches_text_filter(&expr, &node, SizeMode::Apparent));

        let wrong_extension = ferail_core::filter_expr::FilterExpr::parse("ext:zip", FILTER_CTX);
        assert!(!node_matches_text_filter(
            &wrong_extension,
            &node,
            SizeMode::Apparent
        ));
        let unavailable_metadata =
            ferail_core::filter_expr::FilterExpr::parse("locked:yes", FILTER_CTX);
        assert!(!node_matches_text_filter(
            &unavailable_metadata,
            &node,
            SizeMode::Apparent
        ));
    }

    #[test]
    fn category_projection_does_not_turn_structural_folders_into_matches() {
        let mut folder = DiskUsageNode::new(NodeId::from_raw(10).unwrap());
        folder.display_name = "Documents".into();
        folder.kind = NodeKind::Container;
        folder.file_category = FileCategory::Other;
        let empty = ferail_core::filter_expr::FilterExpr::default();

        assert!(!node_matches_projection(
            &empty,
            Some(FileCategory::Other),
            &folder,
            SizeMode::Apparent,
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn fast_names_round_trip_as_opaque_utf16() {
        use std::os::windows::ffi::OsStrExt as _;

        let base = 1_u64 << 62;
        let mut arena = DiskUsagePathArena::new(PathBuf::from(r"C:\scan-root"), base);
        let root = arena.root_id();
        let file = NodeId::from_raw(base + 2).unwrap();
        let raw_name = vec![
            b'x' as u16,
            0xD800,
            b'.' as u16,
            b't' as u16,
            b'x' as u16,
            b't' as u16,
        ];
        let facts = vec![
            DiskUsageFact::NodeDiscovered {
                node: file,
                kind: ferail_disk_usage::NodeKind::File,
                file_category: FileCategory::Document,
                mtime: None,
                name: String::from_utf16_lossy(&raw_name),
                is_cloud: false,
            },
            DiskUsageFact::NodeLinked {
                container: root,
                node: file,
            },
        ];
        let mut tree = DiskUsageTree::new(root);
        arena.apply_facts(&facts);
        arena.set_raw_name(file, &raw_name);
        tree.apply_facts(&facts);

        let path = arena.path_for(file, &tree).unwrap();
        assert_eq!(
            path.file_name().unwrap().encode_wide().collect::<Vec<_>>(),
            raw_name
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn fast_row_conversion_preserves_sizes_kind_and_stats() {
        use ferail_ntfs::{FileReference, NeutralNodeKind, NeutralRow};

        let base = 1_u64 << 62;
        let mut stats = ferail_disk_usage::DiskUsageStats {
            dirs_scanned: 1,
            ..Default::default()
        };
        let raw_name: Vec<u16> = "photo.jpg".encode_utf16().collect();
        let batch = super::fast_rows_to_batch(
            vec![NeutralRow {
                id: base + 2,
                parent_id: base + 1,
                file_record: FileReference {
                    record: 42,
                    sequence: 3,
                },
                kind: NeutralNodeKind::File,
                raw_name: raw_name.clone(),
                display_name: "photo.jpg".into(),
                logical_bytes: 123,
                allocated_bytes: 4096,
                modified_ticks: 116_444_736_000_000_000,
            }],
            &mut stats,
        );
        assert_eq!(batch.raw_names[0].1, raw_name);
        assert_eq!(batch.stats.files_scanned, 1);
        assert_eq!(batch.stats.dirs_scanned, 1);
        assert_eq!(batch.stats.bytes_scanned, 123);

        let root = NodeId::from_raw(base + 1).unwrap();
        let file = NodeId::from_raw(base + 2).unwrap();
        let mut tree = DiskUsageTree::new(root);
        tree.apply_facts(&batch.facts);
        let node = tree.nodes.get(&file).unwrap();
        assert_eq!(node.file_category, FileCategory::Image);
        assert_eq!(node.size_bytes, 123);
        assert_eq!(node.allocated_bytes, 4096);
        assert_eq!(node.mtime, Some(std::time::UNIX_EPOCH));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn fast_progress_is_coalesced_at_the_worker_queue_tail() {
        use std::collections::VecDeque;
        use std::sync::{Arc, Mutex};

        use ferail_ntfs::{Progress, ScanPhase};

        let queue = Arc::new(Mutex::new(VecDeque::new()));
        super::push_fast_progress(
            &queue,
            Progress {
                phase: ScanPhase::ReadingRecords,
                completed: 10,
                total: 100,
                live_records: 8,
                corrupt_records: 0,
            },
        );
        super::push_fast_progress(
            &queue,
            Progress {
                phase: ScanPhase::BuildingIndex,
                completed: 100,
                total: 100,
                live_records: 80,
                corrupt_records: 1,
            },
        );

        let queue = queue.lock().unwrap();
        assert_eq!(queue.len(), 1);
        let Some(super::ScanMsg::FastProgress(progress)) = queue.front() else {
            panic!("expected one coalesced Fast progress message")
        };
        assert_eq!(progress.phase, ScanPhase::BuildingIndex);
        assert_eq!(progress.completed, 100);
        assert_eq!(progress.corrupt_records, 1);
    }

    #[test]
    fn scan_duration_format_is_compact() {
        assert_eq!(
            super::humanize_duration(std::time::Duration::from_millis(420)),
            "420 ms"
        );
        assert_eq!(
            super::humanize_duration(std::time::Duration::from_millis(5_240)),
            "5.2 s"
        );
        assert_eq!(
            super::humanize_duration(std::time::Duration::from_secs(125)),
            "2 min 5 s"
        );
    }

    /// The Fast NTFS helper attestation constant is generated by `build.rs`
    /// and consumed inside a `cfg(windows)` function, so nothing would catch a
    /// malformed one on a macOS or Linux build. Compile and inspect it here on
    /// every platform instead: a half-written or non-hex constant fails the
    /// test suite rather than a Windows release.
    #[test]
    fn generated_helper_attestation_is_well_formed() {
        // `None` is the correct, expected value in a development tree; only
        // the packaging script sets the environment that fills it in.
        if let Some((salt, digest)) = super::helper_attestation::HELPER_ATTESTATION {
            for (label, value) in [("salt", salt), ("digest", digest)] {
                assert_eq!(value.len(), 64, "{label} must be 64 hex characters");
                assert!(
                    value.bytes().all(|b| b.is_ascii_hexdigit()),
                    "{label} must be hex"
                );
            }
            assert_ne!(salt, digest, "salt and digest must not be the same value");
        }
    }
}
