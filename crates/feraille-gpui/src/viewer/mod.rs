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

pub mod loader;
pub mod playback;
pub mod stage;
pub mod window;

use gpui::{
    App, AppContext as _, SharedString, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};
use gpui_component::Root;

pub use window::{PlaylistEntry, ViewerWindow};

/// Open the viewer on `playlist`, starting at `start`. One reusable
/// window per process: if a viewer is already live, retarget and
/// activate it instead of stacking another (closing the window drops
/// the entity, so the stored weak handle naturally goes stale and the
/// next open creates a fresh window).
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

    let existing = process.viewer_window.borrow().clone();
    if let Some((handle, weak)) = existing {
        if let Some(view) = weak.upgrade() {
            let reused = handle.update(cx, |_root, window, cx| {
                view.update(cx, |v, cx| v.retarget(playlist.clone(), start, autoplay, cx));
                window.activate_window();
            });
            if reused.is_ok() {
                return;
            }
        }
    }

    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1100.0), px(760.0)), cx)),
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
            *process.viewer_window.borrow_mut() = Some((handle, weak));
        }
        (Err(e), _) => crate::log_warn!(90, "viewer: open_window failed: {e:?}"),
        _ => {}
    }
}
