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

pub mod about;
pub mod ant_trail;
pub mod app_icon;
pub mod app_state;
pub mod archive;
pub mod archive_create;
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
pub mod folder_sizes;
pub mod fs_watcher;
pub mod grid;
pub mod icons;
pub mod keyboard_help;
pub mod keymap;
pub mod multi_table;
pub mod obs;
pub mod path_complete;
pub mod prefetch;
pub mod preview;
pub mod process_state;
pub mod recents_section;
pub mod redact;
pub mod report;
pub mod reset_db;
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
pub mod task_panel;
pub mod tasks;
pub mod text;
pub mod text_preview;
pub mod thumbnails;
pub mod tool_results;
pub mod trail;
pub mod tree;
pub mod video_poster;
pub mod viewer;
pub mod window_cascade;

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
/// AROS rides the shell-linux stub scaffold re-exported under its own crate
/// name (see `ferail-shell-aros`); real workbench.library / icon.library
/// integrations replace re-exports there incrementally.
#[cfg(target_os = "aros")]
pub use ferail_shell_aros as platform_shell;
