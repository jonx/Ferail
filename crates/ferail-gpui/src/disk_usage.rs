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

use crate::text::TextScale as _;
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Key context for the Disk Usage pane — keymap.rs binds the treemap
/// keys (Enter/Backspace/Escape, Cmd+C/I/Backspace) against it.
pub const DISK_USAGE_CONTEXT: &str = "DiskUsage";

gpui::actions!(
    disk_usage,
    [
        DuClearSelection,
        DuOpen,
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

use ferail_core::{EnumerationError, NodeId};
use ferail_disk_usage::{
    DiskUsageFact, DiskUsageLayoutNode, DiskUsageStats, DiskUsageTree, FileCategory, SizeMode,
    TreemapRect, build_layout_node_with_mode, compute_treemap,
};
use ferail_fs_native::NativeFs;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, ElementExt, Root, Selectable, Sizable, WindowExt as _,
    button::{Button, ButtonGroup},
    h_flex, v_flex,
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

pub struct DiskUsageView {
    root_path: PathBuf,
    root_id: NodeId,
    fs: Arc<NativeFs>,

    tree: DiskUsageTree,
    stats: DiskUsageStats,
    scan_complete: bool,
    error: Option<EnumerationError>,
    cancel: Arc<AtomicBool>,

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

    /// True once we've adopted the scanner's canonical root NodeId
    /// from the first incoming fact. Phase 6 regression fix: the
    /// constructor keeps the UI snappy by skipping `canonicalize()`,
    /// but the scanner canonicalises internally, so its facts arrive
    /// under a different NodeId than the one we computed up front.
    /// Without this, focus_id() points at an orphan node and the
    /// treemap layout renders empty even though tree.nodes has data.
    root_resolved: bool,

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

const TOPN_CAP: usize = 50;
const TOPN_PANEL_WIDTH: f32 = 240.0;

impl DiskUsageView {
    pub fn new(
        root_path: PathBuf,
        fs: Arc<NativeFs>,
        tasks: Rc<RefCell<TaskRegistry>>,
        notify_owner: Option<NotifyOwner>,
        dock_owner: Option<DockOwner>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Keep construction UI-cheap. The background scanner performs
        // canonicalisation before walking; opening the window should
        // not wait on filesystem resolution.
        let canonical = root_path.clone();
        let root_id = fs.id_for_path(&canonical);
        let cancel = Arc::new(AtomicBool::new(false));
        let msg_queue = Arc::new(Mutex::new(VecDeque::new()));
        // Volume capacity for the header bar arrives off-thread below —
        // the NSURL/statfs lookup can round-trip to a network mount,
        // and this constructor runs on the UI thread.
        let volume = None;
        let mut view = Self {
            root_path: canonical.clone(),
            root_id,
            fs: fs.clone(),
            tree: DiskUsageTree::new(root_id),
            stats: DiskUsageStats::default(),
            scan_complete: false,
            error: None,
            cancel: cancel.clone(),
            msg_queue: msg_queue.clone(),
            layout_cache: None,
            rects_cache: Vec::new(),
            treemap_size: None,
            scan_generation: 0,
            zoom_path: Vec::new(),
            selected: HashSet::new(),
            lead: None,
            category_filter: None,
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
            root_resolved: false,
            focus_handle: cx.focus_handle(),
        };
        view.start_scan(fs, cx);
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
        #[derive(Clone, Copy)]
        struct TopFileCandidate {
            node_id: NodeId,
            category: FileCategory,
            size_bytes: u64,
        }

        let mut all: Vec<TopFileCandidate> = self
            .tree
            .nodes
            .iter()
            .filter_map(|(id, n)| {
                if n.kind != ferail_disk_usage::NodeKind::File {
                    return None;
                }
                if self
                    .category_filter
                    .is_some_and(|cat| cat != n.file_category)
                {
                    return None;
                }
                let size_bytes = size_for_mode(n.size_bytes, n.allocated_bytes, self.size_mode);
                if size_bytes == 0 {
                    return None;
                }
                Some(TopFileCandidate {
                    node_id: *id,
                    category: n.file_category,
                    size_bytes,
                })
            })
            .collect();
        if all.len() > TOPN_CAP {
            all.select_nth_unstable_by(TOPN_CAP - 1, |a, b| b.size_bytes.cmp(&a.size_bytes));
            all.truncate(TOPN_CAP);
        }
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
    }

    /// Spawn the disk-usage scan on the background executor + start
    /// the FG drain timer.
    fn start_scan(&mut self, fs: Arc<NativeFs>, cx: &mut Context<Self>) {
        self.scan_generation = self.scan_generation.wrapping_add(1);
        let generation = self.scan_generation;
        let root = self.root_path.clone();
        let cancel = self.cancel.clone();
        let cancel_for_push = self.cancel.clone();
        let descend = self.descend_packages;
        let queue_for_scan = self.msg_queue.clone();
        let queue_for_progress = self.msg_queue.clone();
        let queue_for_done = self.msg_queue.clone();

        // Register the scan with the shared task registry so the
        // owning Shell's status-bar progress strip shows indeterminate
        // motion for the duration. Cancellable: the status bar / task
        // panel will pick that up when we wire the cancel button.
        let task_label = tr!("Scanning {path}", path = short_path(&self.root_path)).to_string();
        let task_id = self.with_tasks(cx, |reg| reg.begin(TaskKind::DiskUsage, task_label, true));
        self.task_id = Some(task_id);

        // BG: run the scan. Synchronous I/O on the executor's pool.
        cx.background_executor()
            .spawn(async move {
                let err = fs.scan_disk_usage(
                    &root,
                    ferail_fs_native::DEFAULT_DU_BATCH,
                    &cancel,
                    descend,
                    |batch| {
                        // Backpressure: never let the queue outrun the FG
                        // drain. When it's full, park this (background pool)
                        // thread briefly and retry; bail on cancel so a
                        // cancelled scan / closed window can't hang here.
                        // The walker checks `cancel` itself right after the
                        // callback returns, so dropping the batch is safe.
                        let mut pending = Some(batch);
                        loop {
                            if cancel_for_push.load(Ordering::Relaxed) {
                                return;
                            }
                            match queue_for_scan.lock() {
                                Ok(mut q) => {
                                    if q.len() < DU_QUEUE_CAP {
                                        if let Some(batch) = pending.take() {
                                            q.push_back(ScanMsg::Batch(batch));
                                        }
                                        return;
                                    }
                                }
                                Err(_) => return,
                            }
                            std::thread::sleep(DU_BACKPRESSURE_NAP);
                        }
                    },
                    |progress| {
                        if let Ok(mut q) = queue_for_progress.lock() {
                            if let Some(ScanMsg::Progress(last)) = q.back_mut() {
                                *last = progress;
                            } else {
                                q.push_back(ScanMsg::Progress(progress));
                            }
                        }
                    },
                );
                if let Ok(mut q) = queue_for_done.lock() {
                    q.push_back(ScanMsg::Done(err));
                }
            })
            .detach();

        // FG: drain the queue periodically + apply on the view.
        // CRITICAL: expensive work (layout invalidation, top-N rebuild,
        // cx.notify) runs ONCE per drain tick — not once per message —
        // because at peak scan rate dozens of batches accumulate
        // between drains. Doing 50× sorts of a million-node tree in a
        // single main-thread update is what was freezing the UI.
        let queue_for_drain = self.msg_queue.clone();
        cx.spawn(async move |this, cx| {
            let mut last_topn_rebuild = Instant::now() - DU_TOPN_REBUILD_INTERVAL;
            let mut last_layout_rebuild = Instant::now() - DU_LAYOUT_REBUILD_INTERVAL;
            let mut interval = DU_DRAIN_INTERVAL_IDLE;
            loop {
                cx.background_executor().timer(interval).await;
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
                let mut stale = false;
                let update_result = this.update(cx, |v, cx| {
                    if v.scan_generation != generation {
                        stale = true;
                        return;
                    }
                    for msg in msgs {
                        match &msg {
                            ScanMsg::Batch(_) => had_batch = true,
                            ScanMsg::Done(_) => done = true,
                            _ => {}
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
                        let rebuild_layout =
                            done || last_layout_rebuild.elapsed() >= DU_LAYOUT_REBUILD_INTERVAL;
                        if rebuild_layout {
                            v.invalidate_layout();
                            v.rebuild_layout_if_ready();
                            last_layout_rebuild = Instant::now();
                        }
                        let rebuild_topn =
                            done || last_topn_rebuild.elapsed() >= DU_TOPN_REBUILD_INTERVAL;
                        if rebuild_topn {
                            v.rebuild_top_files();
                            last_topn_rebuild = Instant::now();
                        }
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
            ScanMsg::Batch(facts) => {
                // First scanner fact is a ContainerScanStarted for the
                // *canonical* root NodeId. Adopt it as our root_id so
                // focus_id() points at the node the rest of the
                // facts actually populate. Without this, the layout
                // is rooted at the as-passed (non-canonical) path's
                // id which has no children → empty treemap.
                if !self.root_resolved {
                    for fact in &facts {
                        if let DiskUsageFact::ContainerScanStarted { container } = fact {
                            self.root_id = *container;
                            self.root_resolved = true;
                            break;
                        }
                    }
                }
                self.tree.apply_facts(&facts);
            }
            ScanMsg::Progress(p) => self.stats = p,
            ScanMsg::Done(err) => {
                self.scan_complete = true;
                self.error = err;
            }
        }
    }

    fn invalidate_layout(&mut self) {
        self.layout_cache = None;
        self.rects_cache.clear();
    }

    fn rebuild_layout_if_ready(&mut self) {
        let Some((w, h)) = self.treemap_size else {
            return;
        };
        self.layout_cache = Some(build_layout_node_with_mode(
            &self.tree,
            self.focus_id(),
            DU_LAYOUT_DEPTH,
            self.size_mode,
        ));
        if let Some(layout) = &self.layout_cache {
            self.rects_cache = compute_treemap(layout, (0.0, 0.0, w, h), DU_LAYOUT_DEPTH);
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
        if let Some(id) = self.task_id.take() {
            self.with_tasks(cx, |reg| reg.end(id));
        }
        self.tree = DiskUsageTree::new(self.root_id);
        self.stats = DiskUsageStats::default();
        self.scan_complete = false;
        self.error = None;
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
            let Some(path) = self.fs.path_for(*id) else {
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
        self.rebuild_top_files();
        cx.notify();
    }

    fn toggle_size_mode(&mut self, mode: SizeMode, cx: &mut Context<Self>) {
        if self.size_mode == mode {
            return;
        }
        self.size_mode = mode;
        self.invalidate_layout();
        self.rebuild_layout_if_ready();
        self.rebuild_top_files();
        cx.notify();
    }

    fn toggle_packages(&mut self, cx: &mut Context<Self>) {
        if !self.scan_complete {
            return;
        }
        self.descend_packages = !self.descend_packages;
        self.restart_scan(cx);
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let title = if self.host == ToolHostContext::Docked {
            tr!("Disk Usage").to_string()
        } else {
            ferail_fs_native::paths::display_path(&self.root_path)
        };
        let scanned = humanize_bytes(self.stats.bytes_scanned);
        let files = self.stats.files_scanned;
        let folders = self.stats.dirs_scanned;
        let scanning = !self.scan_complete;
        // A failed scan must say so — it used to store the error and
        // render "0 files, 0 folders, 0 B", indistinguishable from an
        // empty folder (this hid "canonicalize unsupported" on AROS).
        let summary = if let Some(err) = &self.error {
            let why = match err {
                ferail_core::EnumerationError::PermissionDenied => {
                    tr!("permission denied").to_string()
                }
                ferail_core::EnumerationError::NotFound => tr!("folder not found").to_string(),
                ferail_core::EnumerationError::Other(msg) => msg.clone(),
            };
            tr!("Scan failed \u{2014} {detail}", detail = why).to_string()
        } else if self.scan_complete {
            tr!(
                "{files} files, {folders} folders, {scanned}",
                files = files,
                folders = folders,
                scanned = scanned
            )
            .to_string()
        } else {
            tr!(
                "Scanning\u{2026} {files} files, {scanned}",
                files = files,
                scanned = scanned
            )
            .to_string()
        };
        let summary_color = if self.error.is_some() {
            theme.danger
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
        let mut col = v_flex()
            .w_full()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_scale_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(SharedString::from(title)),
                    )
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
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_scale_xs()
                            .text_color(summary_color)
                            .child(SharedString::from(summary)),
                    )
                    .child(
                        ButtonGroup::new("du-size-mode")
                            .small()
                            .outline()
                            .compact()
                            .child(
                                Button::new("du-size-apparent")
                                    .label(tr!("Apparent"))
                                    .selected(self.size_mode == SizeMode::Apparent),
                            )
                            .child(
                                Button::new("du-size-allocated")
                                    .label(tr!("Allocated"))
                                    .selected(self.size_mode == SizeMode::Allocated),
                            )
                            .on_click(cx.listener(|this, clicks: &Vec<usize>, _, cx| {
                                match clicks.first().copied() {
                                    Some(0) => this.toggle_size_mode(SizeMode::Apparent, cx),
                                    Some(1) => this.toggle_size_mode(SizeMode::Allocated, cx),
                                    _ => {}
                                }
                            })),
                    )
                    // Package-descend toggle controls a macOS-specific
                    // concept (.app/.bundle/.framework directories) —
                    // Windows has no equivalent, so hide the button
                    // there to avoid offering a meaningless toggle.
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
                    }),
            );
        // Volume capacity bar — "X.X GB free of Y.Y GB" with the
        // used portion filled in muted_foreground.
        if let Some(v) = &self.volume {
            if let (Some(total), Some(avail)) = (v.total_bytes, v.available_bytes) {
                if total > 0 {
                    let used = total.saturating_sub(avail);
                    let frac = (used as f32 / total as f32).clamp(0.0, 1.0);
                    let bar_w = px(280.0);
                    let fill_w = bar_w * frac;
                    let track_bg = theme.muted_foreground.opacity(0.25);
                    let fill_bg = theme.muted_foreground.opacity(0.85);
                    let label = tr!(
                        "{free} free of {total} on {volume}",
                        free = humanize_bytes(avail),
                        total = humanize_bytes(total),
                        volume = v.name
                    );
                    col = col.child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .pt_1()
                            .child(
                                div()
                                    .w(bar_w)
                                    .h(px(4.0))
                                    .rounded(px(2.0))
                                    .bg(track_bg)
                                    .child(div().h_full().w(fill_w).rounded(px(2.0)).bg(fill_bg)),
                            )
                            .child(
                                div()
                                    .text_scale_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(label),
                            ),
                    );
                }
            }
        }
        col
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
                            .child(SharedString::from(e.name.clone())),
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
                            .child(SharedString::from(humanize_bytes(e.size_bytes))),
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
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_scale_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(tr!("Largest files")),
            )
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
                    format!("{}  {}", n.display_name, humanize_bytes(size))
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
                Some(tr!("{n} selected  {size}", n = n, size = humanize_bytes(total)).to_string())
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
                        let mut menu = menu.menu(tr!("Open"), Box::new(DuOpen)).menu(
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
                .map(|n| n.display_name.clone())
                .unwrap_or_default();
            let size = humanize_bytes(r.size_bytes);
            let show_label = r.width >= 60.0 && r.height >= 24.0;
            let show_size = r.width >= 80.0 && r.height >= 40.0;
            let selected = self.selected.contains(&node_id);
            let dimmed = self
                .category_filter
                .is_some_and(|category| category != r.file_category);
            let mut rect = div()
                .absolute()
                .top(px(r.y))
                .left(px(r.x))
                .w(px(r.width))
                .h(px(r.height))
                .bg(color)
                // Selected: a fat near-black border reads on every
                // category color (the old 1px theme-blue vanished on
                // saturated tiles).
                .map(|this| {
                    if selected {
                        this.border_2().border_color(rgba(0x000000E0))
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
                .hover(|this| this.border_color(cx.theme().selection));
            if show_label {
                let inner = div()
                    .size_full()
                    .px_1()
                    .py_1()
                    .child(
                        div()
                            .text_scale_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgba(0xFFFFFFEE))
                            .child(SharedString::from(name)),
                    )
                    .when(show_size, |this| {
                        this.child(
                            div()
                                .text_scale_xs()
                                .text_color(rgba(0xFFFFFFAA))
                                .child(SharedString::from(size)),
                        )
                    });
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
        cx.notify();
    }

    /// Pop one level of zoom (Cmd+Up or backspace-like) and rebuild
    /// the cached layout against the current treemap size.
    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        if self.zoom_path.pop().is_some() {
            self.invalidate_layout();
            self.rebuild_layout_if_ready();
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
                    total = paths.len()
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
            .map(|(p, _, _)| p.display().to_string())
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
                                ok = ok,
                                failed = failed.len(),
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
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.selected.is_empty() || self.lead.is_some() {
            self.selected.clear();
            self.lead = None;
            cx.notify();
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
        if let Some(id) = self.task_id.take() {
            if let Ok(mut reg) = self.tasks.try_borrow_mut() {
                reg.end(id);
            }
        }
    }
}

impl Render for DiskUsageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let topn_visible = self.topn_visible;
        let viewport = window.viewport_size();
        let (host_w, host_h) = self
            .host_size
            .unwrap_or((viewport.width.as_f32(), viewport.height.as_f32()));
        let side_width = if topn_visible { TOPN_PANEL_WIDTH } else { 0.0 };
        let header_height = if self.volume.is_some() { 118.0 } else { 88.0 };
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
        v_flex()
            .track_focus(&self.focus_handle)
            .key_context(DISK_USAGE_CONTEXT)
            // Context-menu verbs (rect + background menus dispatch
            // these through the view's focus context).
            .on_action(cx.listener(Self::on_du_clear_selection))
            .on_action(cx.listener(Self::on_du_open))
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
    }
}

enum ScanMsg {
    Batch(Vec<DiskUsageFact>),
    Progress(DiskUsageStats),
    Done(Option<EnumerationError>),
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
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(960.0), px(720.0)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(tr!(
                "Disk Usage \u{2014} {path}",
                path = ferail_fs_native::paths::display_path(&root)
            )),
            ..Default::default()
        }),
        ..crate::base_window_options()
    };
    let handle = cx.open_window(opts, |window, cx| cx.new(|cx| Root::new(view, window, cx)))?;
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
