//! Ant Trail base color + favorites-tracking policy.
//!
//! The Ant Trail paints a warm tint behind frequently-visited directory
//! rows (file list) and cells (grid), scaled by per-folder heat. The
//! base hue is customizable, mirroring [`crate::selection_colors`]:
//! [`AntTrailColor`] is a process-wide [`gpui::Global`] seeded from
//! persisted settings at startup and edited live by the Appearance
//! settings color picker. Reading [`base`] during render subscribes the
//! window to changes, so editing the color repaints open windows
//! immediately. `None` (the default) falls back to [`default_base`] —
//! the original warm orange.
//!
//! [`ExcludeFavoritesFromTracking`] is the policy behind
//! `Shell::navigate_from_favorite`: when set (the default), reaching a
//! folder by clicking its favorite does not record a visit — it neither
//! bumps that folder's Ant Trail heat nor pushes it into Recents, since
//! a favorite is a deliberate shortcut rather than organic browsing. The
//! same folder reached by browsing still records normally.

use gpui::{App, Hsla};

/// The user's Ant Trail base-color override. `None` ⇒ [`default_base`].
#[derive(Clone, Copy)]
pub struct AntTrailColor(pub Option<Hsla>);

impl gpui::Global for AntTrailColor {}

/// The original hardcoded warm orange — the heat metaphor's "glow."
/// Returned at full alpha; the per-row heat alpha is applied by [`tint`].
pub fn default_base() -> Hsla {
    gpui::Rgba {
        r: 1.0,
        g: 0.55,
        b: 0.26,
        a: 1.0,
    }
    .into()
}

/// The live Ant Trail base color: the user override if set, else
/// [`default_base`]. Reading it during render subscribes the window to
/// global changes, so editing the picker recolors open windows at once.
pub fn base(cx: &App) -> Hsla {
    cx.try_global::<AntTrailColor>()
        .and_then(|g| g.0)
        .unwrap_or_else(default_base)
}

/// Apply per-folder heat to a base color, producing the row/cell tint.
/// Heat 0→1 maps to alpha 0→0.30 (the original fixed recipe). Alpha is
/// *set*, not multiplied, so a color picked with a partial alpha can't
/// double-dim the tint — the translucency comes from heat alone.
pub fn tint(base: Hsla, heat: f32) -> Hsla {
    base.alpha((heat * 0.30).clamp(0.0, 1.0))
}

// Hex round-trip for persistence + the settings picker reuses
// `selection_colors::{parse_hex, to_hex}` so the `Colorize` import and
// the `#RRGGBB(AA)` format live in one place.

/// Whether navigating to a folder *via a favorite* skips visit recording
/// (Ant Trail heat + Recents). Default `true`. Set live by the Appearance
/// settings toggle; read by `Shell::navigate_from_favorite`.
#[derive(Clone, Copy)]
pub struct ExcludeFavoritesFromTracking(pub bool);

impl gpui::Global for ExcludeFavoritesFromTracking {}

/// Read the live favorites-tracking policy, defaulting to `true`
/// (exclude) when the global hasn't been seeded yet.
pub fn exclude_favorites(cx: &App) -> bool {
    cx.try_global::<ExcludeFavoritesFromTracking>()
        .map(|g| g.0)
        .unwrap_or(true)
}
