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

use gpui::App;

use crate::app_state;

/// Key context for the icon grid. Arrow keys are bound here (more
/// specific than `SHELL_CONTEXT`) so the grid's 2-D navigation
/// overrides the table's 1-D `Cursor*` bindings when the grid is
/// focused.
pub const GRID_CONTEXT: &str = "FerailleGrid";

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

/// Display sizes (logical px, longest edge of the thumbnail slot) the
/// size control snaps to. Small → Large, Finder-style.
pub const ICON_SIZES: &[u32] = &[64, 96, 128, 192, 256];

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
pub const CELL_PAD: f32 = 16.0;

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
    px.clamp(ICON_SIZES[0], ICON_SIZES[ICON_SIZES.len() - 1])
}

/// Full cell width/height for a given icon size: the thumbnail slot
/// plus label + padding. Used to derive `cols_per_row` from pane width.
pub fn cell_width(icon_px: u32) -> f32 {
    icon_px as f32 + CELL_PAD
}

pub fn cell_height(icon_px: u32) -> f32 {
    icon_px as f32 + CELL_LABEL_H
}

/// Columns that fit across `pane_width` at `icon_px`, at least 1.
pub fn cols_per_row(pane_width: f32, icon_px: u32) -> usize {
    ((pane_width / cell_width(icon_px)).floor() as usize).max(1)
}

/// Physical thumbnail fetch size (longest edge) for a grid display
/// size. Snapped to a small bucket set so the size control can't
/// explode the path-keyed cache — the bucketed image scales to the
/// exact display size at paint time. ~2× display for retina crispness.
pub fn thumb_bucket(display_px: u32) -> u32 {
    match display_px {
        0..=96 => 128,
        97..=160 => 256,
        _ => 512,
    }
}

/// Physical fetch size for grid *folder* icons (NSWorkspace bitmaps).
/// Capped at 256 — folder art has little detail beyond that, and the
/// bitmaps are per-path so the cap bounds memory. ~2× display for
/// retina crispness.
pub fn folder_icon_bucket(display_px: u32) -> u32 {
    thumb_bucket(display_px).min(256)
}
