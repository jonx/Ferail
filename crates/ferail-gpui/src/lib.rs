//! Library face of the GPUI shell. The binary at `src/main.rs` parses
//! CLI args and dispatches to either the normal GUI run or the headless
//! screenshot path.
//!
//! Everything that's not entry-point glue lives here so the screenshot
//! harness can construct the same view tree as the live app.

// gpui's `#[test]` macro expansion (via `gpui_macros`) plus `gpui::*`
// import depth exceeds the default 128-step recursion limit. The
// recommendation comes straight from rustc's error message.
#![recursion_limit = "256"]

// First, and `#[macro_use]`, so `tr!` / `trc!` / `trn!` are in textual
// scope for every module below (docs/features/LOCALIZATION.md).
#[macro_use]
pub mod i18n;
pub mod about;
pub mod ant_trail;
pub mod app_icon;
pub mod app_state;
pub mod archive;
pub mod archive_convert;
pub mod archive_create;
#[cfg(test)]
mod archive_tests;
pub mod assets;
pub mod boot;
pub mod bulk_rename;
pub mod diagnostics;
pub mod disk_usage;
pub mod dupe_cache;
pub mod elevation;
pub mod entry_info;
pub mod favorite_icon_picker;
pub mod favorites;
pub mod favorites_section;
pub mod feature_settings;
pub mod file_list;
pub mod filter_complete;
pub mod filter_help;
pub mod folder_sizes;
pub mod fs_watcher;
pub mod grid;
pub mod icons;
pub mod keyboard_help;
pub mod keymap;
pub mod locations_section;
pub mod multi_table;
pub mod obs;
pub mod path_complete;
pub mod prefetch;
pub mod preview;
pub mod preview_panel;
pub mod process_state;
pub mod recents_section;
pub mod redact;
pub mod report;
pub mod reset_db;
pub mod safe_mode;
mod scrub_slider;
// The headless screenshot driver is a CLI path with no live UI to
// freeze — the Prime Directive syscall lint doesn't apply to it.
#[allow(clippy::disallowed_methods)]
pub mod screenshot;
pub mod selection_colors;
pub mod setting_panel;
pub mod settings;
pub mod shell;
pub mod special_folders;
pub mod splitter;
pub mod status_bar;
pub mod syntax_extra;
pub mod system_stats;
pub mod task_panel;
pub mod tasks;
pub mod text;
pub mod text_preview;
pub mod thumbnails;
pub mod tool_results;
pub mod trail;
pub mod tree;
pub mod update_check;
pub mod video_poster;
pub mod viewer;
pub mod watchdog;
pub mod window_cascade;

/// Application identity every Ferail window advertises to the desktop
/// environment (`WindowOptions::app_id` → Wayland `app_id` / X11
/// `WM_CLASS`; a no-op on macOS/Windows). Must equal the basename of
/// the installed `ferail.desktop` and the hicolor icon name (see
/// `resources/linux/ferail.desktop` and the cargo-deb assets) —
/// compositors match these by string, and a mismatch means a generic
/// taskbar icon and mis-grouped windows.
pub const APP_ID: &str = "ferail";

/// Base [`gpui::WindowOptions`] every window starts from: all fields
/// default except the desktop-environment identity above. Use
/// `..base_window_options()` where `..Default::default()` would
/// otherwise close a `WindowOptions` literal, so a new window can't
/// silently drop the app_id.
pub fn base_window_options() -> gpui::WindowOptions {
    gpui::WindowOptions {
        app_id: Some(APP_ID.to_owned()),
        ..Default::default()
    }
}

/// [`gpui::WindowOptions`] for a file-manager shell window.
///
/// Adds the half of gpui-component's `TitleBar` contract that is easy to
/// miss. That component draws our own toolbar across the titlebar row *and*
/// carries the code to move the window itself (it flags a mouse-down in the
/// bar, then calls `start_window_move` on the next mouse-move). But that code
/// is dead unless the window also claims the drag: left at gpui's default,
/// `_opaqueRectForWindowMoveWhenInTitlebar` reports an empty rect and **macOS
/// keeps dragging the window natively from the whole titlebar rect, below
/// gpui entirely**. A button up there never notices — a click is not a drag —
/// but any control that *drags* (the grid icon-size slider) moves the window
/// instead, and no amount of `cx.stop_propagation()` can prevent it, because
/// AppKit never asks gpui in the first place. Claiming the drag routes it
/// back through gpui-component, where stopping propagation works, and also
/// drops the titlebar click delay macOS 27 added.
///
/// Only the shell sets this: it is the only window that renders a
/// `gpui_component::TitleBar`, so it is the only one with an app-side
/// window-move to fall back on. Settings, Get Info and the icon picker draw
/// no custom titlebar and must keep AppKit's.
pub fn shell_window_options() -> gpui::WindowOptions {
    gpui::WindowOptions {
        app_owns_titlebar_drag: true,
        titlebar: Some(gpui_component::TitleBar::title_bar_options()),
        ..base_window_options()
    }
}

/// AROS rides the shell-linux stub scaffold re-exported under its own crate
/// name (see `ferail-shell-aros`); real workbench.library / icon.library
/// integrations replace re-exports there incrementally.
#[cfg(target_os = "aros")]
pub use ferail_shell_aros as platform_shell;
#[cfg(target_os = "linux")]
pub use ferail_shell_linux as platform_shell;
/// Platform shell abstraction. Resolves to `ferail_shell_mac` on
/// macOS, `ferail_shell_win32` on Windows, and `ferail_shell_linux`
/// on Linux; all three crates expose the same `pub fn` / type surface.
/// Call sites in this crate go through `platform_shell::*` so a single
/// cfg switch picks the active impl.
///
/// New shell surfaces should land in **all three** shell crates (mac
/// with a real impl, win32 and linux with at least a stub) so the alias
/// keeps compiling on every target. The shell crates' own internal
/// `cfg(not(target_os = "macos"))` / `cfg(not(windows))` /
/// `cfg(not(target_os = "linux"))` arms exist purely so each crate
/// compiles on the *other* hosts as a workspace member — they're not
/// reached through this alias.
///
/// `ferail-shell-linux` is currently an all-stub scaffold; see
/// `docs/features/linux-port.md` for the surface contract and the
/// freedesktop/D-Bus/XDG mechanism each function maps to.
#[cfg(target_os = "macos")]
pub use ferail_shell_mac as platform_shell;
#[cfg(windows)]
pub use ferail_shell_win32 as platform_shell;
