//! The archive table-of-contents model.
//!
//! An [`ArchiveEntry`] is one record parsed from an archive's directory — a
//! *virtual* file that has no path on disk until extracted. The codec layer in
//! `ferail-fs-native` produces a [`Toc`] off-thread; the archive view renders
//! it by synthesizing file-list rows from these records (they carry
//! pre-computed display fields for exactly that reason — the render path never
//! recomputes anything).
//!
//! Entry paths use forward slashes as the internal separator regardless of the
//! host platform, matching zip/tar on-wire convention.

/// One entry in an archive's table of contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// Full path of the entry inside the archive, `/`-separated
    /// (e.g. `"project/src/main.rs"`). This is the stored path *before* any
    /// zip-slip validation — extraction must still sanitize it against the
    /// destination.
    pub path: String,
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// Uncompressed size in bytes. `None` when the format does not record it
    /// up front (some streamed formats).
    pub uncompressed_size: Option<u64>,
    /// Compressed size in bytes, when the format records it.
    pub compressed_size: Option<u64>,
    /// Last-modified time as a Unix timestamp, when present.
    pub mtime_unix: Option<i64>,
    /// Whether this specific entry's data is encrypted (per-entry encryption
    /// exists in zip). The view uses this to badge entries and to know a
    /// password is required before extraction.
    pub encrypted: bool,
}

impl ArchiveEntry {
    /// The leaf (last path component) for display in a row.
    pub fn leaf(&self) -> &str {
        let trimmed = self.path.trim_end_matches('/');
        trimmed.rsplit('/').next().unwrap_or(trimmed)
    }

    /// Depth in the entry tree — number of `/`-separated ancestors. A
    /// top-level entry has depth 0. Used to detect the single-root case.
    pub fn depth(&self) -> usize {
        self.path.trim_end_matches('/').matches('/').count()
    }

    /// The first path component (top-level ancestor) of this entry, if any.
    /// `"a/b/c"` and `"a/"` both yield `"a"`.
    pub fn top_component(&self) -> &str {
        let trimmed = self.path.trim_start_matches('/');
        trimmed.split('/').next().unwrap_or(trimmed)
    }
}

/// A parsed archive table of contents plus the facts the workbench needs
/// before it can act.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Toc {
    /// All entries, in the order the archive stored them.
    pub entries: Vec<ArchiveEntry>,
    /// Whether opening/extracting this archive requires a password (any
    /// entry is encrypted, or the header itself is encrypted). The view uses
    /// this to prompt before listing content data.
    pub needs_password: bool,
}

impl Toc {
    /// The single distinct top-level component shared by every entry, if one
    /// exists — the "does this archive have one root folder" question that
    /// drives smart extraction. `Some("project")` means every entry lives
    /// under `project/`, so extraction can land in place without wrapping;
    /// `None` means the archive has multiple top-level items and extraction
    /// should create a containing folder named after the archive.
    pub fn single_root(&self) -> Option<&str> {
        let mut root: Option<&str> = None;
        for entry in &self.entries {
            let top = entry.top_component();
            if top.is_empty() {
                return None;
            }
            match root {
                None => root = Some(top),
                Some(existing) if existing == top => {}
                Some(_) => return None,
            }
        }
        root
    }

    /// Total uncompressed size across all file entries, when every entry
    /// reports its size. `None` if any size is unknown (so callers do not
    /// show a misleadingly-low total).
    pub fn total_uncompressed(&self) -> Option<u64> {
        let mut total: u64 = 0;
        for entry in &self.entries {
            if entry.is_dir {
                continue;
            }
            total = total.checked_add(entry.uncompressed_size?)?;
        }
        Some(total)
    }

    /// Number of non-directory entries.
    pub fn file_count(&self) -> usize {
        self.entries.iter().filter(|e| !e.is_dir).count()
    }
}
