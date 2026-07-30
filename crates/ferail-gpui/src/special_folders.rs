//! The sidebar's resolved "Locations", cached as a process-wide global.
//!
//! On Windows, OneDrive's Known-Folder-Move scatters the special folders
//! (Desktop / Documents / Pictures often live inside OneDrive, while
//! Downloads / Music / Videos stay local), and a moved folder usually
//! leaves a local copy behind — so a user genuinely can't tell where any
//! given folder "is". [`ferail_fs_native::paths::SpecialFolderMode`] lets
//! them pin a preferred root; this module owns the live value and the
//! resolved list.
//!
//! ## Prime-directive shape
//!
//! Resolving a mode stats the disk (`prefer_if_exists`), which `render`
//! must never do. So the `Vec<WellKnownLocation>` is resolved **once** — at
//! startup ([`seed`]) and whenever the Settings dropdown changes it
//! ([`set_mode`]) — and stashed in the [`ResolvedLocations`] global. The
//! two sidebar builders only ever read that global ([`locations`]), an
//! in-memory `Rc` clone with no I/O — the same shape as
//! [`crate::thumbnails::ShowThumbnails`].

use std::rc::Rc;

use ferail_fs_native::paths::{self, SpecialFolderMode, WellKnownLocation};

use crate::app_state::{self, AppState};

/// Process-wide cache of the sidebar Locations for the active
/// [`SpecialFolderMode`]. Seeded at startup and replaced wholesale when the
/// mode changes; reading it inside `render` subscribes that window to
/// changes, so flipping the Settings dropdown repaints every sidebar at once.
#[derive(Clone)]
pub struct ResolvedLocations(pub Rc<Vec<WellKnownLocation>>);

impl gpui::Global for ResolvedLocations {}

/// The persisted mode (`Auto` when never set / unset). Reads `app_state`, so
/// call it off the render path — at startup or on a user interaction.
pub fn current_mode() -> SpecialFolderMode {
    app_state::load()
        .special_folder_mode
        .as_deref()
        .map(SpecialFolderMode::from_str)
        .unwrap_or_default()
}

/// The cached Locations. Falls back to an `Auto` resolve when the global
/// hasn't been seeded (headless / screenshot paths that skip startup), so a
/// caller never needs to special-case absence.
pub fn locations(cx: &gpui::App) -> Rc<Vec<WellKnownLocation>> {
    match cx.try_global::<ResolvedLocations>() {
        Some(g) => g.0.clone(),
        None => Rc::new(paths::well_known_locations()),
    }
}

/// Resolve `mode` (the only place that stats) and publish the result to the
/// global. Returns nothing — callers pair this with `cx.refresh_windows()`
/// to repaint open sidebars.
pub fn set_mode(mode: SpecialFolderMode, cx: &mut gpui::App) {
    cx.set_global(ResolvedLocations(Rc::new(paths::well_known_locations_for(
        mode,
    ))));
}

/// Seed the global from the persisted mode. Called once at startup, before
/// the first window opens.
pub fn seed(cx: &mut gpui::App) {
    set_mode(current_mode(), cx);
}

/// Persist `mode` to `app_state` and apply it live: recompute the global and
/// repaint every open window. Driven by the Settings dropdown.
pub fn persist_and_apply(mode: SpecialFolderMode, cx: &mut gpui::App) {
    let existing = app_state::load();
    app_state::save(&AppState {
        special_folder_mode: Some(mode.as_str().to_string()),
        ..existing
    });
    set_mode(mode, cx);
    cx.refresh_windows();
}
