//! The video-player provider seam (plugin point #1).
//!
//! The viewer never names a concrete player. It opens a [`VideoStream`]
//! from whatever [`VideoBackend`] is active and pulls decoded frames from
//! it as tightly-packed BGRA — exactly the windowless frame-pull model the
//! native player already uses, so the video stays a real gpui `img`
//! element (zoom / pan / fit / rotate are the shared still-image path).
//!
//! Providers are selected at runtime (a "Plugins" settings section, not a
//! cargo cfg) and compiled in: the platform-native player is one, an
//! optional VLC backend is another. The trait is deliberately
//! object-shaped (`open` → boxed stream, `Drop` tears the player down) so
//! the viewer holds one `Box<dyn VideoStream>` and ownership maps cleanly.
//!
//! Platform-neutral by construction: only `std` types cross the boundary
//! (a path, a callback, byte buffers), so this can live in `feraille-core`
//! and let an out-of-tree provider crate depend on it without pulling gpui.

use std::path::Path;

/// A view-only colour grade a backend can apply to the decoded video
/// itself. Each field is a signed strength in `[-1, 1]`; all-zero is the
/// neutral identity. Mirrors the viewer's still-image grade so the same
/// adjustments popup drives both — a backend that supports this gets
/// colour grading *on video* without the per-frame CPU pass.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct VideoAdjust {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
}

impl VideoAdjust {
    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }
}

/// Opens video streams. The "plugin" that a provider implements to become
/// the viewer's video player.
pub trait VideoBackend {
    /// Open `path` for playback. `on_ended` fires when the clip reaches its
    /// natural end. It may fire on a non-main thread (libvlc raises it from
    /// a decoder thread), so it must be `Send`; the viewer forwards it
    /// through a channel and must not re-enter the backend synchronously.
    ///
    /// Returns `None` if this backend can't handle the path/format, so the
    /// caller can fall back to another provider.
    fn open(
        &self,
        path: &Path,
        on_ended: Box<dyn Fn() + Send + 'static>,
    ) -> Option<Box<dyn VideoStream>>;
}

/// A live, windowless video stream. Frames are *pulled*, not pushed: the
/// viewer asks for the newest decoded frame each display tick and draws it.
/// Dropping the stream tears the underlying player down.
///
/// All methods are called on the main thread (the viewer's update path).
pub trait VideoStream {
    /// The newest decoded frame as `(width, height, tightly-packed BGRA)`,
    /// or `None` when no new frame is ready since the last call (so a poll
    /// between frames is a cheap no-op).
    fn copy_frame(&mut self) -> Option<(u32, u32, Vec<u8>)>;

    /// Pause or resume playback.
    fn set_paused(&mut self, paused: bool);

    /// Seek to `seconds` from the start.
    fn seek(&mut self, seconds: f64);

    /// Step by `frames` frames (negative = backward). Implies pause.
    fn step(&mut self, frames: i64);

    /// `(position, duration)` in seconds; `(0, 0)` until known.
    fn time(&self) -> (f64, f64);

    /// Intrinsic `(width, height)` in pixels; `(0, 0)` until known.
    fn natural_size(&self) -> (f64, f64);

    /// Apply a colour grade to the video natively. Returns `true` if the
    /// backend handled it (the viewer then skips its CPU grade for video),
    /// or `false` if unsupported (the default) — the viewer keeps grading
    /// frames on the CPU. `VideoAdjust::default()` clears the grade.
    fn set_adjust(&mut self, _adjust: VideoAdjust) -> bool {
        false
    }
}
