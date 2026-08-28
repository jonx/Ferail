//! Content thumbnails Quick Look can't produce on its own: video poster
//! frames and embedded audio cover art.
//!
//! macOS's `QLThumbnailGenerator` only thumbnails what AVFoundation can
//! decode — it refuses whole container families (AVI, WMV/ASF, MKV, …)
//! instantly, so a folder of DivX rips shows nothing but type glyphs. When
//! the user has selected the mpv video provider (Settings → Plugins), the
//! same libmpv that plays those files in the viewer can also pull one frame
//! for a thumbnail. [`fetch_content_thumbnail`] is the single fetch every
//! thumbnail warm path goes through: Quick Look first (it is near-free for
//! anything the OS caches), then embedded cover art for audio files, then
//! the mpv poster fallback for video files.
//!
//! The cover-art step is what makes album art show in the preview pane and
//! the icon grid on **every** platform: macOS Quick Look already digs the
//! embedded picture out of an audio file, but the Windows/Linux shell has no
//! equivalent, so there we read it directly with `lofty`
//! (`ferail_fs_native::media::read_cover_art`) and decode it here. Because
//! this is the shared choke point, one path lights up previews and grid
//! thumbnails alike, through the same cache and BGRA render path.
//!
//! Prime-directive shape: [`fetch_content_thumbnail`] (Quick Look + cover
//! art, bounded blocking) keeps the background pool's existing contract,
//! but poster decodes must NOT park pool tasks — a folder of 90 rips would
//! queue minutes of serialized mpv work, and a convoy of blocked pool
//! threads starves prefetch/folder-sizes/timers until navigation itself
//! stops responding (the exact freeze this design replaced). So posters
//! run on **one dedicated OS thread**: `fetch_content_thumbnail` answers
//! [`Fetched::NeedsPoster`] and the caller *awaits* [`fetch_poster`], which
//! queues a job and yields — no pool thread is held while the worker
//! churns. The single worker also means one libmpv instance at a time, and
//! a file libmpv can't decode costs one bounded deadline before it is
//! negative-cached by [`crate::thumbnails::ThumbnailCache`].

use std::path::{Path, PathBuf};

/// One resolved thumbnail: straight RGBA8 bytes + dimensions, the shape
/// `ThumbnailCache::insert` expects.
pub type ThumbPayload = (Vec<u8>, u32, u32);

/// Outcome of the synchronous fetch tier.
pub enum Fetched {
    /// Resolved here (Quick Look hit, embedded cover art, or nothing left
    /// to try) — `None` means "no thumbnail", which callers negative-cache.
    Done(Option<ThumbPayload>),
    /// A video the mpv poster worker should decode: `await`
    /// [`fetch_poster`] for the result. Only returned when the mpv
    /// provider is actually configured, so a build/setup without mpv never
    /// queues dead jobs.
    NeedsPoster,
}

/// Video containers/streams worth handing to the mpv poster fallback —
/// the viewer's built-in set plus its broad mpv container set. Quick Look
/// already succeeds for the AVFoundation-friendly ones, so membership here
/// only matters after it returned nothing.
const POSTER_VIDEO_EXTS: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "flv", "wmv", "asf", "mpg", "mpeg", "mpe", "m2v",
    "mpv", "3gp", "3g2", "ts", "mts", "m2ts", "vob", "ogv", "ogm", "divx", "rm", "rmvb", "f4v",
    "mxf", "dv", "qt", "amv", "nsv", "y4m", "h264", "hevc", "av1",
];

/// Whether `path`'s extension marks it as a video the poster fallback
/// could decode. Case-insensitive, like every other extension gate.
pub fn is_poster_candidate(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| POSTER_VIDEO_EXTS.contains(&e.as_str()))
}

/// Raster formats the bundled `image` crate is compiled with (Cargo
/// features: png/jpeg/gif/webp/bmp/tiff) — the same decoder set the
/// viewer's `decode_raster` relies on. Case-insensitive.
const RASTER_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif"];

fn is_bundled_raster(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| RASTER_EXTS.contains(&e.as_str()))
}

/// Decode + downscale an image file to a `size_px`-bounded RGBA thumbnail.
/// Blocking (file read + decode) — background pool only, same contract as
/// the Quick Look tier above.
fn fetch_raster_thumbnail(path: &Path, size_px: u32) -> Option<ThumbPayload> {
    let bytes = std::fs::read(path).ok()?;
    let decoded = image::load_from_memory(&bytes).ok()?;
    let (w, h) = (decoded.width(), decoded.height());
    let longest = w.max(h).max(1);
    let decoded = if longest > size_px {
        let scale = size_px as f64 / longest as f64;
        let nw = ((w as f64 * scale).round() as u32).max(1);
        let nh = ((h as f64 * scale).round() as u32).max(1);
        decoded.resize_exact(nw, nh, image::imageops::FilterType::Triangle)
    } else {
        decoded
    };
    let rgba = decoded.into_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Some((rgba.into_raw(), w, h))
}

/// Which surface the pixels are for. The platform shell may answer the
/// two differently: on Windows a grid thumbnail is what Explorer would
/// show (shell thumbnail, native PDF page, else the type icon), while
/// the preview pane may additionally fall back to a brokered
/// `IPreviewHandler` capture — a document rendering with the handler's
/// own chrome, acceptable in the pane, wrong in a grid cell. macOS and
/// Linux answer both the same way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// Icon grid, list rows, the viewer's fallback, `ferail thumb`.
    Thumbnail,
    /// The preview pane (and `ferail thumb --preview`).
    Preview,
}

/// The synchronous content-thumbnail tier for the grid/list: Quick Look,
/// then embedded audio cover art. See [`fetch_content`].
pub fn fetch_content_thumbnail(path: &Path, size_px: u32) -> Fetched {
    fetch_content(path, size_px, Tier::Thumbnail)
}

/// The preview pane's fetch — same tiers as [`fetch_content_thumbnail`],
/// but the platform shell is asked for its richer [`Tier::Preview`] image.
pub fn fetch_content_preview(path: &Path, size_px: u32) -> Fetched {
    fetch_content(path, size_px, Tier::Preview)
}

/// Selection-driven Windows preview fetch with cooperative cancellation. The
/// Windows shell implementation checks the flag while waiting for its broker
/// and terminates that helper when a newer selection supersedes it. Other
/// decoding tiers are checked between stages.
pub fn fetch_content_preview_cancellable(
    path: &Path,
    size_px: u32,
    cancel: &std::sync::atomic::AtomicBool,
) -> Fetched {
    fetch_content_inner(path, size_px, Tier::Preview, Some(cancel))
}

/// The synchronous content fetch: the platform shell (Quick Look on
/// macOS; the shell thumbnail / native PDF page / preview capture on
/// Windows, per `tier`), then the bundled raster decoders, then embedded
/// audio cover art. Bounded blocking — background pool only. Videos the
/// shell refuses are NOT decoded here: they come back as
/// [`Fetched::NeedsPoster`] so the caller can `await` [`fetch_poster`]
/// without parking a pool thread behind the (slow, serialized) mpv worker.
pub fn fetch_content(path: &Path, size_px: u32, tier: Tier) -> Fetched {
    fetch_content_inner(path, size_px, tier, None)
}

fn fetch_content_inner(
    path: &Path,
    size_px: u32,
    tier: Tier,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Fetched {
    if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
        return Fetched::Done(None);
    }
    let shell_hit = match tier {
        Tier::Thumbnail => crate::platform_shell::fetch_quick_look_thumbnail(path, size_px),
        Tier::Preview => {
            #[cfg(windows)]
            {
                match cancel {
                    Some(flag) => {
                        crate::platform_shell::fetch_preview_image_cancellable(path, size_px, flag)
                    }
                    None => crate::platform_shell::fetch_preview_image(path, size_px),
                }
            }
            #[cfg(not(windows))]
            {
                crate::platform_shell::fetch_preview_image(path, size_px)
            }
        }
    };
    if let Some(hit) = shell_hit {
        return Fetched::Done(Some(hit));
    }
    if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
        return Fetched::Done(None);
    }
    // Pure-Rust raster tier: platforms without Quick Look (AROS, Windows,
    // Linux stubs return None above) still get real image thumbnails via
    // the bundled `image` crate — and on macOS this catches files
    // Quick Look refuses.
    if is_bundled_raster(path) {
        if let Some(hit) = fetch_raster_thumbnail(path, size_px) {
            return Fetched::Done(Some(hit));
        }
    }
    // The shell came up empty (or this platform has none) — try embedded
    // cover art. The shared media reader uses the extension as its fast path
    // and a bounded content check for renamed files. This path only runs for
    // viewport-owned thumbnail requests, never across an entire listing.
    if let Some(cover) = fetch_audio_cover(path, size_px) {
        return Fetched::Done(Some(cover));
    }
    if is_poster_candidate(path) && poster_provider_available() {
        return Fetched::NeedsPoster;
    }
    // True last resort, and deliberately *after* every decoder above: the
    // platform's large file-type image. On Windows this is
    // `IShellItemImageFactory` without `THUMBNAILONLY` — asked for any
    // earlier it would mask a decodable image (the shell declines a 512 px
    // extraction for some files whose 256 px grid thumbnail works, e.g. on
    // OneDrive, and the bundled decoder must get its turn first). macOS
    // and Linux return `None` here and keep showing their type glyphs.
    Fetched::Done(crate::platform_shell::fetch_type_icon(path, size_px))
}

/// Is there a poster decoder on this platform? Desktop: the mpv provider
/// (feature + user preference). AROS: the `C:FFThumb` helper from the
/// native ffmpeg port (aros-aarch64 `hosted/ffmpeg/ffthumb.c`), probed
/// once — deployed alongside FFViewX, absent on minimal boot images.
fn poster_provider_available() -> bool {
    #[cfg(target_os = "aros")]
    {
        static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *AVAILABLE.get_or_init(|| std::fs::metadata("C:FFThumb").is_ok())
    }
    #[cfg(not(target_os = "aros"))]
    {
        resolve_mpv_pref().is_some()
    }
}

/// Await the mpv poster frame for a [`Fetched::NeedsPoster`] file. Queues
/// the job on the dedicated poster worker thread and yields until it is
/// done — safe to await from a pooled task (no thread is held).
pub async fn fetch_poster(path: PathBuf, size_px: u32) -> Option<ThumbPayload> {
    let (tx, rx) = async_channel::bounded(1);
    enqueue_poster(PosterJob { path, size_px, tx });
    rx.recv().await.ok().flatten()
}

/// Fully synchronous variant for one-shot CLI use (`ferail thumb`): same
/// tiers, but the poster decodes right on the calling thread instead of
/// through the worker queue. Never call this from the app's executors.
pub fn fetch_content_blocking(path: &Path, size_px: u32, tier: Tier) -> Option<ThumbPayload> {
    match fetch_content(path, size_px, tier) {
        Fetched::Done(r) => r,
        Fetched::NeedsPoster => poster_decode(path, size_px),
    }
}

/// One queued poster request; the answer travels back over a one-shot
/// async channel so the requesting task awaits instead of blocking.
struct PosterJob {
    path: PathBuf,
    size_px: u32,
    tx: async_channel::Sender<Option<ThumbPayload>>,
}

/// The poster work queue, drained **newest-first** by one dedicated OS
/// thread (spawned on first use).
///
/// LIFO is the queue policy, deliberately: navigating to a new folder
/// pushes its rows on top, so the view the user is looking at thumbnails
/// first, while jobs for rows browsed away from sink to the bottom and
/// still complete eventually — their results are cached for the next
/// visit. Nothing is ever *cancelled*: a dropped job would resolve to
/// `None` at its awaiting call site and be negative-cached as "this file
/// has no thumbnail", permanently wrong. Finishing stale jobs is bounded,
/// cheap background churn on this one thread; unlike blocked pool threads
/// it can never stall the rest of the app. Serializing on one thread also
/// bounds mpv to a single instance at a time.
struct PosterQueue {
    jobs: std::sync::Mutex<std::collections::VecDeque<PosterJob>>,
    ready: std::sync::Condvar,
}

fn poster_queue() -> &'static PosterQueue {
    use std::sync::OnceLock;
    static QUEUE: OnceLock<PosterQueue> = OnceLock::new();
    QUEUE.get_or_init(|| {
        std::thread::Builder::new()
            .name("ferail-poster".into())
            .spawn(|| {
                let q = poster_queue();
                loop {
                    let job = {
                        let mut jobs = q.jobs.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            match jobs.pop_back() {
                                Some(job) => break job,
                                None => {
                                    jobs = q.ready.wait(jobs).unwrap_or_else(|e| e.into_inner());
                                }
                            }
                        }
                    };
                    let result = poster_decode(&job.path, job.size_px);
                    // Capacity-1 channel, sole send — try_send can't fail
                    // except when the requester is gone, which is fine.
                    let _ = job.tx.try_send(result);
                }
            })
            .expect("spawn poster worker thread");
        PosterQueue {
            jobs: std::sync::Mutex::new(std::collections::VecDeque::new()),
            ready: std::sync::Condvar::new(),
        }
    })
}

fn enqueue_poster(job: PosterJob) {
    let q = poster_queue();
    q.jobs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(job);
    q.ready.notify_one();
}

/// Read the embedded cover art for an audio `path` and decode + shrink it to
/// fit within `size_px` on its longest edge (aspect preserved). Returns
/// RGBA8 + dimensions, or `None` when there is no cover or the picture bytes
/// won't decode. Blocking — background pool only.
fn fetch_audio_cover(path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    let encoded = ferail_fs_native::media::read_cover_art(path)?;
    // The image crate sniffs the format from the bytes (cover art is almost
    // always JPEG or PNG, both in our decoder feature set).
    let decoded = image::load_from_memory(&encoded).ok()?;
    // `thumbnail` is a fast box filter that fits the image inside the square
    // without cropping or distorting a non-square cover.
    let rgba = decoded.thumbnail(size_px, size_px).to_rgba8();
    let (w, h) = rgba.dimensions();
    Some((rgba.into_raw(), w, h))
}

/// Read the persisted video-provider choice: `Some(libmpv hint path)` when
/// the user picked the mpv provider in Settings → Plugins, `None` for the
/// built-in player. Refused outright in a build without the `mpv` feature,
/// where the preference is unhonourable — otherwise the viewer would route
/// the broad mpv container set (MKV/AVI/3GP…) to the native player, which
/// can't decode it. `app_state::load` is served from an in-memory cache
/// after first read, so this is cheap enough for per-fetch use.
pub fn resolve_mpv_pref() -> Option<PathBuf> {
    if !cfg!(feature = "mpv") {
        return None;
    }
    let st = crate::app_state::load();
    (st.video_backend.as_deref() == Some("mpv")).then(|| {
        PathBuf::from(
            st.mpv_path
                .unwrap_or_else(|| crate::viewer::backend_native::default_mpv_path().to_string()),
        )
    })
}

/// Decode one representative frame of `path` with libmpv and shrink it to
/// `size_px` (longest edge). `None` when libmpv can't be loaded or the file
/// yields no decodable video within the deadline (corrupt files open fine
/// but never produce a frame). Runs on the poster worker thread (or the
/// caller's own thread via the `_blocking` CLI variant) — never on the
/// pool.
#[cfg(all(feature = "mpv", not(target_os = "aros")))]
fn poster_decode(path: &Path, size_px: u32) -> Option<ThumbPayload> {
    use std::time::Duration;

    use ferail_core::video::{VideoEnhance, VideoStream};

    /// How long an open stream gets to produce its first frame before the
    /// file is declared undecodable. Healthy files measure 0.3–3.7s to
    /// first frame even on removable media; corrupt ones never deliver.
    const FIRST_FRAME_DEADLINE: Duration = Duration::from_secs(5);
    /// Grace for the post-seek frame. The seek lands mid-stream, so this
    /// is a decode away — much shorter than the cold open.
    const SEEK_FRAME_DEADLINE: Duration = Duration::from_secs(2);
    /// Where the poster is taken from. A few seconds in skips the black
    /// lead-in / encoder logo most rips open with; mpv clamps a seek past
    /// the end of a shorter clip, so this is safe for any duration.
    const POSTER_SEEK_SECS: f64 = 3.0;

    fn pull_frame(stream: &mut dyn VideoStream, deadline: Duration) -> Option<(u32, u32, Vec<u8>)> {
        let t0 = std::time::Instant::now();
        loop {
            if let Some(frame) = stream.copy_frame() {
                return Some(frame);
            }
            if t0.elapsed() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    let hint = resolve_mpv_pref()?;
    let backend = ferail_video_mpv::backend(&hint)?;
    let mut stream = backend.open(path, Box::new(|| {}), VideoEnhance::default())?;
    stream.set_muted(true);

    // The first frame proves the file decodes at all; then hop past the
    // lead-in and prefer the frame there, falling back to the first when
    // the seek lands nothing before its (short) deadline.
    let first = pull_frame(stream.as_mut(), FIRST_FRAME_DEADLINE)?;
    stream.seek(POSTER_SEEK_SECS);
    let frame = pull_frame(stream.as_mut(), SEEK_FRAME_DEADLINE).unwrap_or(first);
    Some(shrink_poster(frame, size_px))
}

/// Without the `mpv` feature [`fetch_content_thumbnail`] never answers
/// `NeedsPoster` (the pref resolves to `None`), so this is only reachable
/// through the blocking CLI variant — and correctly finds nothing.
#[cfg(all(not(feature = "mpv"), not(target_os = "aros")))]
fn poster_decode(_path: &Path, _size_px: u32) -> Option<ThumbPayload> {
    None
}

/// AROS poster decoder: shell out to `C:FFThumb` (the native ffmpeg port's
/// headless one-frame thumbnailer, aros-aarch64 `hosted/ffmpeg/ffthumb.c`)
/// and read back the PPM it writes to `T:` (RAM-backed). The decoder runs
/// in ITS OWN process — the same crash-isolation qlmanage gives macOS,
/// which matters here because the h264/hevc decoders still fault on this
/// target. Runs on the dedicated poster worker thread; the child blocking
/// blocks only that worker, same contract as the mpv path.
#[cfg(target_os = "aros")]
fn poster_decode(path: &Path, size_px: u32) -> Option<ThumbPayload> {
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Frames FFThumb skips before taking the poster — past the black
    /// fade-in most clips open with (the mpv path seeks 3s for the same
    /// reason; frame-skipping avoids a seek through the custom AVIO).
    const POSTER_SKIP_FRAMES: u32 = 8;

    // RAM: (always mounted), NOT `T:` — the T assign only exists once the
    // full startup-sequence ran, and referencing a missing assign pops a
    // DOS "please insert volume" requester over the app.
    static SEQ: AtomicU32 = AtomicU32::new(0);
    // Normalize the unix-join artifact before the string reaches the AROS
    // shell: `MacRW:/x` (PathBuf::join output) means "parent of the device
    // root" to DOS, and unlike fs-pal calls (whose cstr() fixes this up)
    // Command args pass through verbatim.
    let arg_path = {
        let s = path.to_string_lossy();
        match s.find(":/") {
            Some(i) => format!("{}:{}", &s[..i], &s[i + 2..]),
            None => s.into_owned(),
        }
    };
    let tmp = format!(
        "RAM:ferail-poster-{}.ppm",
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let out = match std::process::Command::new("C:FFThumb")
        .arg(&arg_path)
        .arg(&tmp)
        .arg(size_px.to_string())
        .arg(POSTER_SKIP_FRAMES.to_string())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            crate::log_info!(95, "ffthumb: spawn failed for {}: {e}", path.display());
            return None;
        }
    };
    // Exit code + a parseable PPM are the success signal — stdout capture
    // is unreliable through the AROS process pal (observed empty even on
    // success), and read_ppm_rgba validates the payload anyway.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let ok = out.status.success();
    let payload = if ok {
        read_ppm_rgba(Path::new(&tmp))
    } else {
        None
    };
    crate::log_info!(
        95,
        "ffthumb: {} -> status={:?} ok={ok} payload={} ({})",
        path.display(),
        out.status.code(),
        payload.is_some(),
        stdout.trim()
    );
    let _ = std::fs::remove_file(&tmp);
    payload
}

/// Viewer-window fallback: a big FFThumb frame for video files the raster
/// tier can't decode. Bounded blocking (one spawn + decode) — background
/// executor only; the viewer opens one file at a time so this can't convoy
/// the pool the way batch poster jobs would.
#[cfg(target_os = "aros")]
pub fn ffthumb_full(path: &Path) -> Option<ThumbPayload> {
    if !is_poster_candidate(path) || !poster_provider_available() {
        return None;
    }
    poster_decode(path, 1024)
}

/// Minimal binary-PPM (P6, maxval 255) reader for FFThumb's output.
/// Whitespace/comment-tolerant header, then w*h RGB triples → RGBA.
#[cfg(target_os = "aros")]
fn read_ppm_rgba(path: &Path) -> Option<ThumbPayload> {
    let bytes = std::fs::read(path).ok()?;
    if !bytes.starts_with(b"P6") {
        return None;
    }
    let mut pos = 2usize;
    let mut fields = [0u32; 3]; // width, height, maxval
    for field in fields.iter_mut() {
        // skip whitespace and `#` comments
        loop {
            match bytes.get(pos)? {
                b'#' => {
                    while *bytes.get(pos)? != b'\n' {
                        pos += 1;
                    }
                }
                c if c.is_ascii_whitespace() => pos += 1,
                _ => break,
            }
        }
        let mut v: u32 = 0;
        while let Some(c) = bytes.get(pos) {
            if !c.is_ascii_digit() {
                break;
            }
            v = v.saturating_mul(10).saturating_add((c - b'0') as u32);
            pos += 1;
        }
        *field = v;
    }
    let (w, h, maxval) = (fields[0], fields[1], fields[2]);
    if w == 0 || h == 0 || maxval != 255 {
        return None;
    }
    pos += 1; // the single whitespace byte after maxval
    let need = (w as usize).checked_mul(h as usize)?.checked_mul(3)?;
    let rgb = bytes.get(pos..pos + need)?;
    let mut rgba = Vec::with_capacity(need / 3 * 4);
    for px in rgb.chunks_exact(3) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
    }
    Some((rgba, w, h))
}

/// Convert one mpv frame (tightly packed BGRA) into the straight-RGBA
/// thumbnail payload, downscaled so the longest edge is `size_px` (never
/// upscaled). Alpha is forced opaque — the sw render path's alpha channel
/// is unspecified for plain video and a transparent thumbnail would paint
/// as an empty slot.
#[cfg(feature = "mpv")]
fn shrink_poster((w, h, mut bgra): (u32, u32, Vec<u8>), size_px: u32) -> (Vec<u8>, u32, u32) {
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
        px[3] = 0xFF;
    }
    let long = w.max(h);
    if long <= size_px {
        return (bgra, w, h);
    }
    let scale = size_px as f32 / long as f32;
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    match image::RgbaImage::from_raw(w, h, bgra) {
        Some(img) => (image::imageops::thumbnail(&img, nw, nh).into_raw(), nw, nh),
        // Dimension mismatch can't happen for a frame we just packed, but
        // never panic on a background worker over a thumbnail.
        None => (Vec::new(), 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poster_candidates_by_extension_case_insensitive() {
        assert!(is_poster_candidate(Path::new("/x/clip.avi")));
        assert!(is_poster_candidate(Path::new("/x/CLIP.AVI")));
        assert!(is_poster_candidate(Path::new("/x/clip.wmv")));
        assert!(is_poster_candidate(Path::new("/x/clip.mkv")));
        assert!(!is_poster_candidate(Path::new("/x/photo.jpg")));
        assert!(!is_poster_candidate(Path::new("/x/noext")));
    }

    #[cfg(feature = "mpv")]
    #[test]
    fn shrink_poster_swaps_channels_and_caps_longest_edge() {
        // 4×2 frame of one BGRA pixel value; alpha deliberately 0.
        let bgra = [10u8, 20, 30, 0].repeat(8);
        let (rgba, w, h) = super::shrink_poster((4, 2, bgra.clone()), 96);
        // Under the cap: untouched dims, swapped channels, opaque alpha.
        assert_eq!((w, h), (4, 2));
        assert_eq!(&rgba[..4], &[30, 20, 10, 0xFF]);

        // Over the cap: longest edge shrinks to it, aspect kept.
        let big = [10u8, 20, 30, 0].repeat(200 * 100);
        let (_, w, h) = super::shrink_poster((200, 100, big), 96);
        assert_eq!((w, h), (96, 48));
    }
}
