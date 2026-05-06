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
mod app_menu;

#[cfg(target_os = "macos")]
mod drag;

#[cfg(target_os = "macos")]
mod menu;

#[cfg(target_os = "macos")]
mod theme_observer;

/// Install the application menu bar (`NSApp.mainMenu`) and configure the
/// standard About panel content. Call once at startup, on the main thread,
/// after [`set_app_icon_from_png_bytes`]. No-op on non-macOS.
#[cfg(target_os = "macos")]
pub fn install_app_menu(app_name: &str, tagline: &str, version: &str, copyright: &str) {
    app_menu::install_app_menu(app_name, tagline, version, copyright);
}

#[cfg(not(target_os = "macos"))]
pub fn install_app_menu(_app_name: &str, _tagline: &str, _version: &str, _copyright: &str) {}

/// Register the host-app callback for Feraille-owned menu commands.
/// Fires on the main thread with the [`feraille_core::commands::CommandId`]
/// of the picked item. Pass `None` to clear. No-op on non-macOS.
#[cfg(target_os = "macos")]
pub fn register_command_callback(
    cb: Option<Box<dyn Fn(feraille_core::commands::CommandId) + 'static>>,
) {
    app_menu::register_command_callback(cb);
}

#[cfg(not(target_os = "macos"))]
pub fn register_command_callback(
    _cb: Option<Box<dyn Fn(feraille_core::commands::CommandId) + 'static>>,
) {
}

/// Show the standard About panel using the dictionary configured by
/// [`install_app_menu`]. The host app calls this in response to the
/// `app.about` command. No-op on non-macOS.
#[cfg(target_os = "macos")]
pub fn show_about_panel() {
    app_menu::show_about_panel();
}

#[cfg(not(target_os = "macos"))]
pub fn show_about_panel() {}

/// Update the menu's snapshot of the host's tab count. Used by
/// `validateMenuItem:` to enable / disable `file.close_tab`. Call
/// from the host whenever the tab count changes (open / close /
/// initial state). No-op on non-macOS.
#[cfg(target_os = "macos")]
pub fn set_tab_count(n: usize) {
    app_menu::set_tab_count(n);
}

#[cfg(not(target_os = "macos"))]
pub fn set_tab_count(_n: usize) {}

/// Set whether a command's menu item should render a checkmark.
/// Used for radio-button-style exclusive groups: the host flips one
/// item to `true` and the rest to `false`. Picked up on the next
/// menu open. No-op on non-macOS or before [`install_app_menu`].
#[cfg(target_os = "macos")]
pub fn set_command_state(id: feraille_core::commands::CommandId, on: bool) {
    app_menu::set_command_state(id, on);
}

#[cfg(not(target_os = "macos"))]
pub fn set_command_state(_id: feraille_core::commands::CommandId, _on: bool) {}

/// Show a native modal NSAlert with the given title and body. Single
/// "OK" button, informational style. Used for any "show this text
/// in a sheet" surface the host wants — Settings placeholder, Help
/// shortcuts cheat sheet, etc. Host composes both strings.
#[cfg(target_os = "macos")]
pub fn show_alert(title: &str, body: &str) {
    use objc2_app_kit::{NSAlert, NSAlertStyle};
    use objc2_foundation::{MainThreadMarker, NSString};

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    unsafe {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(body));
        alert.setAlertStyle(NSAlertStyle::Informational);
        alert.addButtonWithTitle(&NSString::from_str("OK"));
        let _ = alert.runModal();
    }
}

#[cfg(not(target_os = "macos"))]
pub fn show_alert(_title: &str, _body: &str) {}

/// Open `url` in the user's default handler (typically the browser).
/// Best-effort — failures are logged at the AppKit level and we don't
/// surface them. macOS uses NSWorkspace; non-macOS falls back to the
/// `open` / `xdg-open` shell command.
#[cfg(target_os = "macos")]
pub fn open_url(url: &str) {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};

    unsafe {
        let ns_str = NSString::from_str(url);
        let Some(ns_url) = NSURL::URLWithString(&ns_str) else {
            return;
        };
        let workspace = NSWorkspace::sharedWorkspace();
        let _ = workspace.openURL(&ns_url);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Show a context menu at `cursor_dips` (relative to the window's content
/// view) with the given titles. Returns the 0-based index of the selected
/// item, or `None` on dismiss. Empty title strings render as separators.
#[cfg(target_os = "macos")]
pub fn show_context_menu(
    window: &winit::window::Window,
    titles: &[&str],
    cursor_dips: (f32, f32),
) -> Option<usize> {
    menu::show_context_menu(window, titles, cursor_dips)
}

#[cfg(not(target_os = "macos"))]
pub fn show_context_menu(
    _window: &winit::window::Window,
    _titles: &[&str],
    _cursor_dips: (f32, f32),
) -> Option<usize> {
    None
}

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

/// Place a string on the system clipboard.
#[cfg(target_os = "macos")]
pub fn copy_to_clipboard(text: &str) {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let ns_text = NSString::from_str(text);
        let _ = pb.setString_forType(&ns_text, NSPasteboardTypeString);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn copy_to_clipboard(_text: &str) {}

/// Open Finder with `path` selected. macOS: shells out to `open -R`.
/// Non-macOS: no-op.
#[cfg(target_os = "macos")]
pub fn reveal_in_finder(path: &std::path::Path) {
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
}

#[cfg(not(target_os = "macos"))]
pub fn reveal_in_finder(_path: &std::path::Path) {}

/// Replace the running process's dock/app icon with the image decoded
/// from `png_bytes`. Useful when the binary is launched outside an .app
/// bundle (cargo run, debugger) where macOS would otherwise show the
/// generic executable icon.
///
/// Silently no-ops on non-macOS, and on macOS if the PNG can't be
/// decoded into an `NSImage`. Must be called on the main thread (the
/// usual app-startup constraint).
/// Result of [`set_app_icon_from_png_bytes`], for callers that want to
/// log success vs. failure.
#[derive(Debug)]
pub enum SetIconResult {
    /// Icon was decoded and assigned to NSApplication.
    Ok,
    /// Not on macOS; no-op stub.
    NotMacOs,
    /// Called off the main thread; refused.
    NotMainThread,
    /// `NSImage initWithData:` returned nil — PNG decode failed.
    DecodeFailed,
}

/// Replace the running process's dock/app icon with the image decoded
/// from `png_bytes`. Useful when the binary is launched outside an .app
/// bundle (cargo run, debugger) where macOS would otherwise show the
/// generic executable icon.
///
/// Must be called on the main thread, **after** winit has built its
/// event loop (which initialises `NSApplication`). Calling from
/// `ApplicationHandler::resumed` is the safe place.
#[cfg(target_os = "macos")]
pub fn set_app_icon_from_png_bytes(png_bytes: &[u8]) -> SetIconResult {
    use objc2::ClassType;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{MainThreadMarker, NSData, NSString};

    let Some(mtm) = MainThreadMarker::new() else {
        return SetIconResult::NotMainThread;
    };
    unsafe {
        let data = NSData::dataWithBytes_length(
            png_bytes.as_ptr() as *mut std::ffi::c_void,
            png_bytes.len(),
        );
        let alloc = NSImage::alloc();
        let Some(img) = NSImage::initWithData(alloc, &data) else {
            return SetIconResult::DecodeFailed;
        };
        // Drives dock + cmd-tab switcher.
        let app = NSApplication::sharedApplication(mtm);
        app.setApplicationIconImage(Some(&img));
        // Drives the standard About panel and any other consumer of
        // NSImage(named: "NSApplicationIcon"). Without this, About falls
        // back to whatever the bundle / Finder thinks the executable's
        // icon is — for an unbundled cargo-run binary, the generic
        // folder/exec glyph.
        let name = NSString::from_str("NSApplicationIcon");
        img.setName(Some(&name));
    }
    SetIconResult::Ok
}

#[cfg(not(target_os = "macos"))]
pub fn set_app_icon_from_png_bytes(_png_bytes: &[u8]) -> SetIconResult {
    SetIconResult::NotMacOs
}

/// `true` if the system is currently in Dark Mode. Reads
/// `NSApp.effectiveAppearance.name` and compares against
/// `NSAppearanceNameDarkAqua`, picking up both system Dark mode and
/// any per-app appearance override higher in the responder chain.
/// Must run on the main thread (NSApp constraint); off-thread
/// callers get `false`.
///
/// Returns `false` on non-macOS or if the lookup fails for any
/// reason — callers can layer their own override (e.g. an env var)
/// on top.
#[cfg(target_os = "macos")]
pub fn system_is_dark() -> bool {
    use objc2_app_kit::{NSAppearanceNameDarkAqua, NSApplication};
    use objc2_foundation::MainThreadMarker;

    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    unsafe {
        let app = NSApplication::sharedApplication(mtm);
        let appearance = app.effectiveAppearance();
        let name = appearance.name();
        // The reported name on Sonoma+ may be `NSAppearanceNameDarkAqua`,
        // `NSAppearanceNameAccessibilityHighContrastDarkAqua`, etc.
        // Treat any "Dark" appearance as dark.
        let n = name.to_string();
        n == NSAppearanceNameDarkAqua.to_string() || n.contains("Dark")
    }
}

#[cfg(not(target_os = "macos"))]
pub fn system_is_dark() -> bool {
    false
}

/// Subscribe to macOS Appearance change notifications. The callback
/// fires on the main thread with the new dark-mode state every time
/// the user toggles System Settings → Appearance, or "Auto" mode
/// crosses the day/night boundary.
///
/// Idempotent: re-registering replaces the callback without stacking
/// duplicate observers. Must be called on the main thread (after
/// winit has built its event loop / NSApp); off-thread or non-macOS
/// calls are a no-op.
#[cfg(target_os = "macos")]
pub fn start_system_theme_observer(callback: Box<dyn Fn(bool) + 'static>) {
    theme_observer::start(callback);
}

#[cfg(not(target_os = "macos"))]
pub fn start_system_theme_observer(_callback: Box<dyn Fn(bool) + 'static>) {}

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

    let Ok(handle) = window.window_handle() else {
        return 0.0;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return 0.0;
    };
    let ns_view_ptr = h.ns_view.as_ptr();
    if ns_view_ptr.is_null() {
        return 0.0;
    }
    unsafe {
        let ns_view: &NSView = &*(ns_view_ptr as *const NSView);
        let Some(ns_window) = ns_view.window() else {
            return 0.0;
        };
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
