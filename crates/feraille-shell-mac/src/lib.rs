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
mod archive;

#[cfg(target_os = "macos")]
mod drag;

#[cfg(target_os = "macos")]
mod file_ops;

#[cfg(target_os = "macos")]
mod menu;

#[cfg(target_os = "macos")]
mod open_with;

#[cfg(target_os = "macos")]
mod quick_look;

#[cfg(target_os = "macos")]
pub(crate) mod services;

#[cfg(target_os = "macos")]
mod share;

#[cfg(target_os = "macos")]
mod tags;

#[cfg(target_os = "macos")]
mod theme_observer;

#[cfg(target_os = "macos")]
mod video_overlay;

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
/// [`install_app_menu`] or [`set_about_options`]. The host app calls
/// this in response to the `app.about` command. No-op on non-macOS.
#[cfg(target_os = "macos")]
pub fn show_about_panel() {
    app_menu::show_about_panel();
}

#[cfg(not(target_os = "macos"))]
pub fn show_about_panel() {}

/// Populate the About-panel options dictionary without installing the
/// full menu bar. Useful when the host builds its menu through another
/// channel (e.g. gpui's `cx.set_menus`) but still wants
/// [`show_about_panel`] to display a populated dialog.
#[cfg(target_os = "macos")]
pub fn set_about_options(app_name: &str, tagline: &str, version: &str, copyright: &str) {
    app_menu::set_about_options(app_name, tagline, version, copyright);
}

#[cfg(not(target_os = "macos"))]
pub fn set_about_options(_app_name: &str, _tagline: &str, _version: &str, _copyright: &str) {}

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

/// Show a modal NSAlert with a single-line text input. Returns
/// `Some(value)` on OK, `None` if the user cancelled (or non-macOS).
/// Used by the Favorites rename flow and the Locate… repoint flow.
/// Must be called on the main thread.
#[cfg(target_os = "macos")]
pub fn prompt_for_text(title: &str, body: &str, default: &str) -> Option<String> {
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSTextField};
    use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

    let mtm = MainThreadMarker::new()?;
    unsafe {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(body));
        alert.setAlertStyle(NSAlertStyle::Informational);
        alert.addButtonWithTitle(&NSString::from_str("OK"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        let input = NSTextField::new(mtm);
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(280.0, 24.0));
        input.setFrame(frame);
        input.setStringValue(&NSString::from_str(default));
        alert.setAccessoryView(Some(&input));
        if alert.runModal() != NSAlertFirstButtonReturn {
            return None;
        }
        Some(input.stringValue().to_string())
    }
}

#[cfg(not(target_os = "macos"))]
pub fn prompt_for_text(_title: &str, _body: &str, _default: &str) -> Option<String> {
    None
}

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

/// Plan-driven context menu types. Build a [`MenuPlan`] at the
/// right-click site, hand it to [`show_context_menu`], and dispatch
/// the returned [`MenuPick`].
#[cfg(target_os = "macos")]
pub use menu::{MenuPick, MenuPlan, MenuPlanItem};

/// Non-macOS shadow of [`MenuPlan`]. Same shape so call-sites
/// compile uniformly; [`show_context_menu`] is a no-op.
#[cfg(not(target_os = "macos"))]
#[derive(Clone, Debug, Default)]
pub struct MenuPlan {
    pub items: Vec<MenuPlanItem>,
}

#[cfg(not(target_os = "macos"))]
#[derive(Clone, Debug)]
pub enum MenuPlanItem {
    Action {
        command: feraille_core::commands::CommandId,
        title: String,
        enabled: bool,
        checked: bool,
        payload: Option<feraille_core::commands::CommandPayload>,
    },
    Separator,
    Submenu {
        title: String,
        items: Vec<MenuPlanItem>,
    },
    ServicesSubmenu {
        title: String,
    },
}

#[cfg(not(target_os = "macos"))]
#[derive(Clone, Debug)]
pub struct MenuPick {
    pub command: feraille_core::commands::CommandId,
    pub payload: Option<feraille_core::commands::CommandPayload>,
}

#[cfg(not(target_os = "macos"))]
impl MenuPlan {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, item: MenuPlanItem) -> &mut Self {
        self.items.push(item);
        self
    }
}

#[cfg(not(target_os = "macos"))]
impl MenuPlanItem {
    pub fn action(command: feraille_core::commands::CommandId, title: impl Into<String>) -> Self {
        MenuPlanItem::Action {
            command,
            title: title.into(),
            enabled: true,
            checked: false,
            payload: None,
        }
    }
    pub fn action_with_payload(
        command: feraille_core::commands::CommandId,
        title: impl Into<String>,
        payload: feraille_core::commands::CommandPayload,
    ) -> Self {
        MenuPlanItem::Action {
            command,
            title: title.into(),
            enabled: true,
            checked: false,
            payload: Some(payload),
        }
    }
    pub fn checked(mut self, on: bool) -> Self {
        if let MenuPlanItem::Action {
            ref mut checked, ..
        } = self
        {
            *checked = on;
        }
        self
    }
    pub fn separator() -> Self {
        MenuPlanItem::Separator
    }
    pub fn submenu(title: impl Into<String>, items: Vec<MenuPlanItem>) -> Self {
        MenuPlanItem::Submenu {
            title: title.into(),
            items,
        }
    }
    pub fn services_submenu(title: impl Into<String>) -> Self {
        MenuPlanItem::ServicesSubmenu {
            title: title.into(),
        }
    }
}

/// Show a context menu at `cursor_dips` (relative to the window's
/// content view) with the items in `plan`. Returns the picked
/// item, or `None` on dismiss. Synchronous — blocks the calling
/// thread while the menu is open.
#[cfg(target_os = "macos")]
pub fn show_context_menu(
    window: &winit::window::Window,
    plan: MenuPlan,
    cursor_dips: (f32, f32),
) -> Option<MenuPick> {
    menu::show_context_menu(window, plan, cursor_dips)
}

#[cfg(not(target_os = "macos"))]
pub fn show_context_menu(
    _window: &winit::window::Window,
    _plan: MenuPlan,
    _cursor_dips: (f32, f32),
) -> Option<MenuPick> {
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

/// Open a terminal at `path` (a directory). macOS: `open -a Terminal
/// <dir>`, which launches Terminal.app with `path` as the working
/// directory. Non-macOS: no-op.
#[cfg(target_os = "macos")]
pub fn open_terminal(path: &std::path::Path) {
    let _ = std::process::Command::new("open")
        .arg("-a")
        .arg("Terminal")
        .arg(path)
        .spawn();
}

#[cfg(not(target_os = "macos"))]
pub fn open_terminal(_path: &std::path::Path) {}

/// Duplicate `src` next to itself with Finder's " copy" / " copy 2"
/// naming. Returns the destination path on success, or an error
/// string on failure (collision exhausted, IO error). Synchronous —
/// callers run this on a worker.
#[cfg(target_os = "macos")]
pub fn duplicate_path(src: &std::path::Path) -> Result<std::path::PathBuf, String> {
    file_ops::duplicate(src)
}

#[cfg(not(target_os = "macos"))]
pub fn duplicate_path(_src: &std::path::Path) -> Result<std::path::PathBuf, String> {
    Err("duplicate is macOS-only in this build".into())
}

/// Make a Finder-resolvable alias file pointing at `target`.
/// Synchronous — callers run this on a worker.
#[cfg(target_os = "macos")]
pub fn make_alias(target: &std::path::Path) -> Result<std::path::PathBuf, String> {
    file_ops::make_alias(target)
}

#[cfg(not(target_os = "macos"))]
pub fn make_alias(_target: &std::path::Path) -> Result<std::path::PathBuf, String> {
    Err("make_alias is macOS-only".into())
}

/// Compress `targets` into a `.zip` next to the first target's
/// parent (Finder behaviour: `Foo.zip` for one source, `Archive.zip`
/// for several). Synchronous — callers run this on a worker.
#[cfg(target_os = "macos")]
pub fn compress_paths(targets: &[&std::path::Path]) -> Result<std::path::PathBuf, String> {
    archive::compress(targets)
}

#[cfg(not(target_os = "macos"))]
pub fn compress_paths(_targets: &[&std::path::Path]) -> Result<std::path::PathBuf, String> {
    Err("compress is macOS-only in this build".into())
}

/// Open Quick Look on `paths`. Stage B uses the standalone
/// `qlmanage -p` window so we don't need responder-chain plumbing
/// for `QLPreviewPanel`. Non-blocking: spawns and detaches.
#[cfg(target_os = "macos")]
pub fn show_quick_look(paths: &[&std::path::Path]) -> Result<(), String> {
    quick_look::show(paths)
}

/// Generate a Quick Look thumbnail for `path` at `size_px` (longest
/// edge). Returns RGBA8888 bytes plus the actual image dimensions.
/// Synchronous — call from a worker thread. macOS-only; non-macOS
/// returns `None`.
#[cfg(target_os = "macos")]
pub fn fetch_quick_look_thumbnail(
    path: &std::path::Path,
    size_px: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    quick_look::fetch_thumbnail(path, size_px)
}

#[cfg(not(target_os = "macos"))]
pub fn fetch_quick_look_thumbnail(
    _path: &std::path::Path,
    _size_px: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn show_quick_look(_paths: &[&std::path::Path]) -> Result<(), String> {
    Err("quick_look is macOS-only".into())
}

/// Read the canonical Finder-colour tags currently set on `path`.
/// User-defined tag names are dropped on the floor here; callers
/// that need the raw strings should use `read_tag_names` instead.
#[cfg(target_os = "macos")]
pub fn read_canonical_tags(path: &std::path::Path) -> Vec<feraille_core::commands::TagColor> {
    tags::read_canonical_tags(path)
}

#[cfg(not(target_os = "macos"))]
pub fn read_canonical_tags(_path: &std::path::Path) -> Vec<feraille_core::commands::TagColor> {
    Vec::new()
}

/// Toggle a single Finder colour tag on `path`. Other tags
/// (including user-defined ones) are preserved. Synchronous —
/// callers run this on a worker if the selection is large.
#[cfg(target_os = "macos")]
pub fn toggle_tag(
    path: &std::path::Path,
    color: feraille_core::commands::TagColor,
) -> Result<(), String> {
    tags::toggle_tag(path, color)
}

#[cfg(not(target_os = "macos"))]
pub fn toggle_tag(
    _path: &std::path::Path,
    _color: feraille_core::commands::TagColor,
) -> Result<(), String> {
    Err("toggle_tag is macOS-only".into())
}

/// Strip every tag (including user-defined ones) from `path`.
#[cfg(target_os = "macos")]
pub fn clear_tags(path: &std::path::Path) -> Result<(), String> {
    tags::write_tags(path, &[])
}

#[cfg(not(target_os = "macos"))]
pub fn clear_tags(_path: &std::path::Path) -> Result<(), String> {
    Err("clear_tags is macOS-only".into())
}

/// One app the system would offer in the Open With submenu for a
/// given file. `is_default` flags the system's preferred handler;
/// the right-click builder pins it to the top.
#[derive(Clone, Debug)]
pub struct OpenWithCandidate {
    pub name: String,
    pub path: std::path::PathBuf,
    pub is_default: bool,
}

#[cfg(target_os = "macos")]
impl From<open_with::OpenWithCandidate> for OpenWithCandidate {
    fn from(c: open_with::OpenWithCandidate) -> Self {
        OpenWithCandidate {
            name: c.name,
            path: c.path,
            is_default: c.is_default,
        }
    }
}

/// Enumerate the apps Launch Services would offer to open `path`.
/// Synchronous (~10–50 ms typical). Empty on failure.
#[cfg(target_os = "macos")]
pub fn open_with_candidates(path: &std::path::Path) -> Vec<OpenWithCandidate> {
    open_with::candidates_for(path)
        .into_iter()
        .map(Into::into)
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn open_with_candidates(_path: &std::path::Path) -> Vec<OpenWithCandidate> {
    Vec::new()
}

/// Open `target` with the app at `app_path`. Shells out to
/// `/usr/bin/open -a` so we don't have to wire up the
/// `NSWorkspace.openURLs:` completion-handler contract.
pub fn open_with_app(target: &std::path::Path, app_path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        open_with::open_with(target, app_path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (target, app_path);
        Err("open_with_app is macOS-only".into())
    }
}

/// Splice a Services-vending responder into the window's chain
/// and publish the empty `NSApp.servicesMenu` AppKit will populate
/// on demand. Idempotent. Call once after the main window exists,
/// on the main thread. No-op on non-macOS.
#[cfg(target_os = "macos")]
pub fn install_services_anchor(window: &winit::window::Window) {
    services::install(window);
}

#[cfg(not(target_os = "macos"))]
pub fn install_services_anchor(_window: &winit::window::Window) {}

/// Push the right-clicked selection so the Services anchor has
/// something to vend when AppKit asks. Call from the right-click
/// handler just before [`show_context_menu`]. No-op on non-macOS.
#[cfg(target_os = "macos")]
pub fn set_services_selection(paths: Vec<std::path::PathBuf>) {
    services::set_current_selection(paths);
}

#[cfg(not(target_os = "macos"))]
pub fn set_services_selection(_paths: Vec<std::path::PathBuf>) {}

/// Show the system Share picker (`NSSharingServicePicker`) for
/// `paths`, anchored to the given window's content view.
#[cfg(target_os = "macos")]
pub fn show_share_picker(
    window: &winit::window::Window,
    paths: &[&std::path::Path],
) -> Result<(), String> {
    share::show_picker(window, paths)
}

#[cfg(not(target_os = "macos"))]
pub fn show_share_picker(
    _window: &winit::window::Window,
    _paths: &[&std::path::Path],
) -> Result<(), String> {
    Err("share is macOS-only".into())
}

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

/// AppUserModelID is a Windows-shell concept (taskbar grouping,
/// jump-list, pin-to-Start). No equivalent on macOS — the Dock
/// groups by bundle identifier, set elsewhere. Stub kept so the
/// `platform_shell` alias compiles symmetrically on both targets.
pub fn set_app_user_model_id(_id: &str) {}

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
/// Thread contract: on macOS the callback fires on the MAIN thread —
/// but the shell-win32 twin of this function fires its callback on a
/// WORKER thread. Cross-platform callers must write the callback to
/// the weaker (win32) contract: thread-safe state only, no gpui
/// entities. `main.rs` does this via the atomic
/// `shell::set_system_theme_pending` cell that the Shell polls at
/// render.
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

/// Mount a native AVPlayerView video overlay inside the given content
/// NSView at `frame` (gpui top-left logical coordinates) and start
/// playback. Returns an overlay handle, 0 on failure. Main-thread
/// only. `on_ended` fires on the main thread when the video plays to
/// the end; it must defer (e.g. through a channel), not call overlay
/// APIs synchronously. See docs/features/VIEWER.md.
#[cfg(target_os = "macos")]
pub fn video_overlay_show(
    container_ns_view: *mut std::ffi::c_void,
    path: &std::path::Path,
    frame: (f64, f64, f64, f64),
    on_ended: Box<dyn Fn() + 'static>,
) -> u64 {
    video_overlay::show(container_ns_view, path, frame, on_ended)
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_show(
    _container_ns_view: *mut std::ffi::c_void,
    _path: &std::path::Path,
    _frame: (f64, f64, f64, f64),
    _on_ended: Box<dyn Fn() + 'static>,
) -> u64 {
    0
}

/// Reposition a live video overlay. Main-thread only; stale ids no-op.
#[cfg(target_os = "macos")]
pub fn video_overlay_set_frame(id: u64, frame: (f64, f64, f64, f64)) {
    video_overlay::set_frame(id, frame);
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_set_frame(_id: u64, _frame: (f64, f64, f64, f64)) {}

/// Stop playback and remove a video overlay. Main-thread only; stale
/// ids no-op.
#[cfg(target_os = "macos")]
pub fn video_overlay_remove(id: u64) {
    video_overlay::remove(id);
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_remove(_id: u64) {}

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
