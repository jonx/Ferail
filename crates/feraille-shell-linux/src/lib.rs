//! Linux platform shell — **stub scaffold**.
//!
//! This crate is the Linux arm of the `platform_shell` indirection (see
//! [`feraille_gpui::platform_shell`] and `docs/features/linux-port.md`). Its
//! job is to mirror the public `pub fn` surface that `feraille-gpui` reaches
//! through `crate::platform_shell::*`, so that the app *compiles and links* on
//! `target_os = "linux"`. Every function here is currently a no-op / empty
//! stub: the app will build and run, but shell features (clipboard, trash,
//! reveal, open-with, dark-mode, volumes, video) do nothing until filled in.
//!
//! ## Surface contract
//!
//! The signatures below are the exact subset of `feraille-shell-mac` /
//! `feraille-shell-win32` that gpui invokes through the alias — i.e. *the
//! authoritative inventory of what a Linux shell must implement*. They were
//! derived by grepping `platform_shell::<ident>` call sites in `feraille-gpui`
//! and matching each against the canonical macOS signature. Keep this file in
//! lockstep with the other two shell crates: a symbol must exist (real or
//! stub) in all three or the alias stops resolving on some target.
//!
//! Callback bounds intentionally match **macOS** (`Box<dyn Fn(..) + 'static>`,
//! no `Send`) rather than win32's `+ Send`: macOS is the proven-green contract
//! and the loosest bound that gpui's closures are known to satisfy. A future
//! real impl that needs to hand a callback to a D-Bus worker thread can tighten
//! to `+ Send` then (and adjust call sites if required).
//!
//! ## Filling these in
//!
//! Replace a stub body with a real implementation behind
//! `#[cfg(target_os = "linux")]`, keeping a `#[cfg(not(target_os = "linux"))]`
//! no-op twin so the crate still compiles on macOS/Windows as a workspace
//! member. `docs/features/linux-port.md` §6 maps every function below to the
//! freedesktop / D-Bus / XDG mechanism to reach for (ashpd, zbus, trash,
//! arboard, gio, gstreamer, …). Run all blocking work off the UI thread.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use feraille_core::commands::TagColor;
use feraille_core::power::PowerEvent;

// =============================================================
// Shared types (declared with identical shape in every shell crate
// so they round-trip through the `platform_shell` alias).
// =============================================================

/// An application Launch-Services-style "Open With" would offer for a path.
/// On Linux this will come from MIME associations (`mimeapps.list` +
/// `.desktop` entries); see linux-port.md §6.
#[derive(Debug, Clone)]
pub struct OpenWithCandidate {
    pub name: String,
    pub path: PathBuf,
    pub is_default: bool,
}

/// Result of [`set_app_icon_from_png_bytes`], mirrored from shell-mac /
/// shell-win32. `NotMacOs` is retained as a variant name purely so the `Debug`
/// output matches the other shells one-for-one. On Linux the runtime icon swap
/// is a no-op (icon identity comes from the `.desktop` file + Wayland
/// `app_id` / X11 `WM_CLASS`), so this stub returns `NotMacOs`.
#[derive(Debug)]
pub enum SetIconResult {
    Ok,
    NotMacOs,
    NotMainThread,
    DecodeFailed,
}

/// RAII guard that keeps the system awake while held — twin of shell-mac's
/// `SleepBlocker`. The real Linux impl will inhibit idle-sleep via the
/// `org.freedesktop.login1` / portal `Inhibit` API; dropping it releases.
pub struct SleepBlocker;

// =============================================================
// App lifecycle / chrome / about
// =============================================================

/// Configure the About-panel text. No global menu bar on Linux (the title-bar
/// hamburger covers about/settings) — no-op for v1.
pub fn set_about_options(_app_name: &str, _tagline: &str, _version: &str, _copyright: &str) {}

/// Show the About panel. No-op until an in-window about surface exists.
pub fn show_about_panel() {}

/// Whether a "Show Desktop" affordance is available. Linux has no portable
/// minimize-all primitive across compositors — `false` hides the menu item.
pub fn show_desktop_available() -> bool {
    false
}

/// Minimize-all / show desktop. Returns whether it acted. No portable Linux
/// equivalent yet — `false`.
pub fn show_desktop() -> bool {
    false
}

// =============================================================
// Pickers / launching
// =============================================================

/// Pick a folder. Real impl: portal `org.freedesktop.portal.FileChooser` via
/// `ashpd`. `None` = cancelled / unavailable.
pub fn pick_folder() -> Option<PathBuf> {
    None
}

/// Open a URL in the user's handler. Real impl: portal `OpenURI` or
/// `xdg-open`.
pub fn open_url(_url: &str) {}

/// Reveal a path in the user's file manager. Real impl: D-Bus
/// `org.freedesktop.FileManager1.ShowItems`, fallback `xdg-open <parent>`.
pub fn reveal_in_finder(_path: &Path) {}

/// Open a terminal at `path`. Real impl: detection chain over `$TERMINAL` /
/// `x-terminal-emulator` / known emulators.
pub fn open_terminal(_path: &Path) {}

/// Enumerate apps that can open `path`. Real impl: resolve MIME, list handler
/// `.desktop` files.
pub fn open_with_candidates(_path: &Path) -> Vec<OpenWithCandidate> {
    Vec::new()
}

/// Open `target` with a specific application. Real impl: launch the chosen
/// `.desktop` entry / `gio open -a`.
pub fn open_with_app(_target: &Path, _app_path: &Path) -> Result<(), String> {
    Err("open_with_app: not implemented on Linux yet".into())
}

// =============================================================
// Clipboard (file URLs)
// =============================================================

/// Copy file paths to the clipboard as `text/uri-list` `file://` URIs (plus
/// the GNOME `x-special/gnome-copied-files` target for Nautilus interop).
pub fn clipboard_copy_file_urls(_paths: &[&Path]) {}

/// Read file paths previously placed on the clipboard. Empty if none.
pub fn clipboard_read_file_urls() -> Vec<PathBuf> {
    Vec::new()
}

// =============================================================
// File operations
// =============================================================

/// Duplicate a path in place ("foo (copy)"). Real impl: pure `std::fs`
/// (liftable almost verbatim from the win32 impl).
pub fn duplicate_path(_src: &Path) -> Result<PathBuf, String> {
    Err("duplicate_path: not implemented on Linux yet".into())
}

/// Eject / unmount a volume. Real impl: udisks2 `Filesystem.Unmount` /
/// `Drive.Eject` over D-Bus.
pub fn eject_volume(_path: &Path) -> Result<(), String> {
    Err("eject_volume: not implemented on Linux yet".into())
}

/// Make an alias to `target` beside it. Real impl:
/// `std::os::unix::fs::symlink` (trivial).
pub fn make_alias(_target: &Path) -> Result<PathBuf, String> {
    Err("make_alias: not implemented on Linux yet".into())
}

/// Make an alias to `target` inside `dest_dir`. Real impl: symlink.
pub fn make_alias_in(_target: &Path, _dest_dir: &Path) -> Result<PathBuf, String> {
    Err("make_alias_in: not implemented on Linux yet".into())
}

/// Compress `targets` into a `.zip` beside the first. Real impl: reuse the
/// shared `zip`-crate logic (already cross-platform).
pub fn compress_paths(_targets: &[&Path]) -> Result<PathBuf, String> {
    Err("compress_paths: not implemented on Linux yet".into())
}

// =============================================================
// Quick Look equivalent (preview / thumbnails)
// =============================================================

/// Pop a system preview for `paths`. No Quick Look on Linux — route to the
/// in-app preview pane (or optional `sushi` shell-out).
pub fn show_quick_look(_paths: &[&Path]) -> Result<(), String> {
    Err("show_quick_look: not available on Linux".into())
}

/// Fetch a thumbnail as `(rgba_or_png_bytes, width, height)`. Real impl: reuse
/// the freedesktop thumbnail cache (`$XDG_CACHE_HOME/thumbnails`), else
/// generate via gdk-pixbuf / Tumbler.
pub fn fetch_quick_look_thumbnail(_path: &Path, _size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    None
}

// =============================================================
// Tags (Finder color tags) — no portable Linux equivalent
// =============================================================

/// Read the color tags on `path`. No portable native tag system on Linux;
/// candidate backing is private `feraille-meta` SQLite. Empty for now.
pub fn read_canonical_tags(_path: &Path) -> Vec<TagColor> {
    Vec::new()
}

/// Toggle a color tag on `path`. No-op stub (see `read_canonical_tags`).
pub fn toggle_tag(_path: &Path, _color: TagColor) -> Result<(), String> {
    Err("toggle_tag: not available on Linux".into())
}

// =============================================================
// Appearance / theme
// =============================================================

/// Whether the system prefers dark. Real impl: portal
/// `org.freedesktop.portal.Settings` `org.freedesktop.appearance/color-scheme`.
pub fn system_is_dark() -> bool {
    false
}

/// Push the app's chosen appearance to the platform. No-op on Linux (the
/// compositor owns decorations; gpui themes itself).
pub fn set_app_appearance(_dark: bool) {}

/// Observe system light/dark changes. Real impl: subscribe to the portal
/// `SettingChanged` signal and fire `callback` (off-thread).
pub fn start_system_theme_observer(_callback: Box<dyn Fn(bool) + 'static>) {}

// =============================================================
// App identity / icon
// =============================================================

/// Swap the running app icon. **No-op on Linux** — icon identity comes from a
/// `.desktop` file + Wayland `app_id` / X11 `WM_CLASS`, not a runtime swap.
pub fn set_app_icon_from_png_bytes(_png_bytes: &[u8]) -> SetIconResult {
    SetIconResult::NotMacOs
}

/// Set the Windows AppUserModelID. **No-op on Linux** (taskbar-grouping is the
/// Wayland `app_id`'s job).
pub fn set_app_user_model_id(_id: &str) {}

/// The app bundle path. No bundle concept on Linux — `None`.
pub fn app_bundle_path() -> Option<String> {
    None
}

// =============================================================
// Volumes / power
// =============================================================

/// Observe volume mount/unmount. Real impl: udisks2 `InterfacesAdded/Removed`
/// signals (or `GVolumeMonitor`); call `callback` on each change.
pub fn start_volume_observer(_callback: Box<dyn Fn() + 'static>) {}

/// Observe power transitions (sleep/wake). Real impl: `org.freedesktop.login1`
/// `PrepareForSleep` D-Bus signal mapped to [`PowerEvent`].
pub fn start_power_observer(_callback: Box<dyn Fn(PowerEvent) + 'static>) {}

/// Inhibit idle sleep while the returned guard is held. Real impl:
/// login1 / portal `Inhibit`. `None` = no inhibitor active.
pub fn prevent_idle_sleep(_reason: &str) -> Option<SleepBlocker> {
    None
}

// =============================================================
// Window
// =============================================================

/// Make a window float above others (viewer "always on top"). The
/// macOS-shaped `*mut c_void` (an `NSView`) is meaningless on Linux; a real
/// impl will key off the gpui/compositor handle instead. No-op for now.
pub fn set_window_floating(_handle: *mut c_void, _floating: bool) {}

// =============================================================
// Video overlay (windowless player feeding the viewer BGRA frames).
// Real impl: GStreamer / libmpv. Handle 0 = "no video" (matches win32).
// See docs/features/VIEWER.md.
// =============================================================

/// Start a windowless video for `path`; returns an opaque handle (0 = none).
pub fn video_overlay_show(_path: &Path, _on_ended: Box<dyn Fn() + 'static>) -> u64 {
    0
}

/// Copy the current frame as `(width, height, rgba)`.
pub fn video_overlay_copy_frame(_id: u64) -> Option<(u32, u32, Vec<u8>)> {
    None
}

/// Tear down a video overlay.
pub fn video_overlay_remove(_id: u64) {}

/// Pause / resume.
pub fn video_overlay_set_paused(_id: u64, _paused: bool) {}

/// Restart from the beginning.
pub fn video_overlay_restart(_id: u64) {}

/// `(current_seconds, duration_seconds)`.
pub fn video_overlay_time(_id: u64) -> (f64, f64) {
    (0.0, 0.0)
}

/// Natural `(width, height)` of the video in pixels.
pub fn video_overlay_natural_size(_id: u64) -> (f64, f64) {
    (0.0, 0.0)
}

/// Seek to an absolute time in seconds.
pub fn video_overlay_seek(_id: u64, _seconds: f64) {}

/// Step `frames` forward (positive) or back (negative).
pub fn video_overlay_step(_id: u64, _frames: i64) {}
