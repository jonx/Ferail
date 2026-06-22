//! VLC video provider — a [`feraille_core::video::VideoBackend`] backed by
//! libvlc, so the viewer plays virtually any container/codec and gets live
//! colour grading *on video* for free (via libvlc's adjust filter).
//!
//! Same frame-pull model as the native backend: libvlc decodes into a
//! buffer we own (vmem "RV32" == BGRA) through format/lock/display
//! callbacks; the viewer pulls the newest frame each tick and draws it as
//! a gpui `img`. No window, no NSView overlay.
//!
//! Loading: libvlc is loaded at runtime via `dlopen` from the VLC.app the
//! user points at in Settings → Plugins (no build-time link, so a default
//! build needs no VLC). libvlc only finds its plugins via `VLC_PLUGIN_PATH`
//! (verified: auto-detect and `--plugin-path` both fail on VLC 3.x), so the
//! backend sets that env var *internally* from the settings path right
//! before `libvlc_new` — config still lives in settings, never the env.
//!
//! Threading: the viewer holds the stream on the main thread, but libvlc
//! runs decode + events on its own threads. The shared [`Ctx`] keeps the
//! decode pointer for the vout thread, hands finished frames across under a
//! mutex, and forwards end-of-clip via a `Send` callback.

// libvlc is loaded at runtime on every desktop OS (macOS / Windows / Linux);
// the loader and on-disk layout differ per platform, all handled inside `imp`.
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
mod imp;

use std::path::Path;

use feraille_core::video::VideoBackend;

/// Build the VLC backend from the user-pointed VLC location (a `VLC.app`
/// bundle on macOS, the install dir on Windows/Linux), or `None` if libvlc
/// can't be loaded/initialised there (caller falls back to the native player).
/// The libvlc instance is created once per process and reused.
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
pub fn backend(vlc_path: &Path) -> Option<Box<dyn VideoBackend>> {
    imp::backend(vlc_path)
}

/// Other targets: no VLC provider.
#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub fn backend(_vlc_path: &Path) -> Option<Box<dyn VideoBackend>> {
    None
}
