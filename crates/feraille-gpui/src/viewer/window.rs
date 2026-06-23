//! The viewer window entity: playlist navigation, sticky zoom, and
//! (iter 5) slideshow playback. docs/features/VIEWER.md.
//!
//! Each `super::open_viewer` call opens a new, cascaded window, so
//! several files can be viewed at once; each carries its own playlist
//! and view state. Keyboard goes through gpui actions gated on
//! [`VIEWER_CONTEXT`] so Shell shortcuts can't fire here and vice versa.

use crate::text::TextScale as _;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Selectable, Sizable, button::Button, checkbox::Checkbox, h_flex, v_flex,
};

use std::time::Duration;

use feraille_core::video::{ChromaKey, VideoAdjust, VideoEnhance, VideoStream};

use super::backend_native::video_backend;
use super::loader::{self, FrameState, ViewerFrame};
use super::playback::Playback;
use super::stage::{self, StageState, ZoomMode};
use crate::process_state::ProcessState;

/// Key-binding context for the viewer window (`KeyBinding::new(_, _,
/// Some(VIEWER_CONTEXT))` in `keymap::install_extras`).
pub const VIEWER_CONTEXT: &str = "Viewer";

actions!(
    viewer,
    [
        ViewerPrev,
        ViewerNext,
        ViewerLeft,
        ViewerRight,
        ViewerZoomIn,
        ViewerZoomOut,
        ViewerZoomReset,
        ViewerActualSize,
        ViewerToggleFullscreen,
        ViewerTogglePlay,
        ViewerRotateCw,
        ViewerRotateCcw,
        ViewerToggleAdjust,
        ViewerDismiss
    ]
);

/// One playlist item, snapshotted from the file list at open time.
/// The viewer never re-reads the directory (prime directive — the
/// snapshot is the contract; live sync is deferred to watcher work).
#[derive(Clone)]
pub struct PlaylistEntry {
    pub path: PathBuf,
    pub name: String,
}

const TOOLBAR_H: f32 = 44.0;
const STATUS_H: f32 = 28.0;
/// Interactive height of the custom video seek bar (the track sits
/// centred within it; the taller hit area keeps a horizontal drag from
/// slipping off the thin track).
const SEEK_BAR_H: f32 = 18.0;
/// Size (px) of an In/Out cue grip triangle.
const SEEK_GRIP: f32 = 18.0;
/// How close (in px) the cursor must be to a cue to grab it instead of
/// scrubbing the playhead. Kept >= half the grip (plus a margin for the
/// triangle glyph's side-bearing) so a click anywhere on the visible
/// triangle grabs it, rather than just outside it.
const SEEK_GRAB_PX: f32 = 16.0;
/// Per-step zoom factor for the toolbar buttons / Cmd+= / Cmd+-.
const ZOOM_STEP: f32 = 1.25;
/// Fullscreen: hovering within this many px of the window top reveals
/// the hidden toolbar.
const CHROME_REVEAL_STRIP: f32 = 56.0;

/// Extensions the built-in (AVFoundation) player reliably plays. Routed to
/// the video path for *any* backend. Everything else stays a Quick Look
/// poster unless the VLC backend (broad set below) is active. [mac]
const VIDEO_EXTS: &[&str] = &["mp4", "m4v", "mov"];

/// Containers VLC plays that the built-in player can't — only treated as
/// video when the VLC backend is selected (otherwise they'd open as a
/// Quick Look poster image, e.g. a 3GP showing as a still). Not exhaustive
/// — libvlc handles more — but covers the common cases.
const VLC_VIDEO_EXTS: &[&str] = &[
    "mkv", "webm", "avi", "flv", "wmv", "asf", "mpg", "mpeg", "mpe", "m2v", "mpv", "3gp", "3g2",
    "ts", "mts", "m2ts", "vob", "ogv", "ogm", "divx", "rm", "rmvb", "f4v", "mxf", "dv", "qt",
    "amv", "nsv", "y4m", "h264", "hevc", "av1",
];

/// `M:SS` (or `H:MM:SS` past an hour) for the seek-bar time labels.
fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// CPU-rotate a decoded bitmap by `quarter_turns` clockwise (1/2/3).
/// gpui's `img` element has no rotation transform (only `svg` does —
/// docs/GPUI-UPSTREAM.md #5), so view-only rotation re-lays-out the
/// pixels. Channel order is irrelevant here — rotation just moves whole
/// 4-byte pixels — so the loader's BGRA buffer round-trips correctly.
/// Returns `None` for a no-op turn or if the bitmap can't be read.
fn rotate_render_image(img: &RenderImage, quarter_turns: u8) -> Option<Arc<RenderImage>> {
    let qt = quarter_turns % 4;
    if qt == 0 {
        return None;
    }
    let size = img.size(0);
    let (w, h) = (size.width.0 as u32, size.height.0 as u32);
    let buf = image::RgbaImage::from_raw(w, h, img.as_bytes(0)?.to_vec())?;
    let rotated = match qt {
        1 => image::imageops::rotate90(&buf),
        2 => image::imageops::rotate180(&buf),
        _ => image::imageops::rotate270(&buf),
    };
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(rotated)])))
}

/// Wrap tightly-packed BGRA bytes (pulled from the native video player)
/// into a single-frame `RenderImage`. gpui's `RenderImage` stores BGRA
/// directly, so — unlike the still loader's `build_frame` — there is no
/// channel swap. `None` if `bgra` isn't exactly `w * h * 4` bytes.
fn build_video_frame(bgra: Vec<u8>, w: u32, h: u32) -> Option<Arc<RenderImage>> {
    let buf = image::RgbaImage::from_raw(w, h, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
}

/// One composited background layer beneath the primary video — a muted video
/// stream whose latest frame is drawn under the (keyed) top video so the
/// keyed-transparent pixels reveal it (docs/features/VIDEO-MPV.md, layers).
struct BgLayer {
    stream: Box<dyn VideoStream>,
    path: PathBuf,
    name: String,
    frame: Option<Arc<RenderImage>>,
    seq: u64,
}

/// Number of draggable sliders in the adjustments popup. The colour group
/// (brightness/contrast/color, plus hue/gamma for VLC video) and the
/// enhancement group (denoise/sharpen, plus debanding/grain for VLC video).
/// Upscale is buttons, not a slider. The two chroma-key sliders (similarity /
/// blend) bring the mpv-video total to 11.
const SLIDER_COUNT: usize = 11;
/// Longest-edge cap (px) for an upscale, matching the loader's decode cap
/// so a 4× enlargement of a large image can't blow past texture limits.
const UPSCALE_MAX_EDGE: u32 = 8192;

/// A draggable slider in the adjustments popup. The colour ones write to
/// [`ColorAdjust`] (signed, centre-detented); denoise/sharpen write to
/// [`EnhanceParams`] (one-sided 0..1).
#[derive(Clone, Copy, PartialEq, Eq)]
enum SliderId {
    Brightness,
    Contrast,
    /// "Color" in the UI — chroma intensity.
    Saturation,
    /// Hue rotation — VLC video only.
    Hue,
    /// Gamma — VLC video only.
    Gamma,
    Denoise,
    Sharpen,
    /// Gradient debanding (`gradfun`) — VLC video only.
    Banding,
    /// Film grain (`grain`) — VLC video only.
    Grain,
    /// Chroma-key range width (`colorkey` similarity) — mpv video only.
    Similarity,
    /// Chroma-key edge feather (`colorkey` blend) — mpv video only.
    Blend,
}

impl SliderId {
    /// Stable index into the per-slider track-bounds array.
    fn idx(self) -> usize {
        match self {
            SliderId::Brightness => 0,
            SliderId::Contrast => 1,
            SliderId::Saturation => 2,
            SliderId::Hue => 3,
            SliderId::Gamma => 4,
            SliderId::Denoise => 5,
            SliderId::Sharpen => 6,
            SliderId::Banding => 7,
            SliderId::Grain => 8,
            SliderId::Similarity => 9,
            SliderId::Blend => 10,
        }
    }
    fn label(self) -> &'static str {
        match self {
            SliderId::Brightness => "Brightness",
            SliderId::Contrast => "Contrast",
            SliderId::Saturation => "Color",
            SliderId::Hue => "Hue",
            SliderId::Gamma => "Gamma",
            SliderId::Denoise => "Denoise",
            SliderId::Sharpen => "Sharpen",
            SliderId::Banding => "Debanding",
            SliderId::Grain => "Film grain",
            SliderId::Similarity => "Similarity",
            SliderId::Blend => "Blend",
        }
    }
    /// `(min, max)` of the value, and whether it detents to zero at centre.
    /// Colour controls are bipolar; enhancement controls are one-sided.
    fn range(self) -> (f32, f32) {
        match self {
            SliderId::Denoise
            | SliderId::Sharpen
            | SliderId::Banding
            | SliderId::Grain
            | SliderId::Similarity
            | SliderId::Blend => (0.0, 1.0),
            _ => (-1.0, 1.0),
        }
    }
    fn centered(self) -> bool {
        matches!(
            self,
            SliderId::Brightness
                | SliderId::Contrast
                | SliderId::Saturation
                | SliderId::Hue
                | SliderId::Gamma
        )
    }
}

/// View-only colour grade applied to the displayed bitmap. Each field is
/// a signed strength in `[-1, 1]`; all-zero is the neutral identity. Like
/// rotation this is purely in-memory (gpui's `img` has no colour filter —
/// docs/GPUI-UPSTREAM.md), so the pixels are transformed on the CPU and
/// re-uploaded. Window-level: not saved anywhere, but it does carry across
/// navigation so a grade set once applies to every item you flip through.
#[derive(Clone, Copy, PartialEq)]
struct ColorAdjust {
    brightness: f32,
    contrast: f32,
    saturation: f32,
    /// Hue / gamma are VLC-video-only (the still CPU grade ignores them);
    /// they ride here so the shared colour sliders can write them. Both
    /// `[-1, 1]`, 0 = neutral.
    hue: f32,
    gamma: f32,
}

impl Default for ColorAdjust {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 0.0,
            saturation: 0.0,
            hue: 0.0,
            gamma: 0.0,
        }
    }
}

impl ColorAdjust {
    fn is_neutral(&self) -> bool {
        *self == Self::default()
    }
}

/// View-only enhancement for stills: denoise (Gaussian) and sharpen
/// (unsharp mask) as `0..1` strengths, plus an integer upscale factor
/// (1 = off). All run off-thread via [`process_still_pixels`]; neutral by
/// default and, like the colour grade, window-level across navigation.
#[derive(Clone, Copy, PartialEq)]
struct EnhanceParams {
    denoise: f32,
    sharpen: f32,
    upscale: u8,
    /// Debanding (`gradfun`) and film grain (`grain`) are VLC-video-only
    /// filters; the still CPU pipeline ignores them. Both `0..1`, 0 = off.
    banding: f32,
    grain: f32,
}

impl Default for EnhanceParams {
    fn default() -> Self {
        Self {
            denoise: 0.0,
            sharpen: 0.0,
            upscale: 1,
            banding: 0.0,
            grain: 0.0,
        }
    }
}

impl EnhanceParams {
    fn is_neutral(&self) -> bool {
        self.denoise == 0.0 && self.sharpen == 0.0 && self.upscale <= 1
    }
}

/// The full off-thread still pipeline: colour grade → denoise → upscale →
/// sharpen → rotate, over a packed BGRA buffer. Returns the final
/// `(width, height, BGRA bytes)`. Heavy (convolutions + resampling), so it
/// only ever runs on the background executor — never the render path.
///
/// `image`'s blur / resize / unsharpen act per channel (or purely
/// spatially), so the BGRA byte order is irrelevant and round-trips; only
/// the colour grade and the eventual display care about channel identity.
fn process_still_pixels(
    bgra: &[u8],
    w: u32,
    h: u32,
    rot: u8,
    grade: ColorAdjust,
    enh: EnhanceParams,
) -> Option<(u32, u32, Vec<u8>)> {
    use image::imageops;

    let mut buf = bgra.to_vec();
    if !grade.is_neutral() {
        grade_bgra(&mut buf, grade);
    }
    let mut img = image::RgbaImage::from_raw(w, h, buf)?;

    if enh.denoise > 0.0 {
        // 0..1 → a gentle 0..3 px Gaussian radius.
        img = imageops::fast_blur(&img, enh.denoise * 3.0);
    }
    if enh.upscale > 1 {
        let f = enh.upscale as u32;
        let (mut nw, mut nh) = (w.saturating_mul(f), h.saturating_mul(f));
        let longest = nw.max(nh);
        if longest > UPSCALE_MAX_EDGE {
            let s = UPSCALE_MAX_EDGE as f64 / longest as f64;
            nw = ((nw as f64 * s).round() as u32).max(1);
            nh = ((nh as f64 * s).round() as u32).max(1);
        }
        img = imageops::resize(&img, nw, nh, imageops::FilterType::Lanczos3);
    }
    if enh.sharpen > 0.0 {
        // Sharpen *after* any upscale so it crisps the enlarged result.
        // Radius grows with strength; threshold 0 sharpens everything.
        img = imageops::unsharpen(&img, 1.0 + enh.sharpen * 2.0, 0);
    }
    let img = match rot % 4 {
        1 => imageops::rotate90(&img),
        2 => imageops::rotate180(&img),
        3 => imageops::rotate270(&img),
        _ => img,
    };
    Some((img.width(), img.height(), img.into_raw()))
}

/// Apply a [`ColorAdjust`] to a bitmap, returning a fresh `RenderImage`.
/// `None` for a neutral grade (caller reuses the source) or if the
/// bitmap can't be read. Brightness + contrast fold into a single 256-
/// entry LUT; saturation, which mixes channels, runs per pixel only when
/// it's off-neutral. Channel order is BGRA in `RenderImage` storage, so
/// the luma weights index the bytes accordingly. Alpha is untouched.
fn apply_color_adjust(img: &RenderImage, adj: ColorAdjust) -> Option<Arc<RenderImage>> {
    if adj.is_neutral() {
        return None;
    }
    let size = img.size(0);
    let (w, h) = (size.width.0 as u32, size.height.0 as u32);
    let mut buf = img.as_bytes(0)?.to_vec();
    grade_bgra(&mut buf, adj);
    let rgba = image::RgbaImage::from_raw(w, h, buf)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(rgba)])))
}

/// In-place colour grade over a packed BGRA byte buffer (the storage
/// order of `RenderImage`). Brightness + contrast fold into one 256-entry
/// LUT; saturation, which mixes channels, runs per pixel only when it's
/// off-neutral. Alpha (`px[3]`) is left untouched. Pure — no gpui types —
/// so the maths is unit-testable on a plain `Vec<u8>`.
fn grade_bgra(buf: &mut [u8], adj: ColorAdjust) {
    // Classic contrast factor (GIMP/ImageMagick), with the param mapped
    // to the [-128, 128] code range, then brightness as a post-add.
    let c = (adj.contrast * 128.0).clamp(-255.0, 255.0);
    let cf = (259.0 * (c + 255.0)) / (255.0 * (259.0 - c));
    let b = adj.brightness * 255.0;
    let lut: [u8; 256] =
        core::array::from_fn(|i| (cf * (i as f32 - 128.0) + 128.0 + b).clamp(0.0, 255.0) as u8);

    let sat = 1.0 + adj.saturation;
    let do_sat = (sat - 1.0).abs() > f32::EPSILON;
    for px in buf.chunks_exact_mut(4) {
        // Stored BGRA: px[0]=B, px[1]=G, px[2]=R.
        let mut bl = lut[px[0] as usize] as f32;
        let mut g = lut[px[1] as usize] as f32;
        let mut r = lut[px[2] as usize] as f32;
        if do_sat {
            let lum = 0.299 * r + 0.587 * g + 0.114 * bl;
            r = (lum + (r - lum) * sat).clamp(0.0, 255.0);
            g = (lum + (g - lum) * sat).clamp(0.0, 255.0);
            bl = (lum + (bl - lum) * sat).clamp(0.0, 255.0);
        }
        px[0] = bl as u8;
        px[1] = g as u8;
        px[2] = r as u8;
    }
}

/// What a live drag on the custom seek bar is manipulating: the
/// playhead (scrub), or one of the two cue grips.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SeekTarget {
    Playhead,
    CueIn,
    CueOut,
}

/// Read the persisted video-provider choice once at viewer construction —
/// settings I/O must never touch the render path. `Some(path)` selects the
/// VLC provider (effective only in a `vlc`-feature build); `None` keeps the
/// built-in player.
fn resolve_vlc_pref() -> Option<PathBuf> {
    // The VLC provider only exists in a `vlc`-feature build. Without it the
    // saved preference is unhonourable, so refuse it here — otherwise the
    // broad VLC container set (MKV/AVI/3GP…) would be treated as displayable
    // video and mis-routed to the native player, which can't decode it.
    if !cfg!(feature = "vlc") {
        return None;
    }
    let st = crate::app_state::load();
    (st.video_backend.as_deref() == Some("vlc")).then(|| {
        PathBuf::from(
            st.vlc_app_path
                .unwrap_or_else(|| super::backend_native::default_vlc_path().to_string()),
        )
    })
}

pub struct ViewerWindow {
    playlist: Vec<PlaylistEntry>,
    index: usize,
    cache: loader::ViewerCache,
    /// Sticky zoom/pan — survives navigation by design.
    stage: StageState,
    focus_handle: FocusHandle,
    /// Stage-area size captured at render time so keyboard zoom (which
    /// has no cursor position) can anchor at the viewport center.
    last_stage_size: (f32, f32),
    /// Window backing scale factor (1 on standard displays, 2 on
    /// Retina), captured at render time. Content dimensions arrive in
    /// device pixels; the stage math runs in logical points (so does the
    /// viewport), so dims are divided by this before layout — that's what
    /// makes 1:1 one image pixel per *physical* pixel and keeps "fit"
    /// from upscaling sub-viewport content on HiDPI.
    scale_factor: f32,
    /// Last cursor position of an in-flight left-button drag, in
    /// stage-local coordinates. `None` when not dragging.
    drag_last: Option<(f32, f32)>,
    /// Stage area's top edge in window coordinates, captured at render
    /// time (0 in fullscreen, TOOLBAR_H otherwise).
    stage_origin_y: f32,
    /// Slideshow state (docs/features/VIEWER.md §playback).
    playback: Playback,
    /// In fullscreen the chrome hides; hovering the top strip brings
    /// the toolbar back. Pure mouse-position state — no timers.
    chrome_hover: bool,
    /// Title last pushed to the platform window, so render-time title
    /// sync only crosses into AppKit when the text actually changed.
    last_title: String,
    /// Live windowless video player: (the open stream, the entry path it
    /// plays). `None` when the current entry isn't a video. The concrete
    /// player is chosen behind the [`VideoBackend`] seam (native or VLC);
    /// frames are pulled out and drawn through the same stage path as
    /// stills, so the video is a real gpui element (docs/features/VIEWER.md).
    video_overlay: Option<(Box<dyn VideoStream>, PathBuf)>,
    /// Composited background layers beneath the primary video, bottom-first.
    /// Each is a muted video stream pulled in the same poll and drawn under the
    /// keyed top video so its transparent pixels reveal them.
    video_layers: Vec<BgLayer>,
    /// Bumped on every `open`; the frame-pull loop captures it and exits
    /// once it no longer matches (a newer clip opened, or playback ended).
    /// Replaces the old player-handle-as-key now that the handle is boxed.
    video_epoch: u64,
    /// `Some(VLC.app path)` when the user selected the VLC provider in
    /// Settings → Plugins. Resolved once at construction (no settings I/O
    /// on the render path); `None` keeps the native player.
    vlc_pref: Option<PathBuf>,
    /// Whether the active video backend applied the colour grade itself
    /// (VLC does, natively on the GPU/decoder). When true the viewer skips
    /// its per-frame CPU grade for video — the frames already carry it.
    /// Doubles as "the current video stream grades natively" (mpv does).
    video_adjust_native: bool,
    /// The denoise/sharpen/deband/grain filters last pushed to the current
    /// stream. Compared against the live popup values so a slider release only
    /// re-pushes the filter chain when it actually changed (mpv applies it
    /// live — no re-open).
    video_enhance_applied: VideoEnhance,
    /// The latest decoded video frame (unrotated), uploaded as a
    /// `RenderImage` and drawn like any image. `None` until the first
    /// frame lands — the Quick Look poster stands in until then.
    video_frame_image: Option<Arc<RenderImage>>,
    /// Monotonic counter bumped on every new pulled frame; keys the
    /// rotated-frame cache so a rotation re-uses the last rotate.
    video_frame_seq: u64,
    /// One-slot cache of the rotated current frame, keyed by
    /// (frame seq, quarter-turns) — mirrors [`Self::rotated`] for stills
    /// so a rotated video rotates once per frame, not once per render,
    /// and rotates live even while paused. [mac]
    video_rotated: Option<(u64, u8, Arc<RenderImage>)>,
    /// Video frames whose atlas textures must be evicted: a fresh
    /// `RenderImage` per displayed frame would grow VRAM without bound,
    /// so superseded frames queue here and are dropped via
    /// `Window::drop_image` at the top of the next render.
    video_frames_to_drop: Vec<Arc<RenderImage>>,
    /// Intrinsic video size in pixels (from the native player), or (0,0)
    /// while unknown. Kept for the status-strip dimensions display.
    video_dims: (f64, f64),
    /// Whether the current video is paused (we drive play/pause from our
    /// own gpui control since the native controls are hidden).
    video_paused: bool,
    /// Loop the current video instead of advancing the slideshow when it
    /// reaches the end. Viewer-level toggle (the loop checkbox).
    video_loop: bool,
    /// Keep the viewer window above other windows (the stay-on-top
    /// checkbox). Applied to the native window level.
    stay_on_top: bool,
    /// Current video `(position, duration)` in seconds, refreshed by a
    /// poll while a video overlay is live. Drives the seek bar + time.
    video_position: (f64, f64),
    /// In / Out cue points as fractions (0..1) of the duration: playback
    /// is bounded to `[cue_in, cue_out]`. Reset to 0 / 1 whenever a clip
    /// becomes current (not remembered). Drawn as two draggable grips on
    /// the seek bar with the active region shaded between them.
    cue_in: f32,
    cue_out: f32,
    /// Seek-bar track bounds, captured each render (via a `canvas`) so the
    /// custom bar's drag handlers can map a cursor x to a fraction.
    seek_bar_bounds: Bounds<Pixels>,
    /// What the in-flight seek-bar drag is moving (`None` when idle).
    seek_drag: Option<SeekTarget>,
    /// Video-ended events, keyed by entry path so a stale end (user
    /// already navigated away) is dropped instead of advancing.
    video_ended_tx: async_channel::Sender<PathBuf>,
    /// Process singleton — the shared 512 px preview cache doubles as
    /// an instant placeholder while the full-res decode is in flight.
    process: Rc<ProcessState>,
    /// Ephemeral, per-item view rotation in clockwise quarter-turns
    /// (1 = 90°, 2 = 180°, 3 = 270°), keyed by playlist index. Not
    /// saved anywhere and not global — it lives only as long as this
    /// window and applies to one item at a time (docs/features/VIEWER.md).
    /// Absent / 0 means upright. gpui can't rotate an `img` element
    /// (docs/GPUI-UPSTREAM.md), so the pixels are rotated on the CPU.
    rotations: HashMap<usize, u8>,
    /// One-slot cache of the rotated bitmap for the current
    /// (index, quarter-turns), so we rotate once per change instead of
    /// every frame. Invalidated on rotate and on navigation.
    rotated: Option<(usize, u8, Arc<RenderImage>)>,
    /// Live colour grade (brightness / contrast / "color") applied to the
    /// displayed image or video frame. Neutral by default; window-level.
    /// gpui's `img` has no colour filter so the pixels are transformed on
    /// the CPU (see [`grade_bgra`]) — same approach as rotation.
    adjust: ColorAdjust,
    /// Live enhancement (denoise / sharpen / upscale) applied to *stills*
    /// only — too heavy for live video frames. Neutral by default.
    enhance: EnhanceParams,
    /// Whether the adjustments popup is open (toggled by `E`, a right-click
    /// on the stage, or the toolbar button).
    adjust_panel_open: bool,
    /// Transparent-colour (chroma-key) state for mpv video — keyed pixels go
    /// transparent so the stage background (or, later, a lower layer) shows
    /// through. Window-level and view-only, carried across navigation like
    /// the grade. `chroma_color` is RGB.
    chroma_on: bool,
    chroma_color: [u8; 3],
    chroma_similarity: f32,
    chroma_blend: f32,
    /// While true, the next stage click samples a pixel as the key colour
    /// (the swatch arms it); reads from `video_frame_raw`.
    eyedrop_armed: bool,
    /// The latest decoded frame's raw BGRA, kept only while keying or arming
    /// the eyedropper, so a stage click can sample the key colour without
    /// reading back the GPU `RenderImage`.
    video_frame_raw: Option<(u32, u32, Vec<u8>)>,
    /// Screenshot-only: force the adjustments popup to render its full
    /// mpv-video control set (and skip opening a real stream) so the headless
    /// harness can capture the layout without a live frame-pull poll.
    sim_video_panel: bool,
    /// Which slider the in-flight pointer drag is moving (`None` idle).
    slider_drag: Option<SliderId>,
    /// Track bounds for each popup slider, captured each render so a cursor
    /// x maps to a value. Indexed by [`SliderId::idx`].
    slider_bounds: [Bounds<Pixels>; SLIDER_COUNT],
    /// Final processed still for the current (index, turns, grade, enhance),
    /// produced off-thread (grade + denoise + upscale + sharpen + rotate)
    /// since enhancement is far too heavy for the render path. While a
    /// non-matching result is pending the plain rotated original stands in.
    processed: Option<(usize, u8, ColorAdjust, EnhanceParams, Arc<RenderImage>)>,
    /// Monotonic token: a background process result is only accepted if its
    /// token still matches, so superseded runs (params changed mid-flight)
    /// are dropped instead of flashing a stale grade.
    process_gen: u64,
    /// Single-flight guard: at most one process task runs at a time. A
    /// slider drag fires many param changes; without this each would spawn
    /// a fresh full-res (and possibly upscaled) job and they'd pile up into
    /// an out-of-memory crash. New requests during a run are coalesced —
    /// the in-flight task re-checks the latest params when it finishes.
    process_inflight: bool,
    /// One-slot cache of the colour-graded video frame for the current
    /// (frame seq, quarter-turns, grade); the seq changes every frame, so
    /// this de-dups re-renders of the same frame rather than across frames.
    video_adjusted: Option<(u64, u8, ColorAdjust, Arc<RenderImage>)>,
}

impl ViewerWindow {
    pub fn new(
        playlist: Vec<PlaylistEntry>,
        start: usize,
        autoplay: bool,
        process: Rc<ProcessState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        // Re-render on window resize so the image/video refit (FitDown
        // recomputes from the live viewport) and the native video overlay
        // repositions to the new stage.
        cx.observe_window_bounds(window, |_, _, cx| cx.notify())
            .detach();
        let interval = crate::app_state::load()
            .viewer_slideshow_interval
            .unwrap_or(super::playback::DEFAULT_INTERVAL_SECS);
        // Video-ended events arrive from the AppKit main loop with no
        // App context; the drain task re-enters through entity update.
        let (video_ended_tx, video_ended_rx) = async_channel::unbounded::<PathBuf>();
        cx.spawn(async move |this, cx| {
            while let Ok(path) = video_ended_rx.recv().await {
                let Some(this) = this.upgrade() else { break };
                this.update(cx, |v, cx| v.on_video_ended(&path, cx));
            }
        })
        .detach();
        let mut this = Self {
            index: start.min(playlist.len().saturating_sub(1)),
            playlist,
            cache: loader::ViewerCache::default(),
            stage: StageState::default(),
            focus_handle,
            last_stage_size: (1100.0, 760.0 - TOOLBAR_H - STATUS_H),
            scale_factor: 1.0,
            drag_last: None,
            stage_origin_y: TOOLBAR_H,
            playback: Playback::new(interval),
            chrome_hover: false,
            last_title: String::new(),
            video_overlay: None,
            video_layers: Vec::new(),
            video_epoch: 0,
            vlc_pref: resolve_vlc_pref(),
            video_adjust_native: false,
            video_enhance_applied: VideoEnhance::default(),
            video_frame_image: None,
            video_frame_seq: 0,
            video_rotated: None,
            video_frames_to_drop: Vec::new(),
            video_dims: (0.0, 0.0),
            video_paused: false,
            video_loop: false,
            stay_on_top: false,
            video_position: (0.0, 0.0),
            cue_in: 0.0,
            cue_out: 1.0,
            seek_bar_bounds: Bounds::default(),
            seek_drag: None,
            video_ended_tx,
            process,
            rotations: HashMap::new(),
            rotated: None,
            adjust: ColorAdjust::default(),
            enhance: EnhanceParams::default(),
            adjust_panel_open: false,
            chroma_on: false,
            chroma_color: [0, 255, 0],
            chroma_similarity: 0.30,
            chroma_blend: 0.05,
            eyedrop_armed: false,
            video_frame_raw: None,
            sim_video_panel: false,
            slider_drag: None,
            slider_bounds: [Bounds::default(); SLIDER_COUNT],
            processed: None,
            process_gen: 0,
            process_inflight: false,
            video_adjusted: None,
        };
        this.request_current(cx);
        this.prefetch_neighbors(cx);
        // "Slideshow from Here" opens straight into playback.
        if autoplay {
            this.set_playing(true, cx);
        }
        this
    }

    /// Point the live window at a new playlist (user invoked Open
    /// Viewer again). Fresh intent → fresh zoom; the frame cache stays,
    /// revisiting the same folder is instant.
    pub fn retarget(
        &mut self,
        playlist: Vec<PlaylistEntry>,
        start: usize,
        autoplay: bool,
        cx: &mut Context<Self>,
    ) {
        self.index = start.min(playlist.len().saturating_sub(1));
        self.playlist = playlist;
        self.stage = StageState::default();
        // New playlist → indices no longer mean the same items; drop the
        // per-item rotations (they don't outlive a retarget either).
        self.rotations.clear();
        self.rotated = None;
        self.playback.playing = false;
        self.playback.bump();
        self.request_current(cx);
        self.prefetch_neighbors(cx);
        self.schedule_process(cx);
        // "Slideshow from Here" into an already-open viewer starts the
        // show on the new anchor; a plain re-open leaves it paused.
        if autoplay {
            self.set_playing(true, cx);
        }
        cx.notify();
    }

    fn current(&self) -> Option<&PlaylistEntry> {
        self.playlist.get(self.index)
    }

    /// Render-time title sync. The platform call only happens when the
    /// text changed, so per-frame cost is one string compare.
    fn sync_title(&mut self, window: &mut Window) {
        let title = match self.current() {
            Some(e) => format!(
                "{} \u{2014} {} of {}",
                e.name,
                self.index + 1,
                self.playlist.len()
            ),
            None => "Viewer".to_string(),
        };
        if title != self.last_title {
            window.set_window_title(&title);
            self.last_title = title;
        }
    }

    /// Kick the background decode for the current entry unless the
    /// cache already has it (in any state — Pending dedups in-flight
    /// work, Failed prevents retry storms). Same shape as
    /// `preview::request`.
    fn request_current(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.current().map(|e| e.path.clone()) else {
            return;
        };
        self.request_path(path, cx);
    }

    fn request_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.cache.get(&path).is_some() {
            return;
        }
        self.cache.insert(path.clone(), FrameState::Pending);
        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            let p = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { loader::decode_full_res(&p) })
                .await;
            let Some(this) = weak.upgrade() else { return };
            this.update(cx, |this, cx| {
                let state = match result {
                    Some((rgba, w, h)) => {
                        FrameState::Loaded(Arc::new(loader::build_frame(rgba, w, h)))
                    }
                    None => FrameState::Failed,
                };
                this.cache.insert(path, state);
                // If this is the current item and a grade/enhance is live,
                // its full-res frame is now available to process.
                this.schedule_process(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Move through the playlist with wrap-around. The stage state is
    /// deliberately NOT touched — sticky zoom is the feature. Any
    /// navigation (manual or timer-driven) re-arms the slideshow
    /// timer when playing, so manual skips don't stop the show.
    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.playlist.len();
        if len == 0 {
            return;
        }
        self.index = (self.index as isize + delta).rem_euclid(len as isize) as usize;
        self.request_current(cx);
        self.prefetch_neighbors(cx);
        // Re-process for the new item (no-op if neutral, or if its frame is
        // still decoding — the request completion re-triggers it).
        self.schedule_process(cx);
        let epoch = self.playback.bump();
        // Video entries advance on their own end-of-playback event,
        // not the interval timer — a 4-minute clip plays through.
        if self.playback.playing && !self.current_is_video() {
            self.arm_timer(epoch, cx);
        }
        cx.notify();
    }

    // -- video overlay [mac] ---------------------------------------

    /// Whether `path` should open as video for the *active* backend: the
    /// built-in formats always, plus the broad VLC container set when the
    /// VLC backend is selected (so a 3GP/MKV/AVI plays instead of showing a
    /// Quick Look poster).
    fn is_video_path(&self, path: &std::path::Path) -> bool {
        let Some(ext) = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
        else {
            return false;
        };
        VIDEO_EXTS.contains(&ext.as_str())
            || (self.vlc_pref.is_some() && VLC_VIDEO_EXTS.contains(&ext.as_str()))
    }

    /// True when the *current* entry plays as video (slideshow advance is
    /// then driven by the video's end, not the interval timer).
    fn current_is_video(&self) -> bool {
        self.current()
            .map(|e| self.is_video_path(&e.path))
            .unwrap_or(false)
    }

    /// Render-time overlay sync, change-detected like `sync_title`:
    /// attach when the current entry became a video, retarget on nav,
    /// reposition on resize, tear down otherwise. Overlay creation
    /// does no blocking I/O (AVFoundation loads media asynchronously);
    /// steady-state frames compare two tuples and do nothing.
    /// Reconcile the live video player with the current entry: open one
    /// for a freshly-selected video, tear it down when leaving a video.
    /// No geometry/rotation is pushed across the boundary anymore — the
    /// frames are drawn as a gpui image in `stage_area`, so zoom / pan /
    /// fit / rotation are all the shared still-image path.
    fn sync_video(&mut self, cx: &mut Context<Self>) {
        if self.sim_video_panel {
            return; // screenshot fixture: no live stream
        }
        let want = self
            .current()
            .map(|e| e.path.clone())
            .filter(|p| self.is_video_path(p));
        match (&want, &self.video_overlay) {
            (None, None) => {}
            (None, Some(_)) => self.teardown_video(),
            (Some(p), Some((_, current))) if current == p => {}
            (Some(p), _) => {
                self.teardown_video();
                // Cues are not remembered: reset to the whole clip.
                self.cue_in = 0.0;
                self.cue_out = 1.0;
                self.seek_drag = None;
                self.video_position = (0.0, 0.0);
                self.open_video_stream(p.clone(), cx);
            }
        }
    }

    /// The current enhancement filters (denoise / sharpen / debanding /
    /// film grain) a VLC stream would be opened with. Upscale is still-only,
    /// so it's excluded.
    fn video_enhance(&self) -> VideoEnhance {
        VideoEnhance {
            denoise: self.enhance.denoise,
            sharpen: self.enhance.sharpen,
            banding: self.enhance.banding,
            grain: self.enhance.grain,
        }
    }

    /// Open a video stream for `path` via the active backend and start the
    /// frame-pull loop. A fresh open auto-plays; the current enhancement
    /// filters are baked in at open and then changed live as the user drags
    /// the sliders (mpv swaps its filter chain at runtime — no re-open).
    fn open_video_stream(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let tx = self.video_ended_tx.clone();
        let ended_path = path.clone();
        let enhance = self.video_enhance();
        let stream = video_backend(self.vlc_pref.as_deref()).open(
            &path,
            Box::new(move || {
                let _ = tx.try_send(ended_path.clone());
            }),
            enhance,
        );
        if let Some(stream) = stream {
            self.video_epoch = self.video_epoch.wrapping_add(1);
            let epoch = self.video_epoch;
            // Fresh open auto-plays.
            self.video_paused = false;
            self.video_dims = (0.0, 0.0);
            self.video_overlay = Some((stream, path));
            self.video_enhance_applied = enhance;
            // Push any live grade into the backend (mpv applies it natively;
            // the native player reports unsupported).
            self.apply_video_adjust();
            self.start_video_poll(epoch, cx);
        }
    }

    /// Push a changed denoise/sharpen/deband/grain filter set to the live
    /// stream. mpv swaps its filter chain at runtime, so this is a cheap live
    /// update — no re-open, no playhead/pause dance. No-op unless a
    /// native-grading video (mpv) is current and its filters actually changed.
    fn commit_video_enhance(&mut self, cx: &mut Context<Self>) {
        if !self.video_adjust_native || !self.current_is_video() {
            return; // not an mpv video
        }
        let enhance = self.video_enhance();
        if enhance == self.video_enhance_applied {
            return;
        }
        if let Some((stream, _)) = &mut self.video_overlay {
            stream.set_enhance(enhance);
            self.video_enhance_applied = enhance;
            cx.notify();
        }
    }

    fn teardown_video(&mut self) {
        // Dropping the stream tears the underlying player down (the
        // backend's `Drop` does the native remove / mpv teardown).
        drop(self.video_overlay.take());
        // Retire the on-screen frame + its rotated cache so their atlas
        // textures are evicted on the next render.
        if let Some(img) = self.video_frame_image.take() {
            self.video_frames_to_drop.push(img);
        }
        if let Some((_, _, img)) = self.video_rotated.take() {
            self.video_frames_to_drop.push(img);
        }
        self.video_dims = (0.0, 0.0);
        // Drop background layers (their streams tear down on drop) and retire
        // their frames for atlas eviction. Layers don't persist across nav.
        for mut layer in self.video_layers.drain(..) {
            if let Some(img) = layer.frame.take() {
                self.video_frames_to_drop.push(img);
            }
        }
    }

    /// Toggle play/pause of the current video (our gpui control stands in
    /// for the hidden native transport).
    fn toggle_video_paused(&mut self, cx: &mut Context<Self>) {
        self.video_paused = !self.video_paused;
        let paused = self.video_paused;
        let (cur, dur) = self.video_position;
        let (cue_in, cue_out) = (self.cue_in, self.cue_out);
        if let Some((stream, _)) = &mut self.video_overlay {
            // Resuming from a playhead parked at/after the Out cue would
            // immediately re-trigger the Out pause; restart from In so
            // play means "play the region".
            if !paused && dur > 0.0 && cur >= cue_out as f64 * dur - 0.05 {
                stream.seek(cue_in as f64 * dur);
            }
            stream.set_paused(paused);
        }
        cx.notify();
    }

    /// Quiesce playback when the machine or its displays go to sleep
    /// (docs/features/POWER.md): pause a playing video and stop the
    /// slideshow timer. We deliberately do **not** auto-resume on wake —
    /// a clip springing back to life as the screen relights is jarring;
    /// the user hits space. No-op if nothing is playing.
    pub fn suspend_for_power(&mut self, cx: &mut Context<Self>) {
        if !self.video_paused {
            if let Some((stream, _)) = &mut self.video_overlay {
                stream.set_paused(true);
                self.video_paused = true;
            }
        }
        // Halt the slideshow advance timer too, so it doesn't fire the
        // instant we wake and jump the user past a slide.
        if self.playback.playing {
            self.set_playing(false, cx);
        }
        cx.notify();
    }

    /// Step the current video by `frames` frames (negative = backward).
    /// Stepping pauses playback.
    fn step_video(&mut self, frames: i64, cx: &mut Context<Self>) {
        if let Some((stream, _)) = &mut self.video_overlay {
            stream.step(frames);
            self.video_paused = true;
            cx.notify();
        }
    }

    /// Hand the current colour grade to the video backend. A backend that
    /// applies it natively (VLC) returns true, and the viewer then skips
    /// its per-frame CPU grade for video; the native player returns false,
    /// leaving the CPU path in charge. No-op when no video is open.
    fn apply_video_adjust(&mut self) {
        let a = VideoAdjust {
            brightness: self.adjust.brightness,
            contrast: self.adjust.contrast,
            saturation: self.adjust.saturation,
            hue: self.adjust.hue,
            gamma: self.adjust.gamma,
        };
        self.video_adjust_native = match &mut self.video_overlay {
            Some((stream, _)) => stream.set_adjust(a),
            None => false,
        };
    }

    /// The active transparent-colour key, or `None` when the toggle is off.
    fn chroma_key(&self) -> Option<ChromaKey> {
        self.chroma_on.then_some(ChromaKey {
            color: self.chroma_color,
            similarity: self.chroma_similarity,
            blend: self.chroma_blend,
        })
    }

    /// Push the transparent-colour key to the live video stream. mpv keys it
    /// in its filter chain so the keyed pixels arrive with alpha = 0 (the
    /// stage background, or a lower layer, shows through); the native player
    /// reports it unsupported. No-op when no video is open.
    fn apply_video_chroma(&mut self) {
        let key = self.chroma_key();
        if let Some((stream, _)) = &mut self.video_overlay {
            stream.set_chroma_key(key);
        }
    }

    /// Eyedropper: sample the key colour from the live frame at a stage-local
    /// cursor, turn keying on, and disarm. Reads the kept raw BGRA frame
    /// (`video_frame_raw`) through the same stage layout the frame is drawn
    /// with. Rotation isn't accounted for (keying a rotated video is rare —
    /// a follow-up).
    fn pick_chroma_at(&mut self, cursor: (f32, f32), cx: &mut Context<Self>) {
        self.eyedrop_armed = false;
        let Some((w, h, bytes)) = self.video_frame_raw.clone() else {
            cx.notify();
            return;
        };
        let img = self.to_logical((w as f32, h as f32));
        let r = stage::layout(img, self.last_stage_size, self.stage);
        let fx = ((cursor.0 - r.x) / r.w).clamp(0.0, 1.0);
        let fy = ((cursor.1 - r.y) / r.h).clamp(0.0, 1.0);
        let px = ((fx * w as f32) as u32).min(w.saturating_sub(1));
        let py = ((fy * h as f32) as u32).min(h.saturating_sub(1));
        let i = (py as usize * w as usize + px as usize) * 4;
        if i + 2 < bytes.len() {
            self.chroma_color = [bytes[i + 2], bytes[i + 1], bytes[i]]; // BGRA → RGB
            self.chroma_on = true;
            self.apply_video_chroma();
        }
        cx.notify();
    }

    /// Drive the live video at ~display rate while player `id` is the
    /// current one: pull the newest decoded frame (BGRA → `RenderImage`)
    /// and refresh the seek bar's `(position, duration)` + intrinsic
    /// size. Each new frame supersedes the last, which queues for atlas
    /// eviction. Self-terminates when the player changes or goes away.
    ///
    /// The pull runs inside the entity update, i.e. on the main thread —
    /// required, since the native player registry is main-thread-only.
    /// A bounded in-memory frame copy is not a prime-directive blocker
    /// (no I/O / Finder / SQLite); if 4K60 shows main-thread cost, the
    /// follow-up is a CVDisplayLink background pull (docs/GPUI-UPSTREAM.md).
    fn start_video_poll(&self, epoch: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                // ~60 Hz. Frames arrive only as fast as the video's own
                // rate; `copy_frame` returns None between them, so this
                // is a cheap no-op poll when there's nothing new.
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let Some(this) = this.upgrade() else { break };
                let keep = this.update(cx, |this, cx| this.video_poll_tick(epoch, cx));
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

    /// One ~60 Hz poll step for player `id`: refresh position, pull the
    /// newest frame, and enforce the Out cue. Returns whether to keep
    /// polling (false once `id` is no longer the live player).
    fn video_poll_tick(&mut self, epoch: u64, cx: &mut Context<Self>) -> bool {
        // Stop once a newer clip opened (or playback was torn down).
        if self.video_epoch != epoch || self.video_overlay.is_none() {
            return false;
        }
        // Pull position / size / newest frame in one scoped stream borrow.
        let (pos, dims, frame) = {
            let stream = &mut self.video_overlay.as_mut().unwrap().0;
            (stream.time(), stream.natural_size(), stream.copy_frame())
        };
        if dims.0 > 0.0 && dims.1 > 0.0 {
            self.video_dims = dims;
        }
        self.video_position = pos;
        if let Some((w, h, bytes)) = frame {
            // Keep the raw BGRA for the eyedropper while the adjustments popup
            // is open on an mpv video (or while keying/arming) — so the last
            // frame is held even after a pause (videos auto-play on open, so a
            // frame is captured during playback before the user pauses to
            // pick). Cheap no-op otherwise.
            if self.video_adjust_native
                && (self.adjust_panel_open || self.chroma_on || self.eyedrop_armed)
            {
                self.video_frame_raw = Some((w, h, bytes.clone()));
            }
            if let Some(img) = build_video_frame(bytes, w, h) {
                if let Some(old) = self.video_frame_image.replace(img) {
                    self.video_frames_to_drop.push(old);
                }
                self.video_frame_seq = self.video_frame_seq.wrapping_add(1);
            }
        }
        // Pull each background layer's newest frame (muted, plays independently
        // of the primary). Collect retired frames out-of-loop to avoid a
        // double borrow of `self`.
        let mut layer_drop = Vec::new();
        for layer in &mut self.video_layers {
            if let Some((w, h, bytes)) = layer.stream.copy_frame() {
                if let Some(img) = build_video_frame(bytes, w, h) {
                    if let Some(old) = layer.frame.replace(img) {
                        layer_drop.push(old);
                    }
                    layer.seq = layer.seq.wrapping_add(1);
                }
            }
        }
        self.video_frames_to_drop.extend(layer_drop);
        // Enforce the Out cue. A full-length Out (1.0) is the clip's
        // natural end — left to the end-of-play notification
        // (`on_video_ended`) so we don't race it; only a real trim
        // (`cue_out < 1.0`) is enforced here.
        let (cur, dur) = self.video_position;
        if dur > 0.0 && self.cue_out < 1.0 && cur >= self.cue_out as f64 * dur {
            if self.video_loop {
                // Region repeats: jump back to the In cue and keep playing.
                let in_s = self.cue_in as f64 * dur;
                if let Some((stream, _)) = &mut self.video_overlay {
                    stream.seek(in_s);
                    stream.set_paused(false);
                }
                self.video_paused = false;
            } else if self.playback.playing {
                // Slideshow: an Out cue acts as the clip's end → advance.
                self.step(1, cx);
                return false;
            } else {
                // Not looping, not a slideshow: pause at the Out cue.
                if let Some((stream, _)) = &mut self.video_overlay {
                    stream.set_paused(true);
                }
                self.video_paused = true;
            }
        }
        cx.notify();
        true
    }

    /// A video played to its end. Only advances when the show is
    /// playing AND the event belongs to the entry still on screen —
    /// ends queued behind a manual navigation are dropped.
    fn on_video_ended(&mut self, path: &PathBuf, cx: &mut Context<Self>) {
        let current = self.current().map(|e| e.path.clone());
        if current.as_ref() != Some(path) {
            return;
        }
        // Loop takes precedence: replay from the In cue (0 by default).
        if self.video_loop {
            let (_, dur) = self.video_position;
            let in_s = self.cue_in as f64 * dur;
            if let Some((stream, _)) = &mut self.video_overlay {
                stream.seek(in_s);
                stream.set_paused(false);
            }
            return;
        }
        if self.playback.playing {
            self.step(1, cx);
        }
    }

    // -- slideshow --------------------------------------------------

    fn set_playing(&mut self, playing: bool, cx: &mut Context<Self>) {
        if self.playback.playing == playing {
            return;
        }
        self.playback.playing = playing;
        let epoch = self.playback.bump();
        if playing && !self.current_is_video() {
            self.arm_timer(epoch, cx);
        }
        cx.notify();
    }

    /// One-shot timer tick. The epoch check makes stale ticks inert —
    /// the same staleness idiom enumeration cancel flags use.
    fn arm_timer(&mut self, epoch: u64, cx: &mut Context<Self>) {
        let interval = Duration::from_secs(self.playback.interval_secs);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(interval).await;
            let Some(this) = this.upgrade() else { return };
            this.update(cx, |this, cx| {
                if this.playback.playing && this.playback.epoch == epoch {
                    this.step(1, cx);
                }
            });
        })
        .detach();
    }

    fn cycle_interval(&mut self, cx: &mut Context<Self>) {
        self.playback.interval_secs = Playback::next_interval(self.playback.interval_secs);
        if self.playback.playing && !self.current_is_video() {
            let epoch = self.playback.bump();
            self.arm_timer(epoch, cx);
        }
        let mut state = crate::app_state::load();
        state.viewer_slideshow_interval = Some(self.playback.interval_secs);
        crate::app_state::save(&state);
        cx.notify();
    }

    /// Warm the cache for the entries on either side of the current
    /// one so the next arrow press is usually instant. Silent — no
    /// task registry entries for speculative work.
    fn prefetch_neighbors(&mut self, cx: &mut Context<Self>) {
        let len = self.playlist.len();
        if len <= 1 {
            return;
        }
        for d in [1isize, -1] {
            let ix = (self.index as isize + d).rem_euclid(len as isize) as usize;
            if let Some(path) = self.playlist.get(ix).map(|e| e.path.clone()) {
                // Full-res decode for the eventual swap, plus the cheap
                // 512 px Quick Look thumbnail. The thumbnail lands first
                // and stands in instantly when the slideshow advances,
                // so the dwell on the current slide is spent rendering
                // the next one.
                self.request_path(path.clone(), cx);
                crate::preview::warm(&self.process, path, cx);
            }
        }
    }

    fn current_frame(&mut self) -> Option<Arc<ViewerFrame>> {
        let path = self.current()?.path.clone();
        match self.cache.get(&path) {
            Some(FrameState::Loaded(f)) => Some(f),
            _ => None,
        }
    }

    /// Keyboard/toolbar zoom anchors at the viewport center (wheel
    /// zoom, which has a real cursor, lands in iter 4).
    /// Intrinsic dimensions of the current item for zoom/pan math: the
    /// image frame size, or the (rotation-adjusted) video size. None
    /// until known.
    fn content_dims(&mut self) -> Option<(f32, f32)> {
        // Video first: an eligible video usually also has a Quick Look
        // *poster* in the loader cache (a different, smaller size), so we
        // must NOT let `current_frame()` below answer for it — zoom / pan
        // / the % readout have to track the frame `video_stage` actually
        // renders. Prefer the pulled frame's size, then the intrinsic
        // player size; only fall through to the poster before either is
        // known.
        if self.current_is_video() {
            let rot_swaps = self.current_rotation() % 2 == 1;
            if let Some(img) = &self.video_frame_image {
                let sz = img.size(0);
                let (w, h) = (sz.width.0 as f32, sz.height.0 as f32);
                let dims = if rot_swaps { (h, w) } else { (w, h) };
                return Some(self.to_logical(dims));
            }
            let (vw, vh) = self.video_dims;
            if vw > 0.0 && vh > 0.0 {
                let dims = if rot_swaps {
                    (vh as f32, vw as f32)
                } else {
                    (vw as f32, vh as f32)
                };
                return Some(self.to_logical(dims));
            }
        }
        if let Some(f) = self.current_frame() {
            return Some(self.to_logical((f.w as f32, f.h as f32)));
        }
        None
    }

    /// Convert content pixel dimensions (device pixels — as decoded or
    /// pulled from the video) into logical points by the window scale
    /// factor, the unit the stage math and viewport use. This is what
    /// makes 1:1 = one image pixel per physical pixel, and keeps "fit"
    /// from upscaling content smaller than the viewport on HiDPI.
    fn to_logical(&self, dims: (f32, f32)) -> (f32, f32) {
        let sf = self.scale_factor.max(1.0);
        (dims.0 / sf, dims.1 / sf)
    }

    fn zoom_by(&mut self, factor: f32, cx: &mut Context<Self>) {
        let Some(img) = self.content_dims() else {
            return;
        };
        let view = self.last_stage_size;
        let center = (view.0 / 2.0, view.1 / 2.0);
        self.stage = stage::zoom_at(self.stage, center, img, view, factor);
        self.set_playing(false, cx);
        cx.notify();
    }

    /// Stage-local coordinates for a window-space event position (the
    /// stage sits directly under the toolbar, or at the window top in
    /// fullscreen; render keeps `stage_origin_y` current).
    fn stage_local(&self, position: Point<Pixels>) -> (f32, f32) {
        (
            position.x.as_f32(),
            position.y.as_f32() - self.stage_origin_y,
        )
    }

    /// Wheel = zoom toward the cursor (image-viewer convention; the
    /// stage has nothing to scroll). Exponential mapping keeps trackpad
    /// and clicky-wheel speeds comparable.
    fn on_stage_scroll(
        &mut self,
        e: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dy = e.delta.pixel_delta(window.line_height()).y.as_f32();
        if dy == 0.0 {
            return;
        }
        let Some(img) = self.content_dims() else {
            return;
        };
        let factor = 2.0_f32.powf(dy / 240.0);
        let cursor = self.stage_local(e.position);
        self.stage = stage::zoom_at(self.stage, cursor, img, self.last_stage_size, factor);
        // Zooming means the user is inspecting — auto-advancing under
        // them is exactly the "intrusive" behavior we're avoiding.
        self.set_playing(false, cx);
        cx.notify();
    }

    fn on_stage_mouse_down(
        &mut self,
        e: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Eyedropper: while armed, a stage click samples the key colour from
        // the live frame instead of dismissing the popup or starting a pan.
        if self.eyedrop_armed {
            self.pick_chroma_at(self.stage_local(e.position), cx);
            return;
        }
        // A click on the stage (i.e. outside the popup, which swallows its
        // own clicks) dismisses the adjustments popup.
        if self.adjust_panel_open {
            self.adjust_panel_open = false;
            cx.notify();
        }
        if e.click_count >= 2 {
            self.toggle_actual_at(self.stage_local(e.position), cx);
            self.drag_last = None;
            return;
        }
        self.drag_last = Some(self.stage_local(e.position));
    }

    fn on_stage_mouse_move(
        &mut self,
        e: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Fullscreen chrome reveal: hovering the top strip shows the
        // toolbar. Pure position state — only notify on transitions.
        if window.is_fullscreen() {
            let want = e.position.y.as_f32() < CHROME_REVEAL_STRIP;
            if want != self.chrome_hover {
                self.chrome_hover = want;
                cx.notify();
            }
        }
        if e.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some(last) = self.drag_last else { return };
        let now = self.stage_local(e.position);
        let delta = (now.0 - last.0, now.1 - last.1);
        self.drag_last = Some(now);
        if let Some(img) = self.content_dims() {
            self.stage = stage::pan_by(self.stage, delta, img, self.last_stage_size);
            self.set_playing(false, cx);
            cx.notify();
        }
    }

    fn end_drag(&mut self) {
        self.drag_last = None;
    }

    /// Double-click: fit ↔ 1:1. Going to 1:1 centers the viewport on
    /// the clicked image point so "double-click the detail" inspects
    /// that detail.
    fn toggle_actual_at(&mut self, cursor: (f32, f32), cx: &mut Context<Self>) {
        let Some(f) = self.current_frame() else {
            return;
        };
        if self.stage.mode == ZoomMode::Actual {
            self.stage = StageState::default();
        } else {
            let img = self.to_logical((f.w as f32, f.h as f32));
            let r = stage::layout(img, self.last_stage_size, self.stage);
            let frac = (
                ((cursor.0 - r.x) / r.w).clamp(0.0, 1.0),
                ((cursor.1 - r.y) / r.h).clamp(0.0, 1.0),
            );
            self.stage = StageState {
                mode: ZoomMode::Actual,
                center: frac,
            };
        }
        self.set_playing(false, cx);
        cx.notify();
    }

    // -- action handlers ------------------------------------------

    fn on_prev(&mut self, _: &ViewerPrev, _window: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, cx);
    }

    fn on_next(&mut self, _: &ViewerNext, _window: &mut Window, cx: &mut Context<Self>) {
        self.step(1, cx);
    }

    /// Left arrow. On a video it steps one frame back (pausing); on a
    /// still it's plain previous-entry navigation. Up/Down stay on
    /// `on_prev`/`on_next` so a video is still navigable from the
    /// keyboard while Left/Right scrub it frame by frame.
    fn on_left(&mut self, _: &ViewerLeft, _window: &mut Window, cx: &mut Context<Self>) {
        if self.current_is_video() {
            self.step_video(-1, cx);
        } else {
            self.step(-1, cx);
        }
    }

    /// Right arrow — mirror of `on_left`: one frame forward on a video,
    /// next-entry on a still.
    fn on_right(&mut self, _: &ViewerRight, _window: &mut Window, cx: &mut Context<Self>) {
        if self.current_is_video() {
            self.step_video(1, cx);
        } else {
            self.step(1, cx);
        }
    }

    /// Space. On a video it toggles the clip's own play/pause; on a
    /// still it toggles the slideshow.
    fn on_toggle_play(
        &mut self,
        _: &ViewerTogglePlay,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.current_is_video() {
            self.toggle_video_paused(cx);
        } else {
            let playing = self.playback.playing;
            self.set_playing(!playing, cx);
        }
    }

    fn on_zoom_in(&mut self, _: &ViewerZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(ZOOM_STEP, cx);
    }

    fn on_zoom_out(&mut self, _: &ViewerZoomOut, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(1.0 / ZOOM_STEP, cx);
    }

    fn on_zoom_reset(&mut self, _: &ViewerZoomReset, _window: &mut Window, cx: &mut Context<Self>) {
        self.stage = StageState::default();
        cx.notify();
    }

    fn on_actual_size(
        &mut self,
        _: &ViewerActualSize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Toggle: 1:1 ↔ fit, keeping the pan center so the user stays
        // on the region they were inspecting.
        self.stage.mode = match self.stage.mode {
            ZoomMode::Actual => ZoomMode::FitDown,
            _ => ZoomMode::Actual,
        };
        cx.notify();
    }

    fn on_toggle_fullscreen(
        &mut self,
        _: &ViewerToggleFullscreen,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        window.toggle_fullscreen();
    }

    fn on_rotate_cw(&mut self, _: &ViewerRotateCw, _window: &mut Window, cx: &mut Context<Self>) {
        self.rotate_by(1, cx);
    }

    fn on_rotate_ccw(&mut self, _: &ViewerRotateCcw, _window: &mut Window, cx: &mut Context<Self>) {
        self.rotate_by(-1, cx);
    }

    /// Current item's view rotation in clockwise quarter-turns (0..=3).
    fn current_rotation(&self) -> u8 {
        self.rotations.get(&self.index).copied().unwrap_or(0)
    }

    /// Rotate the current item by `delta` quarter-turns (+1 CW / -1 CCW),
    /// view-only and per-item. Works for images (CPU bitmap rotate) and
    /// videos (native layer transform, applied in `sync_video`); both are
    /// in-memory/GPU only — nothing is written to disk.
    fn rotate_by(&mut self, delta: i8, cx: &mut Context<Self>) {
        if self.current().is_none() {
            return;
        }
        let next = (self.current_rotation() as i8 + delta).rem_euclid(4) as u8;
        if next == 0 {
            self.rotations.remove(&self.index);
        } else {
            self.rotations.insert(self.index, next);
        }
        // Drop the cached rotated bitmap; the stage rebuilds it on demand.
        self.rotated = None;
        // Rotation changes the processed key (the pipeline bakes in the
        // turn), so re-process if a grade/enhance is live.
        self.schedule_process(cx);
        cx.notify();
    }

    /// Esc — close the adjustments popup first, then leave fullscreen,
    /// then close the window.
    fn on_dismiss(&mut self, _: &ViewerDismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.adjust_panel_open {
            self.adjust_panel_open = false;
            cx.notify();
        } else if window.is_fullscreen() {
            window.toggle_fullscreen();
        } else {
            window.remove_window();
        }
    }

    /// `E` / toolbar button / right-click — toggle the colour-adjust popup.
    fn on_toggle_adjust(
        &mut self,
        _: &ViewerToggleAdjust,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.adjust_panel_open = !self.adjust_panel_open;
        cx.notify();
    }

    /// Force the adjustments popup open. Used by the headless screenshot
    /// harness (`--viewer-adjust`) so the colour/enhance panel can be
    /// captured without a live keystroke.
    pub fn open_adjust_panel(&mut self) {
        self.adjust_panel_open = true;
    }

    /// Screenshot-only fixture: open the adjustments popup with the full
    /// mpv-video control set (colour + enhance + transparent-colour) visible,
    /// without opening a live video stream. Lets the headless harness capture
    /// the panel layout (the real video poll would never let it settle).
    pub fn sim_full_adjust_panel(&mut self) {
        self.sim_video_panel = true;
        self.video_adjust_native = true;
        self.chroma_on = true;
        self.adjust_panel_open = true;
    }

    /// Current value of a popup slider (reads `adjust` or `enhance`).
    fn slider_value(&self, id: SliderId) -> f32 {
        match id {
            SliderId::Brightness => self.adjust.brightness,
            SliderId::Contrast => self.adjust.contrast,
            SliderId::Saturation => self.adjust.saturation,
            SliderId::Hue => self.adjust.hue,
            SliderId::Gamma => self.adjust.gamma,
            SliderId::Denoise => self.enhance.denoise,
            SliderId::Sharpen => self.enhance.sharpen,
            SliderId::Banding => self.enhance.banding,
            SliderId::Grain => self.enhance.grain,
            SliderId::Similarity => self.chroma_similarity,
            SliderId::Blend => self.chroma_blend,
        }
    }

    /// Map a window-space x to a slider's value, clamped to its range, with
    /// a small detent that snaps a bipolar control near-centre to neutral.
    fn slider_value_at(&self, x: Pixels, id: SliderId) -> f32 {
        let b = self.slider_bounds[id.idx()];
        let w = b.size.width.as_f32();
        let (lo, hi) = id.range();
        if w <= 0.0 {
            return lo;
        }
        let frac = ((x.as_f32() - b.origin.x.as_f32()) / w).clamp(0.0, 1.0);
        let v = lo + frac * (hi - lo);
        if id.centered() && v.abs() < 0.04 {
            0.0
        } else {
            v
        }
    }

    /// Commit a slider value to the matching `adjust`/`enhance` field, then
    /// re-process. The slider thumb tracks instantly; the (heavy) bitmap
    /// recompute is scheduled off-thread so the UI never stalls.
    fn set_slider(&mut self, id: SliderId, v: f32, cx: &mut Context<Self>) {
        match id {
            SliderId::Brightness => self.adjust.brightness = v,
            SliderId::Contrast => self.adjust.contrast = v,
            SliderId::Saturation => self.adjust.saturation = v,
            SliderId::Hue => self.adjust.hue = v,
            SliderId::Gamma => self.adjust.gamma = v,
            SliderId::Denoise => self.enhance.denoise = v,
            SliderId::Sharpen => self.enhance.sharpen = v,
            SliderId::Banding => self.enhance.banding = v,
            SliderId::Grain => self.enhance.grain = v,
            SliderId::Similarity => self.chroma_similarity = v,
            SliderId::Blend => self.chroma_blend = v,
        }
        self.after_adjust_change(cx);
    }

    /// Set the integer upscale factor (1 = off).
    fn set_upscale(&mut self, factor: u8, cx: &mut Context<Self>) {
        self.enhance.upscale = factor.max(1);
        self.after_adjust_change(cx);
    }

    /// Restore the neutral grade *and* enhancement.
    fn reset_adjust(&mut self, cx: &mut Context<Self>) {
        self.adjust = ColorAdjust::default();
        self.enhance = EnhanceParams::default();
        self.after_adjust_change(cx);
    }

    /// Shared tail for any grade/enhance change: drop the now-stale video
    /// grade (stills go through `processed`), kick off a fresh background
    /// process, and repaint.
    fn after_adjust_change(&mut self, cx: &mut Context<Self>) {
        if let Some((.., old)) = self.video_adjusted.take() {
            self.video_frames_to_drop.push(old);
        }
        // A live video gets the colour grade pushed to its backend (VLC
        // applies it natively; native player keeps the CPU path).
        if self.current_is_video() {
            self.apply_video_adjust();
            if self.chroma_on {
                self.apply_video_chroma();
            }
        }
        self.schedule_process(cx);
        cx.notify();
    }

    /// Spawn the off-thread still pipeline for the current item if its
    /// result isn't already cached. No-op for a neutral grade+enhance (the
    /// plain image is shown) or when the full-res frame hasn't decoded yet
    /// (a later [`Self::request_path`] completion re-triggers this).
    fn schedule_process(&mut self, cx: &mut Context<Self>) {
        if self.current_is_video() {
            return;
        }
        let neutral = self.adjust.is_neutral() && self.enhance.is_neutral();
        if neutral {
            // Nothing to apply — release any cached result so the plain
            // image shows immediately.
            if let Some((.., old)) = self.processed.take() {
                self.video_frames_to_drop.push(old);
            }
            return;
        }
        let path = match self.current() {
            Some(e) => e.path.clone(),
            None => return,
        };
        let frame = match self.cache.get(&path) {
            Some(FrameState::Loaded(f)) => f.clone(),
            _ => return, // still decoding; retried on load completion
        };
        let rot = self.current_rotation();
        let (idx, grade, enh) = (self.index, self.adjust, self.enhance);
        // Already have (or are about to show) the exact result?
        if matches!(
            &self.processed,
            Some((i, r, g, e, _)) if *i == idx && *r == rot && *g == grade && *e == enh
        ) {
            return;
        }
        // Single-flight: don't pile up heavy jobs. The running task re-runs
        // `schedule_process` on completion, so it converges to these params.
        if self.process_inflight {
            return;
        }
        let Some(src) = frame.image.as_bytes(0).map(|b| b.to_vec()) else {
            return;
        };
        let (w, h) = (frame.w, frame.h);
        self.process_gen += 1;
        let token = self.process_gen;
        self.process_inflight = true;
        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            let out = cx
                .background_executor()
                .spawn(async move { process_still_pixels(&src, w, h, rot, grade, enh) })
                .await;
            let Some(this) = weak.upgrade() else { return };
            this.update(cx, |this, cx| {
                this.process_inflight = false;
                // `out` is None only on a genuine pipeline failure (bad
                // dims). Bail without rescheduling — re-running the same
                // failing job would be a tight crash loop.
                let Some((rw, rh, buf)) = out else { return };
                if this.process_gen == token {
                    if let Some(img) = build_video_frame(buf, rw, rh) {
                        if let Some((.., old)) = this.processed.replace((idx, rot, grade, enh, img))
                        {
                            this.video_frames_to_drop.push(old);
                        }
                        cx.notify();
                    }
                }
                // Catch up to the live params if they changed mid-flight
                // (a no-op once the cached result matches them).
                this.schedule_process(cx);
            });
        })
        .detach();
    }

    /// The processed (grade+enhance+rotate) still for the current item, if
    /// its cached result matches the live params. `None` while neutral or
    /// while a fresh result is still computing (caller shows the original).
    fn processed_still(&self, rot: u8) -> Option<Arc<RenderImage>> {
        match &self.processed {
            Some((i, r, g, e, img))
                if *i == self.index && *r == rot && *g == self.adjust && *e == self.enhance =>
            {
                Some(img.clone())
            }
            _ => None,
        }
    }

    /// Apply the current grade to a resolved video frame, reusing the
    /// one-slot cache keyed by (frame seq, turns, grade).
    fn graded_video(&mut self, base: Arc<RenderImage>, rot: u8) -> Arc<RenderImage> {
        if self.adjust.is_neutral() {
            return base;
        }
        let seq = self.video_frame_seq;
        let fresh = matches!(
            &self.video_adjusted,
            Some((s, r, a, _)) if *s == seq && *r == rot && *a == self.adjust
        );
        if !fresh {
            if let Some(graded) = apply_color_adjust(&base, self.adjust) {
                if let Some((.., old)) =
                    self.video_adjusted.replace((seq, rot, self.adjust, graded))
                {
                    self.video_frames_to_drop.push(old);
                }
            }
        }
        match &self.video_adjusted {
            Some((s, r, a, img)) if *s == seq && *r == rot && *a == self.adjust => img.clone(),
            _ => base,
        }
    }

    // -- render pieces --------------------------------------------

    fn toolbar(&mut self, cx: &mut Context<Self>) -> Div {
        let count = self.playlist.len();
        let counter = format!("{} / {}", (self.index + 1).min(count), count);
        let zoom_label = self
            .content_dims()
            .map(|img| {
                let s = stage::effective_scale(self.stage.mode, img, self.last_stage_size);
                format!("{:.0}%", s * 100.0)
            })
            .unwrap_or_else(|| "\u{2014}".to_string());
        let actual = self.stage.mode == ZoomMode::Actual;
        let name = self.current().map(|e| e.name.clone()).unwrap_or_default();
        // Custom video transport + window state (native controls hidden).
        let is_video = self.current_is_video();
        let video_paused = self.video_paused;
        let video_loop = self.video_loop;
        let stay_on_top = self.stay_on_top;
        let entity = cx.entity().clone();

        h_flex()
            .h(px(TOOLBAR_H))
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("viewer-prev")
                    .icon(gpui_component::Icon::empty().path("icons/chevron-left.svg"))
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.step(-1, cx))),
            )
            .child(
                div()
                    .text_scale_xs()
                    .text_color(cx.theme().muted_foreground)
                    .min_w(px(56.0))
                    .text_center()
                    .child(counter),
            )
            .child(
                Button::new("viewer-next")
                    .icon(gpui_component::Icon::empty().path("icons/chevron-right.svg"))
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.step(1, cx))),
            )
            .child(
                Button::new("viewer-play")
                    .icon(
                        gpui_component::Icon::empty().path(if self.playback.playing {
                            "icons/pause.svg"
                        } else {
                            "icons/play.svg"
                        }),
                    )
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        let playing = this.playback.playing;
                        this.set_playing(!playing, cx);
                    })),
            )
            .child(
                Button::new("viewer-interval")
                    .label(Playback::interval_label(self.playback.interval_secs))
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.cycle_interval(cx))),
            )
            .child(div().w(px(1.0)).h(px(20.0)).bg(cx.theme().border))
            .child(
                Button::new("viewer-zoom-out")
                    .icon(gpui_component::Icon::empty().path("icons/minus.svg"))
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by(1.0 / ZOOM_STEP, cx))),
            )
            .child(
                div()
                    .text_scale_xs()
                    .text_color(cx.theme().muted_foreground)
                    .min_w(px(44.0))
                    .text_center()
                    .child(zoom_label),
            )
            .child(
                Button::new("viewer-zoom-in")
                    .icon(gpui_component::Icon::empty().path("icons/plus.svg"))
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_by(ZOOM_STEP, cx))),
            )
            .child(
                Button::new("viewer-actual")
                    .label(if actual { "Fit" } else { "1:1" })
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_actual_size(&ViewerActualSize, window, cx)
                    })),
            )
            // Rotate the current item (image or video). View-only,
            // per-item — R / Shift-R do the same from the keyboard.
            .when(self.current().is_some(), |bar| {
                bar.child(
                    Button::new("viewer-rotate")
                        .icon(gpui_component::Icon::empty().path("icons/redo.svg"))
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_rotate_cw(&ViewerRotateCw, window, cx)
                        })),
                )
                // Colour adjustments popup — also `E` / right-click.
                .child(
                    Button::new("viewer-adjust")
                        .icon(gpui_component::Icon::empty().path("icons/palette.svg"))
                        .small()
                        .selected(self.adjust_panel_open)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_toggle_adjust(&ViewerToggleAdjust, window, cx)
                        })),
                )
            })
            // Video transport (native controls are hidden so they work
            // at any rotation): play/pause + a loop toggle.
            .when(is_video, |bar| {
                let loop_entity = entity.clone();
                bar.child(
                    Button::new("viewer-video-step-back")
                        .label("\u{2212}1f")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.step_video(-1, cx))),
                )
                .child(
                    Button::new("viewer-video-play")
                        .icon(gpui_component::Icon::empty().path(if video_paused {
                            "icons/play.svg"
                        } else {
                            "icons/pause.svg"
                        }))
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_video_paused(cx))),
                )
                .child(
                    Button::new("viewer-video-step-fwd")
                        .label("+1f")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.step_video(1, cx))),
                )
                .child(
                    Checkbox::new("viewer-video-loop")
                        .small()
                        .label("Loop")
                        .checked(video_loop)
                        .on_click(move |checked, _window, app| {
                            let on = *checked;
                            loop_entity.update(app, |this, cx| {
                                this.video_loop = on;
                                cx.notify();
                            });
                        }),
                )
            })
            // Keep the viewer above other windows.
            .child(
                Checkbox::new("viewer-stay-on-top")
                    .small()
                    .label("Stay on top")
                    .checked(stay_on_top)
                    .on_click(move |checked, window, app| {
                        let on = *checked;
                        let ns_view = content_ns_view(window);
                        entity.update(app, |this, cx| {
                            this.stay_on_top = on;
                            if let Some(v) = ns_view {
                                crate::platform_shell::set_window_floating(v, on);
                            }
                            cx.notify();
                        });
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_scale_sm()
                    .text_center()
                    .truncate()
                    .text_color(cx.theme().foreground)
                    .child(name),
            )
            .child(
                Button::new("viewer-fullscreen")
                    .icon(gpui_component::Icon::empty().path("icons/maximize.svg"))
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_toggle_fullscreen(&ViewerToggleFullscreen, window, cx)
                    })),
            )
    }

    /// The gpui image element for the current video frame, laid out
    /// through the shared `StageState` (zoom / pan / fit match stills),
    /// or `None` while no frame has been pulled yet. Rotation reuses the
    /// one-slot `video_rotated` cache and the superseded rotate queues
    /// for atlas eviction — mirroring stills.
    fn video_stage(&mut self, stage_w: f32, stage_h: f32) -> Option<gpui::Img> {
        let base = self.video_frame_image.clone()?;
        let rot = self.current_rotation();
        let image = if rot == 0 {
            base
        } else {
            let fresh = matches!(
                &self.video_rotated,
                Some((seq, r, _)) if *seq == self.video_frame_seq && *r == rot
            );
            if !fresh {
                if let Some(rotated) = rotate_render_image(&base, rot) {
                    if let Some((_, _, old)) =
                        self.video_rotated
                            .replace((self.video_frame_seq, rot, rotated))
                    {
                        self.video_frames_to_drop.push(old);
                    }
                }
            }
            match &self.video_rotated {
                Some((seq, r, img)) if *seq == self.video_frame_seq && *r == rot => img.clone(),
                _ => base,
            }
        };
        // Skip the per-frame CPU grade when the backend already graded the
        // pixels (VLC). Otherwise apply it on the CPU (native player).
        let image = if self.video_adjust_native {
            image
        } else {
            self.graded_video(image, rot)
        };
        // The frame (rotated or not) already carries its displayed dims,
        // so layout uses them directly — no manual aspect swap.
        let sz = image.size(0);
        let dims = self.to_logical((sz.width.0 as f32, sz.height.0 as f32));
        let r = stage::layout(dims, (stage_w, stage_h), self.stage);
        Some(
            gpui::img(image)
                .absolute()
                .left(px(r.x))
                .top(px(r.y))
                .w(px(r.w))
                .h(px(r.h)),
        )
    }

    /// Background-layer frames laid out under the primary video, bottom-first,
    /// through the same `stage::layout` so they stay aligned with the top one.
    fn layer_stages(&self, stage_w: f32, stage_h: f32) -> Vec<gpui::Img> {
        self.video_layers
            .iter()
            .filter_map(|layer| {
                let img = layer.frame.clone()?;
                let sz = img.size(0);
                let dims = self.to_logical((sz.width.0 as f32, sz.height.0 as f32));
                let r = stage::layout(dims, (stage_w, stage_h), self.stage);
                Some(
                    gpui::img(img)
                        .absolute()
                        .left(px(r.x))
                        .top(px(r.y))
                        .w(px(r.w))
                        .h(px(r.h)),
                )
            })
            .collect()
    }

    /// Add the next eligible playlist video (not the current entry, not already
    /// a layer) as a muted background layer beneath the primary video.
    fn add_layer(&mut self, cx: &mut Context<Self>) {
        let current = self.current().map(|e| e.path.clone());
        let used: std::collections::HashSet<PathBuf> =
            self.video_layers.iter().map(|l| l.path.clone()).collect();
        let pick = self
            .playlist
            .iter()
            .find(|e| {
                self.is_video_path(&e.path)
                    && Some(&e.path) != current.as_ref()
                    && !used.contains(&e.path)
            })
            .cloned();
        let Some(entry) = pick else { return };
        let stream = video_backend(self.vlc_pref.as_deref()).open(
            &entry.path,
            Box::new(|| {}),
            VideoEnhance::default(),
        );
        if let Some(mut stream) = stream {
            stream.set_muted(true);
            self.video_layers.push(BgLayer {
                stream,
                path: entry.path,
                name: entry.name,
                frame: None,
                seq: 0,
            });
            cx.notify();
        }
    }

    /// Remove the background layer at `idx`, retiring its frame.
    fn remove_layer(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx < self.video_layers.len() {
            let mut layer = self.video_layers.remove(idx);
            if let Some(img) = layer.frame.take() {
                self.video_frames_to_drop.push(img);
            }
            cx.notify();
        }
    }

    fn stage_area(&mut self, stage_w: f32, stage_h: f32, cx: &mut Context<Self>) -> Div {
        let path = self.current().map(|e| e.path.clone());
        let state = path.as_ref().and_then(|p| self.cache.get(p));
        let area = div()
            .relative()
            .overflow_hidden()
            .w_full()
            .h(px(stage_h))
            .bg(cx.theme().secondary.opacity(0.35))
            .on_scroll_wheel(cx.listener(Self::on_stage_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_stage_mouse_down))
            // Right-click anywhere on the stage opens (toggles) the
            // colour-adjustments popup — same as `E`.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    this.adjust_panel_open = !this.adjust_panel_open;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(Self::on_stage_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.end_drag()),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.end_drag()),
            );
        // A video draws its pulled frame through the same stage layout as
        // a still; until the first frame lands it falls through to the
        // Quick Look poster below.
        if self.current_is_video() {
            if let Some(child) = self.video_stage(stage_w, stage_h) {
                // Background layers render under the (keyed) primary so its
                // transparent pixels reveal them; primary drawn last = on top.
                let layers = self.layer_stages(stage_w, stage_h);
                return area.children(layers).child(child);
            }
        }
        match state {
            Some(FrameState::Loaded(f)) => {
                let rot = self.current_rotation();
                // Swap the aspect for quarter turns so the rotated image
                // is fit/zoomed against its post-rotation dimensions.
                let (dw, dh) = if rot % 2 == 1 {
                    (f.h as f32, f.w as f32)
                } else {
                    (f.w as f32, f.h as f32)
                };
                // Prefer the off-thread processed bitmap (grade + denoise +
                // upscale + sharpen + rotate) when its cached result matches
                // the live params. Otherwise fall back to the plain frame —
                // upright, or the one-slot CPU-rotated cache — so something
                // always shows while a fresh process is still computing.
                let image = if let Some(p) = self.processed_still(rot) {
                    p
                } else if rot == 0 {
                    f.image.clone()
                } else {
                    let fresh = matches!(
                        &self.rotated,
                        Some((i, r, _)) if *i == self.index && *r == rot
                    );
                    if !fresh {
                        if let Some(rotated) = rotate_render_image(&f.image, rot) {
                            self.rotated = Some((self.index, rot, rotated));
                        }
                    }
                    match &self.rotated {
                        Some((i, r, img)) if *i == self.index && *r == rot => img.clone(),
                        _ => f.image.clone(),
                    }
                };
                let r = stage::layout(self.to_logical((dw, dh)), (stage_w, stage_h), self.stage);
                area.child(
                    gpui::img(image)
                        .absolute()
                        .left(px(r.x))
                        .top(px(r.y))
                        .w(px(r.w))
                        .h(px(r.h)),
                )
            }
            Some(FrameState::Failed) => area.flex().items_center().justify_center().child(
                div()
                    .text_scale_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No preview available"),
            ),
            // Pending (or first paint before the request lands): show
            // the shared 512 px info-pane thumbnail as an instant
            // stand-in when one is cached, laid out with the same
            // stage state so the swap to full-res doesn't jump.
            _ => {
                let thumb = path.as_ref().and_then(|p| {
                    crate::preview::loaded_image(self.process.preview_cache.borrow().get(p))
                });
                match thumb {
                    Some(img) => {
                        let sz = img.size(0);
                        let dims = self.to_logical((sz.width.0 as f32, sz.height.0 as f32));
                        let r = stage::layout(dims, (stage_w, stage_h), self.stage);
                        area.child(
                            gpui::img(img)
                                .absolute()
                                .left(px(r.x))
                                .top(px(r.y))
                                .w(px(r.w))
                                .h(px(r.h)),
                        )
                    }
                    None => area.flex().items_center().justify_center().child(
                        div()
                            .text_scale_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Loading\u{2026}"),
                    ),
                }
            }
        }
    }

    /// A drag in progress on a slider tracks at the panel level so it
    /// keeps following the cursor across the whole popup, not just the
    /// thin track it began on.
    fn on_adjust_move(&mut self, e: &MouseMoveEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.slider_drag else { return };
        if e.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let v = self.slider_value_at(e.position.x, id);
        self.set_slider(id, v, cx);
    }

    /// A small muted group label inside the adjustments popup, separating the
    /// Colour / Enhance / Transparent-colour sections.
    fn section_header(&self, label: &'static str, cx: &mut Context<Self>) -> Div {
        div()
            .mt_1()
            .text_scale_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.theme().muted_foreground)
            .child(label)
    }

    /// The adjustments popup: colour grade (Brightness / Contrast / Color)
    /// always, plus the still-only enhancement controls (Denoise, Sharpen,
    /// Upscale) and Reset, floating at the top-right of the stage. Pointer
    /// drags are handled at the panel level (see [`Self::on_adjust_move`])
    /// and `stop_propagation` keeps clicks off the stage beneath.
    fn adjust_panel(&mut self, cx: &mut Context<Self>) -> Div {
        let bg = cx.theme().background;
        let border = cx.theme().border;
        let foreground = cx.theme().foreground;
        let top = self.stage_origin_y + 12.0;
        let is_video = self.current_is_video() || self.sim_video_panel;
        // Denoise/sharpen apply to stills (CPU) and to mpv video (filter
        // chain); upscale stays stills-only. `video_adjust_native` marks an
        // mpv stream — the native player has no filter chain.
        let vlc_video = self.sim_video_panel || (is_video && self.video_adjust_native);
        let show_enhance = !is_video || vlc_video;
        // Chroma-key state read up front so the section's builder closure
        // doesn't re-borrow self for these.
        let chroma_on = self.chroma_on;
        let chroma_armed = self.eyedrop_armed;
        let chroma_color = self.chroma_color;
        let layers: Vec<(usize, String)> = self
            .video_layers
            .iter()
            .enumerate()
            .map(|(i, l)| (i, l.name.clone()))
            .collect();

        let header = h_flex()
            .justify_between()
            .items_center()
            .child(
                div()
                    .text_scale_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(foreground)
                    .child("Adjustments"),
            )
            .child(
                Button::new("viewer-adjust-reset")
                    .label("Reset")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.reset_adjust(cx))),
            );

        div()
            .absolute()
            .top(px(top))
            .right(px(12.0))
            .w(px(248.0))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(bg)
            .rounded(px(10.0))
            .border_1()
            .border_color(border)
            .shadow_lg()
            // Keep clicks (left scrub, right re-toggle) off the stage.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(cx.listener(Self::on_adjust_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.slider_drag = None;
                    // Denoise/sharpen for a VLC video are baked in at open,
                    // so a changed value re-opens the stream on release
                    // (kept off the live drag — re-opening per move thrashes).
                    this.commit_video_enhance(cx);
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.slider_drag = None;
                    this.commit_video_enhance(cx);
                }),
            )
            .child(header)
            .child(self.section_header("Colour", cx))
            .child(self.slider_row(SliderId::Brightness, cx))
            .child(self.slider_row(SliderId::Contrast, cx))
            .child(self.slider_row(SliderId::Saturation, cx))
            // Hue + gamma are live libvlc colour-adjust controls — only the
            // VLC backend applies them, so they're hidden for stills and the
            // built-in player.
            .when(vlc_video, |d| {
                d.child(self.slider_row(SliderId::Hue, cx))
                    .child(self.slider_row(SliderId::Gamma, cx))
            })
            .when(show_enhance, |d| {
                let d = d
                    .child(self.section_header("Enhance", cx))
                    .child(self.slider_row(SliderId::Denoise, cx))
                    .child(self.slider_row(SliderId::Sharpen, cx));
                // Debanding + film grain are VLC-only filters (gradfun /
                // grain), baked into the decode like denoise/sharpen.
                let d = d.when(vlc_video, |d| {
                    d.child(self.slider_row(SliderId::Banding, cx))
                        .child(self.slider_row(SliderId::Grain, cx))
                });
                // Upscale is a still-only pre-scale; not meaningful for the
                // video player (it plays at native size, fit to the stage).
                if is_video {
                    d
                } else {
                    d.child(self.upscale_row(cx))
                }
            })
            // Transparent colour (chroma key) — mpv video only. Keyed pixels
            // go transparent so the stage background (later: a lower layer)
            // shows through. Pick the colour with the eyedropper swatch.
            .when(vlc_video, |d| {
                let col = chroma_color;
                let swatch =
                    gpui::rgb(((col[0] as u32) << 16) | ((col[1] as u32) << 8) | col[2] as u32);
                let hex = format!("#{:02X}{:02X}{:02X}", col[0], col[1], col[2]);
                let d = d.child(
                    h_flex()
                        .mt_1()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_scale_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().muted_foreground)
                                .child("Transparent colour"),
                        )
                        .child(
                            Button::new("viewer-chroma-toggle")
                                .label(if chroma_on { "On" } else { "Off" })
                                .small()
                                .selected(chroma_on)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.chroma_on = !this.chroma_on;
                                    if !this.chroma_on {
                                        this.eyedrop_armed = false;
                                    }
                                    this.apply_video_chroma();
                                    cx.notify();
                                })),
                        ),
                );
                if !chroma_on {
                    return d;
                }
                // Swatch (also arms the eyedropper) · hex readout · Pick button.
                d.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .id("viewer-chroma-swatch")
                                .w(px(22.0))
                                .h(px(18.0))
                                .rounded(px(4.0))
                                .border_1()
                                .border_color(if chroma_armed {
                                    cx.theme().primary
                                } else {
                                    border
                                })
                                .bg(swatch)
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.eyedrop_armed = !this.eyedrop_armed;
                                        cx.stop_propagation();
                                        cx.notify();
                                    }),
                                ),
                        )
                        .child(div().flex_1().text_scale_xs().text_color(foreground).child(hex))
                        .child(
                            Button::new("viewer-chroma-pick")
                                .label(if chroma_armed { "Picking\u{2026}" } else { "Pick" })
                                .small()
                                .selected(chroma_armed)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.eyedrop_armed = !this.eyedrop_armed;
                                    cx.notify();
                                })),
                        ),
                )
                .when(chroma_armed, |d| {
                    d.child(
                        div()
                            .text_scale_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Click the video to sample a colour."),
                    )
                })
                .child(self.slider_row(SliderId::Similarity, cx))
                .child(self.slider_row(SliderId::Blend, cx))
            })
            // Layers — composite background videos beneath the keyed top one.
            // The top video's transparent (keyed) pixels reveal them.
            .when(vlc_video, |d| {
                let d = d.child(
                    h_flex()
                        .mt_1()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_scale_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().muted_foreground)
                                .child("Layers"),
                        )
                        .child(
                            Button::new("viewer-layer-add")
                                .label("Add")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.add_layer(cx))),
                        ),
                );
                if layers.is_empty() {
                    return d.child(
                        div()
                            .text_scale_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Add a video to show beneath the keyed top."),
                    );
                }
                d.children(layers.into_iter().map(|(i, name)| {
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().flex_1().text_scale_xs().text_color(foreground).child(name))
                        .child(
                            Button::new(("viewer-layer-rm", i))
                                .label("\u{2715}")
                                .small()
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.remove_layer(i, cx)),
                                ),
                        )
                }))
            })
    }

    /// One slider row: a label, a draggable track with a fill (centre-
    /// anchored for the bipolar colour controls, left-anchored for the
    /// one-sided enhancement ones) and a thumb, plus a value readout. The
    /// track bounds are captured via `canvas` so a cursor x maps back to a
    /// value.
    fn slider_row(&self, id: SliderId, cx: &mut Context<Self>) -> impl IntoElement {
        const ROW_H: f32 = 16.0;
        let value = self.slider_value(id);
        let (lo_v, hi_v) = id.range();
        let frac = ((value - lo_v) / (hi_v - lo_v)).clamp(0.0, 1.0);
        // Fill span: from neutral (centre for bipolar, left edge otherwise).
        let (fill_lo, fill_hi) = if id.centered() {
            (frac.min(0.5), frac.max(0.5))
        } else {
            (0.0, frac)
        };
        let readout = if id.centered() {
            format!("{:+}", (value * 100.0).round() as i32)
        } else {
            format!("{}", (value * 100.0).round() as i32)
        };
        let track = cx.theme().slider_bar.opacity(0.3);
        let fill = cx.theme().primary;
        let thumb = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let entity = cx.entity();
        let idx = id.idx();

        let bar = div()
            .relative()
            .flex_1()
            .h(px(ROW_H))
            // Capture the painted track bounds for cursor→value mapping.
            .child(
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |this, _| this.slider_bounds[idx] = bounds)
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top(px(ROW_H / 2.0 - 1.5))
                    .left_0()
                    .right_0()
                    .h(px(3.0))
                    .rounded_full()
                    .bg(track),
            )
            .child(
                div()
                    .absolute()
                    .top(px(ROW_H / 2.0 - 1.5))
                    .left(relative(fill_lo))
                    .right(relative(1.0 - fill_hi))
                    .h(px(3.0))
                    .rounded_full()
                    .bg(fill),
            )
            .child(
                div()
                    .absolute()
                    .top(px(ROW_H / 2.0 - 6.0))
                    .left(relative(frac))
                    .ml(px(-6.0))
                    .w(px(12.0))
                    .h(px(12.0))
                    .rounded_full()
                    .bg(thumb)
                    .border_1()
                    .border_color(fill),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _w, cx| {
                    this.slider_drag = Some(id);
                    let v = this.slider_value_at(e.position.x, id);
                    this.set_slider(id, v, cx);
                }),
            );

        h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .w(px(64.0))
                    .text_scale_xs()
                    .text_color(muted)
                    .child(id.label()),
            )
            .child(bar)
            .child(
                div()
                    .w(px(30.0))
                    .flex()
                    .justify_end()
                    .child(div().text_scale_xs().text_color(muted).child(readout)),
            )
    }

    /// The Upscale row: 1× / 2× / 4× Lanczos resample buttons (1× = off).
    fn upscale_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let cur = self.enhance.upscale;
        h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .w(px(64.0))
                    .text_scale_xs()
                    .text_color(muted)
                    .child("Upscale"),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("viewer-upscale-1")
                            .label("1\u{00d7}")
                            .small()
                            .selected(cur <= 1)
                            .on_click(cx.listener(|this, _, _, cx| this.set_upscale(1, cx))),
                    )
                    .child(
                        Button::new("viewer-upscale-2")
                            .label("2\u{00d7}")
                            .small()
                            .selected(cur == 2)
                            .on_click(cx.listener(|this, _, _, cx| this.set_upscale(2, cx))),
                    )
                    .child(
                        Button::new("viewer-upscale-4")
                            .label("4\u{00d7}")
                            .small()
                            .selected(cur == 4)
                            .on_click(cx.listener(|this, _, _, cx| this.set_upscale(4, cx))),
                    ),
            )
    }

    fn status_strip(&mut self, cx: &mut Context<Self>) -> Div {
        // Native pixel resolution. For a video, report the decoded frame
        // (or intrinsic player size) — NOT `current_frame()`, which is the
        // smaller Quick Look poster.
        let dims = if self.current_is_video() {
            self.video_frame_image
                .as_ref()
                .map(|img| {
                    let s = img.size(0);
                    (s.width.0 as u32, s.height.0 as u32)
                })
                .or_else(|| {
                    let (w, h) = self.video_dims;
                    (w > 0.0 && h > 0.0).then_some((w as u32, h as u32))
                })
                .map(|(w, h)| format!("{w}\u{00d7}{h}"))
                .unwrap_or_default()
        } else {
            self.current_frame()
                .map(|f| format!("{}\u{00d7}{}", f.w, f.h))
                .unwrap_or_default()
        };
        let pos = format!(
            "{} of {}",
            (self.index + 1).min(self.playlist.len()),
            self.playlist.len()
        );
        h_flex()
            .h(px(STATUS_H))
            .flex_shrink_0()
            .items_center()
            .gap_3()
            .px_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_scale_xs()
            .text_color(cx.theme().muted_foreground)
            .child(pos)
            .when(!dims.is_empty(), |this| this.child(dims))
            .when(self.playback.playing, |this| {
                this.child(format!(
                    "Slideshow \u{00b7} {}",
                    Playback::interval_label(self.playback.interval_secs)
                ))
            })
            // Seek bar (with In/Out cues) + elapsed/total for the video.
            .when(self.current_is_video(), |this| {
                let (cur, dur) = self.video_position;
                this.child(fmt_time(cur))
                    .child(self.seek_bar(cx))
                    .child(fmt_time(dur))
            })
    }

    /// Custom video seek bar: a track with the active `[In, Out]` region
    /// shaded between two draggable cue grips, plus the playhead. Drag a
    /// grip to retrim; click/drag the track to scrub. Hit-testing is by
    /// proximity (in `on_seek_down`) rather than per-handle, so the grips
    /// never steal a scrub click away from a cue.
    fn seek_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (cur, dur) = self.video_position;
        let pos = if dur > 0.0 {
            (cur / dur).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let cin = self.cue_in.clamp(0.0, 1.0);
        let cout = self.cue_out.clamp(0.0, 1.0);
        let track = cx.theme().slider_bar.opacity(0.25);
        let region = cx.theme().primary;
        let playhead = cx.theme().foreground;
        let entity = cx.entity();

        // A cue grip: a filled triangle glyph centred at `frac`. `glyph`
        // is ▶ for In (points right, into the region) and ◀ for Out
        // (points left, into the region).
        let grip = move |frac: f32, glyph: &'static str| {
            div()
                .absolute()
                .top(px(SEEK_BAR_H / 2.0 - SEEK_GRIP / 2.0))
                .left(relative(frac))
                .ml(px(-SEEK_GRIP / 2.0))
                .w(px(SEEK_GRIP))
                .h(px(SEEK_GRIP))
                .flex()
                .items_center()
                .justify_center()
                .text_color(region)
                .text_size(px(SEEK_GRIP))
                .line_height(px(SEEK_GRIP))
                .child(glyph)
        };

        div()
            .id("seek-bar")
            .relative()
            .flex_1()
            .min_w(px(120.0))
            .h(px(SEEK_BAR_H))
            // Capture the track's painted bounds so the drag handlers can
            // map a cursor x → fraction.
            .child(
                canvas(
                    move |bounds, _, cx| entity.update(cx, |this, _| this.seek_bar_bounds = bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            // Track.
            .child(
                div()
                    .absolute()
                    .top(px(SEEK_BAR_H / 2.0 - 1.5))
                    .left_0()
                    .right_0()
                    .h(px(3.0))
                    .rounded_full()
                    .bg(track),
            )
            // Active region between the cues.
            .child(
                div()
                    .absolute()
                    .top(px(SEEK_BAR_H / 2.0 - 2.5))
                    .left(relative(cin))
                    .right(relative(1.0 - cout))
                    .h(px(5.0))
                    .rounded_full()
                    .bg(region.opacity(0.55)),
            )
            // Playhead.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(pos))
                    .w(px(2.0))
                    .bg(playhead),
            )
            .child(grip(cin, "\u{25B6}"))
            .child(grip(cout, "\u{25C0}"))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_seek_down))
            .on_mouse_move(cx.listener(Self::on_seek_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.seek_drag = None;
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.seek_drag = None),
            )
    }

    /// Fraction (0..1) of the seek bar at window-space x `x`.
    fn seek_frac_at(&self, x: Pixels) -> f32 {
        let w = self.seek_bar_bounds.size.width.as_f32();
        if w <= 0.0 {
            return 0.0;
        }
        ((x.as_f32() - self.seek_bar_bounds.origin.x.as_f32()) / w).clamp(0.0, 1.0)
    }

    /// Press on the seek bar: grab the nearer cue grip if the cursor is
    /// within reach of one, else start a playhead scrub.
    fn on_seek_down(&mut self, e: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let frac = self.seek_frac_at(e.position.x);
        let w = self.seek_bar_bounds.size.width.as_f32().max(1.0);
        let grab = (SEEK_GRAB_PX / w).clamp(0.0, 0.25);
        let d_in = (frac - self.cue_in).abs();
        let d_out = (frac - self.cue_out).abs();
        let target = if d_in <= grab && d_in <= d_out {
            SeekTarget::CueIn
        } else if d_out <= grab {
            SeekTarget::CueOut
        } else {
            SeekTarget::Playhead
        };
        self.seek_drag = Some(target);
        self.apply_seek_drag(target, frac, cx);
    }

    fn on_seek_move(&mut self, e: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.seek_drag else { return };
        let frac = self.seek_frac_at(e.position.x);
        self.apply_seek_drag(target, frac, cx);
    }

    /// Apply a seek-bar drag to its target. Cues keep a small gap so In
    /// can't cross Out; the playhead scrub seeks the player.
    fn apply_seek_drag(&mut self, target: SeekTarget, frac: f32, cx: &mut Context<Self>) {
        const GAP: f32 = 0.002;
        match target {
            SeekTarget::Playhead => {
                let (_, dur) = self.video_position;
                if dur > 0.0 {
                    if let Some((stream, _)) = &mut self.video_overlay {
                        stream.seek(frac as f64 * dur);
                    }
                    // Immediate visual feedback before the next poll lands.
                    self.video_position.0 = frac as f64 * dur;
                }
            }
            SeekTarget::CueIn => {
                self.cue_in = frac.clamp(0.0, (self.cue_out - GAP).max(0.0));
            }
            SeekTarget::CueOut => {
                self.cue_out = frac.clamp((self.cue_in + GAP).min(1.0), 1.0);
            }
        }
        cx.notify();
    }
}

/// gpui's window content NSView, for mounting the native video
/// overlay. `None` on non-AppKit platforms (the win32 overlay stub
/// ignores it anyway).
fn content_ns_view(window: &Window) -> Option<*mut std::ffi::c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    // UFCS: gpui::Window has an inherent `window_handle()` (the gpui
    // entity handle) that shadows the raw-window-handle trait method.
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::AppKit(h) => Some(h.ns_view.as_ptr()),
        _ => None,
    }
}

impl Focusable for ViewerWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Drop for ViewerWindow {
    /// Window close must stop playback — the overlay is an AppKit
    /// child of the (dying) window, but the AVPlayer would keep the
    /// audio session alive without an explicit pause.
    fn drop(&mut self) {
        self.teardown_video();
    }
}

impl Render for ViewerWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_title(window);
        let fullscreen = window.is_fullscreen();
        if !fullscreen {
            self.chrome_hover = false;
        }
        let viewport = window.viewport_size();
        let chrome_h = if fullscreen {
            0.0
        } else {
            TOOLBAR_H + STATUS_H
        };
        let stage_w = viewport.width.as_f32();
        let stage_h = (viewport.height.as_f32() - chrome_h).max(100.0);
        self.last_stage_size = (stage_w, stage_h);
        self.scale_factor = window.scale_factor();
        self.stage_origin_y = if fullscreen { 0.0 } else { TOOLBAR_H };
        // Evict the textures of frames superseded since the last render
        // (a new RenderImage per video frame would otherwise grow VRAM).
        for img in self.video_frames_to_drop.drain(..) {
            let _ = window.drop_image(img);
        }
        self.sync_video(cx);
        let stage_area = self.stage_area(stage_w, stage_h, cx);

        let root = v_flex()
            .key_context(VIEWER_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_prev))
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_left))
            .on_action(cx.listener(Self::on_right))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_zoom_reset))
            .on_action(cx.listener(Self::on_actual_size))
            .on_action(cx.listener(Self::on_toggle_fullscreen))
            .on_action(cx.listener(Self::on_toggle_play))
            .on_action(cx.listener(Self::on_rotate_cw))
            .on_action(cx.listener(Self::on_rotate_ccw))
            .on_action(cx.listener(Self::on_toggle_adjust))
            .on_action(cx.listener(Self::on_dismiss))
            .relative()
            .size_full()
            .bg(cx.theme().background);

        let panel = if self.adjust_panel_open {
            Some(self.adjust_panel(cx))
        } else {
            None
        };

        if fullscreen {
            // Image edge to edge; toolbar only as a hover overlay at
            // the top (no timers — pure mouse-position state).
            let chrome = self.chrome_hover.then(|| {
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bg(cx.theme().background.opacity(0.92))
                    .child(self.toolbar(cx))
            });
            root.child(stage_area)
                .when_some(chrome, Div::child)
                .when_some(panel, Div::child)
        } else {
            let toolbar = self.toolbar(cx);
            let status = self.status_strip(cx);
            root.child(toolbar)
                .child(stage_area)
                .child(status)
                .when_some(panel, Div::child)
        }
    }
}

/// Pixel-maths tests for the rotate / colour / enhancement pipeline.
/// Deliberately uses *narrow* imports (not `use super::*`): the parent
/// glob pulls in `gpui::*`, which re-exports `gpui_macros::test`, and that
/// heavy proc-macro blows the crate recursion limit. Importing only the
/// symbols under test keeps `#[test]` the lightweight std attribute.
#[cfg(test)]
mod grade_tests {
    use super::{
        ColorAdjust, EnhanceParams, apply_color_adjust, grade_bgra, process_still_pixels,
        rotate_render_image,
    };
    use gpui::RenderImage;

    /// A 90°/270° turn swaps width and height; the pixel buffer length
    /// is preserved. (Verifies the CPU rotate path that stands in for
    /// gpui's missing `img` rotation — docs/GPUI-UPSTREAM.md #5.)
    #[test]
    fn rotate_swaps_dimensions_for_quarter_turns() {
        // 3×2 opaque image (RGBA), distinct rows so a real rotation is
        // observable, not just a copy.
        let buf = image::RgbaImage::from_fn(3, 2, |x, y| {
            image::Rgba([x as u8 * 10, y as u8 * 10, 0, 255])
        });
        let base = RenderImage::new(vec![image::Frame::new(buf)]);

        let cw = rotate_render_image(&base, 1).expect("90° produces an image");
        let s = cw.size(0);
        assert_eq!((s.width.0, s.height.0), (2, 3), "90° swaps w/h");

        let half = rotate_render_image(&base, 2).expect("180° produces an image");
        let s = half.size(0);
        assert_eq!((s.width.0, s.height.0), (3, 2), "180° keeps w/h");

        assert!(rotate_render_image(&base, 0).is_none(), "0° is a no-op");
        assert!(
            rotate_render_image(&base, 4).is_none(),
            "full turn is a no-op"
        );
    }

    #[test]
    fn grade_brightness_raises_channels_alpha_untouched() {
        // One BGRA pixel of mid grey with a distinctive alpha.
        let mut buf = vec![100u8, 100, 100, 200];
        grade_bgra(
            &mut buf,
            ColorAdjust {
                brightness: 0.2,
                ..ColorAdjust::default()
            },
        );
        assert!(buf[0] > 100 && buf[1] > 100 && buf[2] > 100, "brightened");
        assert_eq!(buf[3], 200, "alpha preserved");
    }

    #[test]
    fn grade_full_desaturation_equalizes_rgb() {
        // A saturated red: B=0, G=0, R=255 in BGRA storage.
        let mut buf = vec![0u8, 0, 255, 255];
        grade_bgra(
            &mut buf,
            ColorAdjust {
                saturation: -1.0,
                ..ColorAdjust::default()
            },
        );
        // Fully desaturated → all three colour channels collapse to luma.
        assert_eq!(buf[0], buf[1], "B == G when desaturated");
        assert_eq!(buf[1], buf[2], "G == R when desaturated");
    }

    #[test]
    fn neutral_grade_skips_new_bitmap() {
        // The neutral identity must not allocate a graded bitmap.
        let buf = image::RgbaImage::from_pixel(2, 2, image::Rgba([40, 80, 120, 255]));
        let base = RenderImage::new(vec![image::Frame::new(buf)]);
        assert!(apply_color_adjust(&base, ColorAdjust::default()).is_none());
    }

    #[test]
    fn enhance_upscale_doubles_dimensions() {
        // 4×3 → 2× upscale → 8×6, byte count tracks the new dims.
        let bgra = vec![128u8; 4 * 3 * 4];
        let enh = EnhanceParams {
            upscale: 2,
            ..EnhanceParams::default()
        };
        let (w, h, out) =
            process_still_pixels(&bgra, 4, 3, 0, ColorAdjust::default(), enh).expect("processed");
        assert_eq!((w, h), (8, 6), "2× upscale doubles each dimension");
        assert_eq!(out.len(), (8 * 6 * 4) as usize, "buffer matches new dims");
    }

    #[test]
    fn enhance_upscale_then_rotate_swaps_dimensions() {
        // 4×3 upscaled 2× → 8×6, then a quarter turn → 6×8.
        let bgra = vec![128u8; 4 * 3 * 4];
        let enh = EnhanceParams {
            upscale: 2,
            ..EnhanceParams::default()
        };
        let (w, h, _) =
            process_still_pixels(&bgra, 4, 3, 1, ColorAdjust::default(), enh).expect("processed");
        assert_eq!((w, h), (6, 8), "90° turn swaps the upscaled dims");
    }
}
