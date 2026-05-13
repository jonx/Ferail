//! Library face of the GPUI shell. The binary at `src/main.rs` parses
//! CLI args and dispatches to either the normal GUI run or the headless
//! screenshot path.
//!
//! Everything that's not entry-point glue lives here so the screenshot
//! harness can construct the same view tree as the live app.

pub mod screenshot;
pub mod shell;
