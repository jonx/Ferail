//! Domain types shared between FS, controls, and app layers.
//! This crate has zero platform deps and zero UI deps. That is enforced by
//! convention, not the compiler — if you find yourself reaching for `windows`
//! or `winit` here, stop.

pub mod commands;
pub mod favorites;
pub mod navigation;
pub mod node_store;
pub mod path_guard;

use std::num::NonZeroU64;

/// Stable identifier for a tree/list node. Opaque to the UI; the FS layer
/// owns the mapping `NodeId <-> path/PIDL`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NodeId(NonZeroU64);

impl NodeId {
    pub fn from_raw(raw: u64) -> Option<Self> {
        NonZeroU64::new(raw).map(Self)
    }
    pub fn as_raw(self) -> u64 {
        self.0.get()
    }
}

impl From<u64> for NodeId {
    fn from(v: u64) -> Self {
        Self(NonZeroU64::new(v.max(1)).expect("post-max nonzero"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
}

/// One row in the file pane. Display strings are pre-formatted; paint never
/// formats numbers or dates.
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub id: NodeId,
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_unix: i64,
    pub display_size: String,
    pub display_mtime: String,
    /// Friendly type label — "Folder", "Symlink", uppercased extension
    /// (e.g. "RS", "MD"), or "File" when there's no extension. macOS shell
    /// crate (iter-4) replaces this with `NSWorkspace.localizedDescription`.
    pub display_kind: String,
    /// Magic-byte detected type, e.g. "PNG image", "Mach-O 64-bit", "Plain text".
    /// Empty string when not yet detected or no match. Populated lazily by
    /// the host (App) — `feraille-core` never blocks on file I/O.
    pub display_magic: String,
    /// Hot-path flag for the icon-overlay dot. True when the file carries
    /// `com.apple.quarantine` (macOS Mark-of-the-Web equivalent). Populated
    /// lazily by the host alongside `quarantine`; defaults to false.
    pub is_quarantined: bool,
    /// Detail-panel rows for downloaded files. `None` until the prefetch
    /// worker reports back; `Some` with empty fields means "we looked,
    /// nothing to show beyond the flag."
    pub quarantine: Option<QuarantineDetails>,
}

impl FileEntry {
    /// Unified Format label for the file list (next-level Phase 1):
    /// prefer the magic-detected description, fall back to the
    /// extension-derived kind. Returns `(primary, has_mismatch)` where
    /// `has_mismatch` is true when both fields are populated *and*
    /// they describe genuinely different format families (a renamed
    /// PDF claiming `.txt`, for example) — not just terminology
    /// differences (e.g. `JSON` / `Plain text` is fine).
    pub fn format_label(&self) -> (String, bool) {
        let mag = self.display_magic.trim();
        let kind = self.display_kind.trim();
        if mag.is_empty() {
            return (kind.to_string(), false);
        }
        if kind.is_empty() {
            return (mag.to_string(), false);
        }
        (mag.to_string(), !formats_compatible(kind, mag))
    }
}

/// Heuristic: do the extension-derived `kind` and the magic-detected
/// `magic` strings describe compatible format families? Used to drive
/// the file-list mismatch indicator without raising false alarms for
/// the common "extension says JSON, magic says plain-text" case.
fn formats_compatible(kind: &str, magic: &str) -> bool {
    let k = normalize_format(kind);
    let m = normalize_format(magic);
    if k.is_empty() || m.is_empty() {
        return true;
    }
    // Placeholder kinds ("File" / "Folder" / "Symlink") fire when a
    // file has no extension or we couldn't derive one — they're
    // *missing* information, not an assertion about format. A Mach-O
    // binary with no extension still shows kind="File", which
    // doesn't contradict the magic-detected type. Same for folders
    // and symlinks (which won't reach magic detection but we belt-
    // and-suspender it).
    if matches!(k.as_str(), "file" | "folder" | "symlink") {
        return true;
    }
    if k == m || m.contains(&k) || k.contains(&m) {
        return true;
    }
    // Textual extensions all live happily under "plain text" / "ascii text" / "utf-8".
    let textual = [
        "txt", "md", "markdown", "rst", "log", "json", "yaml",
        "toml", "ini", "csv", "tsv", "xml", "html", "css", "scss",
        "rs", "py", "js", "ts", "go", "rb", "c", "cpp", "h", "hpp",
        "java", "kt", "swift", "sh", "bash", "zsh", "vim", "lua",
        "sql", "graphql", "proto", "tex", "el", "svg",
    ];
    if (m.contains("text") || m.contains("script") || m.contains("source"))
        && textual.iter().any(|t| k.contains(t))
    {
        return true;
    }
    // Office / EPUB / JAR / APK formats are ZIP archives at the byte level.
    let zip_kindly = [
        "docx", "xlsx", "pptx", "epub", "jar", "apk", "ipa", "odt",
        "ods", "odp", "zip", "war",
    ];
    if m.contains("zip") && zip_kindly.iter().any(|t| k.contains(t)) {
        return true;
    }
    false
}

/// Normalize a format label for comparison. Strips common qualifier
/// words (`image`, `archive`, `document`, `file`, `data`), then maps
/// known aliases to a single canonical spelling so e.g. `JPG` and
/// `JPEG image` both reduce to `jpeg`. Pure ASCII so the lowercasing
/// is locale-independent.
fn normalize_format(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let stripped = lower
        .replace(" image", "")
        .replace(" archive", "")
        .replace(" document", "")
        .replace(" file", "")
        .replace(" data", "")
        .trim()
        .to_string();
    match stripped.as_str() {
        "jpg" | "jpeg" => "jpeg".to_string(),
        "tif" | "tiff" => "tiff".to_string(),
        "htm" | "html" => "html".to_string(),
        "mpg" | "mpeg" => "mpeg".to_string(),
        "yml" | "yaml" => "yaml".to_string(),
        "md" | "markdown" => "markdown".to_string(),
        "rs" | "rust" => "rust".to_string(),
        "py" | "python" => "python".to_string(),
        "js" | "javascript" => "javascript".to_string(),
        "ts" | "typescript" => "typescript".to_string(),
        _ => stripped,
    }
}

#[cfg(test)]
mod format_label_tests {
    use super::*;

    fn entry(kind: &str, magic: &str) -> FileEntry {
        FileEntry {
            id: NodeId(std::num::NonZeroU64::new(1).unwrap()),
            name: String::new(),
            kind: EntryKind::File,
            size: 0,
            mtime_unix: 0,
            display_size: String::new(),
            display_mtime: String::new(),
            display_kind: kind.into(),
            display_magic: magic.into(),
            is_quarantined: false,
            quarantine: None,
        }
    }

    #[test]
    fn magic_is_primary() {
        let (label, mismatch) = entry("PNG", "PNG image").format_label();
        assert_eq!(label, "PNG image");
        assert!(!mismatch);
    }

    #[test]
    fn empty_magic_falls_back_to_kind() {
        let (label, mismatch) = entry("PDF", "").format_label();
        assert_eq!(label, "PDF");
        assert!(!mismatch);
    }

    #[test]
    fn json_vs_plain_text_is_compatible() {
        let (_, mismatch) = entry("JSON", "Plain text").format_label();
        assert!(!mismatch);
    }

    #[test]
    fn docx_vs_zip_archive_is_compatible() {
        let (_, mismatch) = entry("DOCX", "Zip archive").format_label();
        assert!(!mismatch);
    }

    #[test]
    fn png_kind_vs_plain_text_magic_is_mismatch() {
        let (_, mismatch) = entry("PNG", "Plain text").format_label();
        assert!(mismatch, "PNG declared but content is text → flag");
    }

    #[test]
    fn pdf_kind_vs_zip_archive_magic_is_mismatch() {
        let (_, mismatch) = entry("PDF", "Zip archive").format_label();
        assert!(mismatch, "PDF declared but content is zip → flag");
    }

    // Regression: phase 1 review caught these as false positives —
    // alias normalization in normalize_format covers them now.
    #[test]
    fn jpg_kind_vs_jpeg_image_magic_is_compatible() {
        let (_, mismatch) = entry("JPG", "JPEG image").format_label();
        assert!(!mismatch, "jpg ≡ jpeg");
    }

    #[test]
    fn tif_kind_vs_tiff_image_magic_is_compatible() {
        let (_, mismatch) = entry("TIF", "TIFF image").format_label();
        assert!(!mismatch, "tif ≡ tiff");
    }

    #[test]
    fn htm_kind_vs_html_magic_is_compatible() {
        let (_, mismatch) = entry("HTM", "HTML document").format_label();
        assert!(!mismatch, "htm ≡ html");
    }

    #[test]
    fn yml_kind_vs_yaml_magic_is_compatible() {
        let (_, mismatch) = entry("YML", "YAML data").format_label();
        assert!(!mismatch, "yml ≡ yaml");
    }

    #[test]
    fn pdf_kind_vs_pdf_document_magic_is_compatible() {
        let (_, mismatch) = entry("PDF", "PDF document").format_label();
        assert!(!mismatch, "qualifier strip — pdf == pdf document");
    }

    #[test]
    fn png_image_kind_vs_png_image_magic_is_compatible() {
        // Both sides arrive as the same display string; trivial equality
        // after normalisation.
        let (_, mismatch) = entry("PNG image", "PNG image").format_label();
        assert!(!mismatch);
    }

    #[test]
    fn placeholder_file_kind_never_mismatches_magic() {
        // No-extension files surface as kind="File" — that's a missing-
        // info placeholder, not an assertion about the format. It
        // shouldn't ever flag a mismatch.
        let (_, mismatch) = entry("File", "Mach-O 64-bit").format_label();
        assert!(
            !mismatch,
            "kind=File is a placeholder, can't disagree with magic"
        );
        let (_, mismatch) = entry("File", "ELF executable").format_label();
        assert!(!mismatch);
    }

    #[test]
    fn folder_and_symlink_kinds_never_mismatch() {
        let (_, mismatch) = entry("Folder", "directory").format_label();
        assert!(!mismatch);
        let (_, mismatch) = entry("Symlink", "symbolic link").format_label();
        assert!(!mismatch);
    }
}

/// Display-ready provenance fields for a quarantined file. Strings are
/// pre-formatted in the worker so paint never allocates or parses.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QuarantineDetails {
    /// Quarantining agent name from the `com.apple.quarantine` string —
    /// e.g. "Safari", "com.google.Chrome". `None` when the field was empty.
    pub agent: Option<String>,
    /// ISO-8601 download timestamp from the quarantine record. `None` when
    /// missing or unparseable.
    pub downloaded_iso: Option<String>,
    /// Source URLs from `kMDItemWhereFroms`. May be empty.
    pub where_from: Vec<String>,
}

/// Filesystem trait — implemented by `feraille-fs-native` (cross-platform std::fs)
/// and `feraille-shell-win32` (Windows shell namespace, PIDLs, virtual roots).
/// The UI talks to *this*, never to platform APIs directly.
pub trait FsBackend: Send + Sync {
    /// Begin an enumeration of `node`. The returned handle can be polled for
    /// streamed batches; the UI never blocks.
    fn enumerate(&self, node: NodeId) -> EnumerationHandle;
}

/// Why an enumeration failed to produce a complete listing. UI surfaces
/// this as an empty-state when `initial` is empty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnumerationError {
    /// macOS TCC / Unix EACCES — the user can grant access via System
    /// Settings → Privacy & Security → Files and Folders (macOS) or by
    /// running with appropriate permissions (Linux).
    PermissionDenied,
    /// Path doesn't exist or has been moved/deleted.
    NotFound,
    /// Other I/O error. The string is a human-readable hint, not a
    /// programmable code.
    Other(String),
}

/// Opaque handle to a streamed enumeration. Real impl pushes batches over a
/// channel; the slice's stub returns one synchronous batch. `error` is
/// `Some` only on hard failure — partial listings are not currently
/// represented (would land alongside async enumeration).
pub struct EnumerationHandle {
    pub initial: Vec<FileEntry>,
    pub error: Option<EnumerationError>,
}

/// Folder usage heat — how many times the user has navigated into a node.
/// Iter-3 keeps this in-memory; iter-6 persists it to SQLite (matching
/// Ferail's predecessor implementation). Heat is reported normalized to
/// the most-visited node so a single very-busy folder doesn't washed out
/// the rest.
#[derive(Default, Clone, Debug)]
pub struct AntTrail {
    visits: std::collections::HashMap<NodeId, u32>,
    max: u32,
}

impl AntTrail {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, id: NodeId) {
        let v = self.visits.entry(id).or_insert(0);
        *v += 1;
        if *v > self.max {
            self.max = *v;
        }
    }

    /// 0.0..=1.0 normalized heat. Log-scaled so a 10-visit folder isn't
    /// 10× brighter than a 5-visit one. Returns 0.0 for never-visited.
    pub fn heat(&self, id: NodeId) -> f32 {
        let Some(&v) = self.visits.get(&id) else { return 0.0 };
        if self.max <= 1 {
            return 1.0;
        }
        ((v as f32 + 1.0).log2() / (self.max as f32 + 1.0).log2()).clamp(0.0, 1.0)
    }

    /// Up to `n` most-visited NodeIds, descending by visit count.
    /// Ties broken by NodeId order. Used by the tree to populate the
    /// "Recents" section.
    pub fn most_visited(&self, n: usize) -> Vec<NodeId> {
        let mut v: Vec<(NodeId, u32)> = self.visits.iter().map(|(k, c)| (*k, *c)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v.into_iter().take(n).map(|(id, _)| id).collect()
    }
}
