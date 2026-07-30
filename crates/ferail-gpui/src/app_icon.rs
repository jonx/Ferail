//! Platform-specific application icon assets.
//!
//! Windows gets its shell/taskbar icon from `resources/ferail.ico`,
//! embedded by `build.rs`. The PNG here is for runtime surfaces that
//! accept image bytes directly, such as the macOS Dock icon and the
//! in-app About dialog.

#[cfg(target_os = "macos")]
pub const PNG: &[u8] = include_bytes!("../resources/ferail-macos.png");

#[cfg(not(target_os = "macos"))]
pub const PNG: &[u8] = include_bytes!("../resources/ferail.png");
