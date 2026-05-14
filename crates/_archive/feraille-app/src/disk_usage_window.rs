//! Dedicated Disk Usage window — winit window + softbuffer surface +
//! soft renderer + per-window paint state. Independent lifecycle from
//! the main window: created on first `Cmd+Shift+D`, closed by the
//! user's window-close gesture; the App routes events to it by
//! [`winit::window::WindowId`].
//!
//! Iter-6.3 layout (top → bottom, left → right):
//!
//! ```text
//! +-----------------------------------------------------+
//! | Disk Usage · /path · 1.2 GB · Done       [Refresh] | path row (28 DIP)
//! +-----------------------------------------------------+
//! | ● Macintosh HD — 245 GB free of 460 GB [██████░░░] | volume row (24 DIP)
//! +---------------------------------------+--+----------+
//! |                                       |  |  Top N   |
//! |        Treemap                        |≡ |  files   |
//! |                                       |  |          |
//! +---------------------------------------+--+----------+
//! ```
//!
//! Iter-6.4 will add a right-click context menu + Move-to-Trash live
//! updates.

use std::rc::Rc;

use feraille_controls::primitives::splitter::Splitter;
use feraille_controls::{treemap, TreemapColoring};
use feraille_core::NodeId;
use feraille_design::{Color, FontWeight, Tokens};
use feraille_disk_usage::{
    build_layout_node_with_mode, compute_treemap, hit_test, DiskUsageNode,
};
use feraille_render::{Bitmap, Point, Rect, Renderer, SoftRenderer, TextStyle};
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::disk_usage_state::{
    DiskUsageState, TopFileEntry, DU_LAYOUT_DEBOUNCE_MS, DU_LAYOUT_DEPTH,
};

/// Path / total / status / refresh row. 28 DIPs.
pub const PATH_ROW_H: f32 = 28.0;
/// Volume free/used row. 24 DIPs.
pub const VOLUME_ROW_H: f32 = 24.0;
/// Category-filter legend row. 28 DIPs.
pub const LEGEND_ROW_H: f32 = 28.0;
/// Total header strip height.
pub const HEADER_H: f32 = PATH_ROW_H + VOLUME_ROW_H + LEGEND_ROW_H;
/// Width of one category chip in the legend.
const CHIP_W: f32 = 70.0;
/// Gap between chips.
const CHIP_GAP: f32 = 6.0;
/// Inset between the legend strip's left edge and the first chip.
const LEGEND_INSET: f32 = 12.0;
/// Top-N panel header height.
pub const TOPN_HEADER_H: f32 = 26.0;
/// One Top-N row height — fits a name line on top and a parent +
/// secondary line below.
pub const TOPN_ROW_H: f32 = 36.0;
/// Min Top-N panel width.
pub const TOPN_WIDTH_MIN: f32 = 200.0;
/// Max Top-N panel width.
pub const TOPN_WIDTH_MAX: f32 = 520.0;
/// Refresh button width in the path row.
const REFRESH_BTN_W: f32 = 80.0;

/// Hit-test result for a click anywhere in the window. Used to route
/// button presses to the right action without the call site needing
/// to know the geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuHit {
    None,
    RefreshButton,
    Splitter,
    /// Legend chip — `None` is the "All" chip; `Some(cat)` filters to
    /// that category.
    LegendChip(Option<feraille_disk_usage::FileCategory>),
    TreemapNode(NodeId),
    TopNRow(NodeId),
    /// Click on the Top-N panel's column-header sort button.
    TopNSortHeader(crate::disk_usage_state::TopNSort),
}

/// Categories shown in the legend, in display order. "All" is
/// rendered first as a separate chip mapped to `None`.
pub const LEGEND_CATEGORIES: &[feraille_disk_usage::FileCategory] = &[
    feraille_disk_usage::FileCategory::Image,
    feraille_disk_usage::FileCategory::Video,
    feraille_disk_usage::FileCategory::Audio,
    feraille_disk_usage::FileCategory::Archive,
    feraille_disk_usage::FileCategory::Document,
    feraille_disk_usage::FileCategory::Executable,
    feraille_disk_usage::FileCategory::Other,
];

fn legend_chip_label(c: Option<feraille_disk_usage::FileCategory>) -> &'static str {
    use feraille_disk_usage::FileCategory;
    match c {
        None => "All",
        Some(FileCategory::Image) => "Image",
        Some(FileCategory::Video) => "Video",
        Some(FileCategory::Audio) => "Audio",
        Some(FileCategory::Archive) => "Archive",
        Some(FileCategory::Document) => "Document",
        Some(FileCategory::Executable) => "Executable",
        Some(FileCategory::Other) => "Other",
    }
}

/// Rect for the chip at `slot_index` (0 = "All").
pub fn legend_chip_rect(viewport: Rect, slot_index: usize) -> Rect {
    let y = PATH_ROW_H + VOLUME_ROW_H + (LEGEND_ROW_H - 20.0) / 2.0;
    let x = LEGEND_INSET + (slot_index as f32) * (CHIP_W + CHIP_GAP);
    let _ = viewport;
    Rect::new(x, y, CHIP_W, 20.0)
}

/// Hit-test the legend strip at `(px, py)`. Returns the chip's filter
/// value if hit (None for "All", Some(cat) for a category) wrapped in
/// an outer Option (outer None = no chip hit).
pub fn legend_chip_at(viewport: Rect, px: f32, py: f32) -> Option<Option<feraille_disk_usage::FileCategory>> {
    if py < PATH_ROW_H + VOLUME_ROW_H || py >= PATH_ROW_H + VOLUME_ROW_H + LEGEND_ROW_H {
        return None;
    }
    // Slot 0 = All; slots 1..=N for LEGEND_CATEGORIES.
    let total_slots = 1 + LEGEND_CATEGORIES.len();
    for slot in 0..total_slots {
        let r = legend_chip_rect(viewport, slot);
        if px >= r.left() && px < r.right() && py >= r.top() && py < r.bottom() {
            return Some(if slot == 0 {
                None
            } else {
                Some(LEGEND_CATEGORIES[slot - 1])
            });
        }
    }
    None
}

/// Visual state of a chrome button. Press flips back to Idle on
/// release; if the cursor leaves the rect mid-press the state goes
/// back to Idle so the button doesn't stay "stuck" looking pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ButtonState {
    #[default]
    Idle,
    Hover,
    Pressed,
}

pub struct DiskUsageWindow {
    pub window: Rc<Window>,
    /// Owned solely to keep the softbuffer context alive for the
    /// surface — never read directly after construction.
    #[allow(dead_code)]
    pub sb_context: softbuffer::Context<Rc<Window>>,
    pub surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    pub renderer: SoftRenderer,
    pub width_px: u32,
    pub height_px: u32,
    pub scale_factor: f32,
    pub state: DiskUsageState,
    /// Cursor position in DIPs, when known. Used for hover updates and
    /// click hit-tests.
    pub pointer_dips: Option<Point>,
    /// Splitter between treemap and Top-N pane.
    pub topn_splitter: Splitter,
    /// Visual state of the [Refresh] button in the path row.
    pub refresh_button: ButtonState,
}

impl DiskUsageWindow {
    pub fn new(
        window: Rc<Window>,
        sb_context: softbuffer::Context<Rc<Window>>,
        surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
        renderer: SoftRenderer,
        width_px: u32,
        height_px: u32,
        scale_factor: f32,
        state: DiskUsageState,
    ) -> Self {
        Self {
            window,
            sb_context,
            surface,
            renderer,
            width_px,
            height_px,
            scale_factor,
            state,
            pointer_dips: None,
            topn_splitter: Splitter::new(0.0, 0.0),
            refresh_button: ButtonState::Idle,
        }
    }

    pub fn viewport_dips(&self) -> Rect {
        Rect::new(
            0.0,
            0.0,
            self.width_px as f32 / self.scale_factor,
            self.height_px as f32 / self.scale_factor,
        )
    }

    pub fn treemap_pane(&self) -> Rect {
        treemap_pane(&self.state, self.viewport_dips())
    }

    pub fn topn_pane(&self) -> Option<Rect> {
        topn_pane(&self.state, self.viewport_dips())
    }

    fn splitter_x(&self) -> Option<f32> {
        splitter_x(&self.state, self.viewport_dips())
    }

    fn refresh_button_rect(&self) -> Rect {
        refresh_button_rect(self.viewport_dips())
    }

    pub fn handle_resize(&mut self, size: PhysicalSize<u32>) {
        self.width_px = size.width.max(1);
        self.height_px = size.height.max(1);
        self.renderer.resize(self.width_px, self.height_px);
        self.state.invalidate_layout();
    }

    pub fn handle_scale_factor(&mut self, scale: f32) {
        self.scale_factor = scale;
        self.renderer.set_scale_factor(scale);
        self.state.invalidate_layout();
    }

    /// Apply a streamed batch of facts. Rebuilds layout (and Top-N)
    /// only if the debounce window has elapsed.
    pub fn apply_batch(&mut self, facts: &[feraille_disk_usage::DiskUsageFact]) {
        self.state.tree.apply_facts(facts);
        let elapsed_ms = self.state.last_rebuild.elapsed().as_millis();
        if elapsed_ms >= DU_LAYOUT_DEBOUNCE_MS {
            self.state.invalidate_layout();
            self.state.rebuild_topn();
            self.state.last_rebuild = std::time::Instant::now();
        }
    }

    pub fn mark_complete(&mut self) {
        self.state.scan_complete = true;
        self.state.tree.complete = true;
        self.state.invalidate_layout();
        self.state.rebuild_topn();
        self.state.last_rebuild = std::time::Instant::now();
    }

    pub fn drilldown(&mut self, id: NodeId) {
        let has_children = self
            .state
            .tree
            .containers
            .get(&id)
            .map(|m| !m.is_empty())
            .unwrap_or(false);
        if !has_children {
            return;
        }
        self.state.zoom_path.push(id);
        self.state.invalidate_layout();
    }

    pub fn zoom_out(&mut self) {
        if self.state.zoom_path.pop().is_some() {
            self.state.invalidate_layout();
        }
    }

    /// Whole-window hit test. Order: refresh button → splitter →
    /// Top-N row → treemap rect → none.
    pub fn hit_at(&self, p: Point) -> DuHit {
        if self.refresh_button_rect().contains(p) {
            return DuHit::RefreshButton;
        }
        // Legend chips live above the treemap pane; check before
        // splitter / treemap so a click on the strip doesn't fall
        // through to the pane below.
        if let Some(filter) = legend_chip_at(self.viewport_dips(), p.x, p.y) {
            return DuHit::LegendChip(filter);
        }
        if let Some(x) = self.splitter_x() {
            let v = self.viewport_dips();
            let zone = Splitter::hit_rect(x, Rect::new(0.0, HEADER_H, v.size.width, v.size.height - HEADER_H));
            if zone.contains(p) {
                return DuHit::Splitter;
            }
        }
        if let Some(topn) = self.topn_pane() {
            if topn.contains(p) {
                // Header strip: route to a sort button if it lands on one.
                if p.y < topn.top() + TOPN_HEADER_H {
                    use crate::disk_usage_state::TopNSort;
                    for key in [TopNSort::Size, TopNSort::Name, TopNSort::Age] {
                        let r = topn_sort_button_rect(topn, key);
                        if p.x >= r.left() && p.x < r.right() && p.y >= r.top() && p.y < r.bottom()
                        {
                            return DuHit::TopNSortHeader(key);
                        }
                    }
                    return DuHit::None;
                }
                // Rows below the header — apply scroll offset.
                let local_y = p.y - topn.top() - TOPN_HEADER_H + self.state.topn_scroll_offset;
                let row_index = (local_y / TOPN_ROW_H) as isize;
                if row_index >= 0 {
                    if let Some(entry) = self.state.topn_files.get(row_index as usize) {
                        return DuHit::TopNRow(entry.node_id);
                    }
                }
                return DuHit::None;
            }
        }
        let pane = self.treemap_pane();
        if pane.contains(p) {
            if let Some(rect) = hit_test(&self.state.rects_cache, p.x, p.y) {
                return DuHit::TreemapNode(rect.node_id);
            }
        }
        DuHit::None
    }

    /// Convenience for hover updates — same as `hit_at` for the
    /// current pointer position, returning only NodeId hits (button /
    /// splitter / empty space all map to None).
    pub fn hover_node(&self) -> Option<NodeId> {
        let p = self.pointer_dips?;
        match self.hit_at(p) {
            DuHit::TreemapNode(id) | DuHit::TopNRow(id) => Some(id),
            _ => None,
        }
    }

    pub fn paint(&mut self, tokens: &Tokens) {
        let viewport = self.viewport_dips();
        paint_du(
            &mut self.state,
            viewport,
            &mut self.topn_splitter,
            &mut self.renderer,
            tokens,
            self.refresh_button,
        );
    }

    /// Push the painted pixels to the window.
    pub fn present(&mut self) {
        if self.width_px == 0 || self.height_px == 0 {
            return;
        }
        if let (Some(w), Some(h)) = (
            std::num::NonZeroU32::new(self.width_px),
            std::num::NonZeroU32::new(self.height_px),
        ) {
            if self.surface.resize(w, h).is_err() {
                return;
            }
        }
        let Ok(mut buf) = self.surface.buffer_mut() else {
            return;
        };
        let pixels = self.renderer.pixels();
        let n = pixels.len().min(buf.len());
        buf[..n].copy_from_slice(&pixels[..n]);
        let _ = buf.present();
    }

    /// Splitter drag entry. Returns true if a drag was started.
    pub fn begin_splitter_drag(&mut self, p: Point) -> bool {
        let Some(x) = self.splitter_x() else { return false };
        let v = self.viewport_dips();
        let container = Rect::new(0.0, HEADER_H, v.size.width, v.size.height - HEADER_H);
        // Update min/max to the live viewport at drag-start.
        self.topn_splitter.min = v.size.width - TOPN_WIDTH_MAX;
        self.topn_splitter.max = (v.size.width - TOPN_WIDTH_MIN).max(self.topn_splitter.min);
        self.topn_splitter.begin_drag_at(x, container, p)
    }

    /// Splitter drag motion. Returns true if topn_width changed.
    pub fn update_splitter_drag(&mut self, p: Point) -> bool {
        if let Some(new_x) = self.topn_splitter.position_for_drag(p) {
            let v = self.viewport_dips();
            let new_width = (v.size.width - new_x).clamp(TOPN_WIDTH_MIN, TOPN_WIDTH_MAX);
            if (new_width - self.state.topn_width_dips).abs() > 0.5 {
                self.state.topn_width_dips = new_width;
                self.state.invalidate_layout();
                return true;
            }
        }
        false
    }

    pub fn end_splitter_drag(&mut self) {
        self.topn_splitter.end_drag();
    }

    pub fn splitter_dragging(&self) -> bool {
        self.topn_splitter.is_dragging()
    }

    /// Update the Top-N splitter's hover state from the latest cursor
    /// position (or `None` on cursor-leave). Returns true if the
    /// hover state changed and a repaint is warranted. Mirrors the
    /// pattern used by the main window's sidebar / preview splitters
    /// so the visual affordance — faint backdrop fill, handle dots,
    /// thicker rule — is identical across windows.
    pub fn update_splitter_hover(&mut self, point: Option<Point>) -> bool {
        let viewport = self.viewport_dips();
        let Some(x) = splitter_x(&self.state, viewport) else {
            return false;
        };
        let container = Rect::new(
            0.0,
            HEADER_H,
            viewport.size.width,
            (viewport.size.height - HEADER_H).max(0.0),
        );
        self.topn_splitter.update_hover(x, container, point)
    }
}

// ---- Free-function geometry + paint helpers (parameterized on
// `viewport`) so the headless screenshot path can render the same
// layout without instantiating a real winit window.

pub fn topn_width(state: &DiskUsageState, viewport: Rect) -> f32 {
    if !state.topn_visible {
        return 0.0;
    }
    if viewport.size.width < 700.0 {
        return 0.0;
    }
    state
        .topn_width_dips
        .clamp(TOPN_WIDTH_MIN, (viewport.size.width - 320.0).max(TOPN_WIDTH_MIN))
}

pub fn treemap_pane(state: &DiskUsageState, viewport: Rect) -> Rect {
    let topn_w = topn_width(state, viewport);
    let split_rule_w = if topn_w > 0.0 { 1.0 } else { 0.0 };
    Rect::new(
        0.0,
        HEADER_H,
        (viewport.size.width - topn_w - split_rule_w).max(0.0),
        (viewport.size.height - HEADER_H).max(0.0),
    )
}

pub fn topn_pane(state: &DiskUsageState, viewport: Rect) -> Option<Rect> {
    let topn_w = topn_width(state, viewport);
    if topn_w <= 0.0 {
        return None;
    }
    Some(Rect::new(
        viewport.size.width - topn_w,
        HEADER_H,
        topn_w,
        (viewport.size.height - HEADER_H).max(0.0),
    ))
}

pub fn splitter_x(state: &DiskUsageState, viewport: Rect) -> Option<f32> {
    let topn = topn_pane(state, viewport)?;
    Some(topn.left() - 0.5)
}

pub fn refresh_button_rect(viewport: Rect) -> Rect {
    let right = viewport.size.width - 8.0;
    Rect::new(
        right - REFRESH_BTN_W,
        (PATH_ROW_H - 20.0) / 2.0,
        REFRESH_BTN_W,
        20.0,
    )
}

/// Recompute the rect cache if bounds or focus path changed since
/// last paint. Returns the focus container's aggregated total.
pub fn rebuild_rects(state: &mut DiskUsageState, viewport: Rect) -> u64 {
    let pane = treemap_pane(state, viewport);
    let bounds = (pane.left(), pane.top(), pane.size.width, pane.size.height);
    let needs = state.layout_cache.is_none() || state.rects_bounds != Some(bounds);
    if !needs {
        return state
            .layout_cache
            .as_ref()
            .map(|n| n.size_bytes)
            .unwrap_or(0);
    }
    let focus = state.focus_id();
    let layout = build_layout_node_with_mode(&state.tree, focus, DU_LAYOUT_DEPTH, state.size_mode);
    let total = layout.size_bytes;
    state.rects_cache = compute_treemap(&layout, bounds, DU_LAYOUT_DEPTH);
    state.layout_cache = Some(layout);
    state.rects_bounds = Some(bounds);
    total
}

/// One-shot whole-window paint. Both [`DiskUsageWindow::paint`] and
/// the headless screenshot path call this so the two views stay in
/// pixel-level sync by construction.
pub fn paint_du(
    state: &mut DiskUsageState,
    viewport: Rect,
    topn_splitter: &mut Splitter,
    renderer: &mut dyn Renderer,
    tokens: &Tokens,
    refresh_button: ButtonState,
) {
    let total = rebuild_rects(state, viewport);
    let pane = treemap_pane(state, viewport);

    // Background.
    renderer.fill_rect(viewport, tokens.bg.base);

    // Three-row header (path / volume / legend).
    paint_header(state, viewport, total, refresh_button, renderer, tokens);
    paint_legend(state, viewport, renderer, tokens);

    // Treemap pane.
    let coloring: TreemapColoring = state.coloring;
    let filter = state.category_filter;
    let nodes = &state.tree.nodes;
    let rects = &state.rects_cache;
    treemap::paint(
        rects,
        pane,
        state.hovered,
        &state.selection,
        coloring,
        filter,
        tokens,
        renderer,
        |id| name_for(nodes, id),
    );

    // Empty-state hint while the scan hasn't produced rects yet.
    if rects.is_empty() && !state.scan_complete {
        let style = TextStyle {
            size: tokens.text.md,
            weight: FontWeight::Regular,
            color: tokens.fg.secondary,
        };
        let msg = "Walking filesystem…";
        let metrics = renderer.measure_text(msg, style);
        let cx = pane.left() + (pane.size.width - metrics.width) / 2.0;
        let cy = pane.top() + (pane.size.height - tokens.text.md) / 2.0;
        renderer.draw_text(Point::new(cx, cy), msg, style);
    }

    // Splitter + Top-N pane.
    if let Some(tp) = topn_pane(state, viewport) {
        if let Some(x) = splitter_x(state, viewport) {
            let split_container = Rect::new(
                0.0,
                HEADER_H,
                viewport.size.width,
                viewport.size.height - HEADER_H,
            );
            topn_splitter.paint(x, split_container, tokens, renderer);
        }
        paint_topn(state, tp, renderer, tokens);
    }

    // Cloud-glyph overlay for iCloud-resident cells. Drawn after the
    // treemap and before toasts so it sits above the cell fill but
    // below transient messages.
    {
        let glyph_style = TextStyle {
            size: tokens.text.sm,
            weight: FontWeight::Regular,
            color: feraille_design::Color::rgba(0xFF, 0xFF, 0xFF, 220),
        };
        for r in &state.rects_cache {
            if !matches!(r.kind, feraille_disk_usage::NodeKind::File) {
                continue;
            }
            // Only enough room? Skip tiny cells.
            if r.width < 36.0 || r.height < 24.0 {
                continue;
            }
            let is_cloud = state
                .tree
                .nodes
                .get(&r.node_id)
                .map(|n| n.is_cloud)
                .unwrap_or(false);
            if !is_cloud {
                continue;
            }
            let glyph = "\u{2601}"; // ☁
            let metrics = renderer.measure_text(glyph, glyph_style);
            renderer.draw_text(
                Point::new(
                    r.x + r.width - metrics.width - 4.0,
                    r.y + 2.0,
                ),
                glyph,
                glyph_style,
            );
        }
    }

    // Toasts — bottom-right of the treemap pane, above any other
    // chrome. Prune expired ones at paint time.
    let now = std::time::Instant::now();
    state.toasts.prune(now);
    if !state.toasts.is_empty() {
        let toast_area = Rect::new(
            pane.left(),
            pane.top(),
            pane.size.width,
            pane.size.height,
        );
        state.toasts.paint(toast_area, tokens, renderer);
    }
}

fn paint_header(
    state: &DiskUsageState,
    viewport: Rect,
    total: u64,
    refresh_button: ButtonState,
    renderer: &mut dyn Renderer,
    tokens: &Tokens,
) {
    // Backdrop for both rows.
    renderer.fill_rect(
        Rect::new(0.0, 0.0, viewport.size.width, HEADER_H),
        tokens.bg.layer2,
    );
    // Separator under the header.
    renderer.fill_rect(
        Rect::new(0.0, HEADER_H - 1.0, viewport.size.width, 1.0),
        tokens.border.subtle,
    );

    // ----- Path row -----
    let path_style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::SemiBold,
        color: tokens.fg.primary,
    };
    let status = if !state.scan_complete {
        format!(
            "Scanning… {} files · {} dirs",
            state.stats.files_scanned, state.stats.dirs_scanned,
        )
    } else if let Some(err) = &state.error {
        format!("Error: {err:?}")
    } else {
        "Done".to_string()
    };
    let path_text = format!(
        "Disk Usage  ·  {}  ·  {}  ·  {}",
        state.root_path.display(),
        humanize_bytes(total),
        status,
    );
    renderer.draw_text(
        Point::new(tokens.space.md, (PATH_ROW_H - tokens.text.sm) / 2.0 - 1.0),
        &path_text,
        path_style,
    );

    // Refresh button.
    let btn_rect = refresh_button_rect(viewport);
    let (btn_fill, btn_border) = match refresh_button {
        ButtonState::Idle => (tokens.bg.layer1, tokens.border.default),
        ButtonState::Hover => (tokens.bg.layer3, tokens.border.default),
        ButtonState::Pressed => (tokens.accent.subtle, tokens.border.focus),
    };
    renderer.fill_rect(btn_rect, btn_fill);
    renderer.stroke_rect(btn_rect, 1.0, btn_border);
    let btn_label = "Refresh";
    let btn_style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::Regular,
        color: tokens.fg.primary,
    };
    let btn_metrics = renderer.measure_text(btn_label, btn_style);
    renderer.draw_text(
        Point::new(
            btn_rect.left() + (btn_rect.size.width - btn_metrics.width) / 2.0,
            btn_rect.top() + (btn_rect.size.height - tokens.text.sm) / 2.0 - 1.0,
        ),
        btn_label,
        btn_style,
    );

    // ----- Volume row -----
    let vol_y = PATH_ROW_H;
    let vol_style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::Regular,
        color: tokens.fg.secondary,
    };

    let vol_text = match &state.volume {
        Some(v) => match (v.total_bytes, v.available_bytes) {
            (Some(total_b), Some(avail)) => {
                let used = total_b.saturating_sub(avail);
                format!(
                    "{}  —  {} free of {}  ·  {} used",
                    v.name,
                    humanize_bytes(avail),
                    humanize_bytes(total_b),
                    humanize_bytes(used),
                )
            }
            _ => format!("{}  (capacity unavailable)", v.name),
        },
        None => "Volume info unavailable".to_string(),
    };
    renderer.draw_text(
        Point::new(tokens.space.md, vol_y + (VOLUME_ROW_H - tokens.text.sm) / 2.0 - 1.0),
        &vol_text,
        vol_style,
    );

    // Used/total bar on the right side of the volume row.
    if let Some(volume) = &state.volume {
        if let (Some(total_b), Some(avail_b)) = (volume.total_bytes, volume.available_bytes) {
            if total_b > 0 {
                let frac = ((total_b.saturating_sub(avail_b)) as f64 / total_b as f64)
                    .clamp(0.0, 1.0) as f32;
                let bar_w: f32 = 220.0;
                let bar_right = viewport.size.width - 8.0;
                let bar_left = (bar_right - bar_w).max(0.0);
                let bar_h: f32 = 6.0;
                let bar_top = vol_y + (VOLUME_ROW_H - bar_h) / 2.0;
                let track = Rect::new(bar_left, bar_top, bar_w, bar_h);
                renderer.fill_rect(track, tokens.border.subtle);
                let fill_w = bar_w * frac;
                if fill_w > 0.0 {
                    let fill_color = if frac > 0.85 {
                        tokens.status.danger
                    } else if frac > 0.7 {
                        tokens.status.warning
                    } else {
                        tokens.accent.fill
                    };
                    renderer.fill_rect(
                        Rect::new(bar_left, bar_top, fill_w, bar_h),
                        fill_color,
                    );
                }
            }
        }
    }
}

/// Sub-rect helpers for the Top-N panel header so click-to-sort
/// hit-testing and paint use the same geometry.
pub fn topn_sort_button_rect(pane: Rect, key: crate::disk_usage_state::TopNSort) -> Rect {
    use crate::disk_usage_state::TopNSort;
    let label_w = match key {
        TopNSort::Size => 50.0,
        TopNSort::Name => 50.0,
        TopNSort::Age => 50.0,
    };
    let gap = 6.0;
    // Buttons aligned right-to-left so size sits closest to the
    // numeric column.
    let right = pane.right() - 8.0;
    let (idx_from_right, _) = match key {
        TopNSort::Size => (0, label_w),
        TopNSort::Name => (1, label_w),
        TopNSort::Age => (2, label_w),
    };
    let x = right - (idx_from_right as f32 + 1.0) * label_w - (idx_from_right as f32) * gap;
    let y = pane.top() + (TOPN_HEADER_H - 18.0) / 2.0;
    Rect::new(x, y, label_w, 18.0)
}

fn paint_topn(
    state: &DiskUsageState,
    pane: Rect,
    renderer: &mut dyn Renderer,
    tokens: &Tokens,
) {
    use crate::disk_usage_state::TopNSort;
    renderer.fill_rect(pane, tokens.bg.layer1);
    renderer.fill_rect(
        Rect::new(pane.left(), pane.top(), pane.size.width, TOPN_HEADER_H),
        tokens.bg.layer2,
    );
    renderer.fill_rect(
        Rect::new(pane.left(), pane.top() + TOPN_HEADER_H - 1.0, pane.size.width, 1.0),
        tokens.border.subtle,
    );
    let header_style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::SemiBold,
        color: tokens.fg.primary,
    };
    let header_label = format!("Top {} largest files", state.topn_files.len().min(50));
    renderer.draw_text(
        Point::new(
            pane.left() + tokens.space.md,
            pane.top() + (TOPN_HEADER_H - tokens.text.sm) / 2.0 - 1.0,
        ),
        &header_label,
        header_style,
    );

    // Sort buttons (Age | Name | Size).
    for (key, label) in [
        (TopNSort::Size, "Size"),
        (TopNSort::Name, "Name"),
        (TopNSort::Age, "Age"),
    ] {
        let r = topn_sort_button_rect(pane, key);
        let active = state.topn_sort == key;
        let (fill, stroke, color) = if active {
            (tokens.accent.subtle, tokens.border.focus, tokens.fg.primary)
        } else {
            (tokens.bg.layer1, tokens.border.subtle, tokens.fg.secondary)
        };
        renderer.fill_rect(r, fill);
        renderer.stroke_rect(r, 1.0, stroke);
        let style = TextStyle {
            size: tokens.text.xs,
            weight: FontWeight::Regular,
            color,
        };
        let metrics = renderer.measure_text(label, style);
        renderer.draw_text(
            Point::new(
                r.left() + (r.size.width - metrics.width) / 2.0,
                r.top() + (r.size.height - tokens.text.xs) / 2.0 - 1.0,
            ),
            label,
            style,
        );
    }

    // Clip rows so scroll + overflow don't bleed into the header or
    // beyond the pane bottom.
    let rows_top = pane.top() + TOPN_HEADER_H;
    let rows_clip = Rect::new(
        pane.left(),
        rows_top,
        pane.size.width,
        (pane.bottom() - rows_top).max(0.0),
    );
    renderer.push_clip(rows_clip);

    let primary_style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::Regular,
        color: tokens.fg.primary,
    };
    let secondary_style = TextStyle {
        size: tokens.text.xs,
        weight: FontWeight::Regular,
        color: tokens.fg.secondary,
    };

    let mut y = rows_top - state.topn_scroll_offset;
    for entry in state.topn_files.iter() {
        if y + TOPN_ROW_H <= rows_clip.top() {
            y += TOPN_ROW_H;
            continue;
        }
        if y >= rows_clip.bottom() {
            break;
        }
        let row_rect = Rect::new(pane.left(), y, pane.size.width, TOPN_ROW_H);

        let is_selected = state.selection.contains(&entry.node_id);
        let is_hovered = state.hovered == Some(entry.node_id);
        let bg = if is_selected {
            tokens.accent.subtle
        } else if is_hovered {
            tokens.bg.layer2
        } else {
            Color::TRANSPARENT
        };
        if bg.a > 0 {
            renderer.fill_rect(row_rect, bg);
        }

        let size_str = humanize_bytes(entry.size_bytes);
        let size_metrics = renderer.measure_text(&size_str, secondary_style);
        renderer.draw_text(
            Point::new(
                row_rect.right() - size_metrics.width - tokens.space.sm,
                y + 6.0,
            ),
            &size_str,
            secondary_style,
        );

        let max_text_w = (row_rect.size.width
            - size_metrics.width
            - tokens.space.md
            - tokens.space.sm * 2.0)
            .max(0.0);
        let name_drawn =
            truncate_with_ellipsis_dyn(&entry.display_name, max_text_w, primary_style, renderer);
        renderer.draw_text(
            Point::new(row_rect.left() + tokens.space.md, y + 4.0),
            &name_drawn,
            primary_style,
        );
        if !entry.parent_display_name.is_empty() {
            let sub = format!("in {}", entry.parent_display_name);
            let sub_drawn = truncate_with_ellipsis_dyn(&sub, max_text_w, secondary_style, renderer);
            renderer.draw_text(
                Point::new(
                    row_rect.left() + tokens.space.md,
                    y + 4.0 + tokens.text.sm + 2.0,
                ),
                &sub_drawn,
                secondary_style,
            );
        }

        y += TOPN_ROW_H;
    }

    renderer.pop_clip();

    if state.topn_files.is_empty() {
        let style = TextStyle {
            size: tokens.text.sm,
            weight: FontWeight::Regular,
            color: tokens.fg.secondary,
        };
        let msg = if state.scan_complete {
            "No files yet."
        } else {
            "Scanning…"
        };
        renderer.draw_text(
            Point::new(pane.left() + tokens.space.md, rows_top + 8.0),
            msg,
            style,
        );
    }
}

fn paint_legend(
    state: &DiskUsageState,
    viewport: Rect,
    renderer: &mut dyn Renderer,
    tokens: &Tokens,
) {
    // Strip backdrop.
    let strip_rect = Rect::new(
        0.0,
        PATH_ROW_H + VOLUME_ROW_H,
        viewport.size.width,
        LEGEND_ROW_H,
    );
    renderer.fill_rect(strip_rect, tokens.bg.layer1);
    renderer.fill_rect(
        Rect::new(0.0, strip_rect.bottom() - 1.0, viewport.size.width, 1.0),
        tokens.border.subtle,
    );

    let label_style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::Regular,
        color: tokens.fg.primary,
    };
    let active_style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::SemiBold,
        color: tokens.fg.on_accent,
    };

    // Slot 0 = All; slots 1..=N for LEGEND_CATEGORIES.
    let total_slots = 1 + LEGEND_CATEGORIES.len();
    for slot in 0..total_slots {
        let chip_filter: Option<feraille_disk_usage::FileCategory> = if slot == 0 {
            None
        } else {
            Some(LEGEND_CATEGORIES[slot - 1])
        };
        let label = legend_chip_label(chip_filter);
        let rect = legend_chip_rect(viewport, slot);
        let active = state.category_filter == chip_filter;

        let (fill, border, style) = if active {
            (tokens.accent.fill, tokens.border.focus, active_style)
        } else {
            (tokens.bg.layer2, tokens.border.subtle, label_style)
        };
        renderer.fill_rect(rect, fill);
        renderer.stroke_rect(rect, 1.0, border);

        // Color swatch for category chips, left of the label.
        if let Some(cat) = chip_filter {
            let swatch = Rect::new(rect.left() + 6.0, rect.top() + 6.0, 8.0, 8.0);
            let tint = category_swatch_color(cat, tokens);
            renderer.fill_rect(swatch, tint);
        }

        let metrics = renderer.measure_text(label, style);
        let label_x = if chip_filter.is_some() {
            rect.left() + 18.0
        } else {
            rect.left() + (rect.size.width - metrics.width) / 2.0
        };
        renderer.draw_text(
            Point::new(label_x, rect.top() + (20.0 - tokens.text.sm) / 2.0 - 1.0),
            label,
            style,
        );
    }
}

/// Mirror of the treemap's category tint, used for the chip swatch
/// so the legend matches the cell colors at a glance.
fn category_swatch_color(c: feraille_disk_usage::FileCategory, tokens: &Tokens) -> Color {
    use feraille_disk_usage::FileCategory;
    match c {
        FileCategory::Image => tokens.magic.image,
        FileCategory::Video | FileCategory::Audio => tokens.magic.media,
        FileCategory::Archive => tokens.magic.archive,
        FileCategory::Document => tokens.magic.doc,
        FileCategory::Executable => tokens.magic.code,
        FileCategory::Other => tokens.magic.data,
    }
}

fn truncate_with_ellipsis_dyn(
    text: &str,
    max_w: f32,
    style: TextStyle,
    renderer: &mut dyn Renderer,
) -> String {
    const ELLIPSIS: &str = "\u{2026}";
    if max_w <= 0.0 {
        return String::new();
    }
    let full_w = renderer.measure_text(text, style).width;
    if full_w <= max_w {
        return text.to_string();
    }
    let ellipsis_w = renderer.measure_text(ELLIPSIS, style).width;
    if ellipsis_w > max_w {
        return String::new();
    }
    let budget = (max_w - ellipsis_w).max(0.0);
    let mut acc = String::new();
    for ch in text.chars() {
        let mut t = acc.clone();
        t.push(ch);
        if renderer.measure_text(&t, style).width > budget {
            break;
        }
        acc.push(ch);
    }
    if acc.is_empty() {
        return String::new();
    }
    acc.push_str(ELLIPSIS);
    acc
}

#[allow(dead_code)] // called via a closure crossing crate boundary; see paint()
fn name_for(
    nodes: &std::collections::HashMap<NodeId, DiskUsageNode>,
    id: NodeId,
) -> String {
    nodes
        .get(&id)
        .map(|n| n.display_name.clone())
        .unwrap_or_default()
}

fn humanize_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[allow(dead_code)]
fn _link_bitmap_export(_: Bitmap) {}

/// Helper accessor exposed for tests.
#[allow(dead_code)]
pub fn topn_files_for_test(state: &DiskUsageState) -> &[TopFileEntry] {
    &state.topn_files
}
