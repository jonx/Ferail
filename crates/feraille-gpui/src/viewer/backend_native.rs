//! The built-in video provider: the platform-native player behind the
//! [`VideoBackend`] seam.
//!
//! This is a thin adapter — it forwards every call to the existing
//! `platform_shell::video_overlay_*` functions (AVFoundation on macOS, a
//! stub on win32), keyed by the opaque `u64` handle they hand back. The
//! whole point is that the viewer no longer names these functions
//! directly; an alternative provider (mpv) can take their place.
//!
//! NB: the optional provider is **mpv** (`feraille-video-mpv`), gated behind
//! the `mpv` cargo feature and selected via the `video_backend` = `"mpv"`
//! setting, with its libmpv path in `mpv_path` (Settings → Plugins).

use std::path::Path;

use feraille_core::video::{VideoBackend, VideoEnhance, VideoStream};

/// The default libmpv location for this OS, used as the default in
/// Settings → Plugins when the user hasn't set one. The user may point at the
/// libmpv shared library, a directory containing it, or `mpv.app`;
/// [`feraille_video_mpv::backend`] resolves the actual dylib from there (and
/// falls back to the platform's usual install paths).
pub fn default_mpv_path() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        // Homebrew's libmpv; the resolver also probes Intel/`/usr/local`.
        "/opt/homebrew/opt/mpv/lib/libmpv.dylib"
    }
    #[cfg(windows)]
    {
        r"C:\Program Files\mpv"
    }
    #[cfg(target_os = "linux")]
    {
        // Pointed at a dir we load from there; left blank, the loader finds the
        // system libmpv.so by soname. A common explicit location:
        "/usr/lib"
    }
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    {
        ""
    }
}

/// Select the active video provider. `hint` is `Some(path)` when the user
/// picked the mpv provider in Settings → Plugins (resolved once by the viewer,
/// so no settings I/O happens on a hot path). In a build with the `mpv`
/// feature this returns the mpv backend when libmpv loads; otherwise (or on
/// any failure) it falls back to the native player.
pub fn video_backend(hint: Option<&Path>) -> Box<dyn VideoBackend> {
    #[cfg(feature = "mpv")]
    if let Some(path) = hint {
        if let Some(b) = feraille_video_mpv::backend(path) {
            return b;
        }
        // Selected but unavailable → fall through to the native player.
    }
    #[cfg(not(feature = "mpv"))]
    let _ = hint;
    Box::new(NativeBackend)
}

/// Provider that wraps the platform-native windowless player.
pub struct NativeBackend;

impl VideoBackend for NativeBackend {
    fn open(
        &self,
        path: &Path,
        on_ended: Box<dyn Fn() + Send + 'static>,
        _enhance: VideoEnhance,
    ) -> Option<Box<dyn VideoStream>> {
        // AVFoundation has no denoise/sharpen filter chain — `_enhance` is
        // ignored (those controls are hidden for the native player).
        // The native layer's callback is plain `Fn`; a `Send` one coerces.
        let id = crate::platform_shell::video_overlay_show(path, on_ended);
        // The native layer returns 0 when it can't open the media.
        (id != 0).then(|| Box::new(NativeStream { id }) as Box<dyn VideoStream>)
    }
}

/// One live native player, identified by its registry handle.
struct NativeStream {
    id: u64,
}

impl VideoStream for NativeStream {
    fn copy_frame(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        crate::platform_shell::video_overlay_copy_frame(self.id)
    }
    fn set_paused(&mut self, paused: bool) {
        crate::platform_shell::video_overlay_set_paused(self.id, paused);
    }
    fn seek(&mut self, seconds: f64) {
        crate::platform_shell::video_overlay_seek(self.id, seconds);
    }
    fn step(&mut self, frames: i64) {
        crate::platform_shell::video_overlay_step(self.id, frames);
    }
    fn time(&self) -> (f64, f64) {
        crate::platform_shell::video_overlay_time(self.id)
    }
    fn natural_size(&self) -> (f64, f64) {
        crate::platform_shell::video_overlay_natural_size(self.id)
    }
}

impl Drop for NativeStream {
    fn drop(&mut self) {
        crate::platform_shell::video_overlay_remove(self.id);
    }
}
