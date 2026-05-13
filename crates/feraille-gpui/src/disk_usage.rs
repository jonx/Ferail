//! Disk Usage window (Harvest Stage 7).
//!
//! A second native window showing a squarified treemap of the scanned
//! folder's contents. Reuses every piece of the existing
//! `feraille-disk-usage` crate (scan tree, layout, classification) and
//! `feraille-fs-native::scan_disk_usage` for the walker — the new
//! code is just orchestration + GPUI rendering.
//!
//! Streaming pattern: the BG scan pushes fact batches into an
//! `Arc<Mutex<Vec<ScanMsg>>>` queue; a FG timer drains the queue every
//! 80 ms and applies batches to the tree, debouncing layout rebuilds
//! the same way the old `disk_usage_state` did. Cancellation is
//! cooperative via `AtomicBool`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use feraille_core::{EnumerationError, NodeId};
use feraille_disk_usage::{
    build_layout_node_with_mode, compute_treemap, DiskUsageFact, DiskUsageLayoutNode,
    DiskUsageStats, DiskUsageTree, FileCategory, SizeMode, TreemapRect,
};
use feraille_fs_native::NativeFs;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{h_flex, v_flex, ActiveTheme, Root};

/// Treemap recursion depth used by the DU window. Matches the old
/// app's DU_LAYOUT_DEPTH.
const DU_LAYOUT_DEPTH: u32 = 4;
/// FG drain interval — bounded so the user sees incremental progress
/// without thrashing layout on every batch.
const DU_DRAIN_INTERVAL: Duration = Duration::from_millis(80);

pub struct DiskUsageView {
    root_path: PathBuf,
    root_id: NodeId,

    tree: DiskUsageTree,
    stats: DiskUsageStats,
    scan_complete: bool,
    error: Option<EnumerationError>,
    cancel: Arc<AtomicBool>,

    /// Queue of messages produced by the BG scanner; drained by the
    /// FG timer task. `Arc<Mutex<_>>` for cross-thread share.
    msg_queue: Arc<Mutex<Vec<ScanMsg>>>,

    /// Cached layout for the current scan + bounds; invalidated when
    /// new facts come in or the user clicks a rect to zoom.
    layout_cache: Option<DiskUsageLayoutNode>,
    rects_cache: Vec<TreemapRect>,
    rects_bounds: Option<(f32, f32, f32, f32)>,

    /// `last() == focus` — the deepest folder the user has clicked
    /// into. Empty == root.
    zoom_path: Vec<NodeId>,

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

    focus_handle: FocusHandle,
}

/// One entry in the Top-N largest-files panel.
#[derive(Clone, Debug)]
struct TopFileEntry {
    name: String,
    size_bytes: u64,
}

const TOPN_CAP: usize = 50;
const TOPN_PANEL_WIDTH: f32 = 240.0;

impl DiskUsageView {
    pub fn new(
        root_path: PathBuf,
        fs: Arc<NativeFs>,
        cx: &mut Context<Self>,
    ) -> Self {
        let canonical = std::fs::canonicalize(&root_path)
            .unwrap_or_else(|_| root_path.clone());
        let root_id = fs.id_for_path(&canonical);
        let cancel = Arc::new(AtomicBool::new(false));
        let msg_queue = Arc::new(Mutex::new(Vec::new()));
        // Resolve volume capacity for the header capacity bar.
        // None on non-macOS or when the volume info lookup failed.
        let volume = feraille_fs_native::volume_info_for_path(&canonical);
        let mut view = Self {
            root_path: canonical.clone(),
            root_id,
            tree: DiskUsageTree::new(root_id),
            stats: DiskUsageStats::default(),
            scan_complete: false,
            error: None,
            cancel: cancel.clone(),
            msg_queue: msg_queue.clone(),
            layout_cache: None,
            rects_cache: Vec::new(),
            rects_bounds: None,
            zoom_path: Vec::new(),
            size_mode: SizeMode::Apparent,
            descend_packages: false,
            volume,
            top_files: Vec::new(),
            topn_visible: true,
            focus_handle: cx.focus_handle(),
        };
        view.start_scan(fs, cx);
        view
    }

    /// Recompute the Top-N largest-files list from the current tree.
    /// Single pass + partial sort, capped at `TOPN_CAP`.
    fn rebuild_top_files(&mut self) {
        let mut all: Vec<TopFileEntry> = self
            .tree
            .nodes
            .iter()
            .filter_map(|(_id, n)| match n.kind {
                feraille_disk_usage::NodeKind::File if n.size_bytes > 0 => {
                    Some(TopFileEntry {
                        name: n.display_name.clone(),
                        size_bytes: n.size_bytes,
                    })
                }
                _ => None,
            })
            .collect();
        all.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
        all.truncate(TOPN_CAP);
        self.top_files = all;
    }

    /// Spawn the disk-usage scan on the background executor + start
    /// the FG drain timer.
    fn start_scan(&mut self, fs: Arc<NativeFs>, cx: &mut Context<Self>) {
        let root = self.root_path.clone();
        let cancel = self.cancel.clone();
        let descend = self.descend_packages;
        let queue_for_scan = self.msg_queue.clone();
        let queue_for_progress = self.msg_queue.clone();
        let queue_for_done = self.msg_queue.clone();

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
                            q.push(ScanMsg::Batch(batch));
                        }
                    },
                    |progress| {
                        if let Ok(mut q) = queue_for_progress.lock() {
                            q.push(ScanMsg::Progress(progress));
                        }
                    },
                );
                if let Ok(mut q) = queue_for_done.lock() {
                    q.push(ScanMsg::Done(err));
                }
            })
            .detach();

        // FG: drain the queue periodically + apply on the view.
        let queue_for_drain = self.msg_queue.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(DU_DRAIN_INTERVAL)
                    .await;
                let msgs: Vec<ScanMsg> = match queue_for_drain.lock() {
                    Ok(mut q) => std::mem::take(&mut *q),
                    Err(_) => break,
                };
                if msgs.is_empty() {
                    continue;
                }
                let mut done = false;
                if this
                    .update(cx, |v, cx| {
                        for msg in msgs {
                            if matches!(msg, ScanMsg::Done(_)) {
                                done = true;
                            }
                            v.handle_scan_msg(msg, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    fn handle_scan_msg(&mut self, msg: ScanMsg, cx: &mut Context<Self>) {
        match msg {
            ScanMsg::Batch(facts) => {
                self.tree.apply_facts(&facts);
                self.invalidate_layout();
                self.rebuild_top_files();
                cx.notify();
            }
            ScanMsg::Progress(p) => {
                self.stats = p;
                cx.notify();
            }
            ScanMsg::Done(err) => {
                self.scan_complete = true;
                self.error = err;
                self.invalidate_layout();
                self.rebuild_top_files();
                cx.notify();
            }
        }
    }

    fn invalidate_layout(&mut self) {
        self.layout_cache = None;
        self.rects_cache.clear();
        self.rects_bounds = None;
    }

    fn focus_id(&self) -> NodeId {
        self.zoom_path.last().copied().unwrap_or(self.root_id)
    }

    fn ensure_layout(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let bounds_match = self.rects_bounds == Some((x, y, w, h));
        if bounds_match && !self.rects_cache.is_empty() {
            return;
        }
        if self.layout_cache.is_none() {
            self.layout_cache = Some(build_layout_node_with_mode(
                &self.tree,
                self.focus_id(),
                DU_LAYOUT_DEPTH,
                self.size_mode,
            ));
        }
        if let Some(layout) = &self.layout_cache {
            self.rects_cache = compute_treemap(layout, (x, y, w, h), DU_LAYOUT_DEPTH);
            self.rects_bounds = Some((x, y, w, h));
        }
    }

    fn header(&self, cx: &mut App) -> Div {
        let theme = cx.theme();
        let title = self.root_path.to_string_lossy().into_owned();
        let scanned = humanize_bytes(self.stats.bytes_scanned);
        let files = self.stats.files_scanned;
        let folders = self.stats.dirs_scanned;
        let summary = if self.scan_complete {
            format!("{files} files, {folders} folders, {scanned}")
        } else {
            format!("Scanning\u{2026} {files} files, {scanned}")
        };
        let mut col = v_flex()
            .w_full()
            .gap_1()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(summary)),
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
                                    .child(
                                        div()
                                            .h_full()
                                            .w(fill_w)
                                            .rounded(px(2.0))
                                            .bg(fill_bg),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
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
    fn top_panel(&self, cx: &mut App) -> Div {
        let theme = cx.theme();
        let rows: Vec<Div> = self
            .top_files
            .iter()
            .enumerate()
            .map(|(ix, e)| {
                h_flex()
                    .w_full()
                    .gap_2()
                    .py_1()
                    .px_2()
                    .when(ix % 2 == 0, |this| {
                        this.bg(theme.muted_foreground.opacity(0.04))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme.foreground)
                            .child(SharedString::from(e.name.clone())),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(humanize_bytes(e.size_bytes))),
                    )
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
                    .text_xs()
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
                    .child(v_flex().w_full().children(rows)),
            )
    }

    /// Color legend at the bottom — one chip per FileCategory. Just
    /// visual reference today; clicking to filter lands in a
    /// follow-on.
    fn legend(&self, cx: &mut App) -> Div {
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
        let chips = entries.iter().map(|(cat, label)| {
            h_flex()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .w(px(10.0))
                        .h(px(10.0))
                        .rounded(px(2.0))
                        .bg(category_color(*cat)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(*label)),
                )
        });
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(theme.border)
            .children(chips)
    }

    fn treemap(&mut self, cx: &mut Context<Self>) -> Div {
        let (w, h) = (900.0_f32, 600.0_f32);
        self.ensure_layout(0.0, 0.0, w, h);
        let rects = self.rects_cache.clone();
        // Snapshot display name + size string per rect — drops the
        // tree borrow before we build click handlers below.
        let labels: Vec<(String, String)> = rects
            .iter()
            .map(|r| {
                let name = self
                    .tree
                    .nodes
                    .get(&r.node_id)
                    .map(|n| n.display_name.clone())
                    .unwrap_or_default();
                (name, humanize_bytes(r.size_bytes))
            })
            .collect();
        let mut container = div()
            .relative()
            .w(px(w))
            .h(px(h))
            .bg(cx.theme().background);
        for (ix, r) in rects.iter().enumerate() {
            if r.width < 1.0 || r.height < 1.0 {
                continue;
            }
            let color = category_color(r.file_category);
            let node_id = r.node_id;
            let has_children = r.has_children;
            let (name, size) = labels[ix].clone();
            let show_label = r.width >= 60.0 && r.height >= 24.0;
            let show_size = r.width >= 80.0 && r.height >= 40.0;
            let mut rect = div()
                .absolute()
                .top(px(r.y))
                .left(px(r.x))
                .w(px(r.width))
                .h(px(r.height))
                .bg(color)
                .border_1()
                .border_color(rgba(0x00000033))
                .id(("du-rect", ix))
                .cursor_pointer();
            if show_label {
                let inner = div()
                    .size_full()
                    .px_1()
                    .py_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgba(0xFFFFFFEE))
                            .child(SharedString::from(name)),
                    )
                    .when(show_size, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(rgba(0xFFFFFFAA))
                                .child(SharedString::from(size)),
                        )
                    });
                rect = rect.child(inner);
            }
            // Click on a container row zooms in. Files have no
            // children — clicking them just selects (no zoom).
            if has_children {
                rect = rect.on_click(cx.listener(move |this, _, _, cx| {
                    this.zoom_into(node_id, cx);
                }));
            }
            container = container.child(rect);
        }
        container
    }

    /// Zoom into `target` (clicked container rect). Pushes onto the
    /// zoom path, drops cached layout so the next paint recomputes.
    pub fn zoom_into(&mut self, target: NodeId, cx: &mut Context<Self>) {
        // Ignore the root — already focused.
        if target == self.focus_id() {
            return;
        }
        self.zoom_path.push(target);
        self.invalidate_layout();
        cx.notify();
    }

    /// Pop one level of zoom (Cmd+Up or backspace-like). Returns to
    /// the parent focus.
    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        if self.zoom_path.pop().is_some() {
            self.invalidate_layout();
            cx.notify();
        }
    }
}

impl Focusable for DiskUsageView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DiskUsageView {
    fn render(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let header = self.header(cx);
        let treemap = self.treemap(cx);
        let topn_visible = self.topn_visible && !self.top_files.is_empty();
        let topn = if topn_visible {
            Some(self.top_panel(cx))
        } else {
            None
        };
        let legend = self.legend(cx);
        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
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
        FileCategory::Image => Rgba { r: 0.30, g: 0.60, b: 0.95, a: 0.85 },
        FileCategory::Video => Rgba { r: 0.85, g: 0.25, b: 0.45, a: 0.85 },
        FileCategory::Audio => Rgba { r: 0.70, g: 0.40, b: 0.85, a: 0.85 },
        FileCategory::Document => Rgba { r: 0.95, g: 0.75, b: 0.20, a: 0.85 },
        FileCategory::Archive => Rgba { r: 0.60, g: 0.50, b: 0.30, a: 0.85 },
        FileCategory::Executable => Rgba { r: 0.55, g: 0.55, b: 0.55, a: 0.85 },
        FileCategory::Other => Rgba { r: 0.65, g: 0.65, b: 0.65, a: 0.70 },
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

/// Open the Disk Usage window for `root`. Independent of the main
/// shell — closing one doesn't affect the other.
pub fn open_window(
    root: PathBuf,
    fs: Arc<NativeFs>,
    cx: &mut App,
) -> Result<WindowHandle<Root>, anyhow::Error> {
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
    let handle = cx.open_window(opts, |window, cx| {
        let view = cx.new(|cx| DiskUsageView::new(root.clone(), fs.clone(), cx));
        cx.new(|cx| Root::new(view, window, cx))
    })?;
    Ok(handle)
}
