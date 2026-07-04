//! Real macOS file icons for the GPUI shell.
//!
//! Pulls bytes from `NSWorkspace iconForFile:` via the existing
//! `feraille_fs_native::fetch_icon_rgba` bridge, swaps the channel
//! order from RGBA → BGRA (gpui's `RenderImage` wants BGRA even
//! though the wrapping type is named `RgbaImage` — see
//! `gpui::assets::RenderImage` docs), and caches the result.
//!
//! Cache key is the file's kind for files (extension lowercased,
//! "Symlink" for symlinks) and the full path for directories. Files
//! dedupe massively; folders stay path-specific so special/custom
//! Finder folder icons don't bleed into every other directory.
//!
//! `NSWorkspace.iconForFile` is main-thread-only, so `icon_for` must
//! be called from a Render path. The cost on miss is a single
//! NSWorkspace round-trip (~1 ms); subsequent renders for the same
//! key are a HashMap hit.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use feraille_core::{EntryKind, FileEntry};
use feraille_fs_native::fetch_icon_rgba;
use gpui::{App, Hsla, RenderImage};
use gpui_component::ActiveTheme;
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

    /// Lookup-or-fetch the icon for `entry` rooted at `path`. Files
    /// are keyed by kind so most calls are O(1) HashMap reads after
    /// the first hit per extension; directories are keyed by path so
    /// custom/special folder artwork stays distinct. Returns the
    /// blank-placeholder arc when NSWorkspace can't produce an icon
    /// (typical only on non-macOS targets).
    pub fn icon_for(&mut self, entry: &FileEntry, path: &Path) -> Arc<RenderImage> {
        if matches!(entry.kind, EntryKind::Directory) {
            return self.folder_icon_for(path);
        }
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

    /// Whether a path-keyed (folder/sidebar) icon is already cached.
    /// Lets the shell decide which rows still need a background warm
    /// without mutating the cache from a render pass.
    pub fn has_folder_icon(&self, path: &Path) -> bool {
        self.by_kind
            .contains_key(&format!("path:{}", path.display()))
    }

    /// Fetch-and-cache a path-keyed icon outside the render path.
    /// Unlike `folder_icon_for`, a failed NSWorkspace fetch caches the
    /// blank placeholder under the key so the warm scheduler (which
    /// re-collects "not cached yet" paths every render) converges
    /// instead of re-requesting the same unfetchable path forever.
    pub fn warm_folder_icon(&mut self, path: &Path) {
        let key = format!("path:{}", path.display());
        if self.by_kind.contains_key(&key) {
            return;
        }
        if feraille_core::path_guard::is_rendering() {
            return;
        }
        let icon = match fetch_icon_rgba(path, ICON_PX) {
            Some((rgba, w, h)) => Arc::new(build_render_image(rgba, w, h)),
            None => self.blank_icon(),
        };
        self.by_kind.insert(key, icon);
    }

    /// Read-only lookup of a path-keyed icon cached at a specific pixel
    /// size (the icon grid fetches folder icons large so they stay crisp
    /// at 128–256 px, where the 32 px list icon would upscale to mush).
    /// `None` when not yet warmed — the caller falls back to the small
    /// icon as a placeholder. Non-mutating: safe from `render`.
    pub fn get_folder_icon_sized(&self, path: &Path, size_px: u32) -> Option<Arc<RenderImage>> {
        self.by_kind
            .get(&format!("path:{}@{}", path.display(), size_px))
            .cloned()
    }

    /// Whether a grid-sized path icon is already cached (warmed or
    /// failed). Lets the warm loop skip the notify when nothing new was
    /// fetched, avoiding a render→warm→notify feedback loop.
    pub fn has_folder_icon_sized(&self, path: &Path, size_px: u32) -> bool {
        self.by_kind
            .contains_key(&format!("path:{}@{}", path.display(), size_px))
    }

    /// Fetch-and-cache a path-keyed icon at `size_px`, off the render
    /// path (NSWorkspace `iconForFile:` is main-thread-only, so this
    /// must run from a deferred/non-render main-thread context). Caches
    /// the blank placeholder on failure so the warm loop converges.
    pub fn warm_folder_icon_sized(&mut self, path: &Path, size_px: u32) {
        let key = format!("path:{}@{}", path.display(), size_px);
        if self.by_kind.contains_key(&key) || feraille_core::path_guard::is_rendering() {
            return;
        }
        let icon = match fetch_icon_rgba(path, size_px) {
            Some((rgba, w, h)) => Arc::new(build_render_image(rgba, w, h)),
            None => self.blank_icon(),
        };
        self.by_kind.insert(key, icon);
    }

    /// Whether `img` is the shared blank placeholder — i.e. the platform
    /// couldn't produce an icon (either a transient failure, or a platform
    /// whose `fetch_icon_rgba` is still a stub: Linux scaffold, AROS).
    /// Render paths use this to fall back to the Lucide type glyph instead
    /// of an empty slot.
    pub fn is_blank(&self, img: &Arc<RenderImage>) -> bool {
        self.blank.as_ref().is_some_and(|b| Arc::ptr_eq(b, img))
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
        EntryKind::Directory => format!("path:{}", path.display()),
        EntryKind::Symlink => "<symlink>".into(),
        EntryKind::File => path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_lowercase()))
            .unwrap_or_else(|| "<noext>".into()),
    }
}

// =============================================================================
// Phase 1 (next-level plan): Lucide-style file-type icons.
// =============================================================================
//
// Strategy: the macOS NSWorkspace path above stays the default for
// folders + volumes (users customise folder icons, sync overlays look
// nice). Files use the bundle-shipped Lucide SVGs below — outlined
// glyphs that tint via theme tokens, give the file list a scannable
// visual rhythm without relying on extension-specific raster icons.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileTypeTint {
    Folder,
    Image,
    Video,
    Audio,
    Document,
    Code,
    Archive,
    Disk,
    Executable,
    Symlink,
    Unknown,
}

pub struct FileTypeIcon {
    /// Asset-source path the upstream `Icon` / `svg()` resolves.
    pub path: &'static str,
    pub tint: FileTypeTint,
}

/// Classify a file entry into a tinted icon. Pure function over the
/// already-stored display fields — does no I/O, no extension parsing
/// at paint time beyond a small ASCII match on the existing name.
pub fn file_type_icon(entry: &FileEntry) -> FileTypeIcon {
    match entry.kind {
        EntryKind::Directory => {
            return FileTypeIcon {
                path: "icons/folder.svg",
                tint: FileTypeTint::Folder,
            };
        }
        EntryKind::Symlink => {
            return FileTypeIcon {
                path: "icons/file/symlink.svg",
                tint: FileTypeTint::Symlink,
            };
        }
        EntryKind::File => {}
    }
    let ext = entry
        .name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let tint = classify_file(&entry.name, &entry.display_magic);
    // Extension-specific SVG overrides — keep the category tint, swap
    // the asset path for richer differentiation at row scale. Falls
    // through to the tint's default icon when the extension doesn't
    // earn a bespoke glyph.
    let path = match ext.as_str() {
        "pdf" => "icons/file/pdf.svg",
        "html" | "htm" => "icons/file/html.svg",
        "csv" | "tsv" | "xls" | "xlsx" | "ods" | "numbers" => "icons/file/spreadsheet.svg",
        _ => match tint {
            FileTypeTint::Image => "icons/file/image.svg",
            FileTypeTint::Video => "icons/file/video.svg",
            FileTypeTint::Audio => "icons/file/audio.svg",
            FileTypeTint::Document => "icons/file/text.svg",
            FileTypeTint::Code => "icons/file/code.svg",
            FileTypeTint::Archive => "icons/file/archive.svg",
            FileTypeTint::Disk => "icons/file/disk.svg",
            FileTypeTint::Executable => "icons/file/app.svg",
            FileTypeTint::Symlink => "icons/file/symlink.svg",
            FileTypeTint::Folder => "icons/folder.svg",
            FileTypeTint::Unknown => "icons/file/generic.svg",
        },
    };
    FileTypeIcon { path, tint }
}

fn classify_file(name: &str, magic: &str) -> FileTypeTint {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    // Extension is the primary signal — users renaming files
    // deliberately get the look they expect from the name.
    let by_ext = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "ico" | "heic"
        | "heif" | "svg" | "avif" | "psd" => Some(FileTypeTint::Image),
        "mp4" | "mov" | "avi" | "mkv" | "webm" | "m4v" | "mpg" | "mpeg" | "wmv" | "flv" => {
            Some(FileTypeTint::Video)
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" | "aiff" | "alac" | "wma" => {
            Some(FileTypeTint::Audio)
        }
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "pages" | "epub" | "txt" | "md" | "markdown"
        | "rst" | "log" | "tex" | "csv" | "tsv" | "xls" | "xlsx" | "ods" | "numbers" | "ppt"
        | "pptx" | "odp" | "keynote" => Some(FileTypeTint::Document),
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "mjs" | "cjs" | "go" | "rb" | "c" | "cpp"
        | "cc" | "h" | "hpp" | "java" | "kt" | "swift" | "json" | "toml" | "yaml" | "yml"
        | "xml" | "html" | "htm" | "css" | "scss" | "sass" | "less" | "sh" | "bash" | "zsh"
        | "fish" | "vim" | "lua" | "sql" | "graphql" | "proto" | "ml" | "hs" | "ex" | "exs"
        | "erl" | "scala" | "clj" | "el" | "asm" | "dart" => Some(FileTypeTint::Code),
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" | "lz" | "lzma" => {
            Some(FileTypeTint::Archive)
        }
        "dmg" | "iso" | "img" | "vmdk" | "qcow2" | "vhd" | "vdi" | "sparseimage" => {
            Some(FileTypeTint::Disk)
        }
        "app" | "exe" | "dll" | "so" | "dylib" | "bin" | "appimage" | "msi" | "deb" | "rpm"
        | "pkg" => Some(FileTypeTint::Executable),
        _ => None,
    };
    if let Some(t) = by_ext {
        return t;
    }
    // Fallback: magic-derived description.
    let m = magic.to_ascii_lowercase();
    if m.contains("image") {
        return FileTypeTint::Image;
    }
    if m.contains("video") {
        return FileTypeTint::Video;
    }
    if m.contains("audio") {
        return FileTypeTint::Audio;
    }
    if m.contains("zip")
        || m.contains("archive")
        || m.contains("compressed")
        || m.contains("gzip")
        || m.contains("tar")
    {
        return FileTypeTint::Archive;
    }
    if m.contains("mach-o") || m.contains("elf") || m.contains("executable") {
        return FileTypeTint::Executable;
    }
    if m.contains("disk image") || m.contains("iso") {
        return FileTypeTint::Disk;
    }
    if m.contains("text") || m.contains("script") || m.contains("source") {
        return FileTypeTint::Document;
    }
    FileTypeTint::Unknown
}

/// Resolve a tint to a concrete `Hsla` using theme tokens. Uses the
/// theme's chart palette (5 visually distinct hues by design) plus
/// `primary` / `danger` / `muted_foreground` for the semantic slots.
/// No hard-coded HSLA values — every colour rides the active theme.
pub fn tint_color(tint: FileTypeTint, cx: &App) -> Hsla {
    let theme = cx.theme();
    match tint {
        FileTypeTint::Folder => theme.primary,
        FileTypeTint::Image => theme.chart_1,
        FileTypeTint::Video => theme.chart_2,
        FileTypeTint::Audio => theme.chart_3,
        FileTypeTint::Document => theme.chart_4,
        FileTypeTint::Code => theme.chart_5,
        FileTypeTint::Archive => theme.muted_foreground,
        FileTypeTint::Disk => theme.info,
        FileTypeTint::Executable => theme.danger,
        FileTypeTint::Symlink => theme.info,
        FileTypeTint::Unknown => theme.muted_foreground,
    }
}

/// Build a `RenderImage` from RGBA8888 bytes by swapping channels
/// in place (BGRA is what gpui's renderer expects) and wrapping in
/// a single-frame SmallVec.
pub(crate) fn build_render_image(mut rgba: Vec<u8>, w: u32, h: u32) -> RenderImage {
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let buf = RgbaImage::from_raw(w, h, rgba).expect("rgba dims match");
    let frame = Frame::new(buf);
    RenderImage::new(SmallVec::from_elem(frame, 1))
}
