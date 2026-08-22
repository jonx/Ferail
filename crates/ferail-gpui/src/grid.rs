//! Icon (grid) view for the file list.
//!
//! A second view mode alongside the table. The grid is a *renderer*
//! over the same per-tab data and selection model the table uses — it
//! is not a `TableDelegate`. It reads the live `FileListDelegate`
//! (`entries`, `selected_set`, `lead`, thumbnail cache) and routes
//! every gesture through the same `Shell` methods the table's
//! `TableEvent`s do, so selection / keyboard / drag / context-menu
//! behaviour stays identical across views.
//!
//! Rendering + warming live in `crate::shell::render` (the `Shell` owns
//! the entity context the grid needs); this module owns the view-mode /
//! icon-size state, persistence, and the small layout helpers.

use gpui::{App, ObjectFit};

use crate::app_state;

/// Key context for the icon grid. Arrow keys are bound here (more
/// specific than `SHELL_CONTEXT`) so the grid's 2-D navigation
/// overrides the table's 1-D `Cursor*` bindings when the grid is
/// focused.
pub const GRID_CONTEXT: &str = "FerailGrid";

/// Which renderer the file pane uses for a tab. Per-tab interaction
/// state (Finder-style per-folder), seeded from a persisted global
/// default on tab creation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    /// The virtualized table (Name / Size / Format / Modified columns).
    List,
    /// The Finder-style icon grid.
    Grid,
}

impl ViewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ViewMode::List => "list",
            ViewMode::Grid => "grid",
        }
    }

    // Intentional inherent method (infallible token parse), not `std::str::FromStr`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "grid" => ViewMode::Grid,
            _ => ViewMode::List,
        }
    }

    /// The persisted default applied to newly opened tabs.
    pub fn persisted_default() -> Self {
        Self::from_str(app_state::load().view_mode.as_deref().unwrap_or("list"))
    }
}

// =============================================================================
// Icon size
// =============================================================================

/// Smallest / largest grid icon size any control may reach (logical px,
/// longest edge of the thumbnail slot). The toolbar slider is
/// continuous across this whole range — these two bound it.
pub const MIN_ICON_SIZE: u32 = 32;
pub const MAX_ICON_SIZE: u32 = 512;

/// Display sizes the −/＋ stepper snaps to, Small → Large. Only the
/// buttons use these; the slider picks any size in between. The list
/// spans the full [`MIN_ICON_SIZE`]..=[`MAX_ICON_SIZE`] range so ＋ from
/// an off-stop size (which the slider makes common) can still climb to
/// the top instead of snapping back down.
pub const ICON_SIZES: &[u32] = &[32, 48, 64, 96, 128, 192, 256, 384, 512];

/// Default grid icon size when the user hasn't chosen one.
pub const DEFAULT_ICON_SIZE: u32 = 128;

/// Smallest icon size at which the crowding-prone per-cell adornments
/// (Finder tag dots + favorite star) are painted. At the 64px stop a
/// 12px star and a row of dots swamp the thumbnail, so we drop them
/// there and let the quarantine badge / heat tint (which read clearly
/// at any size) carry on alone — Finder hides the same chrome on its
/// smallest icons.
pub const ADORN_MIN_ICON: u32 = 96;

/// Extra width/height a cell spends beyond the thumbnail slot on its
/// label, padding, and selection inset.
pub const CELL_LABEL_H: f32 = 34.0;
pub const CELL_PAD: f32 = 30.0;

/// Default uniform inset (px) between a grid cell's footprint and its
/// highlighted (selection fill/border) box, when the user hasn't chosen
/// one. The gutter is *added* to the `cell_width`/`cell_height` stride
/// (see [`cell_width`]), so the icon + label area is invariant to the
/// gap and two adjacent selected cells show a `2 * gap` gutter between
/// their fills.
pub const DEFAULT_CELL_GAP: f32 = 4.0;

/// Largest gap the setting allows (px). Bounds how much the gutter can
/// eat into column density.
pub const MAX_CELL_GAP: f32 = 16.0;

/// Clamp an arbitrary gap into the supported range.
pub fn clamp_cell_gap(v: f32) -> f32 {
    v.clamp(0.0, MAX_CELL_GAP)
}

/// Process-wide live grid cell gap (px). Seeded from persisted settings
/// at startup and updated by the Settings dropdown; the grid reads it
/// during render so a change re-lays-out immediately (same mechanism as
/// [`IconSize`]).
#[derive(Clone, Copy)]
pub struct CellGap(pub f32);

impl gpui::Global for CellGap {}

/// The live grid cell gap, defaulting to [`DEFAULT_CELL_GAP`].
pub fn cell_gap(cx: &App) -> f32 {
    cx.try_global::<CellGap>()
        .map(|g| clamp_cell_gap(g.0))
        .unwrap_or(DEFAULT_CELL_GAP)
}

/// Process-wide live grid icon size (display px). Seeded from persisted
/// settings at startup and updated by the toolbar size control; reading
/// it during render subscribes the window to changes, so dragging the
/// control re-lays-out the grid immediately (same mechanism as the
/// theme / [`crate::thumbnails::ShowThumbnails`]).
#[derive(Clone, Copy)]
pub struct IconSize(pub u32);

impl gpui::Global for IconSize {}

/// The live grid icon size, defaulting to [`DEFAULT_ICON_SIZE`].
pub fn icon_size(cx: &App) -> u32 {
    cx.try_global::<IconSize>()
        .map(|g| clamp_icon_size(g.0))
        .unwrap_or(DEFAULT_ICON_SIZE)
}

/// Clamp an arbitrary value into the supported range.
pub fn clamp_icon_size(px: u32) -> u32 {
    px.clamp(MIN_ICON_SIZE, MAX_ICON_SIZE)
}

/// Full cell width/height for a given icon size and gap: the thumbnail
/// slot plus label + padding, then `2 * gap` for the selection gutter on
/// each axis. Adding the gutter to the stride (rather than insetting it
/// out of a fixed cell) keeps the icon + label area — and so the label's
/// clearance from the rounded selection border — constant as the gap
/// changes. Used to derive `cols_per_row` from pane width.
pub fn cell_width(icon_px: u32, gap: f32) -> f32 {
    icon_px as f32 + CELL_PAD + 2.0 * gap
}

pub fn cell_height(icon_px: u32, gap: f32) -> f32 {
    icon_px as f32 + CELL_LABEL_H + 2.0 * gap
}

/// Columns that fit across `pane_width` at `icon_px` / `gap`, at least 1.
pub fn cols_per_row(pane_width: f32, icon_px: u32, gap: f32) -> usize {
    ((pane_width / cell_width(icon_px, gap)).floor() as usize).max(1)
}

// =============================================================================
// Thumbnail fit
// =============================================================================

/// How a thumbnail is laid into the square icon slot in grid view.
///
/// Only *real* thumbnails obey this — photos, video poster frames, PDF
/// first pages. Folder icons and file-type glyphs are square by
/// construction, so every mode would look identical on them; they always
/// draw as [`ThumbFit::Best`].
///
/// The slot is always square, so the two axis-locked modes are not extra
/// *looks* so much as extra *rules*: [`ThumbFit::Width`] means "always
/// match the width", which letterboxes a landscape image and crops a
/// portrait one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ThumbFit {
    /// Scale to fit entirely inside the slot, letterboxing the short
    /// axis. Nothing is cropped. The default, and what the grid did
    /// before the setting existed.
    #[default]
    Best,
    /// Scale until the slot is completely covered, cropping the overflow.
    /// No letterboxing — the image fills the icon.
    Fill,
    /// Scale so the image's *width* matches the slot.
    Width,
    /// Scale so the image's *height* matches the slot.
    Height,
    /// Stretch both axes to fill the slot exactly, distorting the aspect
    /// ratio.
    Stretch,
}

impl ThumbFit {
    pub fn as_str(self) -> &'static str {
        match self {
            ThumbFit::Best => "best",
            ThumbFit::Fill => "fill",
            ThumbFit::Width => "width",
            ThumbFit::Height => "height",
            ThumbFit::Stretch => "stretch",
        }
    }

    // Intentional inherent method (infallible token parse), not `std::str::FromStr`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "fill" => ThumbFit::Fill,
            "width" => ThumbFit::Width,
            "height" => ThumbFit::Height,
            "stretch" => ThumbFit::Stretch,
            _ => ThumbFit::Best,
        }
    }

    /// Whether this mode can scale an image *beyond* what [`ThumbFit::Best`]
    /// would — i.e. whether it needs a chunkier source bucket to stay
    /// crisp. Only Best is purely shrink-to-fit; every other mode
    /// magnifies at least some images past that point.
    pub fn magnifies(self) -> bool {
        !matches!(self, ThumbFit::Best)
    }

    /// The gpui object-fit that realises this mode for an image of the
    /// given pixel dimensions.
    ///
    /// The square slot is what collapses the five modes onto gpui's three:
    /// scaling by width is the *smaller* scale factor for a landscape
    /// image (so, `Contain`) and the *larger* one for a portrait image
    /// (so, `Cover`). Letting gpui do the cropping keeps the painted
    /// element exactly slot-sized — a panorama in `Fill` never lays out a
    /// 20,000-px-wide element that then has to be clipped.
    pub fn object_fit(self, img_w: u32, img_h: u32) -> ObjectFit {
        // Degenerate sizes (a frame that reported 0) have no orientation
        // to key off; fitting inside is the safe answer.
        if img_w == 0 || img_h == 0 {
            return ObjectFit::Contain;
        }
        match self {
            ThumbFit::Best => ObjectFit::Contain,
            ThumbFit::Fill => ObjectFit::Cover,
            ThumbFit::Stretch => ObjectFit::Fill,
            ThumbFit::Width => {
                if img_w >= img_h {
                    ObjectFit::Contain
                } else {
                    ObjectFit::Cover
                }
            }
            ThumbFit::Height => {
                if img_h >= img_w {
                    ObjectFit::Contain
                } else {
                    ObjectFit::Cover
                }
            }
        }
    }
}

/// Process-wide live thumbnail fit mode. Seeded from persisted settings
/// at startup and updated by the Settings dropdown; the grid reads it
/// during render so a change re-draws immediately (same mechanism as
/// [`IconSize`] and [`CellGap`]).
#[derive(Clone, Copy)]
pub struct ThumbFitMode(pub ThumbFit);

impl gpui::Global for ThumbFitMode {}

/// The live thumbnail fit mode, defaulting to [`ThumbFit::Best`].
pub fn thumb_fit(cx: &App) -> ThumbFit {
    cx.try_global::<ThumbFitMode>()
        .map(|f| f.0)
        .unwrap_or_default()
}

/// The bare fetch-size ladder for a display size, before any fit-mode
/// adjustment. Snapped to a small bucket set so the size control can't
/// explode the path-keyed cache — the bucketed image scales to the
/// exact display size at paint time. ~2× display for retina crispness.
fn bucket_ladder(display_px: u32) -> u32 {
    match display_px {
        0..=96 => 128,
        97..=160 => 256,
        _ => 512,
    }
}

/// Physical thumbnail fetch size (longest edge) for a grid display size
/// under a given fit mode.
///
/// [`ThumbFit::Best`] only ever scales a thumbnail *down* into the slot,
/// so the plain ladder is enough. Every other mode magnifies further —
/// a covering mode scales until the *short* edge fills the slot, so a
/// 16:9 photo is drawn about 1.8× larger than Best fit draws it — and
/// at the same bucket that reads as visibly softer. Those modes step up
/// one rung where there is one.
///
/// The step deliberately reuses buckets that already exist rather than
/// adding a rung above 512: a 1024-px tier would cost ~4 MB per cached
/// entry against a 512-entry LRU, and the softness it buys back only
/// shows at the very largest icon sizes.
pub fn thumb_bucket(display_px: u32, fit: ThumbFit) -> u32 {
    let base = bucket_ladder(display_px);
    if !fit.magnifies() {
        return base;
    }
    match base {
        128 => 256,
        256 => 512,
        other => other,
    }
}

/// Physical fetch size for grid *folder* icons (NSWorkspace bitmaps).
/// Folder art is square and has little detail, so it ignores the fit
/// mode and stays capped — at 256 for ordinary sizes, lifted to 512 only
/// once the slot is genuinely bigger than that, where a 2× upscale of a
/// 256-px bitmap starts to look mushy. The bitmaps are per-path, so the
/// cap is what bounds their memory.
pub fn folder_icon_bucket(display_px: u32) -> u32 {
    if display_px > 256 {
        512
    } else {
        bucket_ladder(display_px).min(256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANDSCAPE: (u32, u32) = (1600, 900);
    const PORTRAIT: (u32, u32) = (900, 1600);
    const SQUARE: (u32, u32) = (512, 512);

    /// `gpui::ObjectFit` implements neither `PartialEq` nor `Debug`, so
    /// the assertions below compare a local token rather than the enum.
    fn fit(mode: ThumbFit, (w, h): (u32, u32)) -> &'static str {
        match mode.object_fit(w, h) {
            ObjectFit::Fill => "fill",
            ObjectFit::Contain => "contain",
            ObjectFit::Cover => "cover",
            ObjectFit::ScaleDown => "scale-down",
            ObjectFit::None => "none",
        }
    }

    #[test]
    fn best_fit_never_crops_either_orientation() {
        assert_eq!(fit(ThumbFit::Best, LANDSCAPE), "contain");
        assert_eq!(fit(ThumbFit::Best, PORTRAIT), "contain");
    }

    #[test]
    fn fill_always_covers_the_slot() {
        assert_eq!(fit(ThumbFit::Fill, LANDSCAPE), "cover");
        assert_eq!(fit(ThumbFit::Fill, PORTRAIT), "cover");
    }

    // The axis-locked modes are the interesting pair: on a square slot
    // "match the width" letterboxes a wide image but crops a tall one.
    #[test]
    fn fit_width_letterboxes_landscape_and_crops_portrait() {
        assert_eq!(fit(ThumbFit::Width, LANDSCAPE), "contain");
        assert_eq!(fit(ThumbFit::Width, PORTRAIT), "cover");
    }

    #[test]
    fn fit_height_letterboxes_portrait_and_crops_landscape() {
        assert_eq!(fit(ThumbFit::Height, PORTRAIT), "contain");
        assert_eq!(fit(ThumbFit::Height, LANDSCAPE), "cover");
    }

    #[test]
    fn a_square_image_is_untouched_by_every_mode_but_stretch() {
        // Contain and Cover agree exactly when the ratios match, so
        // whichever one a mode picks, a square image fills the square slot
        // edge-to-edge with nothing cropped.
        for mode in [
            ThumbFit::Best,
            ThumbFit::Fill,
            ThumbFit::Width,
            ThumbFit::Height,
        ] {
            assert!(
                matches!(fit(mode, SQUARE), "contain" | "cover"),
                "{mode:?} distorted a square image"
            );
        }
        assert_eq!(fit(ThumbFit::Stretch, SQUARE), "fill");
    }

    #[test]
    fn a_zero_sized_frame_falls_back_to_fitting_inside() {
        // A frame that reports no dimensions has no orientation to key
        // off; every mode must still paint something sane.
        for mode in [
            ThumbFit::Best,
            ThumbFit::Fill,
            ThumbFit::Width,
            ThumbFit::Height,
            ThumbFit::Stretch,
        ] {
            assert_eq!(fit(mode, (0, 0)), "contain");
            assert_eq!(fit(mode, (1024, 0)), "contain");
        }
    }

    #[test]
    fn only_best_fit_is_pure_shrink_to_fit() {
        assert!(!ThumbFit::Best.magnifies());
        for mode in [
            ThumbFit::Fill,
            ThumbFit::Width,
            ThumbFit::Height,
            ThumbFit::Stretch,
        ] {
            assert!(mode.magnifies(), "{mode:?} should ask for a bigger bucket");
        }
    }

    #[test]
    fn magnifying_modes_step_up_one_bucket_where_there_is_one() {
        assert_eq!(thumb_bucket(64, ThumbFit::Best), 128);
        assert_eq!(thumb_bucket(64, ThumbFit::Fill), 256);
        assert_eq!(thumb_bucket(128, ThumbFit::Best), 256);
        assert_eq!(thumb_bucket(128, ThumbFit::Fill), 512);
        // Already at the top rung: no 1024-px tier, so both agree.
        assert_eq!(thumb_bucket(256, ThumbFit::Best), 512);
        assert_eq!(thumb_bucket(256, ThumbFit::Fill), 512);
    }

    #[test]
    fn buckets_stay_a_closed_set_across_the_whole_size_range() {
        // The thumbnail cache is path+bucket keyed, so a stray bucket
        // value would quietly multiply the number of live entries.
        for px in MIN_ICON_SIZE..=MAX_ICON_SIZE {
            for mode in [ThumbFit::Best, ThumbFit::Fill] {
                assert!(
                    matches!(thumb_bucket(px, mode), 128 | 256 | 512),
                    "{px}px/{mode:?} produced an off-ladder bucket"
                );
            }
            assert!(matches!(folder_icon_bucket(px), 128 | 256 | 512));
        }
    }

    #[test]
    fn folder_icons_ignore_the_fit_mode_and_only_grow_past_a_big_slot() {
        assert_eq!(folder_icon_bucket(64), 128);
        assert_eq!(folder_icon_bucket(128), 256);
        assert_eq!(folder_icon_bucket(256), 256);
        assert_eq!(folder_icon_bucket(384), 512);
        assert_eq!(folder_icon_bucket(MAX_ICON_SIZE), 512);
    }

    #[test]
    fn the_stepper_stops_span_the_sliders_whole_range() {
        // Otherwise ＋ from a slider-picked size above the last stop would
        // snap *down*, which reads as the button being broken.
        assert_eq!(ICON_SIZES[0], MIN_ICON_SIZE);
        assert_eq!(ICON_SIZES[ICON_SIZES.len() - 1], MAX_ICON_SIZE);
        assert!(ICON_SIZES.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(clamp_icon_size(DEFAULT_ICON_SIZE), DEFAULT_ICON_SIZE);
        assert_eq!(clamp_icon_size(0), MIN_ICON_SIZE);
        assert_eq!(clamp_icon_size(9999), MAX_ICON_SIZE);
    }

    #[test]
    fn fit_modes_round_trip_through_their_persisted_token() {
        for mode in [
            ThumbFit::Best,
            ThumbFit::Fill,
            ThumbFit::Width,
            ThumbFit::Height,
            ThumbFit::Stretch,
        ] {
            assert_eq!(ThumbFit::from_str(mode.as_str()), mode);
        }
        // Unknown / absent settings degrade to the pre-setting behaviour.
        assert_eq!(ThumbFit::from_str("nonsense"), ThumbFit::Best);
        assert_eq!(ThumbFit::default(), ThumbFit::Best);
    }
}
