//! Real macOS file icons for the GPUI shell.
//!
//! Pulls bytes from `NSWorkspace iconForFile:` via the existing
//! `feraille_fs_native::fetch_icon_rgba` bridge, swaps the channel
//! order from RGBA → BGRA (gpui's `RenderImage` wants BGRA even
//! though the wrapping type is named `RgbaImage` — see
//! `gpui::assets::RenderImage` docs), and caches the result.
//!
//! Cache key is the file's *kind* (extension lowercased, "Folder"
//! for directories, "Symlink" for symlinks). This dedupes massively
//! — a 200-PNG folder only stores one icon, fetched once.
//!
//! `NSWorkspace.iconForFile` is main-thread-only, so `icon_for` must
//! be called from a Render path. The cost on miss is a single
//! NSWorkspace round-trip (~1 ms); subsequent renders for the same
//! kind are a HashMap hit.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use feraille_core::{EntryKind, FileEntry};
use feraille_fs_native::fetch_icon_rgba;
use gpui::RenderImage;
use image::{Frame, RgbaImage};
use smallvec::SmallVec;

/// Physical-pixel size of the cached icon. We fetch at 2× the
/// logical 16-DIP slot so 2× displays render crisp; the GPU
/// scales down for the 1× case.
const ICON_PX: u32 = 32;

#[derive(Default)]
pub struct IconCache {
    by_kind: HashMap<String, Arc<RenderImage>>,
    /// Single fallback icon used when fetch_icon_rgba returns None
    /// for a given kind, so we don't keep retrying NSWorkspace for
    /// a file the OS doesn't know how to render.
    blank: Option<Arc<RenderImage>>,
}

impl IconCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lookup-or-fetch the icon for `entry` rooted at `path`. Cache
    /// is keyed by kind so most calls are O(1) HashMap reads after
    /// the first hit per extension. Returns the blank-placeholder
    /// arc when NSWorkspace can't produce an icon (typical only on
    /// non-macOS targets).
    pub fn icon_for(&mut self, entry: &FileEntry, path: &Path) -> Arc<RenderImage> {
        let key = cache_key(entry, path);
        if let Some(arc) = self.by_kind.get(&key) {
            return arc.clone();
        }
        if feraille_core::path_guard::is_rendering() {
            return self.blank_icon();
        }
        match fetch_icon_rgba(path, ICON_PX) {
            Some((rgba, w, h)) => {
                let arc = Arc::new(build_render_image(rgba, w, h));
                self.by_kind.insert(key, arc.clone());
                arc
            }
            None => self.blank_icon(),
        }
    }

    /// Sidebar-tree-flavoured lookup: caches by the full path string
    /// so special folders (Applications, Documents, /Volumes/Foo)
    /// each keep their distinctive Finder icon. Per-row cost
    /// amortises over the tree's bounded entry count.
    pub fn folder_icon_for(&mut self, path: &Path) -> Arc<RenderImage> {
        let key = format!("path:{}", path.display());
        if let Some(arc) = self.by_kind.get(&key) {
            return arc.clone();
        }
        if feraille_core::path_guard::is_rendering() {
            return self.blank_icon();
        }
        match fetch_icon_rgba(path, ICON_PX) {
            Some((rgba, w, h)) => {
                let arc = Arc::new(build_render_image(rgba, w, h));
                self.by_kind.insert(key, arc.clone());
                arc
            }
            None => self.blank_icon(),
        }
    }

    fn blank_icon(&mut self) -> Arc<RenderImage> {
        if let Some(b) = &self.blank {
            return b.clone();
        }
        // A single transparent pixel — drawn at any size will still
        // hold the row's icon slot so layout doesn't jitter when a
        // real icon is missing.
        let arc = Arc::new(build_render_image(vec![0, 0, 0, 0], 1, 1));
        self.blank = Some(arc.clone());
        arc
    }
}

fn cache_key(entry: &FileEntry, path: &Path) -> String {
    match entry.kind {
        EntryKind::Directory => "<dir>".into(),
        EntryKind::Symlink => "<symlink>".into(),
        EntryKind::File => path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_lowercase()))
            .unwrap_or_else(|| "<noext>".into()),
    }
}

/// Build a `RenderImage` from RGBA8888 bytes by swapping channels
/// in place (BGRA is what gpui's renderer expects) and wrapping in
/// a single-frame SmallVec.
fn build_render_image(mut rgba: Vec<u8>, w: u32, h: u32) -> RenderImage {
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let buf = RgbaImage::from_raw(w, h, rgba).expect("rgba dims match");
    let frame = Frame::new(buf);
    RenderImage::new(SmallVec::from_elem(frame, 1))
}
