//! Built-in image redaction / annotation editor
//! (docs/features/IMAGE_EDITOR.md).
//!
//! One standalone window per image, deliberately tiny: two modes (Redact
//! (opaque black; Annotate) coloured), two tools (rectangle, brush), undo,
//! Cmd+S saves an "edited" copy beside the original, Cmd+Shift+S overwrites
//! the original after a confirmation. Not a paint program: no zoom, no
//! layers, no filters.
//!
//! Pixel discipline: the window keeps only a bounded display-resolution
//! copy of the image in memory. Every interactive repaint composites the
//! strokes over that copy off-thread (single-flight, latest-wins, using the
//! `schedule_process` shape from the viewer); the full-resolution image is
//! re-decoded from the file only inside the save worker, so a 50-megapixel
//! photo never rides the UI thread and is never persisted at preview
//! quality. Strokes are stored in full-image coordinates and scale
//! losslessly between the preview and the save.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable as _, Root, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    notification::Notification,
    v_flex,
};
use image::{Frame, RgbaImage};
use smallvec::SmallVec;

use crate::shell::Shell;
use crate::text::TextScale as _;
use crate::viewer::stage::{self, StageState, ZoomMode};

/// Key-binding context for the editor window. Bound in
/// `keymap::install_extras`: Cmd+S copy-save, Cmd+Shift+S overwrite,
/// Cmd+Z undo, Esc / Cmd+W close through the unsaved-edits guard.
pub const IMAGE_EDITOR_CONTEXT: &str = "ImageEditor";

actions!(
    image_edit,
    [
        ImageSaveCopy,
        ImageOverwrite,
        ImageUndo,
        ImageEditorDismiss,
        ImageZoomIn,
        ImageZoomOut,
        ImageZoomReset,
        ImageRevealFile,
    ]
);

/// Zoom step per gesture, matching the viewer's feel.
const ZOOM_STEP: f32 = 1.25;

fn command_tooltip(label: SharedString, mac: &str, other: &str) -> SharedString {
    format!(
        "{label} ({})",
        if cfg!(target_os = "macos") {
            mac
        } else {
            other
        }
    )
    .into()
}

/// Formats the editor accepts: decodable AND re-encodable by the bundled
/// `image` crate. GIF is deliberately out (re-encoding would silently drop
/// animation); HEIC/AVIF/RAW have no encoder here.
const EDITABLE_EXTS: &[&str] = &["png", "jpg", "jpeg", "jpe", "bmp", "tif", "tiff", "webp"];

/// Refuse images past this many pixels: a full-res RGBA buffer is 4 B/px
/// in the save worker, so 64 MP ≈ 256 MB peak, the ceiling of "fast and
/// simple".
const MAX_EDIT_PIXELS: u64 = 64_000_000;

/// Longest edge of the display-resolution preview copy.
const DISP_MAX_EDGE: u32 = 2048;

/// Annotation palette (red first = default).
const ANNOTATE_COLORS: &[[u8; 3]] = &[
    [0xE5, 0x3E, 0x3E],
    [0xF6, 0x8B, 0x1F],
    [0xF7, 0xD1, 0x1E],
    [0x2F, 0xB3, 0x44],
    [0x2B, 0x6C, 0xE5],
    [0xFF, 0xFF, 0xFF],
    [0x00, 0x00, 0x00],
];

/// Brush radii as a fraction of the image's short edge (S / M / L).
const BRUSH_FRACTIONS: &[f32] = &[0.008, 0.018, 0.035];

/// Can the built-in image editor open a file with this display name?
/// Extension-only, so it is safe to call at context-menu build time.
pub fn editable_image_name(name: &str) -> bool {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    EDITABLE_EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

/// Number of image-editor windows open. Drives the shared spiral cascade.
static OPEN_IMAGE_EDITOR_WINDOWS: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct CascadeGuard {
    slot: usize,
}

impl CascadeGuard {
    fn claim() -> Self {
        let slot = OPEN_IMAGE_EDITOR_WINDOWS.fetch_add(1, Ordering::Relaxed);
        CascadeGuard { slot }
    }
}

impl Drop for CascadeGuard {
    fn drop(&mut self) {
        OPEN_IMAGE_EDITOR_WINDOWS.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Rect,
    Brush,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Redact,
    Annotate,
}

/// One committed edit, in full-image pixel coordinates.
#[derive(Clone, Debug, PartialEq)]
enum Stroke {
    Rect {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: [u8; 3],
        /// Redact = filled; Annotate = outline of `width` px.
        fill: bool,
        width: f32,
    },
    Brush {
        points: Vec<(f32, f32)>,
        radius: f32,
        color: [u8; 3],
    },
}

/// Bounded display-resolution copy of the decoded image (RGBA).
#[derive(Clone)]
struct DispBase {
    rgba: Arc<Vec<u8>>,
    w: u32,
    h: u32,
}

enum LoadState {
    Loading,
    Ready,
    /// Refused on purpose (too large / undecodable); the file is untouched.
    Refused(SharedString),
    Failed(SharedString),
}

enum LoadOutcome {
    Ready {
        full_w: u32,
        full_h: u32,
        disp: DispBase,
    },
    TooLarge {
        w: u32,
        h: u32,
    },
    Undecodable,
    Failed(String),
}

/// Open a standalone image-editor window for `path`.
pub fn open(
    path: PathBuf,
    name: String,
    shell: WeakEntity<Shell>,
    origin_tab: crate::shell::TabId,
    cx: &mut App,
) {
    open_impl(path, name, Some(shell), Some(origin_tab), false, cx);
}

fn open_impl(
    path: PathBuf,
    name: String,
    shell: Option<WeakEntity<Shell>>,
    origin_tab: Option<crate::shell::TabId>,
    demo_on_load: bool,
    cx: &mut App,
) {
    let cascade = CascadeGuard::claim();
    let title: SharedString = tr!(
        "Edit: {name}",
        name = crate::private_mode::present_leaf_str(&name, false)
    );
    let window_size = size(px(980.0), px(720.0));
    let opts = WindowOptions {
        window_bounds: Some(crate::window_cascade::cascaded_bounds(
            cascade.slot,
            window_size,
            cx,
        )),
        titlebar: Some(TitlebarOptions {
            title: Some(title.clone()),
            ..Default::default()
        }),
        ..crate::base_window_options()
    };
    let menu_label = title.to_string();
    let handle = cx.open_window(opts, move |window, cx| {
        crate::boot::install_dev_window_callback_cleanup(window, cx);
        let view = cx.new(|cx| {
            ImageEditView::new(
                path,
                name,
                shell,
                origin_tab,
                Some(cascade),
                demo_on_load,
                window,
                cx,
            )
        });
        // OS close button honours the unsaved-edits guard; programmatic
        // `remove_window` bypasses it (Discard relies on that).
        let weak = view.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |view, cx| view.platform_should_close(window, cx))
                .unwrap_or(true)
        });
        let target_window = window.window_handle();
        let escape_view = view.downgrade();
        let escape_subscription = cx.intercept_keystrokes(move |event, window, app| {
            if event.keystroke.key != "escape"
                || window.window_handle() != target_window
                || window.has_active_dialog(app)
            {
                return;
            }
            let _ = escape_view.update(app, |view, cx| view.request_dismiss(window, cx));
            app.stop_propagation();
        });
        view.update(cx, |view, _| {
            view._escape_subscription = Some(escape_subscription)
        });
        cx.new(|cx| Root::new(view, window, cx))
    });
    if let Ok(handle) = handle {
        crate::process_state::process_state(cx).register_aux_window(handle.into(), menu_label);
        crate::boot::refresh_window_menu(cx);
    }
}

pub struct ImageEditView {
    path: PathBuf,
    name: String,
    load: LoadState,
    full_dims: (u32, u32),
    disp: Option<DispBase>,
    strokes: Vec<Stroke>,
    /// Stroke being drawn (mouse held). Brush strokes composite live;
    /// rectangles preview as an overlay element and composite on release.
    live: Option<Stroke>,
    /// Bumped on every stroke mutation; drives compositing and dirtiness.
    rev: u64,
    saved_rev: u64,
    tool: Tool,
    mode: Mode,
    color_ix: usize,
    size_ix: usize,
    composited: Option<Arc<RenderImage>>,
    composited_rev: u64,
    compose_inflight: bool,
    /// Stage bounds in window coordinates, captured by a canvas probe each
    /// frame: pure cached geometry, no I/O.
    stage_bounds: Bounds<Pixels>,
    /// Zoom / pan of the image on the stage, shared model with the viewer
    /// (`viewer::stage`). Fit-down by default so a small image is not
    /// blown up; the toolbar and Cmd+= / Cmd+- drive it from there.
    stage: StageState,
    /// Pan drag in progress: last cursor position, window coordinates.
    panning: Option<Point<Pixels>>,
    saving: bool,
    allow_close: bool,
    did_focus: bool,
    last_title: String,
    focus_handle: FocusHandle,
    shell: Option<WeakEntity<Shell>>,
    origin_tab: Option<crate::shell::TabId>,
    demo_on_load: bool,
    _escape_subscription: Option<Subscription>,
    _cascade: Option<CascadeGuard>,
}

impl ImageEditView {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        path: PathBuf,
        name: String,
        shell: Option<WeakEntity<Shell>>,
        origin_tab: Option<crate::shell::TabId>,
        cascade: Option<CascadeGuard>,
        demo_on_load: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let handle = window.window_handle();
        let load_path = path.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { load_for_edit(&load_path) })
                .await;
            let _ = handle.update(cx, |_, _, cx| {
                let _ = this.update(cx, |view: &mut Self, cx| {
                    view.apply_load(outcome, cx);
                });
            });
        })
        .detach();

        Self {
            path,
            name,
            load: LoadState::Loading,
            full_dims: (0, 0),
            disp: None,
            strokes: Vec::new(),
            live: None,
            rev: 0,
            saved_rev: 0,
            tool: Tool::Rect,
            mode: Mode::Redact,
            color_ix: 0,
            size_ix: 1,
            composited: None,
            composited_rev: u64::MAX,
            compose_inflight: false,
            stage_bounds: Bounds::default(),
            stage: StageState::reset(ZoomMode::FitDown),
            panning: None,
            saving: false,
            allow_close: false,
            did_focus: false,
            last_title: String::new(),
            focus_handle: cx.focus_handle(),
            shell,
            origin_tab,
            demo_on_load,
            _escape_subscription: None,
            _cascade: cascade,
        }
    }

    fn dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    fn apply_load(&mut self, outcome: LoadOutcome, cx: &mut Context<Self>) {
        match outcome {
            LoadOutcome::Ready {
                full_w,
                full_h,
                disp,
            } => {
                self.full_dims = (full_w, full_h);
                self.disp = Some(disp);
                self.load = LoadState::Ready;
                if self.demo_on_load {
                    self.demo_strokes();
                }
                self.schedule_compose(cx);
            }
            LoadOutcome::TooLarge { w, h } => {
                self.load = LoadState::Refused(tr!(
                    "This image is {w}×{h}, too large for the built-in editor.",
                    w = w,
                    h = h
                ));
            }
            LoadOutcome::Undecodable => {
                self.load = LoadState::Refused(tr!(
                    "This file can't be decoded as an image, so it can't be edited here."
                ));
            }
            LoadOutcome::Failed(error) => {
                self.load =
                    LoadState::Failed(tr!("Could not read the image: {error}", error = error));
            }
        }
        cx.notify();
    }

    /// Screenshot-only: one redaction bar and one annotation ellipse-ish
    /// brush arc, sized relative to the image.
    fn demo_strokes(&mut self) {
        let (w, h) = (self.full_dims.0 as f32, self.full_dims.1 as f32);
        self.strokes.push(Stroke::Rect {
            x0: w * 0.08,
            y0: h * 0.12,
            x1: w * 0.42,
            y1: h * 0.22,
            color: [0, 0, 0],
            fill: true,
            width: 0.0,
        });
        self.strokes.push(Stroke::Rect {
            x0: w * 0.5,
            y0: h * 0.4,
            x1: w * 0.85,
            y1: h * 0.7,
            color: ANNOTATE_COLORS[0],
            fill: false,
            width: self.outline_width(),
        });
        let r = self.brush_radius();
        let points = (0..=24)
            .map(|i| {
                let t = i as f32 / 24.0 * std::f32::consts::PI;
                (
                    w * (0.15 + 0.2 * t.cos().abs()),
                    h * (0.82 - 0.06 * t.sin()),
                )
            })
            .collect();
        self.strokes.push(Stroke::Brush {
            points,
            radius: r,
            color: ANNOTATE_COLORS[4],
        });
        self.rev += 3;
    }

    fn brush_radius(&self) -> f32 {
        let min_dim = self.full_dims.0.min(self.full_dims.1) as f32;
        (min_dim * BRUSH_FRACTIONS[self.size_ix.min(BRUSH_FRACTIONS.len() - 1)]).max(3.0)
    }

    fn outline_width(&self) -> f32 {
        let min_dim = self.full_dims.0.min(self.full_dims.1) as f32;
        (min_dim * 0.006).max(3.0)
    }

    fn stroke_color(&self) -> [u8; 3] {
        match self.mode {
            Mode::Redact => [0, 0, 0],
            Mode::Annotate => ANNOTATE_COLORS[self.color_ix.min(ANNOTATE_COLORS.len() - 1)],
        }
    }

    /// Fit rect of the preview inside the current stage bounds, in
    /// stage-local coordinates.
    fn stage_rect(&self) -> Option<stage::StageRect> {
        let disp = self.disp.as_ref()?;
        let view = (
            self.stage_bounds.size.width.as_f32(),
            self.stage_bounds.size.height.as_f32(),
        );
        if view.0 <= 1.0 || view.1 <= 1.0 {
            return None;
        }
        Some(stage::layout(
            (disp.w as f32, disp.h as f32),
            view,
            self.stage,
        ))
    }

    /// Window position → full-image pixel coordinates (clamped).
    fn image_point(&self, pos: Point<Pixels>) -> Option<(f32, f32)> {
        let r = self.stage_rect()?;
        let local = (
            pos.x.as_f32() - self.stage_bounds.origin.x.as_f32(),
            pos.y.as_f32() - self.stage_bounds.origin.y.as_f32(),
        );
        if r.w <= 0.0 || r.h <= 0.0 {
            return None;
        }
        let fx = ((local.0 - r.x) / r.w).clamp(0.0, 1.0);
        let fy = ((local.1 - r.y) / r.h).clamp(0.0, 1.0);
        Some((fx * self.full_dims.0 as f32, fy * self.full_dims.1 as f32))
    }

    fn on_stage_down(&mut self, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.load, LoadState::Ready) || self.live.is_some() {
            return;
        }
        let Some((x, y)) = self.image_point(e.position) else {
            return;
        };
        let color = self.stroke_color();
        self.live = Some(match self.tool {
            Tool::Rect => Stroke::Rect {
                x0: x,
                y0: y,
                x1: x,
                y1: y,
                color,
                fill: self.mode == Mode::Redact,
                width: self.outline_width(),
            },
            Tool::Brush => Stroke::Brush {
                points: vec![(x, y)],
                radius: self.brush_radius(),
                color,
            },
        });
        if matches!(self.tool, Tool::Brush) {
            self.rev += 1;
            self.schedule_compose(cx);
        }
        cx.notify();
    }

    fn on_stage_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if e.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some((x, y)) = self.image_point(e.position) else {
            return;
        };
        match &mut self.live {
            Some(Stroke::Rect { x1, y1, .. }) => {
                *x1 = x;
                *y1 = y;
                cx.notify();
            }
            // Guard skips sub-pixel jitter; image coords, so 2px is tiny.
            Some(Stroke::Brush { points, .. })
                if points
                    .last()
                    .map(|(px, py)| (px - x).abs() + (py - y).abs() > 2.0)
                    .unwrap_or(true) =>
            {
                points.push((x, y));
                self.rev += 1;
                self.schedule_compose(cx);
                cx.notify();
            }
            Some(Stroke::Brush { .. }) | None => {}
        }
    }

    fn on_stage_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(stroke) = self.live.take() else {
            return;
        };
        // A no-drag click paints nothing: drop degenerate rectangles
        // (< 2 image px on either side); a single-point brush tap is a
        // legitimate dot and commits.
        if let Stroke::Rect { x0, y0, x1, y1, .. } = &stroke
            && ((x0 - x1).abs() < 2.0 || (y0 - y1).abs() < 2.0)
        {
            self.rev += 1; // brush live may have bumped; converge compose
            self.schedule_compose(cx);
            cx.notify();
            return;
        }
        self.strokes.push(stroke);
        self.rev += 1;
        self.schedule_compose(cx);
        cx.notify();
    }

    /// Right-button (or space-less middle) drag pans the zoomed image;
    /// the left button always draws, so panning never steals a stroke.
    fn on_pan_down(&mut self, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.panning = Some(e.position);
        cx.notify();
    }

    fn on_pan_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(last) = self.panning else {
            return;
        };
        if e.pressed_button != Some(MouseButton::Right) {
            self.panning = None;
            return;
        }
        let (Some(disp), Some(_)) = (self.disp.as_ref(), self.stage_rect()) else {
            return;
        };
        let delta = (
            (e.position.x - last.x).as_f32(),
            (e.position.y - last.y).as_f32(),
        );
        let view = (
            self.stage_bounds.size.width.as_f32(),
            self.stage_bounds.size.height.as_f32(),
        );
        self.stage = stage::pan_by(self.stage, delta, (disp.w as f32, disp.h as f32), view);
        self.panning = Some(e.position);
        cx.notify();
    }

    fn on_pan_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.panning.take().is_some() {
            cx.notify();
        }
    }

    /// Wheel zooms toward the cursor, the viewer's convention (the stage
    /// has nothing to scroll).
    fn on_stage_scroll(
        &mut self,
        e: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(disp) = self.disp.clone() else {
            return;
        };
        let dy = e.delta.pixel_delta(window.line_height()).y.as_f32();
        if dy.abs() < 0.01 {
            return;
        }
        let factor = (dy / 180.0).exp();
        let cursor = (
            e.position.x.as_f32() - self.stage_bounds.origin.x.as_f32(),
            e.position.y.as_f32() - self.stage_bounds.origin.y.as_f32(),
        );
        let view = (
            self.stage_bounds.size.width.as_f32(),
            self.stage_bounds.size.height.as_f32(),
        );
        self.stage = stage::zoom_at(
            self.stage,
            cursor,
            (disp.w as f32, disp.h as f32),
            view,
            factor,
        );
        cx.notify();
    }

    fn zoom_by(&mut self, factor: f32, cx: &mut Context<Self>) {
        let Some(disp) = self.disp.clone() else {
            return;
        };
        let view = (
            self.stage_bounds.size.width.as_f32(),
            self.stage_bounds.size.height.as_f32(),
        );
        // Anchor at the stage centre for button/keyboard zoom.
        let cursor = (view.0 / 2.0, view.1 / 2.0);
        self.stage = stage::zoom_at(
            self.stage,
            cursor,
            (disp.w as f32, disp.h as f32),
            view,
            factor,
        );
        cx.notify();
    }

    fn on_zoom_in(&mut self, _: &ImageZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(ZOOM_STEP, cx);
    }

    fn on_zoom_out(&mut self, _: &ImageZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(1.0 / ZOOM_STEP, cx);
    }

    fn on_zoom_reset(&mut self, _: &ImageZoomReset, _: &mut Window, cx: &mut Context<Self>) {
        self.stage = StageState::reset(ZoomMode::FitDown);
        cx.notify();
    }

    /// Current zoom as a percentage of the image's own pixels.
    fn zoom_percent(&self) -> Option<f32> {
        let disp = self.disp.as_ref()?;
        let view = (
            self.stage_bounds.size.width.as_f32(),
            self.stage_bounds.size.height.as_f32(),
        );
        if view.0 <= 1.0 {
            return None;
        }
        // `disp` may be a downscale of the original, so convert the stage
        // scale back to full-image pixels before showing it.
        let disp_scale =
            stage::effective_scale(self.stage.mode, (disp.w as f32, disp.h as f32), view);
        let full_ratio = disp.w as f32 / self.full_dims.0.max(1) as f32;
        Some(disp_scale * full_ratio * 100.0)
    }

    /// Select this image back in a Ferail browsing window so its folder,
    /// siblings and Get Info are one click away.
    fn on_reveal_file(&mut self, _: &ImageRevealFile, _: &mut Window, cx: &mut Context<Self>) {
        if let (Some(shell), Some(tab_id)) = (&self.shell, self.origin_tab) {
            crate::shell::reselect_path_in_origin(cx, shell, tab_id, self.path.clone());
        } else {
            crate::shell::reveal_path_in_app(cx, self.path.clone());
        }
    }

    fn on_undo(&mut self, _: &ImageUndo, _: &mut Window, cx: &mut Context<Self>) {
        if self.strokes.pop().is_some() {
            self.rev += 1;
            self.schedule_compose(cx);
            cx.notify();
        }
    }

    /// Off-thread preview compositing: single-flight, latest-wins,
    /// reconverges on completion (the viewer's `schedule_process` shape).
    fn schedule_compose(&mut self, cx: &mut Context<Self>) {
        if self.compose_inflight {
            return;
        }
        let Some(disp) = self.disp.clone() else {
            return;
        };
        let rev = self.rev;
        if self.composited_rev == rev && self.composited.is_some() {
            return;
        }
        self.compose_inflight = true;
        let mut strokes = self.strokes.clone();
        if let Some(live @ Stroke::Brush { .. }) = &self.live {
            strokes.push(live.clone());
        }
        let scale = disp.w as f32 / (self.full_dims.0.max(1)) as f32;
        cx.spawn(async move |this, cx| {
            let img = cx
                .background_executor()
                .spawn(async move {
                    let mut buf = (*disp.rgba).clone();
                    apply_strokes(&mut buf, disp.w, disp.h, &strokes, scale);
                    Arc::new(build_render_image(buf, disp.w, disp.h))
                })
                .await;
            let _ = this.update(cx, |view: &mut Self, cx| {
                view.compose_inflight = false;
                view.composited = Some(img);
                view.composited_rev = rev;
                if view.rev != rev {
                    view.schedule_compose(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn on_save_copy(&mut self, _: &ImageSaveCopy, window: &mut Window, cx: &mut Context<Self>) {
        self.save(false, false, window, cx);
    }

    fn on_overwrite(&mut self, _: &ImageOverwrite, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || !matches!(self.load, LoadState::Ready) || self.strokes.is_empty() {
            return;
        }
        if window.has_active_dialog(cx) {
            return;
        }
        let weak = cx.weak_entity();
        let name = crate::private_mode::present_leaf_str(&self.name, false);
        window.open_dialog(cx, move |dialog, _, _| {
            let weak = weak.clone();
            dialog
                .title(tr!("Overwrite Image?"))
                .child(div().text_scale_sm().child(tr!(
                    "This replaces \u{201C}{name}\u{201D} on disk with the edited image.",
                    name = name.clone()
                )))
                .button_props(
                    gpui_component::dialog::DialogButtonProps::default().ok_text(tr!("Overwrite")),
                )
                .on_ok(move |_, window, cx| {
                    let _ = weak.update(cx, |view, cx| view.save(true, false, window, cx));
                    true
                })
        });
    }

    /// Render full-res + encode + write, off-thread. `overwrite` rewrites
    /// the original in place (backup-sibling first); otherwise an
    /// "<stem> <edited>[ n].<ext>" copy lands beside it.
    fn save(
        &mut self,
        overwrite: bool,
        close_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.saving || !matches!(self.load, LoadState::Ready) || self.strokes.is_empty() {
            return;
        }
        self.saving = true;
        cx.notify();
        let rev = self.rev;
        let strokes = self.strokes.clone();
        let path = self.path.clone();
        // The copy suffix is user-visible text; resolve it on the UI thread.
        let suffix = tr!("edited").to_string();
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { save_edited(&path, &strokes, overwrite, &suffix) })
                .await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |view: &mut Self, cx| {
                    view.saving = false;
                    match result {
                        Ok(written) => {
                            view.saved_rev = rev;
                            if !overwrite && let Some(leaf) = written.file_name() {
                                window.push_notification(
                                    Notification::info(tr!(
                                        "Saved {name}",
                                        name = crate::private_mode::present_leaf_str(
                                            &leaf.to_string_lossy(),
                                            false
                                        )
                                    )),
                                    cx,
                                );
                            }
                            if let (Some(shell), Some(dir)) =
                                (view.shell.as_ref(), view.path.parent())
                            {
                                let dir = dir.to_path_buf();
                                let _ = shell.update(cx, |shell, cx| {
                                    shell.reload_tabs_matching_paths(&[dir], cx);
                                });
                            }
                            if close_after && !view.dirty() {
                                view.allow_close = true;
                                window.remove_window();
                                return;
                            }
                        }
                        Err(error) => {
                            window.push_notification(
                                Notification::error(tr!(
                                    "Could not save {name}: {error}",
                                    name = crate::private_mode::present_leaf_str(&view.name, false),
                                    error = error
                                )),
                                cx,
                            );
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn on_dismiss(&mut self, _: &ImageEditorDismiss, window: &mut Window, cx: &mut Context<Self>) {
        self.request_dismiss(window, cx);
    }

    fn request_dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty() && !self.allow_close {
            self.prompt_unsaved(window, cx);
        } else {
            window.remove_window();
        }
    }

    fn platform_should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.allow_close || !self.dirty() {
            return true;
        }
        self.prompt_unsaved(window, cx);
        false
    }

    fn prompt_unsaved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let weak = cx.weak_entity();
        let name = crate::private_mode::present_leaf_str(&self.name, false);
        window.open_dialog(cx, move |dialog, _, _| {
            let weak_save = weak.clone();
            let weak_discard = weak.clone();
            dialog
                .title(tr!("Unsaved Changes"))
                .child(div().text_scale_sm().child(tr!(
                    "Your edits to \u{201C}{name}\u{201D} haven't been saved.",
                    name = name.clone()
                )))
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("image-discard")
                                .label(tr!("Discard"))
                                .small()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = weak_discard.update(cx, |view, _| {
                                        view.allow_close = true;
                                    });
                                    window.remove_window();
                                }),
                        )
                        .child(
                            Button::new("image-close-cancel")
                                .label(tr!("Cancel"))
                                .small()
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("image-save-copy-close")
                                .label(tr!("Save Copy"))
                                .primary()
                                .small()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = weak_save.update(cx, |view, cx| {
                                        view.save(false, true, window, cx);
                                    });
                                }),
                        ),
                )
        });
    }

    /// Icon toolbar under the title bar. Three clusters: what to draw
    /// with (mode + tool + its options), how to look at it (zoom), and
    /// what to do with the result (undo / locate / save / overwrite).
    /// Every button carries a tooltip because none of them show a label.
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ready = matches!(self.load, LoadState::Ready);
        let has_edits = !self.strokes.is_empty();
        let zoom_label = self
            .zoom_percent()
            .map(|p| format!("{p:.0}%"))
            .unwrap_or_else(|| "\u{2026}".to_string());
        let sep = |cx: &mut Context<Self>| div().w(px(1.0)).h(px(20.0)).bg(cx.theme().border);

        let mut bar = h_flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(cx.theme().border)
            // Mode: blacking out versus marking up.
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("mode-redact")
                            .icon(gpui_component::Icon::empty().path("icons/eraser.svg"))
                            .small()
                            .tooltip(tr!("Redact"))
                            .selected(self.mode == Mode::Redact)
                            .disabled(!ready)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.mode = Mode::Redact;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("mode-annotate")
                            .icon(gpui_component::Icon::empty().path("icons/pencil.svg"))
                            .small()
                            .tooltip(tr!("Annotate"))
                            .selected(self.mode == Mode::Annotate)
                            .disabled(!ready)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.mode = Mode::Annotate;
                                cx.notify();
                            })),
                    ),
            )
            // Tool: rectangle versus freehand.
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("tool-rect")
                            .icon(gpui_component::Icon::empty().path("icons/square.svg"))
                            .small()
                            .tooltip(tr!("Rectangle"))
                            .selected(self.tool == Tool::Rect)
                            .disabled(!ready)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.tool = Tool::Rect;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("tool-brush")
                            .icon(gpui_component::Icon::empty().path("icons/brush.svg"))
                            .small()
                            .tooltip(tr!("Brush"))
                            .selected(self.tool == Tool::Brush)
                            .disabled(!ready)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.tool = Tool::Brush;
                                cx.notify();
                            })),
                    ),
            );
        // Colour only means something while annotating; redaction is
        // always opaque black.
        if self.mode == Mode::Annotate {
            let mut swatches = h_flex().gap_1();
            for (ix, rgbv) in ANNOTATE_COLORS.iter().enumerate() {
                let selected = ix == self.color_ix;
                let color =
                    gpui::rgb(((rgbv[0] as u32) << 16) | ((rgbv[1] as u32) << 8) | rgbv[2] as u32);
                swatches = swatches.child(
                    div()
                        .id(("annotate-color", ix))
                        .size_4()
                        .rounded_full()
                        .bg(color)
                        .border_2()
                        .border_color(if selected {
                            cx.theme().ring
                        } else {
                            cx.theme().border
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.color_ix = ix;
                            cx.notify();
                        })),
                );
            }
            bar = bar.child(sep(cx)).child(swatches);
        }
        // Brush width, as three dots that show the actual relative size.
        if self.tool == Tool::Brush {
            let mut sizes = h_flex().gap_1().items_center();
            for (ix, dot) in [px(5.0), px(8.0), px(12.0)].into_iter().enumerate() {
                let selected = self.size_ix == ix;
                sizes = sizes.child(
                    div()
                        .id(("brush-size", ix))
                        .size(px(20.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .cursor_pointer()
                        .when(selected, |d| d.bg(cx.theme().secondary_active))
                        .child(div().size(dot).rounded_full().bg(if selected {
                            cx.theme().foreground
                        } else {
                            cx.theme().muted_foreground
                        }))
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.size_ix = ix;
                            cx.notify();
                        })),
                );
            }
            bar = bar.child(sep(cx)).child(sizes);
        }

        bar.child(sep(cx))
            // Zoom cluster, same shape and feel as the viewer's.
            .child(
                Button::new("image-zoom-out")
                    .icon(gpui_component::Icon::empty().path("icons/minus.svg"))
                    .small()
                    .tooltip(command_tooltip(tr!("Zoom Out"), "⌘−", "Ctrl+−"))
                    .disabled(!ready)
                    .on_click(cx.listener(|view, _, _, cx| view.zoom_by(1.0 / ZOOM_STEP, cx))),
            )
            .child(
                div()
                    .id("image-zoom-reset")
                    .text_scale_xs()
                    .text_color(cx.theme().muted_foreground)
                    .min_w(px(48.0))
                    .text_center()
                    .cursor_pointer()
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(command_tooltip(
                            tr!("Fit to Window"),
                            "⌘0",
                            "Ctrl+0",
                        ))
                        .build(window, cx)
                    })
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.on_zoom_reset(&ImageZoomReset, window, cx)
                    }))
                    .child(zoom_label),
            )
            .child(
                Button::new("image-zoom-in")
                    .icon(gpui_component::Icon::empty().path("icons/plus.svg"))
                    .small()
                    .tooltip(command_tooltip(tr!("Zoom In"), "⌘+", "Ctrl++"))
                    .disabled(!ready)
                    .on_click(cx.listener(|view, _, _, cx| view.zoom_by(ZOOM_STEP, cx))),
            )
            .child(div().flex_1())
            .when(self.dirty(), |bar| {
                bar.child(
                    div()
                        .text_scale_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("Unsaved")),
                )
            })
            .child(
                Button::new("image-undo")
                    .icon(gpui_component::Icon::empty().path("icons/undo.svg"))
                    .small()
                    .tooltip(command_tooltip(tr!("Undo"), "⌘Z", "Ctrl+Z"))
                    .disabled(!ready || !has_edits)
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.on_undo(&ImageUndo, window, cx);
                    })),
            )
            .child(
                Button::new("image-reveal")
                    .icon(gpui_component::Icon::empty().path("icons/locate-fixed.svg"))
                    .small()
                    .tooltip(command_tooltip(tr!("Show in Ferail"), "⌘R", "Ctrl+R"))
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.on_reveal_file(&ImageRevealFile, window, cx)
                    })),
            )
            .child(sep(cx))
            .child(
                Button::new("image-save-copy")
                    .icon(gpui_component::Icon::empty().path("icons/save.svg"))
                    .primary()
                    .small()
                    .tooltip(command_tooltip(tr!("Save Copy"), "⌘S", "Ctrl+S"))
                    .disabled(!ready || !has_edits || self.saving)
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.save(false, false, window, cx);
                    })),
            )
            .child(
                Button::new("image-overwrite")
                    .icon(gpui_component::Icon::empty().path("icons/replace.svg"))
                    .small()
                    .tooltip(command_tooltip(
                        tr!("Overwrite\u{2026}"),
                        "⌘⇧S",
                        "Ctrl+Shift+S",
                    ))
                    .disabled(!ready || !has_edits || self.saving)
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.on_overwrite(&ImageOverwrite, window, cx);
                    })),
            )
    }

    /// The live rectangle preview overlay, in stage-local coordinates.
    fn live_rect_overlay(&self) -> Option<Div> {
        let Some(Stroke::Rect {
            x0,
            y0,
            x1,
            y1,
            color,
            fill,
            ..
        }) = &self.live
        else {
            return None;
        };
        let r = self.stage_rect()?;
        let (fw, fh) = (self.full_dims.0 as f32, self.full_dims.1 as f32);
        if fw <= 0.0 || fh <= 0.0 {
            return None;
        }
        let sx = |v: f32| r.x + v / fw * r.w;
        let sy = |v: f32| r.y + v / fh * r.h;
        let (l, t) = (sx(x0.min(*x1)), sy(y0.min(*y1)));
        let (w, h) = ((x1 - x0).abs() / fw * r.w, (y1 - y0).abs() / fh * r.h);
        let c = gpui::rgb(((color[0] as u32) << 16) | ((color[1] as u32) << 8) | color[2] as u32);
        let mut d = div().absolute().left(px(l)).top(px(t)).w(px(w)).h(px(h));
        if *fill {
            d = d.bg(c);
        } else {
            d = d.border_2().border_color(c);
        }
        Some(d)
    }
}

impl Focusable for ImageEditView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ImageEditView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let private = crate::private_mode::enabled();
        if !private {
            let base_title = tr!("Edit: {name}", name = self.name.clone());
            let title = if self.dirty() {
                format!("\u{2022} {base_title}")
            } else {
                base_title.to_string()
            };
            if title != self.last_title {
                window.set_window_title(&title);
                self.last_title = title;
            }
        } else {
            self.last_title.clear();
        }
        if !self.did_focus {
            self.did_focus = true;
            window.focus(&self.focus_handle, cx);
        }
        let muted = cx.theme().muted_foreground;

        let stage: AnyElement = if private {
            // Fail-closed: the pixels are user content (same stance as the
            // viewer); keep chrome, blank the stage.
            div().flex_1().into_any_element()
        } else {
            match &self.load {
                LoadState::Loading => v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_scale_sm()
                            .text_color(muted)
                            .child(tr!("Loading…")),
                    )
                    .into_any_element(),
                LoadState::Ready => {
                    let entity = cx.entity();
                    let mut stage = div()
                        .id("image-edit-stage")
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .child(
                            canvas(
                                move |bounds, _, cx| {
                                    entity.update(cx, |view, _| view.stage_bounds = bounds)
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .size_full(),
                        )
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::on_stage_down))
                        .on_mouse_move(cx.listener(Self::on_stage_move))
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::on_stage_up))
                        .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_stage_up))
                        // Right-drag pans, wheel zooms: the left button is
                        // reserved for drawing.
                        .on_mouse_down(MouseButton::Right, cx.listener(Self::on_pan_down))
                        .on_mouse_move(cx.listener(Self::on_pan_move))
                        .on_mouse_up(MouseButton::Right, cx.listener(Self::on_pan_up))
                        .on_mouse_up_out(MouseButton::Right, cx.listener(Self::on_pan_up))
                        .on_scroll_wheel(cx.listener(Self::on_stage_scroll));
                    if let (Some(img), Some(r)) = (self.composited.clone(), self.stage_rect()) {
                        stage = stage.child(
                            gpui::img(img)
                                .absolute()
                                .left(px(r.x))
                                .top(px(r.y))
                                .w(px(r.w))
                                .h(px(r.h)),
                        );
                    }
                    if let Some(overlay) = self.live_rect_overlay() {
                        stage = stage.child(overlay);
                    }
                    stage.into_any_element()
                }
                LoadState::Refused(msg) | LoadState::Failed(msg) => v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_scale_sm()
                            .text_color(muted)
                            .max_w(px(420.))
                            .text_center()
                            .child(msg.clone()),
                    )
                    .into_any_element(),
            }
        };

        let content = v_flex()
            .key_context(IMAGE_EDITOR_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_save_copy))
            .on_action(cx.listener(Self::on_overwrite))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_dismiss))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_zoom_reset))
            .on_action(cx.listener(Self::on_reveal_file))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .when(!private, |this| this.child(self.toolbar(cx)))
            .child(stage)
            .children(Root::render_dialog_layer(window, cx))
            .when(!private, |this| {
                this.children(Root::render_notification_layer(window, cx))
            })
            .into_any_element();
        crate::private_mode::protect(content, cx)
    }
}

// ---------------------------------------------------------------------------
// Pure pixel workers: blocking, background executor only.
// ---------------------------------------------------------------------------

fn load_for_edit(path: &Path) -> LoadOutcome {
    ferail_core::path_guard::assert_off_ui_thread("image_edit load");
    let img = match image::open(path) {
        Ok(i) => i,
        Err(image::ImageError::IoError(e)) => return LoadOutcome::Failed(e.to_string()),
        Err(_) => return LoadOutcome::Undecodable,
    };
    let (w, h) = (img.width(), img.height());
    if (w as u64) * (h as u64) > MAX_EDIT_PIXELS {
        return LoadOutcome::TooLarge { w, h };
    }
    let disp_img = if w.max(h) > DISP_MAX_EDGE {
        img.resize(
            DISP_MAX_EDGE,
            DISP_MAX_EDGE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    let (dw, dh) = (disp_img.width(), disp_img.height());
    let rgba = disp_img.into_rgba8().into_raw();
    LoadOutcome::Ready {
        full_w: w,
        full_h: h,
        disp: DispBase {
            rgba: Arc::new(rgba),
            w: dw,
            h: dh,
        },
    }
}

/// Twin of `preview::build_render_image`: RGBA → BGRA in place, single
/// frame.
fn build_render_image(mut rgba: Vec<u8>, w: u32, h: u32) -> RenderImage {
    for pxl in rgba.chunks_exact_mut(4) {
        pxl.swap(0, 2);
    }
    let buf = RgbaImage::from_raw(w, h, rgba).expect("rgba dims match");
    RenderImage::new(SmallVec::from_elem(Frame::new(buf), 1))
}

/// Apply `strokes` (full-image coordinates, scaled by `scale`) onto an
/// RGBA buffer of `w`×`h`.
fn apply_strokes(buf: &mut [u8], w: u32, h: u32, strokes: &[Stroke], scale: f32) {
    for stroke in strokes {
        match stroke {
            Stroke::Rect {
                x0,
                y0,
                x1,
                y1,
                color,
                fill,
                width,
            } => {
                let (l, r) = ((x0.min(*x1) * scale), (x0.max(*x1) * scale));
                let (t, b) = ((y0.min(*y1) * scale), (y0.max(*y1) * scale));
                if *fill {
                    fill_rect(buf, w, h, (l, t, r, b), *color);
                } else {
                    let bw = (width * scale).max(1.5);
                    fill_rect(buf, w, h, (l, t, r, t + bw), *color);
                    fill_rect(buf, w, h, (l, b - bw, r, b), *color);
                    fill_rect(buf, w, h, (l, t, l + bw, b), *color);
                    fill_rect(buf, w, h, (r - bw, t, r, b), *color);
                }
            }
            Stroke::Brush {
                points,
                radius,
                color,
            } => {
                let rad = (radius * scale).max(1.0);
                let mut prev: Option<(f32, f32)> = None;
                for &(px_, py_) in points {
                    let p = (px_ * scale, py_ * scale);
                    match prev {
                        None => stamp_disc(buf, w, h, p.0, p.1, rad, *color),
                        Some(q) => {
                            let dist = ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt();
                            let steps = (dist / (rad * 0.5)).ceil().max(1.0) as u32;
                            for i in 1..=steps {
                                let t = i as f32 / steps as f32;
                                stamp_disc(
                                    buf,
                                    w,
                                    h,
                                    q.0 + (p.0 - q.0) * t,
                                    q.1 + (p.1 - q.1) * t,
                                    rad,
                                    *color,
                                );
                            }
                        }
                    }
                    prev = Some(p);
                }
            }
        }
    }
}

fn fill_rect(buf: &mut [u8], w: u32, h: u32, (l, t, r, b): (f32, f32, f32, f32), color: [u8; 3]) {
    let l = (l.floor().max(0.0) as u32).min(w);
    let r = (r.ceil().max(0.0) as u32).min(w);
    let t = (t.floor().max(0.0) as u32).min(h);
    let b = (b.ceil().max(0.0) as u32).min(h);
    for y in t..b {
        let row = (y as usize * w as usize + l as usize) * 4;
        for x in 0..(r - l) as usize {
            let i = row + x * 4;
            buf[i] = color[0];
            buf[i + 1] = color[1];
            buf[i + 2] = color[2];
            buf[i + 3] = 0xFF;
        }
    }
}

fn stamp_disc(buf: &mut [u8], w: u32, h: u32, cx_: f32, cy: f32, radius: f32, color: [u8; 3]) {
    let l = ((cx_ - radius).floor().max(0.0) as u32).min(w);
    let r = ((cx_ + radius).ceil().max(0.0) as u32).min(w);
    let t = ((cy - radius).floor().max(0.0) as u32).min(h);
    let b = ((cy + radius).ceil().max(0.0) as u32).min(h);
    let r2 = radius * radius;
    for y in t..b {
        for x in l..r {
            let dx = x as f32 + 0.5 - cx_;
            let dy = y as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= r2 {
                let i = (y as usize * w as usize + x as usize) * 4;
                buf[i] = color[0];
                buf[i + 1] = color[1];
                buf[i + 2] = color[2];
                buf[i + 3] = 0xFF;
            }
        }
    }
}

/// Decode the original at full resolution, apply the strokes, encode in the
/// file's own format, and write. Returns the path actually written.
fn save_edited(
    path: &Path,
    strokes: &[Stroke],
    overwrite: bool,
    copy_suffix: &str,
) -> Result<PathBuf, String> {
    ferail_core::path_guard::assert_off_ui_thread("image_edit save");
    let img = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
    let (w, h) = (img.width(), img.height());
    let mut buf = img.into_raw();
    apply_strokes(&mut buf, w, h, strokes, 1.0);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_else(|| "png".to_string());
    let bytes = encode_image(&buf, w, h, &ext)?;
    if overwrite {
        return match crate::safe_write::write_bytes_in_place(path, &bytes, "image-edit") {
            Ok(()) => Ok(path.to_path_buf()),
            Err(fail) => Err(match fail.backup {
                Some(backup) => tr!(
                    "{error}. The edited image was preserved in {backup}.",
                    error = fail.error,
                    backup = backup.display()
                )
                .to_string(),
                None => fail.error,
            }),
        };
    }
    // "<stem> <edited>[ n].<ext>" beside the original; `create_new` makes
    // the claim race-safe against a concurrent writer picking the same name.
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".to_string());
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    for n in 1..100u32 {
        let leaf = if n == 1 {
            format!("{stem} {copy_suffix}.{ext}")
        } else {
            format!("{stem} {copy_suffix} {n}.{ext}")
        };
        let target = dir.join(&leaf);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(mut f) => {
                use std::io::Write as _;
                f.write_all(&bytes).map_err(|e| e.to_string())?;
                return Ok(target);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err(tr!("Too many edited copies of this image already exist here.").to_string())
}

fn encode_image(rgba: &[u8], w: u32, h: u32, ext: &str) -> Result<Vec<u8>, String> {
    use image::ImageEncoder as _;
    let mut out = Vec::new();
    match ext {
        "jpg" | "jpeg" | "jpe" => {
            // JPEG has no alpha channel; flatten first.
            let rgb: Vec<u8> = rgba
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
                .write_image(&rgb, w, h, image::ExtendedColorType::Rgb8)
                .map_err(|e| e.to_string())?;
        }
        _ => {
            let format = image::ImageFormat::from_extension(ext).unwrap_or(image::ImageFormat::Png);
            let img = RgbaImage::from_raw(w, h, rgba.to_vec())
                .ok_or_else(|| "buffer size mismatch".to_string())?;
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut std::io::Cursor::new(&mut out), format)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    // No `use super::*`: `gpui::*`'s `test` attribute macro would shadow
    // the built-in `#[test]`.
    use super::{Stroke, apply_strokes, editable_image_name, encode_image, save_edited};

    fn px_at(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * w as usize + x as usize) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn rect_fill_and_outline_paint_the_right_pixels() {
        let (w, h) = (100u32, 80u32);
        let mut buf = vec![0x40u8; (w * h * 4) as usize];
        apply_strokes(
            &mut buf,
            w,
            h,
            &[Stroke::Rect {
                x0: 10.0,
                y0: 10.0,
                x1: 30.0,
                y1: 20.0,
                color: [0, 0, 0],
                fill: true,
                width: 0.0,
            }],
            1.0,
        );
        assert_eq!(px_at(&buf, w, 15, 15), [0, 0, 0, 0xFF]);
        assert_eq!(px_at(&buf, w, 50, 50), [0x40, 0x40, 0x40, 0x40]);

        // Outline: edge painted, interior untouched.
        apply_strokes(
            &mut buf,
            w,
            h,
            &[Stroke::Rect {
                x0: 40.0,
                y0: 40.0,
                x1: 80.0,
                y1: 70.0,
                color: [255, 0, 0],
                fill: false,
                width: 3.0,
            }],
            1.0,
        );
        assert_eq!(px_at(&buf, w, 41, 41), [255, 0, 0, 0xFF]);
        assert_eq!(px_at(&buf, w, 60, 55), [0x40, 0x40, 0x40, 0x40]);
    }

    #[test]
    fn brush_stamps_a_continuous_stroke_and_scales() {
        let (w, h) = (60u32, 60u32);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        // Two distant points: the interpolator must fill the gap.
        apply_strokes(
            &mut buf,
            w,
            h,
            &[Stroke::Brush {
                points: vec![(10.0, 30.0), (50.0, 30.0)],
                radius: 4.0,
                color: [0, 255, 0],
            }],
            1.0,
        );
        for x in [10u32, 30, 50] {
            assert_eq!(px_at(&buf, w, x, 30), [0, 255, 0, 0xFF], "x={x}");
        }
        // Half-scale compositing lands at half coordinates.
        let mut small = vec![0u8; (30 * 30 * 4) as usize];
        apply_strokes(
            &mut small,
            30,
            30,
            &[Stroke::Brush {
                points: vec![(10.0, 30.0), (50.0, 30.0)],
                radius: 4.0,
                color: [0, 255, 0],
            }],
            0.5,
        );
        assert_eq!(px_at(&small, 30, 15, 15), [0, 255, 0, 0xFF]);
    }

    #[test]
    fn save_copy_names_and_redacts() {
        let dir = std::env::temp_dir().join(format!("ferail-imgedit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("photo.png");
        let img = image::RgbaImage::from_pixel(40, 40, image::Rgba([200, 200, 200, 255]));
        img.save(&src).unwrap();

        let strokes = [Stroke::Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 40.0,
            y1: 40.0,
            color: [0, 0, 0],
            fill: true,
            width: 0.0,
        }];
        let first = save_edited(&src, &strokes, false, "edited").unwrap();
        assert_eq!(
            first.file_name().unwrap().to_str().unwrap(),
            "photo edited.png"
        );
        let second = save_edited(&src, &strokes, false, "edited").unwrap();
        assert_eq!(
            second.file_name().unwrap().to_str().unwrap(),
            "photo edited 2.png"
        );
        // The copy is fully redacted; the original is untouched.
        let saved = image::open(&first).unwrap().to_rgba8();
        assert_eq!(saved.get_pixel(20, 20).0, [0, 0, 0, 255]);
        let original = image::open(&src).unwrap().to_rgba8();
        assert_eq!(original.get_pixel(20, 20).0, [200, 200, 200, 255]);

        // Overwrite rewrites the original in place, leaving no temp sibling.
        save_edited(&src, &strokes, true, "edited").unwrap();
        let replaced = image::open(&src).unwrap().to_rgba8();
        assert_eq!(replaced.get_pixel(20, 20).0, [0, 0, 0, 255]);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn jpeg_encode_flattens_alpha_and_extension_gate_works() {
        let rgba = vec![255u8; 4 * 4 * 4];
        assert!(encode_image(&rgba, 4, 4, "jpg").is_ok());
        assert!(encode_image(&rgba, 4, 4, "png").is_ok());
        assert!(editable_image_name("a.PNG"));
        assert!(editable_image_name("photo.jpeg"));
        assert!(!editable_image_name("clip.gif"));
        assert!(!editable_image_name("doc.txt"));
        assert!(!editable_image_name("noext"));
    }
}
