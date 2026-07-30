//! Selection color palette — single source of truth shared by the file
//! list (`multi_table` Table + `file_list`) and the icon grid
//! (`shell::render`).
//!
//! The grid keyed selection off the saturated `theme.blue` and read
//! clearly; the list keyed off `theme.table_active`, which the
//! gpui-component theme hard-caps at alpha ≤ 0.2 and is a desaturated
//! near-foreground gray — so list selection looked faint next to the
//! grid. Both renderers now read this module instead, so they share one
//! hue and the list matches the grid.
//!
//! The base accent is customizable: [`SelectionAccent`] is a process-
//! wide [`gpui::Global`] (same pattern as [`crate::grid::IconSize`] and
//! [`crate::thumbnails::ShowThumbnails`]), seeded from persisted settings
//! at startup and updated live by the Appearance settings color picker.
//! Reading [`accent`] during render subscribes the window to changes, so
//! editing the color repaints open windows immediately. `None` (the
//! default) falls back to the theme's blue, so an untouched profile keeps
//! the stock Finder-blue look.

use gpui::{App, Hsla};
use gpui_component::{ActiveTheme as _, Colorize as _};

/// The user's selection-accent override. `None` ⇒ use `theme.blue`.
#[derive(Clone, Copy)]
pub struct SelectionAccent(pub Option<Hsla>);

impl gpui::Global for SelectionAccent {}

/// The live selection accent: the user override if set, else the theme's
/// saturated blue. Reading it subscribes the window to global changes.
pub fn accent(cx: &App) -> Hsla {
    cx.try_global::<SelectionAccent>()
        .and_then(|g| g.0)
        .unwrap_or_else(|| cx.theme().blue)
}

// Derived tints. These opacities are the values the grid already used,
// so routing the grid through them is visually identical and the list
// inherits the same scheme. Border width stays constant at the call
// sites so selection never nudges layout by a pixel.

/// Light wash behind a selected cell / row. Grid: cell background.
/// List: every selected row (lead and members).
pub fn fill(cx: &App) -> Hsla {
    accent(cx).opacity(0.14)
}

/// Background pill behind a *non-lead* selected grid label — slightly
/// muted from full strength so the focused (lead) item still stands out.
pub fn member_pill(cx: &App) -> Hsla {
    accent(cx).opacity(0.82)
}

/// Border on a *non-lead* selected grid cell.
pub fn border(cx: &App) -> Hsla {
    accent(cx).opacity(0.55)
}

/// Full-strength accent: the grid lead's border + solid label pill, and
/// the list lead row's focus-ring border.
pub fn strong(cx: &App) -> Hsla {
    accent(cx)
}

/// Foreground for text drawn on a solid accent pill.
pub fn text(_cx: &App) -> Hsla {
    gpui::white()
}

// Hex round-trip for persistence + the settings color picker. Kept here
// so the `Colorize` trait import lives in one place and `app_state` stays
// free of color types. `#RRGGBB` / `#RRGGBBAA`.

/// Parse a persisted hex string into an accent, or `None` if invalid.
pub fn parse_hex(hex: &str) -> Option<Hsla> {
    Hsla::parse_hex(hex).ok()
}

/// Format an accent as a hex string for persistence.
pub fn to_hex(color: Hsla) -> String {
    color.to_hex()
}
