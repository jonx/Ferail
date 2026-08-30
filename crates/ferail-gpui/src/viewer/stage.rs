//! Pure zoom/pan geometry for the viewer window.
//!
//! No gpui types: every function is f32 math over `(w, h)` tuples so
//! the whole sticky-zoom contract is unit-testable and portable
//! (docs/features/VIEWER.md, "stage.rs").
//!
//! The model: a [`StageState`] is `{zoom mode, pan center}` where the
//! pan center is a *fraction of the image* `(0.5, 0.5) = image
//! center`. Keeping pan relative (not in pixels) is what makes zoom
//! sticky across entries of different sizes: "2.5× at the top-right
//! corner" means the same thing for every image in the playlist.

/// How the image is scaled into the viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ZoomMode {
    /// Fit the viewport exactly (aspect preserved): large media scales
    /// down, small media scales *up* to fill the window.
    Fit,
    /// Fit inside the viewport, but never upscale beyond 100 %:
    /// tiny icons render pixel-true instead of as blurry posters.
    FitDown,
    /// 100 %, one image pixel per logical pixel.
    Actual,
    /// Explicit user zoom factor.
    Custom(f32),
}

/// Zoom factor clamp for [`zoom_at`].
pub const MIN_SCALE: f32 = 0.05;
pub const MAX_SCALE: f32 = 32.0;

/// Sticky view state. Lives on the viewer window, *not* per image:
/// navigation keeps it verbatim, which is the feature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StageState {
    pub mode: ZoomMode,
    /// Image point shown at the viewport center, as a fraction of the
    /// image dimensions.
    pub center: (f32, f32),
}

impl Default for StageState {
    fn default() -> Self {
        Self {
            mode: ZoomMode::Fit,
            center: (0.5, 0.5),
        }
    }
}

impl StageState {
    /// Centered state in the given mode: what "reset zoom" returns to.
    pub fn reset(mode: ZoomMode) -> Self {
        Self {
            mode,
            center: (0.5, 0.5),
        }
    }
}

/// On-screen placement of the image inside the viewport, in viewport
/// coordinates (origin top-left).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StageRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Scale that fits `img` to `view` exactly (up- or downscaling), aspect
/// preserved. Not clamped to [`MIN_SCALE`]/[`MAX_SCALE`]: those bound
/// *user* zoom steps; a fit is whatever the window demands.
pub fn fit_scale(img: (f32, f32), view: (f32, f32)) -> f32 {
    if img.0 <= 0.0 || img.1 <= 0.0 {
        return 1.0;
    }
    (view.0 / img.0).min(view.1 / img.1)
}

/// Scale that fits `img` inside `view` without ever upscaling.
pub fn fit_down_scale(img: (f32, f32), view: (f32, f32)) -> f32 {
    fit_scale(img, view).min(1.0)
}

pub fn effective_scale(mode: ZoomMode, img: (f32, f32), view: (f32, f32)) -> f32 {
    match mode {
        ZoomMode::Fit => fit_scale(img, view),
        ZoomMode::FitDown => fit_down_scale(img, view),
        ZoomMode::Actual => 1.0,
        ZoomMode::Custom(s) => s.clamp(MIN_SCALE, MAX_SCALE),
    }
}

/// Per-axis placement: position the image so the `center` fraction
/// sits at the viewport midpoint, then clamp so the image never
/// detaches from the viewport edge; images smaller than the viewport
/// center on that axis.
fn place_axis(drawn: f32, view: f32, center_frac: f32) -> f32 {
    if drawn <= view {
        (view - drawn) / 2.0
    } else {
        (view / 2.0 - center_frac * drawn).clamp(view - drawn, 0.0)
    }
}

/// Compute the on-screen rect for the image under `state`.
pub fn layout(img: (f32, f32), view: (f32, f32), state: StageState) -> StageRect {
    let s = effective_scale(state.mode, img, view);
    let (dw, dh) = (img.0 * s, img.1 * s);
    StageRect {
        x: place_axis(dw, view.0, state.center.0),
        y: place_axis(dh, view.1, state.center.1),
        w: dw,
        h: dh,
    }
}

/// Re-derive the canonical (clamped) center fraction from an on-screen
/// position, per axis. Inverse of [`place_axis`].
fn center_from_pos(pos: f32, drawn: f32, view: f32) -> f32 {
    if drawn <= view {
        0.5
    } else {
        let clamped = pos.clamp(view - drawn, 0.0);
        (view / 2.0 - clamped) / drawn
    }
}

/// Zoom by `factor` keeping the image point under `cursor` (viewport
/// coordinates) fixed on screen. Switches the mode to `Custom`.
pub fn zoom_at(
    state: StageState,
    cursor: (f32, f32),
    img: (f32, f32),
    view: (f32, f32),
    factor: f32,
) -> StageState {
    let s0 = effective_scale(state.mode, img, view);
    let s1 = (s0 * factor).clamp(MIN_SCALE, MAX_SCALE);
    let r0 = layout(img, view, state);
    // Image fraction currently under the cursor.
    let fx = ((cursor.0 - r0.x) / r0.w).clamp(0.0, 1.0);
    let fy = ((cursor.1 - r0.y) / r0.h).clamp(0.0, 1.0);
    let (dw, dh) = (img.0 * s1, img.1 * s1);
    // Position that keeps (fx, fy) under the cursor, then canonicalize.
    let x1 = cursor.0 - fx * dw;
    let y1 = cursor.1 - fy * dh;
    StageState {
        mode: ZoomMode::Custom(s1),
        center: (
            center_from_pos(x1, dw, view.0),
            center_from_pos(y1, dh, view.1),
        ),
    }
}

/// Pan by a pixel delta (drag gesture: the image follows the cursor).
pub fn pan_by(
    state: StageState,
    delta: (f32, f32),
    img: (f32, f32),
    view: (f32, f32),
) -> StageState {
    let r = layout(img, view, state);
    StageState {
        mode: state.mode,
        center: (
            center_from_pos(r.x + delta.0, r.w, view.0),
            center_from_pos(r.y + delta.1, r.h, view.1),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: (f32, f32) = (1000.0, 750.0);

    #[test]
    fn fit_down_never_upscales() {
        assert_eq!(fit_down_scale((100.0, 100.0), VIEW), 1.0);
        let r = layout((100.0, 100.0), VIEW, StageState::reset(ZoomMode::FitDown));
        assert_eq!((r.x, r.y, r.w, r.h), (450.0, 325.0, 100.0, 100.0));
    }

    #[test]
    fn fit_upscales_small_images_to_the_window() {
        assert_eq!(fit_scale((100.0, 100.0), VIEW), 7.5);
        let r = layout((100.0, 100.0), VIEW, StageState::default());
        assert_eq!((r.x, r.y, r.w, r.h), (125.0, 0.0, 750.0, 750.0));
        // Large media fits down identically in both fit modes.
        assert_eq!(fit_scale((4000.0, 3000.0), VIEW), 0.25);
    }

    #[test]
    fn fit_down_scales_large_images() {
        assert_eq!(fit_down_scale((4000.0, 3000.0), VIEW), 0.25);
        let r = layout((4000.0, 3000.0), VIEW, StageState::default());
        assert_eq!((r.w, r.h), (1000.0, 750.0));
        assert_eq!((r.x, r.y), (0.0, 0.0));
    }

    #[test]
    fn actual_and_custom_scales() {
        assert_eq!(effective_scale(ZoomMode::Actual, (9.0, 9.0), VIEW), 1.0);
        assert_eq!(
            effective_scale(ZoomMode::Custom(2.5), (9.0, 9.0), VIEW),
            2.5
        );
        // Custom clamps to the global range.
        assert_eq!(
            effective_scale(ZoomMode::Custom(1000.0), (9.0, 9.0), VIEW),
            MAX_SCALE
        );
    }

    #[test]
    fn zoom_at_keeps_cursor_point_fixed() {
        let img = (4000.0, 3000.0);
        let cursor = (700.0, 200.0);
        let s0 = StageState::default();
        let r0 = layout(img, VIEW, s0);
        let frac_before = ((cursor.0 - r0.x) / r0.w, (cursor.1 - r0.y) / r0.h);

        let s1 = zoom_at(s0, cursor, img, VIEW, 2.0);
        let r1 = layout(img, VIEW, s1);
        let frac_after = ((cursor.0 - r1.x) / r1.w, (cursor.1 - r1.y) / r1.h);

        assert!((frac_before.0 - frac_after.0).abs() < 1e-4);
        assert!((frac_before.1 - frac_after.1).abs() < 1e-4);
        assert_eq!(s1.mode, ZoomMode::Custom(0.5));
    }

    #[test]
    fn zoom_clamps_scale() {
        let img = (100.0, 100.0);
        let mut st = StageState::default();
        for _ in 0..20 {
            st = zoom_at(st, (500.0, 375.0), img, VIEW, 10.0);
        }
        assert_eq!(st.mode, ZoomMode::Custom(MAX_SCALE));
        for _ in 0..40 {
            st = zoom_at(st, (500.0, 375.0), img, VIEW, 0.1);
        }
        assert_eq!(st.mode, ZoomMode::Custom(MIN_SCALE));
    }

    #[test]
    fn pan_clamps_to_edges() {
        let img = (4000.0, 3000.0);
        let st = StageState {
            mode: ZoomMode::Actual,
            center: (0.5, 0.5),
        };
        // Drag way past the left edge: image right edge must stay
        // glued to the viewport right edge (no gap).
        let dragged = pan_by(st, (-10_000.0, -10_000.0), img, VIEW);
        let r = layout(img, VIEW, dragged);
        assert_eq!(r.x, VIEW.0 - r.w);
        assert_eq!(r.y, VIEW.1 - r.h);
        // And the other way.
        let dragged = pan_by(st, (10_000.0, 10_000.0), img, VIEW);
        let r = layout(img, VIEW, dragged);
        assert_eq!((r.x, r.y), (0.0, 0.0));
    }

    #[test]
    fn pan_is_inert_when_image_fits() {
        let img = (100.0, 100.0);
        let st = pan_by(
            StageState::reset(ZoomMode::FitDown),
            (300.0, 300.0),
            img,
            VIEW,
        );
        assert_eq!(st.center, (0.5, 0.5));
        let r = layout(img, VIEW, st);
        assert_eq!((r.x, r.y), (450.0, 325.0));
    }

    #[test]
    fn sticky_center_transfers_between_image_sizes() {
        // Zoom into the top-right quadrant of a landscape image…
        let img_a = (4000.0, 3000.0);
        let mut st = zoom_at(StageState::default(), (900.0, 100.0), img_a, VIEW, 8.0);
        st = pan_by(st, (-50.0, 30.0), img_a, VIEW);
        let r_a = layout(img_a, VIEW, st);
        let frac_a = (
            (VIEW.0 / 2.0 - r_a.x) / r_a.w,
            (VIEW.1 / 2.0 - r_a.y) / r_a.h,
        );
        // …then "navigate" to a different-sized image with the SAME
        // state: the viewport must look at the same relative region.
        let img_b = (2000.0, 2600.0);
        let r_b = layout(img_b, VIEW, st);
        let frac_b = (
            (VIEW.0 / 2.0 - r_b.x) / r_b.w,
            (VIEW.1 / 2.0 - r_b.y) / r_b.h,
        );
        assert!((frac_a.0 - frac_b.0).abs() < 1e-4);
        assert!((frac_a.1 - frac_b.1).abs() < 1e-4);
    }

    #[test]
    fn degenerate_image_dims_do_not_nan() {
        let r = layout((0.0, 0.0), VIEW, StageState::default());
        assert!(r.x.is_finite() && r.y.is_finite());
        assert_eq!(fit_down_scale((0.0, 10.0), VIEW), 1.0);
    }
}
