//! AROS platform shell.
//!
//! The AROS arm of the `platform_shell` indirection (see
//! [`ferail_gpui::platform_shell`]). v1 re-exports the
//! `ferail-shell-linux` surface: on `target_os = "aros"` that crate's
//! `cfg(not(target_os = "linux"))` no-op arms are compiled, which is exactly
//! the safe stub scaffold a new platform starts from (plus the
//! platform-neutral pure-Rust impls like `compress_paths`). The shape is
//! identical to the mac/win32/linux crates, so gpui's alias round-trips.
//!
//! Replacing a re-export with a real AROS implementation (workbench.library
//! reveal, icon.library icons, clipboard.device file URLs, DefIcons
//! thumbnails) is then a local, incremental change in this crate — the same
//! path linux-port.md describes for the Linux arm.

pub use ferail_shell_linux::*;
