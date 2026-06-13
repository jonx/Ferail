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
pub mod app_state;
pub mod assets;
pub mod disk_usage;
pub mod favorites;
pub mod favorites_section;
pub mod recents_section;
pub mod file_list;
pub mod folder_sizes;
pub mod fs_watcher;
pub mod icons;
pub mod keyboard_help;
pub mod keymap;
pub mod multi_table;
pub mod obs;
pub mod path_complete;
pub mod prefetch;
pub mod preview;
pub mod process_state;
pub mod reset_db;
pub mod screenshot;
pub mod settings;
pub mod shell;
pub mod status_bar;
pub mod task_panel;
pub mod tasks;
pub mod text_preview;
pub mod tree;
pub mod viewer;

/// Platform shell abstraction. Resolves to `feraille_shell_mac` on
/// macOS and `feraille_shell_win32` on Windows; both crates expose the
/// same `pub fn` / type surface. Call sites in this crate go through
/// `platform_shell::*` so a single cfg switch picks the active impl.
///
/// New shell surfaces should land in **both** shell crates (mac with
/// a real impl, win32 with at least a stub) so the alias keeps
/// compiling on either target. The shell crates' own internal
/// `cfg(not(target_os = "macos"))` / `cfg(not(windows))` arms exist
/// purely so each crate compiles on the *other* host as a workspace
/// member — they're not reached through this alias.
#[cfg(target_os = "macos")]
pub use feraille_shell_mac as platform_shell;
#[cfg(windows)]
pub use feraille_shell_win32 as platform_shell;
