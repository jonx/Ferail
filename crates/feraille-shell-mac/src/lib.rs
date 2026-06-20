//! macOS shell integration for Feraille.
//!
//! Iter-4.1 ships **native chrome**: transparent titlebar and
//! `.fullSizeContentView` so the tabstrip can sit alongside the
//! traffic-light buttons. Drag-drop (`NSPasteboard`), right-click
//! menus (`NSMenu`), vibrancy (`NSVisualEffectView`), and Quick Look
//! preview land in subsequent iterations.
//!
//! Non-macOS builds get no-op stubs so callers can use this crate
//! unconditionally.

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
mod volume_observer;

#[cfg(target_os = "macos")]
mod video_overlay;

/// Spotlight-backed global search (Tier 2). Cross-platform module with
/// internal cfg gates — non-macOS builds get unsupported stubs.
pub mod spotlight;
pub use spotlight::{spotlight_available, spotlight_search, SpotlightScope};

/// AppKit-sourced facts for the Get Info panel (UTI, localized Kind,
/// date-added, Finder attribute bits). Cross-platform module with internal
/// cfg gates — non-macOS builds get an empty record.
pub mod resource_values;
pub use resource_values::{read_shell_info, set_hidden_extension, ShellInfo};

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

/// Filesystem path of the running application, suitable for pasting
/// into a macOS file picker (e.g. the Full Disk Access "+" sheet).
/// When launched from a `.app` bundle this is the bundle path; a bare
/// binary (e.g. `cargo run`) returns the executable path. `None` if
/// the path can't be determined. Cheap — no directory reads.
#[cfg(target_os = "macos")]
pub fn app_bundle_path() -> Option<String> {
    use objc2_foundation::NSBundle;
    unsafe {
        let bundle = NSBundle::mainBundle();
        let path = bundle.bundlePath();
        let s = path.to_string();
        // A loose binary reports the enclosing directory as its
        // "bundle"; prefer the real executable path in that case.
        if s.ends_with(".app") {
            return Some(s);
        }
    }
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(not(target_os = "macos"))]
pub fn app_bundle_path() -> Option<String> {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

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

/// Unmount and eject the volume mounted at `path`. Synchronous —
/// callers run this on a worker. Non-macOS: error.
#[cfg(target_os = "macos")]
pub fn eject_volume(path: &std::path::Path) -> Result<(), String> {
    file_ops::eject_volume(path)
}

#[cfg(not(target_os = "macos"))]
pub fn eject_volume(_path: &std::path::Path) -> Result<(), String> {
    Err("eject is macOS-only in this build".into())
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

/// Make a Finder-resolvable alias to `target` inside `dest_dir` (used by
/// Cmd+Option alias-drop). Synchronous — callers run this on a worker.
#[cfg(target_os = "macos")]
pub fn make_alias_in(
    target: &std::path::Path,
    dest_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    file_ops::make_alias_in(target, dest_dir)
}

#[cfg(not(target_os = "macos"))]
pub fn make_alias_in(
    _target: &std::path::Path,
    _dest_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
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

/// Force the app-wide native appearance to match the chosen theme,
/// independent of the system Light/Dark setting. Setting `NSApp`'s
/// appearance cascades to every window that doesn't override its own,
/// so native chrome — titlebars, the traffic-light area, menus — on
/// secondary windows (Viewer, Settings) stops rendering system-dark
/// under a light app theme (and vice versa). Main-thread only; a no-op
/// off the main thread or on non-macOS.
#[cfg(target_os = "macos")]
pub fn set_app_appearance(dark: bool) {
    use objc2_app_kit::{
        NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
    };
    use objc2_foundation::MainThreadMarker;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    unsafe {
        let app = NSApplication::sharedApplication(mtm);
        let name = if dark {
            NSAppearanceNameDarkAqua
        } else {
            NSAppearanceNameAqua
        };
        let appearance = NSAppearance::appearanceNamed(name);
        app.setAppearance(appearance.as_deref());
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_app_appearance(_dark: bool) {}

/// macOS "Show Desktop" — the same wallpaper reveal the Dock performs
/// when you click the desktop or hit the Show Desktop shortcut. There
/// is no public API for it, so we resolve the Dock's private
/// `CoreDockSendNotification` symbol at runtime and post the
/// `com.apple.showdesktop.awake` toggle.
///
/// Two functions:
/// - [`show_desktop_available`] — `true` only when we resolved the
///   symbol on a new-enough macOS. UI affordances (toolbar button,
///   menu item) gate their visibility on this.
/// - [`show_desktop`] — performs the reveal, returning whether it
///   dispatched. Both are panic-free: on failure they report
///   unavailable / `false` rather than aborting, so a future OS change
///   that pulls the symbol degrades to "the button quietly disappears."
#[cfg(target_os = "macos")]
mod show_desktop_impl {
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::sync::OnceLock;

    // CoreDockSendNotification(CFStringRef notification, void *unused).
    // The Dock ignores the second argument; passing an extra ignored
    // pointer is safe under the C ABI on both x86_64 and arm64 (unused
    // argument registers are simply not read by the callee).
    type SendNotification = unsafe extern "C" fn(*const c_void, *const c_void);

    // libSystem (always linked on macOS) provides the dynamic loader.
    extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
    const RTLD_LAZY: c_int = 0x1;

    // Frameworks that export `CoreDockSendNotification`, newest-OS first.
    // On macOS 26 (Tahoe) the old private `CoreDock.framework` is gone
    // from the dyld cache; the symbol is vended by the public
    // ApplicationServices umbrella (via its HIServices sub-framework),
    // which is always present. The legacy CoreDock path is kept as a
    // fallback for older systems where it still resolved directly.
    const CANDIDATE_PATHS: &[&str] = &[
        "/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices",
        "/System/Library/PrivateFrameworks/CoreDock.framework/CoreDock",
    ];

    // Floor we trust this private path on. The Show Desktop notification
    // long predates Big Sur, but gating here keeps us off any ancient
    // build where the symbol's contract might differ — and satisfies the
    // "right OS version" guard cheaply.
    const MIN_MAJOR: isize = 11;

    fn os_major() -> isize {
        use objc2_foundation::NSProcessInfo;
        // NSProcessInfo is thread-safe; no MainThreadMarker needed.
        let info = NSProcessInfo::processInfo();
        info.operatingSystemVersion().majorVersion
    }

    /// Resolved function-pointer address, or 0 when unavailable.
    /// Computed once — the `dlopen` happens on the first call only, and
    /// the handle is intentionally leaked for the process lifetime.
    fn resolved_addr() -> usize {
        static CELL: OnceLock<usize> = OnceLock::new();
        *CELL.get_or_init(|| {
            if os_major() < MIN_MAJOR {
                return 0;
            }
            let Ok(sym) = CString::new("CoreDockSendNotification") else {
                return 0;
            };
            // SAFETY: standard dlopen/dlsym against system frameworks.
            // We never dereference `handle`; dlsym returns null when the
            // symbol is absent, which we fold into the 0 sentinel and
            // move on to the next candidate framework.
            for path in CANDIDATE_PATHS {
                let Ok(path) = CString::new(*path) else {
                    continue;
                };
                unsafe {
                    let handle = dlopen(path.as_ptr(), RTLD_LAZY);
                    if handle.is_null() {
                        continue;
                    }
                    let addr = dlsym(handle, sym.as_ptr()) as usize;
                    if addr != 0 {
                        return addr;
                    }
                }
            }
            0
        })
    }

    pub(crate) fn available() -> bool {
        resolved_addr() != 0
    }

    pub(crate) fn trigger() -> bool {
        let addr = resolved_addr();
        if addr == 0 {
            return false;
        }
        // SAFETY: `addr` is a non-null CoreDockSendNotification pointer
        // (validated by resolved_addr). NSString is toll-free bridged to
        // CFStringRef, so its pointer is a valid first argument; the call
        // posts a Dock notification and returns void without retaining
        // the string past the call.
        unsafe {
            let func: SendNotification = std::mem::transmute(addr);
            let name = objc2_foundation::NSString::from_str("com.apple.showdesktop.awake");
            let name_ptr = (&*name as *const objc2_foundation::NSString) as *const c_void;
            func(name_ptr, std::ptr::null());
        }
        true
    }
}

/// `true` when the private Show Desktop path resolved on a supported
/// macOS. See [`show_desktop`]. Cheap after the first call (cached).
#[cfg(target_os = "macos")]
pub fn show_desktop_available() -> bool {
    show_desktop_impl::available()
}

/// Trigger the macOS Show Desktop reveal. Returns `true` if it
/// dispatched, `false` (never panics) if the symbol was unavailable.
#[cfg(target_os = "macos")]
pub fn show_desktop() -> bool {
    show_desktop_impl::trigger()
}

#[cfg(not(target_os = "macos"))]
pub fn show_desktop_available() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn show_desktop() -> bool {
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

/// Write the given paths to the general pasteboard as file URLs —
/// the cross-app file-copy verb (Finder pastes what we copy and vice
/// versa). Replaces the pasteboard's previous contents. Main-thread
/// only (AppKit). docs/features/FILE_OPS.md.
#[cfg(target_os = "macos")]
pub fn clipboard_copy_file_urls(paths: &[&std::path::Path]) {
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardWriting};
    use objc2_foundation::{NSArray, NSString, NSURL};
    if objc2_foundation::MainThreadMarker::new().is_none() {
        return;
    }
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let writers: Vec<objc2::rc::Retained<ProtocolObject<dyn NSPasteboardWriting>>> = paths
            .iter()
            .filter_map(|p| {
                let s = p.to_str()?;
                let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(s), p.is_dir());
                Some(ProtocolObject::from_id(url))
            })
            .collect();
        if writers.is_empty() {
            return;
        }
        let array: objc2::rc::Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>> =
            NSArray::from_vec(writers);
        let _: bool = objc2::msg_send![&*pb, writeObjects: &*array];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn clipboard_copy_file_urls(_paths: &[&std::path::Path]) {}

/// Read file URLs off the general pasteboard (what Cmd+V pastes).
/// Empty when the pasteboard holds no file URLs. Main-thread only.
#[cfg(target_os = "macos")]
pub fn clipboard_read_file_urls() -> Vec<std::path::PathBuf> {
    use objc2::ClassType as _;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSArray, NSURL};
    if objc2_foundation::MainThreadMarker::new().is_none() {
        return Vec::new();
    }
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        let classes: objc2::rc::Retained<NSArray<AnyObject>> = {
            let url_cls: &AnyObject = std::mem::transmute(NSURL::class());
            objc2::msg_send_id![objc2::class!(NSArray), arrayWithObject: url_cls]
        };
        let read: Option<objc2::rc::Retained<NSArray<NSURL>>> =
            objc2::msg_send_id![&*pb, readObjectsForClasses: &*classes, options: std::ptr::null::<AnyObject>()];
        let Some(urls) = read else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..urls.count() {
            let url = urls.objectAtIndex(i);
            let is_file: bool = objc2::msg_send![&*url, isFileURL];
            if !is_file {
                continue;
            }
            if let Some(path) = url.path() {
                out.push(std::path::PathBuf::from(path.to_string()));
            }
        }
        out
    }
}

#[cfg(not(target_os = "macos"))]
pub fn clipboard_read_file_urls() -> Vec<std::path::PathBuf> {
    Vec::new()
}

/// Begin observing volume mount/unmount/rename via NSWorkspace's
/// notification center. The callback runs on the main thread after
/// every change; hosts re-list volumes and fan out. Main-thread-only,
/// idempotent (see [`start_system_theme_observer`] for the contract).
#[cfg(target_os = "macos")]
pub fn start_volume_observer(callback: Box<dyn Fn() + 'static>) {
    volume_observer::start(callback);
}

#[cfg(not(target_os = "macos"))]
pub fn start_volume_observer(_callback: Box<dyn Fn() + 'static>) {}

/// Open a windowless native video player for `path` and start playback,
/// returning a handle (0 on failure). Frames are pulled out as BGRA
/// pixel buffers via [`video_overlay_copy_frame`] and drawn by the gpui
/// host as an image — there is no native overlay NSView. Main-thread
/// only. `on_ended` fires on the main thread when the video plays to
/// the end; it must defer (e.g. through a channel), not call player
/// APIs synchronously. See docs/features/VIEWER.md.
#[cfg(target_os = "macos")]
pub fn video_overlay_show(path: &std::path::Path, on_ended: Box<dyn Fn() + 'static>) -> u64 {
    video_overlay::show(path, on_ended)
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_show(_path: &std::path::Path, _on_ended: Box<dyn Fn() + 'static>) -> u64 {
    0
}

/// Pull the latest decoded frame as tightly-packed BGRA bytes plus its
/// `(width, height)` in pixels, or `None` when no new frame is ready
/// since the last pull (the caller keeps the previous frame / poster).
/// Main-thread only; stale ids give `None`.
#[cfg(target_os = "macos")]
pub fn video_overlay_copy_frame(id: u64) -> Option<(u32, u32, Vec<u8>)> {
    video_overlay::copy_frame(id)
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_copy_frame(_id: u64) -> Option<(u32, u32, Vec<u8>)> {
    None
}

/// Stop playback and remove a video overlay. Main-thread only; stale
/// ids no-op.
#[cfg(target_os = "macos")]
pub fn video_overlay_remove(id: u64) {
    video_overlay::remove(id);
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_remove(_id: u64) {}

/// Pause/resume a live video overlay. Main-thread only; stale ids no-op.
#[cfg(target_os = "macos")]
pub fn video_overlay_set_paused(id: u64, paused: bool) {
    video_overlay::set_paused(id, paused);
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_set_paused(_id: u64, _paused: bool) {}

/// Seek a live video overlay to the start and resume (loop). Main-thread
/// only; stale ids no-op.
#[cfg(target_os = "macos")]
pub fn video_overlay_restart(id: u64) {
    video_overlay::restart(id);
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_restart(_id: u64) {}

/// `(current, duration)` seconds of a live overlay's video; zeros when
/// unknown or for a stale id. Main-thread only.
#[cfg(target_os = "macos")]
pub fn video_overlay_time(id: u64) -> (f64, f64) {
    video_overlay::time(id)
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_time(_id: u64) -> (f64, f64) {
    (0.0, 0.0)
}

/// The current overlay video's intrinsic `(width, height)` in pixels;
/// `(0, 0)` while unknown or for a stale id. Main-thread only.
#[cfg(target_os = "macos")]
pub fn video_overlay_natural_size(id: u64) -> (f64, f64) {
    video_overlay::natural_size(id)
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_natural_size(_id: u64) -> (f64, f64) {
    (0.0, 0.0)
}

/// Seek a live overlay's video to `seconds`. Main-thread only; stale ids no-op.
#[cfg(target_os = "macos")]
pub fn video_overlay_seek(id: u64, seconds: f64) {
    video_overlay::seek(id, seconds);
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_seek(_id: u64, _seconds: f64) {}

/// Step a live overlay's video by `frames` frames (negative = backward).
/// Main-thread only; stale ids no-op.
#[cfg(target_os = "macos")]
pub fn video_overlay_step(id: u64, frames: i64) {
    video_overlay::step(id, frames);
}

#[cfg(not(target_os = "macos"))]
pub fn video_overlay_step(_id: u64, _frames: i64) {}

/// Toggle a window's "stay on top" (floating) level from one of its
/// content NSViews. `floating` true raises it above normal windows;
/// false restores the normal level. Main-thread only; no-op otherwise.
#[cfg(target_os = "macos")]
pub fn set_window_floating(ns_view: *mut std::ffi::c_void, floating: bool) {
    use objc2::{msg_send, msg_send_id, rc::Retained, runtime::AnyObject};
    use objc2_app_kit::NSWindow;
    use objc2_foundation::MainThreadMarker;

    if MainThreadMarker::new().is_none() || ns_view.is_null() {
        return;
    }
    let view: &AnyObject = unsafe { &*(ns_view as *const AnyObject) };
    let window: Option<Retained<NSWindow>> = unsafe { msg_send_id![view, window] };
    if let Some(window) = window {
        // NSFloatingWindowLevel = 3, NSNormalWindowLevel = 0.
        let level: isize = if floating { 3 } else { 0 };
        unsafe {
            let _: () = msg_send![&*window, setLevel: level];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_window_floating(_ns_view: *mut std::ffi::c_void, _floating: bool) {}

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
