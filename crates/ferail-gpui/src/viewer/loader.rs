//! Full-resolution image loading for the viewer window.
//!
//! Two-tier decode strategy (docs/features/VIEWER.md):
//!
//! 1. Raster formats the `image` crate understands (png/jpeg/gif/
//!    webp/bmp/tiff) are read and decoded directly on the background
//!    executor, downscaled to [`MAX_EDGE_PX`] when enormous.
//! 2. Everything else (HEIC, PDF, video posters, …) falls back to a
//!    Quick Look thumbnail at [`QL_FALLBACK_PX`] via the platform
//!    shell (qlmanage on macOS, stubbed on Windows until the
//!    IShellItemImageFactory parity lands).
//!
//! Decoded frames live in a byte-budget LRU ([`ViewerCache`]) owned by
//! the viewer window entity: big photo libraries must not accumulate
//! gigabytes of BGRA. `Pending` markers prevent duplicate in-flight
//! decodes and are never evicted; the entry that was just inserted is
//! likewise protected so a single over-budget image still displays.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::RenderImage;
use image::imageops::FilterType;
use image::{Frame, RgbaImage};
use smallvec::SmallVec;

/// Longest edge a decoded frame may keep. Caps GPU texture size and
/// memory (8192² BGRA ≈ 268 MB worst case); panoramas beyond this are
/// downscaled with a triangle filter during decode, off the UI thread.
pub const MAX_EDGE_PX: u32 = 8192;

/// Quick Look fallback render size for non-raster formats (HEIC, PDF,
/// video posters, …).
///
/// Capped at 1536, not higher: `QLThumbnailGenerator` refuses to
/// *generate* a thumbnail above ~1536 px on a cold request for large
/// HEICs (a 2048 px request returns nil unless a smaller size was
/// already cached), and it caps its own output around 1536–1600 px
/// anyway, so asking for more bought no quality, only intermittent
/// "No preview available" failures on the first view. 1536 generates
/// reliably cold and stays crisp on retina.
pub const QL_FALLBACK_PX: u32 = 1536;

/// Smaller retry size if the primary Quick Look request returns nothing.
/// The on-demand size ceiling that makes large cold requests fail is
/// device- and file-dependent, so a conservative second attempt keeps a
/// real thumbnail on screen instead of "No preview available".
pub const QL_RETRY_PX: u32 = 768;

/// Byte budget for cached full-resolution frames. ~16 typical 12 MP
/// photos, or a couple of panoramas. Revisit after testing against a
/// real photo library.
pub const CACHE_BUDGET_BYTES: usize = 384 * 1024 * 1024;

/// A decoded, render-ready frame.
pub struct ViewerFrame {
    pub image: Arc<RenderImage>,
    pub w: u32,
    pub h: u32,
    /// BGRA payload size used for cache accounting.
    pub bytes: usize,
}

/// Cache entry state. Mirrors `preview::PreviewState` so the two
/// pipelines stay idiomatic twins.
#[derive(Clone)]
pub enum FrameState {
    /// Decode in flight on the background executor.
    Pending,
    /// Ready to render.
    Loaded(Arc<ViewerFrame>),
    /// Unreadable and Quick Look couldn't help either.
    Failed,
}

/// Byte-budget LRU over decoded frames. `get` refreshes recency;
/// inserts evict the least-recently-used `Loaded` entries until the
/// budget is met. `Pending`/`Failed` entries cost zero bytes.
pub struct ViewerCache {
    by_path: HashMap<PathBuf, FrameState>,
    /// Recency order, oldest first.
    order: Vec<PathBuf>,
    budget: usize,
    used: usize,
}

impl Default for ViewerCache {
    fn default() -> Self {
        Self::new(CACHE_BUDGET_BYTES)
    }
}

impl ViewerCache {
    pub fn new(budget: usize) -> Self {
        Self {
            by_path: HashMap::new(),
            order: Vec::new(),
            budget,
            used: 0,
        }
    }

    fn entry_bytes(state: &FrameState) -> usize {
        match state {
            FrameState::Loaded(f) => f.bytes,
            FrameState::Pending | FrameState::Failed => 0,
        }
    }

    fn touch(&mut self, path: &Path) {
        if let Some(pos) = self.order.iter().position(|p| p == path) {
            let p = self.order.remove(pos);
            self.order.push(p);
        }
    }

    pub fn get(&mut self, path: &Path) -> Option<FrameState> {
        let state = self.by_path.get(path).cloned()?;
        self.touch(path);
        Some(state)
    }

    /// Peek without refreshing recency, for render paths that probe
    /// neighbours without intending to keep them hot.
    pub fn peek(&self, path: &Path) -> Option<&FrameState> {
        self.by_path.get(path)
    }

    pub fn insert(&mut self, path: PathBuf, state: FrameState) {
        if let Some(old) = self.by_path.get(&path) {
            self.used -= Self::entry_bytes(old);
        } else {
            self.order.push(path.clone());
        }
        self.used += Self::entry_bytes(&state);
        self.by_path.insert(path.clone(), state);
        self.touch(&path);
        self.evict_over_budget(&path);
    }

    pub fn remove(&mut self, path: &Path) {
        if let Some(old) = self.by_path.remove(path) {
            self.used -= Self::entry_bytes(&old);
            if let Some(pos) = self.order.iter().position(|p| p == path) {
                self.order.remove(pos);
            }
        }
    }

    pub fn used_bytes(&self) -> usize {
        self.used
    }

    /// Evict least-recently-used `Loaded` entries until within budget.
    /// `Pending` markers stay (an in-flight decode must keep its
    /// dedup marker) and `just_inserted` stays (a single over-budget
    /// panorama must still display).
    fn evict_over_budget(&mut self, just_inserted: &Path) {
        let mut idx = 0;
        while self.used > self.budget && idx < self.order.len() {
            let candidate = self.order[idx].clone();
            let evictable = candidate != *just_inserted
                && matches!(self.by_path.get(&candidate), Some(FrameState::Loaded(_)));
            if evictable {
                self.remove(&candidate);
                // `remove` shifted `order`; re-check the same index.
            } else {
                idx += 1;
            }
        }
    }
}

/// Blocking decode: call on the background executor only.
///
/// Returns RGBA bytes + dimensions (channel swap to BGRA happens in
/// [`build_frame`], same split as `preview::build_render_image`).
pub fn decode_full_res(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    if let Some(frame) = std::fs::read(path)
        .ok()
        .and_then(|bytes| decode_raster(&bytes))
    {
        return Some(frame);
    }
    // AROS: videos go through C:FFThumb (native ffmpeg), same frame the
    // preview pane shows, at a viewer-worthy edge. Without this the viewer
    // said "No preview available" for files whose pane thumbnail worked.
    #[cfg(target_os = "aros")]
    if let Some(frame) = crate::video_poster::ffthumb_full(path) {
        return Some(frame);
    }
    // Quick Look fallback for non-raster formats (HEIC, PDF, video).
    // A cold request above QL's on-demand size ceiling can return nil,
    // so fall back to a smaller, always-generatable size rather than
    // leaving the viewer on "No preview available".
    crate::platform_shell::fetch_quick_look_thumbnail(path, QL_FALLBACK_PX)
        .or_else(|| crate::platform_shell::fetch_quick_look_thumbnail(path, QL_RETRY_PX))
}

/// Decode any raster format the `image` crate is built with, capping
/// the longest edge at [`MAX_EDGE_PX`]. `None` for formats it can't
/// parse (HEIC, PDF, …): the caller falls back to Quick Look.
fn decode_raster(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let (w, h) = (decoded.width(), decoded.height());
    let longest = w.max(h);
    let decoded = if longest > MAX_EDGE_PX {
        let scale = MAX_EDGE_PX as f64 / longest as f64;
        let nw = ((w as f64 * scale).round() as u32).max(1);
        let nh = ((h as f64 * scale).round() as u32).max(1);
        decoded.resize_exact(nw, nh, FilterType::Triangle)
    } else {
        decoded
    };
    let rgba = decoded.into_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Some((rgba.into_raw(), w, h))
}

/// Wrap decoded RGBA bytes into a render-ready frame (in-place BGRA
/// channel swap, single-frame `RenderImage`).
pub fn build_frame(mut rgba: Vec<u8>, w: u32, h: u32) -> ViewerFrame {
    let bytes = rgba.len();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let buf = RgbaImage::from_raw(w, h, rgba).expect("rgba dims match");
    let frame = Frame::new(buf);
    ViewerFrame {
        image: Arc::new(RenderImage::new(SmallVec::from_elem(frame, 1))),
        w,
        h,
        bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: a Loaded frame whose accounting size we control
    /// without allocating that much memory.
    fn loaded_sized(bytes: usize) -> FrameState {
        let mut f = build_frame(vec![0u8; 4], 1, 1);
        f.bytes = bytes;
        FrameState::Loaded(Arc::new(f))
    }

    #[test]
    fn evicts_lru_when_over_budget() {
        let mut c = ViewerCache::new(100);
        c.insert("a".into(), loaded_sized(40));
        c.insert("b".into(), loaded_sized(40));
        c.insert("c".into(), loaded_sized(40)); // 120 > 100 → evict a
        assert!(c.peek(Path::new("a")).is_none());
        assert!(c.peek(Path::new("b")).is_some());
        assert!(c.peek(Path::new("c")).is_some());
        assert_eq!(c.used_bytes(), 80);
    }

    #[test]
    fn get_refreshes_recency() {
        let mut c = ViewerCache::new(100);
        c.insert("a".into(), loaded_sized(40));
        c.insert("b".into(), loaded_sized(40));
        c.get(Path::new("a")); // a is now most recent
        c.insert("c".into(), loaded_sized(40)); // evicts b, not a
        assert!(c.peek(Path::new("a")).is_some());
        assert!(c.peek(Path::new("b")).is_none());
    }

    #[test]
    fn pending_and_just_inserted_survive_eviction() {
        let mut c = ViewerCache::new(50);
        c.insert("inflight".into(), FrameState::Pending);
        c.insert("huge".into(), loaded_sized(500)); // way over budget
        // Pending marker kept; the over-budget frame itself kept.
        assert!(matches!(
            c.peek(Path::new("inflight")),
            Some(FrameState::Pending)
        ));
        assert!(matches!(
            c.peek(Path::new("huge")),
            Some(FrameState::Loaded(_))
        ));
        // Next insert evicts the previous over-budget frame.
        c.insert("next".into(), loaded_sized(30));
        assert!(c.peek(Path::new("huge")).is_none());
        assert_eq!(c.used_bytes(), 30);
    }

    #[test]
    fn reinsert_replaces_accounting() {
        let mut c = ViewerCache::new(100);
        c.insert("a".into(), FrameState::Pending);
        assert_eq!(c.used_bytes(), 0);
        c.insert("a".into(), loaded_sized(60));
        assert_eq!(c.used_bytes(), 60);
        c.insert("a".into(), FrameState::Failed);
        assert_eq!(c.used_bytes(), 0);
        assert_eq!(c.order.len(), 1);
    }

    #[test]
    fn decode_raster_caps_longest_edge() {
        // 10_000×10 PNG → capped to 8192 on the long edge.
        let img = RgbaImage::from_pixel(10_000, 10, image::Rgba([10, 20, 30, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let (_, w, h) = decode_raster(&png).unwrap();
        assert_eq!(w, MAX_EDGE_PX);
        assert_eq!(h, 8); // 10 * 8192/10000, rounded
    }

    #[test]
    fn decode_raster_rejects_non_raster() {
        assert!(decode_raster(b"%PDF-1.7 not an image").is_none());
        assert!(decode_raster(&[]).is_none());
    }

    #[test]
    fn build_frame_swaps_to_bgra() {
        let f = build_frame(vec![1, 2, 3, 4], 1, 1);
        assert_eq!((f.w, f.h, f.bytes), (1, 1, 4));
        // The swap happens inside the RenderImage's frame buffer.
        // Dimensions surviving is the observable contract here; the
        // BGRA ordering matches preview::build_render_image.
    }
}
