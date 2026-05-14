//! Native file preview via macOS Quick Look (`qlmanage`).
//!
//! Replaces the soft-renderer preview pane with real
//! NSWorkspace-backed thumbnails. On selection change the Shell
//! kicks off a background worker that runs
//! `crate::platform_shell::quick_look::fetch_thumbnail` (which shells
//! out to `qlmanage -t`), decodes the resulting PNG into RGBA, and
//! delivers the bytes back to the foreground for rendering as a
//! `gpui::img(...)`.
//!
//! Per-path cache keyed by absolute path so revisiting a row is
//! instant; capped at `CACHE_CAP` entries with LRU eviction.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::platform_shell::fetch_quick_look_thumbnail;
use gpui::{App, AsyncApp, RenderImage};
use image::{Frame, RgbaImage};
use smallvec::SmallVec;

use crate::shell::Shell;

/// Largest physical-pixel side of the cached thumbnail. ~512 keeps
/// retina-quality previews while staying small enough that the
/// `qlmanage` round-trip is sub-second for most file types.
pub const PREVIEW_PX: u32 = 512;

/// Maximum number of cached previews kept around. LRU-evicted when
/// over capacity — the preview pane only ever shows one at a time
/// so even 16 is generous.
const CACHE_CAP: usize = 16;

/// Cached preview entry, or a sentinel saying "we tried and failed"
/// (so we don't keep re-spawning qlmanage for unsupported files).
#[derive(Clone)]
pub enum PreviewState {
    /// Fetch is in flight on the background executor.
    Pending,
    /// Decoded RGBA, ready to render via `gpui::img(...)`.
    Loaded(Arc<RenderImage>),
    /// qlmanage couldn't produce a thumbnail.
    Failed,
}

pub struct PreviewCache {
    by_path: HashMap<PathBuf, PreviewState>,
    /// Insertion-order ring for LRU-ish eviction (simple FIFO is
    /// plenty for the bounded cap).
    order: Vec<PathBuf>,
}

impl Default for PreviewCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewCache {
    pub fn new() -> Self {
        Self {
            by_path: HashMap::new(),
            order: Vec::new(),
        }
    }

    pub fn get(&self, path: &Path) -> Option<PreviewState> {
        self.by_path.get(path).cloned()
    }

    pub fn insert(&mut self, path: PathBuf, state: PreviewState) {
        if !self.by_path.contains_key(&path) {
            self.order.push(path.clone());
        }
        self.by_path.insert(path, state);
        while self.order.len() > CACHE_CAP {
            let oldest = self.order.remove(0);
            self.by_path.remove(&oldest);
        }
    }
}

/// Spawn the background fetch for `path` if we haven't already.
/// Marks the cache as `Pending`, runs `qlmanage` on the background
/// executor, and applies the result via `shell.update`.
///
/// Call sites already hold `&mut Shell` (we're inside a shell
/// update closure), so the cache mutation goes straight against
/// `shell.process.preview_cache` rather than re-reading the entity
/// through `cx.entity()` — that would trigger the GPUI re-entrancy
/// guard.
pub fn request(shell: &mut Shell, path: PathBuf, cx: &mut gpui::Context<Shell>) {
    if shell.process.preview_cache.borrow().get(&path).is_some() {
        return;
    }
    shell
        .process
        .preview_cache
        .borrow_mut()
        .insert(path.clone(), PreviewState::Pending);

    let weak = cx.weak_entity();
    cx.spawn(async move |_this, cx| {
        let p_for_bg = path.clone();
        let result = cx
            .background_executor()
            .spawn(async move { fetch_quick_look_thumbnail(&p_for_bg, PREVIEW_PX) })
            .await;
        apply_result(weak, path, result, cx).await;
    })
    .detach();
}

async fn apply_result(
    weak: gpui::WeakEntity<Shell>,
    path: PathBuf,
    rgba: Option<(Vec<u8>, u32, u32)>,
    cx: &mut AsyncApp,
) {
    let state = match rgba {
        Some((rgba, w, h)) => PreviewState::Loaded(Arc::new(build_render_image(rgba, w, h))),
        None => PreviewState::Failed,
    };
    let Some(shell) = weak.upgrade() else { return };
    let _ = shell.update(cx, |shell, cx| {
        shell.process.preview_cache.borrow_mut().insert(path, state);
        cx.notify();
    });
}

/// Build a `RenderImage` from RGBA bytes by swapping channels in
/// place (BGRA is what gpui's renderer expects despite the wrapping
/// type being named `RgbaImage`) and wrapping in a single-frame
/// SmallVec.
fn build_render_image(mut rgba: Vec<u8>, w: u32, h: u32) -> RenderImage {
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let buf = RgbaImage::from_raw(w, h, rgba).expect("rgba dims match");
    let frame = Frame::new(buf);
    RenderImage::new(SmallVec::from_elem(frame, 1))
}

/// Convenience: lookup helper from within `Shell::preview`. Returns
/// the renderable image when available, or `None` for any other
/// state (pending / failed).
pub fn loaded_image(state: Option<PreviewState>) -> Option<Arc<RenderImage>> {
    match state {
        Some(PreviewState::Loaded(img)) => Some(img),
        _ => None,
    }
}

#[allow(dead_code)]
fn _app_unused(_cx: &mut App) {}
