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

pub mod app_state;
pub mod assets;
pub mod disk_usage;
pub mod favorites;
pub mod favorites_section;
pub mod file_list;
pub mod fs_watcher;
pub mod icons;
pub mod keyboard_help;
pub mod keymap;
pub mod multi_table;
pub mod obs;
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
pub mod tree;
