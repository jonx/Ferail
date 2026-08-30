//! Native file preview via macOS Quick Look (`qlmanage`).
//!
//! Renders NSWorkspace-backed thumbnails. On selection change the Shell
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
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{App, AsyncApp, RenderImage};
use image::{Frame, RgbaImage};
use smallvec::SmallVec;

use crate::preview_queue::{Enqueue, LatestRequestQueue};
use crate::shell::Shell;

/// Largest physical-pixel side of the cached thumbnail. ~512 keeps
/// retina-quality previews while staying small enough that the
/// `qlmanage` round-trip is sub-second for most file types.
pub const PREVIEW_PX: u32 = 512;

/// Maximum number of cached previews kept around. LRU-evicted when
/// over capacity: the preview pane only ever shows one at a time
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
    requests: LatestRequestQueue<PathBuf>,
    /// Cancellation token owned by the currently active selection request.
    /// A queued newer selection flips it; on Windows the broker wait observes
    /// the flag and kills the disposable helper immediately.
    active_cancel: Option<Arc<AtomicBool>>,
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
            requests: LatestRequestQueue::default(),
            active_cancel: None,
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

    fn enqueue_request(&mut self, path: PathBuf) -> Enqueue {
        let outcome = self.requests.enqueue(path);
        if matches!(outcome, Enqueue::Queued)
            && let Some(cancel) = &self.active_cancel
        {
            cancel.store(true, Ordering::Relaxed);
        }
        outcome
    }

    fn begin_active_request(&mut self) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        self.active_cancel = Some(cancel.clone());
        cancel
    }

    fn complete_request(&mut self, path: &PathBuf) -> Option<PathBuf> {
        let was_active = self.requests.is_active(path);
        let next = self.requests.complete(path);
        if was_active {
            self.active_cancel = None;
        }
        next
    }

    fn remove(&mut self, path: &Path) {
        self.by_path.remove(path);
        self.order.retain(|entry| entry != path);
    }

    /// Drop completed previews beneath an explicitly refreshed directory.
    /// Pending provider work keeps its queue slot and cancellation contract.
    pub fn invalidate_finished_under(&mut self, root: &Path) {
        self.by_path.retain(|path, state| {
            !path.starts_with(root) || matches!(state, PreviewState::Pending)
        });
        let by_path = &self.by_path;
        self.order.retain(|path| by_path.contains_key(path));
    }
}

/// Spawn the background fetch for `path` if we haven't already.
/// Marks the cache as `Pending`, runs `qlmanage` on the background
/// executor, and applies the result via `shell.update`.
///
/// Call sites already hold `&mut Shell` (we're inside a shell
/// update closure), so the cache mutation goes straight against
/// `shell.process.preview_cache` rather than re-reading the entity
/// through `cx.entity()`: that would trigger the GPUI re-entrancy
/// guard.
pub fn request(shell: &mut Shell, path: PathBuf, cx: &mut gpui::Context<Shell>) {
    // Text/code preview rides the same selection event: the worker
    // decides text-vs-binary, so the render shows inline text for
    // source files and the thumbnail for everything else.
    crate::text_preview::request(shell, path.clone(), cx);

    if shell.process.preview_cache.borrow().get(&path).is_some() {
        return;
    }
    let enqueue = shell
        .process
        .preview_cache
        .borrow_mut()
        .enqueue_request(path.clone());
    crate::obs::breadcrumb(format_args!(
        "preview enqueue outcome={enqueue:?} ext={}",
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("<none>")
    ));
    if !matches!(enqueue, Enqueue::Start) {
        return;
    }
    start_request(shell, path, cx);
}

fn start_request(shell: &mut Shell, path: PathBuf, cx: &mut gpui::Context<Shell>) {
    let cancel = {
        let mut cache = shell.process.preview_cache.borrow_mut();
        cache.insert(path.clone(), PreviewState::Pending);
        cache.begin_active_request()
    };

    let weak = cx.weak_entity();
    let process = shell.process.clone();
    let tasks = process.tasks.clone();
    let task_id = tasks.borrow_mut().begin(
        crate::tasks::TaskKind::ThumbnailPrefetch,
        tr!("Loading preview\u{2026}"),
        false,
    );
    cx.spawn(async move |_this, cx| {
        let p_for_bg = path.clone();
        let result = cx
            .background_executor()
            .spawn({
                let cancel = cancel.clone();
                async move { fetch_preview_thumbnail(p_for_bg, cancel).await }
            })
            .await;
        apply_result(weak, process, path, result, cancel, task_id, cx).await;
    })
    .detach();
}

/// The preview pane's 512 px content fetch: the synchronous tier (Quick
/// Look / cover art) right here on the pool, or an awaited poster-worker
/// decode for videos Quick Look refuses, never a blocked pool thread.
/// Asks for the shell's *preview* tier: on Windows that may be a brokered
/// `IPreviewHandler` capture, which the grid deliberately never gets.
async fn fetch_preview_thumbnail(
    path: PathBuf,
    cancel: Arc<AtomicBool>,
) -> Option<(Vec<u8>, u32, u32)> {
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    match crate::video_poster::fetch_content_preview_cancellable(&path, PREVIEW_PX, &cancel) {
        crate::video_poster::Fetched::Done(r) => r,
        crate::video_poster::Fetched::NeedsPoster => {
            let result = crate::video_poster::fetch_poster(path, PREVIEW_PX).await;
            (!cancel.load(Ordering::Relaxed))
                .then_some(result)
                .flatten()
        }
    }
}

/// Warm the preview cache for `path` from a non-`Shell` entity: the
/// viewer's slideshow prefetch runs on the `ViewerWindow` entity but
/// shares the process-wide cache. While a slide is on screen this
/// renders the *next* file's cheap 512 px thumbnail; it lands well
/// before the full-res decode, so the advance shows an instant
/// stand-in instead of "Loading…".
///
/// Mirrors [`request`] but notifies the caller's entity (any view)
/// rather than the Shell, and skips the text-preview side-channel the
/// viewer never renders. The result is written to the shared cache
/// regardless of whether the entity is still alive: the thumbnail
/// stays useful to the browser's preview pane either way.
pub fn warm<T: 'static>(
    process: &std::rc::Rc<crate::process_state::ProcessState>,
    path: PathBuf,
    cx: &mut gpui::Context<T>,
) {
    if process.preview_cache.borrow().get(&path).is_some() {
        return;
    }
    process
        .preview_cache
        .borrow_mut()
        .insert(path.clone(), PreviewState::Pending);

    let weak = cx.weak_entity();
    let process = process.clone();
    cx.spawn(async move |_this, cx| {
        let p_for_bg = path.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = cx
            .background_executor()
            .spawn(async move { fetch_preview_thumbnail(p_for_bg, cancel).await })
            .await;
        let state = match result {
            Some((rgba, w, h)) => PreviewState::Loaded(Arc::new(build_render_image(rgba, w, h))),
            None => PreviewState::Failed,
        };
        process.preview_cache.borrow_mut().insert(path, state);
        if let Some(this) = weak.upgrade() {
            this.update(cx, |_, cx| cx.notify());
        }
    })
    .detach();
}

async fn apply_result(
    weak: gpui::WeakEntity<Shell>,
    process: std::rc::Rc<crate::process_state::ProcessState>,
    path: PathBuf,
    rgba: Option<(Vec<u8>, u32, u32)>,
    cancel: Arc<AtomicBool>,
    task_id: crate::tasks::TaskId,
    cx: &mut AsyncApp,
) {
    let succeeded = rgba.is_some();
    let next = {
        let mut cache = process.preview_cache.borrow_mut();
        if cancel.load(Ordering::Relaxed) {
            // Supersession is not a provider failure. Leave the old path
            // retryable instead of poisoning it with a negative cache entry.
            cache.remove(&path);
        } else {
            let state = match rgba {
                Some((rgba, w, h)) => {
                    PreviewState::Loaded(Arc::new(build_render_image(rgba, w, h)))
                }
                None => PreviewState::Failed,
            };
            cache.insert(path.clone(), state);
        }
        cache.complete_request(&path)
    };
    crate::obs::breadcrumb(format_args!(
        "preview complete canceled={} success={} next={}",
        cancel.load(Ordering::Relaxed),
        succeeded,
        next.is_some()
    ));
    process.tasks.borrow_mut().end(task_id);
    let Some(shell) = weak.upgrade() else { return };
    shell.update(cx, |shell, cx| {
        cx.notify();
        if let Some(next) = next {
            // Re-enter through `request`: a viewer warm may have filled this
            // path while it was queued, in which case the cache check avoids
            // redundant provider work.
            request(shell, next, cx);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_selection_cancels_active_without_negative_caching_it() {
        let first = PathBuf::from("first.pdf");
        let latest = PathBuf::from("latest.pdf");
        let mut cache = PreviewCache::new();

        assert_eq!(cache.enqueue_request(first.clone()), Enqueue::Start);
        cache.insert(first.clone(), PreviewState::Pending);
        let cancel = cache.begin_active_request();
        assert!(!cancel.load(Ordering::Relaxed));

        assert_eq!(cache.enqueue_request(latest.clone()), Enqueue::Queued);
        assert!(cancel.load(Ordering::Relaxed));
        cache.remove(&first);
        assert!(cache.get(&first).is_none());
        assert_eq!(cache.complete_request(&first), Some(latest));
    }

    #[test]
    fn directory_refresh_keeps_unrelated_and_pending_previews() {
        let root = Path::new("/root/folder");
        let inside = root.join("inside.pdf");
        let pending = root.join("pending.pdf");
        let outside = PathBuf::from("/else/outside.pdf");
        let mut cache = PreviewCache::new();
        cache.insert(inside.clone(), PreviewState::Failed);
        cache.insert(pending.clone(), PreviewState::Pending);
        cache.insert(outside.clone(), PreviewState::Failed);

        cache.invalidate_finished_under(root);

        assert!(cache.get(&inside).is_none());
        assert!(matches!(cache.get(&pending), Some(PreviewState::Pending)));
        assert!(matches!(cache.get(&outside), Some(PreviewState::Failed)));
    }
}
