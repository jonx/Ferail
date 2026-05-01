//! macOS shell integration for Feraille.
//!
//! Iter-4.1 ships **native chrome**: transparent titlebar and
//! `.fullSizeContentView` so the tabstrip can sit alongside the
//! traffic-light buttons. Drag-drop (`NSPasteboard`), right-click
//! menus (`NSMenu`), vibrancy (`NSVisualEffectView`), and Quick Look
//! preview land in subsequent iterations.
//!
//! Non-macOS builds get no-op stubs so `feraille-app` can call into
//! this crate unconditionally.

#[cfg(target_os = "macos")]
mod drag;

/// Drag a list of file paths out to Finder / other apps. Returns `true`
/// if the system accepted the drag; `false` if a prerequisite failed
/// (no window handle, no current NSEvent, etc.). Non-macOS: always `false`.
#[cfg(target_os = "macos")]
pub fn begin_drag(window: &winit::window::Window, paths: &[&std::path::Path]) -> bool {
    drag::begin_drag(window, paths)
}

#[cfg(not(target_os = "macos"))]
pub fn begin_drag(_window: &winit::window::Window, _paths: &[&std::path::Path]) -> bool {
    false
}

/// Width to reserve at the leading edge of the tabstrip so the OS
/// traffic-light buttons (close / minimize / zoom) don't overlap our
/// content. Standard macOS layout puts the leftmost button at ~10 DIPs
/// from the window edge; the cluster ends near 70 DIPs.
pub const TRAFFIC_LIGHT_INSET: f32 = 78.0;

/// Apply native window chrome and return the leading-edge inset (in
/// DIPs) the host should reserve for traffic-light buttons. Returns
/// `0.0` on non-macOS or if the chrome couldn't be applied.
#[cfg(target_os = "macos")]
pub fn apply_native_chrome(window: &winit::window::Window) -> f32 {
    use objc2_app_kit::{NSView, NSWindowStyleMask, NSWindowTitleVisibility};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else { return 0.0 };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return 0.0 };
    let ns_view_ptr = h.ns_view.as_ptr();
    if ns_view_ptr.is_null() {
        return 0.0;
    }
    unsafe {
        let ns_view: &NSView = &*(ns_view_ptr as *const NSView);
        let Some(ns_window) = ns_view.window() else { return 0.0 };
        ns_window.setTitlebarAppearsTransparent(true);
        ns_window.setTitleVisibility(NSWindowTitleVisibility::NSWindowTitleHidden);
        let mask = ns_window.styleMask() | NSWindowStyleMask::FullSizeContentView;
        ns_window.setStyleMask(mask);
    }
    TRAFFIC_LIGHT_INSET
}

#[cfg(not(target_os = "macos"))]
pub fn apply_native_chrome(_window: &winit::window::Window) -> f32 {
    0.0
}
