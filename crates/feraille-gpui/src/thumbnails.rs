//! Real content thumbnails for the file list.
//!
//! Where [`crate::icons::IconCache`] vends generic, type-keyed glyphs,
//! this cache vends the *actual* rendered thumbnail of a file —
//! photo, video poster frame, PDF first page — keyed by full path.
//!
//! Pixels come from `QLThumbnailGenerator` (see
//! `feraille_shell_mac::fetch_quick_look_thumbnail`), which reads the
//! system-wide Quick Look thumbnail cache, so most hits return
//! instantly from disk rather than re-rendering — the same path Finder
//! rides.
//!
//! ## Prime-directive shape
//!
//! - **Render** only ever calls [`ThumbnailCache::get`] — a HashMap
//!   read, no I/O, no allocation beyond an `Arc` clone.
//! - **Fetching** is driven from the table's `visible_rows_changed`
//!   hook (viewport-only) and runs the Quick Look call on a background
//!   thread; the decoded bytes are turned into a `RenderImage` back on
//!   the UI thread and inserted here.
//! - A bounded LRU keeps memory flat even for folders of thousands;
//!   negative results (no thumbnail available) are cached too so we
//!   never re-request a file the OS can't thumbnail.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use feraille_core::{EntryKind, FileEntry};
use gpui::RenderImage;

use crate::icons::{build_render_image, file_type_icon, FileTypeTint};

/// Process-wide live toggle: whether the file list paints real Quick
/// Look thumbnails (`true`) or stays on generic type icons (`false`).
/// Seeded at startup from the persisted `show_thumbnails` setting and
/// updated by the Settings window. Reading it inside `render`
/// subscribes that window to global changes, so flipping the toggle
/// repaints both windows immediately (same mechanism as the theme).
#[derive(Clone, Copy)]
pub struct ShowThumbnails(pub bool);

impl gpui::Global for ShowThumbnails {}

/// The toggle's value, defaulting to `true` (thumbnails on) when the
/// global has not been seeded yet.
pub fn show_thumbnails(cx: &gpui::App) -> bool {
    cx.try_global::<ShowThumbnails>().map(|g| g.0).unwrap_or(true)
}

/// Physical-pixel longest edge we ask Quick Look for. Sized for a
/// retina list row (~2× the logical slot) — crisp without paying for
/// preview-grade resolution per row. The icon view (later) will keep
/// its own larger cache.
pub const THUMB_PX: u32 = 96;

/// How many ready thumbnails to keep across all sizes (list 96 px +
/// grid buckets). A big photo folder scrolls past far more than fit on
/// screen; the LRU keeps the recently-seen ones. At the list size this
/// is ~18 MB; the larger grid buckets cost more per entry, so the cap
/// is entry-count, not bytes — the working set is bounded by what fits
/// in a couple of viewport-heights, well under this.
const CACHE_CAP: usize = 512;

/// Whether `entry` is the kind of file worth asking Quick Look about.
/// Generic data files (archives, binaries, plain text) thumbnail to
/// something indistinguishable from their type icon, so we leave those
/// on the icon path and spend Quick Look only where it pays off:
/// images, video poster frames, and PDF first pages.
pub fn is_thumbnailable(entry: &FileEntry) -> bool {
    if !matches!(entry.kind, EntryKind::File) {
        return false;
    }
    if matches!(file_type_icon(entry).tint, FileTypeTint::Image | FileTypeTint::Video) {
        return true;
    }
    matches!(
        entry
            .name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .as_deref(),
        Some("pdf")
    )
}

/// Cache key: a file path at a specific physical fetch size. The list
/// (96 px) and the icon grid (bucketed, e.g. 128/256/512 px) request
/// different sizes for the same file; keying by `(path, size)` lets
/// both coexist without one clobbering the other.
type Key = (PathBuf, u32);

#[derive(Default)]
pub struct ThumbnailCache {
    /// `(path, size)` → resolved thumbnail. `Some(arc)` is a ready
    /// image; `None` records "Quick Look produced nothing" so we fall
    /// back to the icon and never re-request.
    ready: HashMap<Key, Option<Arc<RenderImage>>>,
    /// Insertion order for LRU eviction (oldest at the front). Every key
    /// in `ready` appears here exactly once.
    order: VecDeque<Key>,
    /// Keys with a fetch currently in flight — gates re-requests while
    /// the background Quick Look call is pending.
    in_flight: HashSet<Key>,
}

impl ThumbnailCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Render-path lookup: the ready thumbnail for `path` at `size_px`,
    /// if any. Non-mutating and allocation-free beyond the `Arc` clone,
    /// so it is safe to call from `render`.
    pub fn get(&self, path: &Path, size_px: u32) -> Option<Arc<RenderImage>> {
        self.ready
            .get(&(path.to_path_buf(), size_px))
            .cloned()
            .flatten()
    }

    /// Whether `(path, size_px)` still needs a background fetch — i.e.
    /// it is neither resolved (positively or negatively) nor in flight.
    pub fn needs_fetch(&self, path: &Path, size_px: u32) -> bool {
        let key = (path.to_path_buf(), size_px);
        !self.ready.contains_key(&key) && !self.in_flight.contains(&key)
    }

    /// Mark a fetch as started so concurrent warming passes don't
    /// double-request the same `(path, size)`.
    pub fn mark_in_flight(&mut self, path: PathBuf, size_px: u32) {
        self.in_flight.insert((path, size_px));
    }

    /// Record the outcome of a background fetch: `Some((rgba, w, h))`
    /// becomes a ready `RenderImage`, `None` caches the miss. Clears
    /// the in-flight marker and evicts the oldest entry past capacity.
    pub fn insert(&mut self, path: PathBuf, size_px: u32, rgba: Option<(Vec<u8>, u32, u32)>) {
        let key = (path, size_px);
        self.in_flight.remove(&key);
        let image = rgba.map(|(bytes, w, h)| Arc::new(build_render_image(bytes, w, h)));
        if self.ready.insert(key.clone(), image).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > CACHE_CAP {
            if let Some(old) = self.order.pop_front() {
                self.ready.remove(&old);
            }
        }
    }
}
