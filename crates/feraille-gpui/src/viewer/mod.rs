//! Viewer window: big preview, slideshow, and sticky zoom.
//!
//! Design and iteration plan: docs/features/VIEWER.md. The module is
//! built in layers — `loader` (full-resolution decode + byte-budget
//! cache), `stage` (pure zoom/pan geometry), `window` (the GPUI
//! entity), `playback` (slideshow timer state).
//!
//! Prime directive: render paths read only the in-memory cache and
//! stage state; every decode and Quick Look shell-out runs on the
//! background executor and re-enters through `entity.update`.

pub mod backend_native;
pub mod loader;
pub mod playback;
pub mod stage;
pub mod window;

use std::sync::atomic::{AtomicU32, Ordering};

use gpui::{
    App, AppContext as _, Bounds, SharedString, TitlebarOptions, WindowBounds, WindowOptions, px,
    size,
};
use gpui_component::Root;

pub use window::{PlaylistEntry, ViewerWindow};

/// Cascade counter so each new viewer window is offset from the last
/// instead of stacking exactly on top of it.
static VIEWER_CASCADE: AtomicU32 = AtomicU32::new(0);

/// Open the viewer on `playlist`, starting at `start`. Each call opens a
/// *new* window (cascaded from the previous one), so several files can be
/// viewed side by side; closing a window just drops its entity.
pub fn open_viewer(playlist: Vec<PlaylistEntry>, start: usize, cx: &mut App) {
    open_viewer_inner(playlist, start, false, cx);
}

/// Like [`open_viewer`] but begins the slideshow immediately — used by
/// the "Slideshow from Here" context action (docs/features/VIEWER.md).
pub fn open_viewer_playing(playlist: Vec<PlaylistEntry>, start: usize, cx: &mut App) {
    open_viewer_inner(playlist, start, true, cx);
}

fn open_viewer_inner(playlist: Vec<PlaylistEntry>, start: usize, autoplay: bool, cx: &mut App) {
    let process = crate::process_state::process_state(cx);

    // Centre on the active display, then cascade by a fixed step (wrapping
    // after a handful) so a fresh window doesn't land exactly atop the last.
    let step = (VIEWER_CASCADE.fetch_add(1, Ordering::Relaxed) % 6) as f32 * 28.0;
    let mut bounds = Bounds::centered(None, size(px(1100.0), px(760.0)), cx);
    bounds.origin.x = bounds.origin.x + px(step);
    bounds.origin.y = bounds.origin.y + px(step);

    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(SharedString::from("Viewer")),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut weak_view = None;
    let handle = cx.open_window(opts, |window, cx| {
        let view =
            cx.new(|cx| ViewerWindow::new(playlist, start, autoplay, process.clone(), window, cx));
        weak_view = Some(view.downgrade());
        cx.new(|cx| Root::new(view, window, cx))
    });
    match (handle, weak_view) {
        (Ok(handle), Some(weak)) => {
            process.register_viewer(handle, weak);
        }
        (Err(e), _) => crate::log_warn!(90, "viewer: open_window failed: {e:?}"),
        _ => {}
    }
}
