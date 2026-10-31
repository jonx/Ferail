//! The sidebar's resolved "Locations", cached as a process-wide global.
//!
//! On Windows, OneDrive's Known-Folder-Move scatters the special folders
//! (Desktop / Documents / Pictures often live inside OneDrive, while
//! Downloads / Music / Videos stay local), and a moved folder usually
//! leaves a local copy behind, so a user genuinely can't tell where any
//! given folder "is". [`ferail_fs_native::paths::SpecialFolderMode`] lets
//! them pin a preferred root; this module owns the live value and the
//! resolved list.
//!
//! ## Prime-directive shape
//!
//! Resolving locations can read XDG configuration or stat redirected folders.
//! Startup and mode changes resolve on the background executor; rendering
//! only clones the cached list. A generation discards outdated resolutions.

use std::rc::Rc;

use ferail_fs_native::paths::{self, SpecialFolderMode, WellKnownLocation};

use crate::app_state::{self, AppState};

/// Process-wide cache of the sidebar Locations for the active
/// [`SpecialFolderMode`]. Seeded at startup and replaced wholesale when the
/// mode changes; reading it inside `render` subscribes that window to
/// changes, so flipping the Settings dropdown repaints every sidebar at once.
#[derive(Clone, Default)]
pub struct ResolvedLocations {
    locations: Rc<Vec<WellKnownLocation>>,
    generation: u64,
}

impl gpui::Global for ResolvedLocations {}

/// The persisted mode (`Auto` when never set / unset). Reads `app_state`, so
/// call it off the render path, at startup or on a user interaction.
pub fn current_mode() -> SpecialFolderMode {
    app_state::load()
        .special_folder_mode
        .as_deref()
        .map(SpecialFolderMode::from_str)
        .unwrap_or_default()
}

/// The cached locations, empty until the first resolution completes.
pub fn locations(cx: &gpui::App) -> Rc<Vec<WellKnownLocation>> {
    cx.try_global::<ResolvedLocations>()
        .map(|g| g.locations.clone())
        .unwrap_or_default()
}

/// Resolve a mode off-thread, retaining the current list until it is ready.
pub fn set_mode(mode: SpecialFolderMode, cx: &mut gpui::App) {
    let cached = cx
        .try_global::<ResolvedLocations>()
        .cloned()
        .unwrap_or_default();
    let generation = cached.generation.wrapping_add(1);
    cx.set_global(ResolvedLocations {
        generation,
        ..cached
    });
    cx.spawn(async move |cx| {
        let locations = cx
            .background_executor()
            .spawn(async move { paths::well_known_locations_for(mode) })
            .await;
        cx.update(|cx| {
            if cx.global::<ResolvedLocations>().generation == generation {
                cx.set_global(ResolvedLocations {
                    locations: Rc::new(locations),
                    generation,
                });
                cx.refresh_windows();
            }
        });
    })
    .detach();
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
