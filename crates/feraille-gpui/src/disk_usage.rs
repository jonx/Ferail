//! Disk Usage window (Harvest Stage 7).
//!
//! A second native window showing a squarified treemap of the scanned
//! folder's contents. Reuses every piece of the existing
//! `feraille-disk-usage` crate (scan tree, layout, classification) and
//! `feraille-fs-native::scan_disk_usage` for the walker — the new
//! code is just orchestration + GPUI rendering.
//!
//! Streaming pattern: the BG scan pushes fact batches into an
//! `Arc<Mutex<VecDeque<ScanMsg>>>` queue; a FG timer drains bounded
//! FIFO chunks on a dynamic cadence and applies them to the tree,
//! debouncing layout rebuilds the same way the old
//! `disk_usage_state` did. Cancellation is cooperative via
//! `AtomicBool`.

use crate::text::TextScale as _;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use feraille_core::{EnumerationError, NodeId};
use feraille_disk_usage::{
    DiskUsageFact, DiskUsageLayoutNode, DiskUsageStats, DiskUsageTree, FileCategory, SizeMode,
    TreemapRect, build_layout_node_with_mode, compute_treemap,
};
use feraille_fs_native::NativeFs;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, ElementExt, Root, Selectable, Sizable,
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
    selected_node: Option<NodeId>,
    category_filter: Option<FileCategory>,

    size_mode: SizeMode,
    descend_packages: bool,

    /// Capacity of the volume containing `root_path`, when known.
    /// Renders as a Finder-style "X.X GB free of Y.Y GB" capacity
    /// bar in the header.
    volume: Option<feraille_fs_native::VolumeInfo>,

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
            selected_node: None,
            category_filter: None,
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
                .spawn(async move { feraille_fs_native::volume_info_for_path(&vol_path) })
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
                if n.kind != feraille_disk_usage::NodeKind::File {
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
        let descend = self.descend_packages;
        let queue_for_scan = self.msg_queue.clone();
        let queue_for_progress = self.msg_queue.clone();
        let queue_for_done = self.msg_queue.clone();

        // Register the scan with the shared task registry so the
        // owning Shell's status-bar progress strip shows indeterminate
        // motion for the duration. Cancellable: the status bar / task
        // panel will pick that up when we wire the cancel button.
        let task_label = format!("Scanning {}", short_path(&self.root_path));
        let task_id = self.with_tasks(cx, |reg| reg.begin(TaskKind::DiskUsage, task_label, true));
        self.task_id = Some(task_id);

        // BG: run the scan. Synchronous I/O on the executor's pool.
        cx.background_executor()
            .spawn(async move {
                let err = fs.scan_disk_usage(
                    &root,
                    feraille_fs_native::DEFAULT_DU_BATCH,
                    &cancel,
                    descend,
                    |batch| {
                        if let Ok(mut q) = queue_for_scan.lock() {
                            q.push_back(ScanMsg::Batch(batch));
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
                        v.invalidate_layout();
                        v.rebuild_layout_if_ready();
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
        self.selected_node = None;
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
        cx.notify();
    }

    fn focus_id(&self) -> NodeId {
        self.zoom_path.last().copied().unwrap_or(self.root_id)
    }

    fn select_node(&mut self, target: NodeId, cx: &mut Context<Self>) {
        if self.selected_node != Some(target) {
            self.selected_node = Some(target);
        }
        cx.notify();
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
            "Disk Usage".to_string()
        } else {
            self.root_path.to_string_lossy().into_owned()
        };
        let scanned = humanize_bytes(self.stats.bytes_scanned);
        let files = self.stats.files_scanned;
        let folders = self.stats.dirs_scanned;
        let scanning = !self.scan_complete;
        let summary = if self.scan_complete {
            format!("{files} files, {folders} folders, {scanned}")
        } else {
            format!("Scanning\u{2026} {files} files, {scanned}")
        };
        // Phase 6 follow-on: header action buttons are icon-only.
        // Each one carries a tooltip with the human-readable name
        // so the affordance is recoverable on hover.
        use gpui_component::Icon;
        let scan_button = if scanning {
            Button::new("du-cancel")
                .small()
                .icon(Icon::empty().path("icons/close.svg"))
                .tooltip("Cancel scan")
                .on_click(cx.listener(|this, _, _, cx| this.cancel_scan(cx)))
        } else {
            Button::new("du-refresh")
                .small()
                .icon(Icon::empty().path("icons/nav/refresh.svg"))
                .tooltip("Refresh")
                .on_click(cx.listener(|this, _, _, cx| this.restart_scan(cx)))
        };
        let dock_button = self.dock_owner.as_ref().map(|dock| {
            let dock = dock.clone();
            let root = self.root_path.clone();
            Button::new("du-dock")
                .small()
                .icon(Icon::empty().path("icons/minimize.svg"))
                .tooltip("Dock in tab")
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
                            .tooltip("Zoom out")
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
                                "Hide largest-files panel"
                            } else {
                                "Show largest-files panel"
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
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(summary)),
                    )
                    .child(
                        ButtonGroup::new("du-size-mode")
                            .small()
                            .outline()
                            .compact()
                            .child(
                                Button::new("du-size-apparent")
                                    .label("Apparent")
                                    .selected(self.size_mode == SizeMode::Apparent),
                            )
                            .child(
                                Button::new("du-size-allocated")
                                    .label("Allocated")
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
                                .tooltip("Scan package folders as containers")
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
                    let label = format!(
                        "{} free of {} on {}",
                        humanize_bytes(avail),
                        humanize_bytes(total),
                        v.name
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
                                    .child(SharedString::from(label)),
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
                let selected = self.selected_node == Some(e.node_id);
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_node(node_id, cx);
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
                    .child("Largest files"),
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
                                .child("No matching files yet"),
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
            (FileCategory::Image, "Image"),
            (FileCategory::Video, "Video"),
            (FileCategory::Audio, "Audio"),
            (FileCategory::Document, "Document"),
            (FileCategory::Archive, "Archive"),
            (FileCategory::Executable, "Executable"),
            (FileCategory::Other, "Other"),
        ];
        let chips = entries.iter().enumerate().map(|(ix, (cat, label))| {
            let cat = *cat;
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
                        .child(SharedString::from(*label)),
                )
        });
        let selection = self
            .selected_node
            .and_then(|id| self.tree.nodes.get(&id))
            .map(|n| {
                let size = size_for_mode(n.size_bytes, n.allocated_bytes, self.size_mode);
                format!("{}  {}", n.display_name, humanize_bytes(size))
            })
            .or_else(|| {
                self.category_filter
                    .map(|cat| format!("Filtering {}", category_label(cat)))
            })
            .unwrap_or_else(|| "All categories".to_owned());
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

    fn treemap(&self, w: f32, h: f32, cx: &mut Context<Self>) -> Div {
        let (w, h) = (w.max(260.0), h.max(220.0));
        let view = cx.entity().clone();
        let mut container = div()
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
            let selected = self.selected_node == Some(node_id);
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
                .border_1()
                .border_color(if selected {
                    cx.theme().selection
                } else {
                    rgba(0x00000033).into()
                })
                .id(("du-rect", ix))
                .cursor_pointer()
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
            // Single click selects; double click on a container zooms
            // in. Right click backs out one level, matching the old
            // native view's quick navigation feel without opening a
            // modal menu on the hot path.
            rect = rect.on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if event.is_right_click() {
                    this.zoom_out(cx);
                } else if event.click_count() >= 2 && has_children {
                    this.selected_node = Some(node_id);
                    this.zoom_into(node_id, cx);
                } else {
                    this.select_node(node_id, cx);
                }
            }));
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

fn category_color(cat: FileCategory) -> Rgba {
    match cat {
        FileCategory::Image => Rgba {
            r: 0.30,
            g: 0.60,
            b: 0.95,
            a: 0.85,
        },
        FileCategory::Video => Rgba {
            r: 0.85,
            g: 0.25,
            b: 0.45,
            a: 0.85,
        },
        FileCategory::Audio => Rgba {
            r: 0.70,
            g: 0.40,
            b: 0.85,
            a: 0.85,
        },
        FileCategory::Document => Rgba {
            r: 0.95,
            g: 0.75,
            b: 0.20,
            a: 0.85,
        },
        FileCategory::Archive => Rgba {
            r: 0.60,
            g: 0.50,
            b: 0.30,
            a: 0.85,
        },
        FileCategory::Executable => Rgba {
            r: 0.55,
            g: 0.55,
            b: 0.55,
            a: 0.85,
        },
        FileCategory::Other => Rgba {
            r: 0.65,
            g: 0.65,
            b: 0.65,
            a: 0.70,
        },
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

fn category_label(cat: FileCategory) -> &'static str {
    match cat {
        FileCategory::Image => "Image",
        FileCategory::Video => "Video",
        FileCategory::Audio => "Audio",
        FileCategory::Archive => "Archive",
        FileCategory::Document => "Document",
        FileCategory::Executable => "Executable",
        FileCategory::Other => "Other",
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
            title: Some(SharedString::from(format!(
                "Disk Usage \u{2014} {}",
                root.display()
            ))),
            ..Default::default()
        }),
        ..Default::default()
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
