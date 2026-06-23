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
    /// Hue rotation, `[-1, 1]` → ±180°.
    pub hue: f32,
    /// Gamma, `[-1, 1]` → a perceptual exponent around 1.0 (0 = neutral).
    pub gamma: f32,
}

impl VideoAdjust {
    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }
}

/// Enhancement filters a backend can bake into the decode at open time:
/// `denoise`, `sharpen`, `banding` (gradient debanding) and `grain` (film
/// grain), each `0..1` (0 = off). Unlike [`VideoAdjust`] (which is live),
/// these sit in the decoder's filter chain — a backend that can't change
/// them live re-opens the stream to apply a new value.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct VideoEnhance {
    pub denoise: f32,
    pub sharpen: f32,
    pub banding: f32,
    pub grain: f32,
}

impl VideoEnhance {
    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }
}

/// A transparent-colour key applied to a layer: pixels within `similarity` of
/// `color` go transparent and `blend` feathers the edge. A backend that can
/// key live (mpv, via a `colorkey` filter) makes the keyed pixels arrive with
/// alpha = 0 so the layer(s) beneath show through. See
/// [docs/features/VIDEO-MPV.md](../../../docs/features/VIDEO-MPV.md).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ChromaKey {
    /// Target colour as `[r, g, b]`, 0..=255.
    pub color: [u8; 3],
    /// How close a pixel must be to `color` to count, `0..1`.
    pub similarity: f32,
    /// Edge feather, `0..1` (0 = hard cut).
    pub blend: f32,
}

/// Opens video streams. The "plugin" that a provider implements to become
/// the viewer's video player.
pub trait VideoBackend {
    /// Open `path` for playback. `on_ended` fires when the clip reaches its
    /// natural end. It may fire on a non-main thread (libvlc raises it from
    /// a decoder thread), so it must be `Send`; the viewer forwards it
    /// through a channel and must not re-enter the backend synchronously.
    ///
    /// `enhance` is the denoise/sharpen filter state to bake in at open
    /// (a backend without such filters ignores it).
    ///
    /// Returns `None` if this backend can't handle the path/format, so the
    /// caller can fall back to another provider.
    fn open(
        &self,
        path: &Path,
        on_ended: Box<dyn Fn() + Send + 'static>,
        enhance: VideoEnhance,
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

    /// Change enhancement filters *live*. Returns `true` if the backend
    /// applied them without a re-open (mpv, via its runtime filter chain), or
    /// `false` (the default) — meaning the caller must re-open the stream to
    /// change `VideoEnhance` (the old libvlc path). `VideoEnhance::default()`
    /// clears them.
    fn set_enhance(&mut self, _enhance: VideoEnhance) -> bool {
        false
    }

    /// Apply or clear a transparent-colour key *live*. Returns `true` if the
    /// backend keyed natively so the keyed pixels carry alpha = 0 (mpv, via a
    /// `colorkey` filter), letting the viewer composite layers beneath; or
    /// `false` (the default) — the viewer keys on the CPU itself. `None`
    /// clears the key.
    fn set_chroma_key(&mut self, _key: Option<ChromaKey>) -> bool {
        false
    }

    /// Mute or unmute this stream's audio. Used to silence composited
    /// background layers so only the focused (top) video is heard. Default
    /// no-op for backends without audio control.
    fn set_muted(&mut self, _muted: bool) {}
}
