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
    App, AppContext as _, Bounds, SharedString, Styled, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, px, size,
};
use gpui_component::Root;

pub use window::{PlaylistEntry, ViewerWindow};

/// Cascade counter so each new viewer window is offset from the last
/// instead of stacking exactly on top of it.
static VIEWER_CASCADE: AtomicU32 = AtomicU32::new(0);

/// Open the viewer on `playlist`, starting at `start`. Each call opens a
/// *new* window (cascaded from the previous one), so several files can be
/// viewed side by side; closing a window just drops its entity. `window` is
/// the window the viewer is opened from — the new window lands on its display.
pub fn open_viewer(playlist: Vec<PlaylistEntry>, start: usize, window: &Window, cx: &mut App) {
    open_viewer_inner(playlist, start, false, window, cx);
}

/// Like [`open_viewer`] but begins the slideshow immediately — used by
/// the "Slideshow from Here" context action (docs/features/VIEWER.md).
pub fn open_viewer_playing(
    playlist: Vec<PlaylistEntry>,
    start: usize,
    window: &Window,
    cx: &mut App,
) {
    open_viewer_inner(playlist, start, true, window, cx);
}

fn open_viewer_inner(
    playlist: Vec<PlaylistEntry>,
    start: usize,
    autoplay: bool,
    window: &Window,
    cx: &mut App,
) {
    let process = crate::process_state::process_state(cx);

    // Centre on the display that hosts the invoking window so a viewer opened
    // from a window on a secondary monitor appears there rather than jumping
    // to the primary display. Then cascade by a fixed step (wrapping after a
    // handful) so a fresh window doesn't land exactly atop the last.
    //
    // Platform wart: gpui's mac backend anchors `window_bounds` to
    // `WindowOptions::display_id` — the origin is *display-relative*, and a
    // `None` display id means the primary screen (which is why global
    // coordinates alone always landed there). Windows/X11 take global
    // coordinates. So pass the display id AND, on macOS, strip the display's
    // global origin from the centred bounds.
    let display = window.display(cx);
    let display_id = display.as_ref().map(|d| d.id());
    let step = (VIEWER_CASCADE.fetch_add(1, Ordering::Relaxed) % 6) as f32 * 28.0;
    let mut bounds = Bounds::centered(display_id, size(px(1100.0), px(760.0)), cx);
    if cfg!(target_os = "macos") {
        if let Some(display) = &display {
            let display_origin = display.bounds().origin;
            bounds.origin.x -= display_origin.x;
            bounds.origin.y -= display_origin.y;
        }
    }
    bounds.origin.x += px(step);
    bounds.origin.y += px(step);

    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id,
        titlebar: Some(TitlebarOptions {
            title: Some(SharedString::from("Viewer")),
            ..Default::default()
        }),
        // Open opaque, matching the default (toggle-off) state. The in-viewer
        // "Transparent" toggle flips the backing live via
        // `set_background_appearance`, which updates both the NSWindow and the
        // CAMetalLayer's opacity (see `MetalRenderer::update_transparency`), so
        // creating it transparent up front is unnecessary — and a permanently
        // non-opaque backing makes the macOS fullscreen animation janky. Opaque
        // by default keeps the fullscreen transition clean; the toggle grants
        // see-through only when the user asks for it.
        window_background: WindowBackgroundAppearance::Opaque,
        ..crate::base_window_options()
    };
    let mut weak_view = None;
    let handle = cx.open_window(opts, |window, cx| {
        let view =
            cx.new(|cx| ViewerWindow::new(playlist, start, autoplay, process.clone(), window, cx));
        weak_view = Some(view.downgrade());
        // gpui_component's Root paints an opaque theme background; override it
        // to transparent so the viewer's own (toggle-controlled) background is
        // what determines see-through. Normal mode still looks opaque because
        // the viewer fills the window with an opaque background of its own.
        cx.new(|cx| Root::new(view, window, cx).bg(gpui::transparent_black()))
    });
    match (handle, weak_view) {
        (Ok(handle), Some(weak)) => {
            process.register_viewer(handle, weak);
        }
        (Err(e), _) => crate::log_warn!(90, "viewer: open_window failed: {e:?}"),
        _ => {}
    }
}
