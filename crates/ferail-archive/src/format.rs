//! Archive format identity and extension-based detection.
//!
//! Detection here is **lexical only** — it looks at the filename suffix and
//! never touches the filesystem. That is deliberate: the context-menu builder
//! and other UI-thread callers decide whether a path is an archive on the hot
//! path, so the decision must not stat, open, or sniff magic bytes (Prime
//! Directive). Content-based confirmation (real magic sniffing) happens
//! off-thread in `ferail-fs-native` when an archive is actually opened.

/// A supported archive format.
///
/// The set is intentionally bounded to the formats Ferail ships codecs for
/// (see the workspace archive plan). "Any format" is not a goal — the long
/// tail (rar, ace, obscure codecs) costs far more than it returns. New arms
/// land here together with their codec in `ferail-fs-native` and their row
/// in [`crate::Capabilities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// `.zip` — random-access, editable, password-capable. The workhorse.
    Zip,
    /// `.tar` — uncompressed archive; no central directory, so not editable
    /// in place.
    Tar,
    /// `.tar.gz` / `.tgz` — gzip-compressed tar stream.
    TarGz,
    /// `.tar.bz2` / `.tbz2` — bzip2-compressed tar stream.
    TarBz2,
    /// `.tar.xz` / `.txz` — xz/LZMA-compressed tar stream.
    TarXz,
    /// `.gz` — a single gzip-compressed member (not a multi-file archive).
    Gzip,
    /// `.bz2` — a single bzip2-compressed member.
    Bzip2,
    /// `.xz` — a single xz/LZMA-compressed member.
    Xz,
    /// `.7z` — 7-Zip container. Read + extract only in v1 (`sevenz-rust`
    /// has no stable write path we expose yet).
    SevenZ,
    /// `.lha` / `.lzh` — the Amiga/MS-DOS era LHarc container, still the
    /// dominant archive format on Aminet and AmigaOS-family systems. Read +
    /// extract only: `delharc` decodes every method we care about (`-lh0-`
    /// stored through `-lh7-`, plus `-lz*-` and `-lhd-` directories) but does
    /// not compress.
    Lha,
}

impl Format {
    /// Detect a format from a path's filename suffix. Case-insensitive.
    ///
    /// Multi-part tar suffixes (`.tar.gz`) are checked before their single
    /// counterparts (`.gz`) so a tarball is never mistaken for a lone gzip
    /// member. Returns `None` for anything not recognized as an archive —
    /// the caller uses that to decide whether Extract / Open-as-archive is
    /// even offered.
    pub fn from_path(path: &str) -> Option<Format> {
        // Lowercase the leaf only; we never touch the filesystem here.
        let leaf = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let lower = leaf.to_ascii_lowercase();

        // Longest / most-specific suffixes first.
        const MULTI: &[(&str, Format)] = &[
            (".tar.gz", Format::TarGz),
            (".tgz", Format::TarGz),
            (".tar.bz2", Format::TarBz2),
            (".tbz2", Format::TarBz2),
            (".tbz", Format::TarBz2),
            (".tar.xz", Format::TarXz),
            (".txz", Format::TarXz),
        ];
        for (suffix, fmt) in MULTI {
            if lower.ends_with(suffix) {
                return Some(*fmt);
            }
        }

        const SINGLE: &[(&str, Format)] = &[
            (".zip", Format::Zip),
            (".tar", Format::Tar),
            (".gz", Format::Gzip),
            (".bz2", Format::Bzip2),
            (".xz", Format::Xz),
            (".7z", Format::SevenZ),
            (".lha", Format::Lha),
            (".lzh", Format::Lha),
        ];
        for (suffix, fmt) in SINGLE {
            if lower.ends_with(suffix) {
                return Some(*fmt);
            }
        }
        None
    }

    /// Whether `path` looks like a supported archive (any format).
    pub fn is_archive_path(path: &str) -> bool {
        Format::from_path(path).is_some()
    }

    /// The canonical filename extension for this format, without the dot
    /// (e.g. `"tar.gz"`). Used when building a default output name.
    pub fn canonical_extension(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::Tar => "tar",
            Format::TarGz => "tar.gz",
            Format::TarBz2 => "tar.bz2",
            Format::TarXz => "tar.xz",
            Format::Gzip => "gz",
            Format::Bzip2 => "bz2",
            Format::Xz => "xz",
            Format::SevenZ => "7z",
            Format::Lha => "lha",
        }
    }

    /// A short human label for menus and the archive-view breadcrumb.
    pub fn label(self) -> &'static str {
        match self {
            Format::Zip => "ZIP",
            Format::Tar => "TAR",
            Format::TarGz => "TAR.GZ",
            Format::TarBz2 => "TAR.BZ2",
            Format::TarXz => "TAR.XZ",
            Format::Gzip => "GZIP",
            Format::Bzip2 => "BZIP2",
            Format::Xz => "XZ",
            Format::SevenZ => "7-Zip",
            Format::Lha => "LHA",
        }
    }

    /// Whether this format wraps a `tar` stream. Tar-family archives share a
    /// codec path (untar after decompressing the stream) and share the same
    /// "no in-place edit" capability.
    pub fn is_tar_family(self) -> bool {
        matches!(
            self,
            Format::Tar | Format::TarGz | Format::TarBz2 | Format::TarXz
        )
    }

    /// Whether this format is a single compressed member rather than a
    /// multi-file archive (`.gz` / `.bz2` / `.xz` on their own). These have
    /// exactly one logical entry — decompressing yields one file.
    pub fn is_single_member(self) -> bool {
        matches!(self, Format::Gzip | Format::Bzip2 | Format::Xz)
    }
}

/// Compression effort for a create operation. Kept to four named steps
/// ("options that suit most people") rather than a raw 0–9 knob; the codec
/// layer maps each step to the concrete level its backend expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressionLevel {
    /// No compression — fastest, store only (where the format supports it).
    Store,
    /// Light compression, favor speed.
    Fast,
    /// Balanced default.
    #[default]
    Normal,
    /// Maximum compression, favor size over speed.
    Maximum,
}

impl CompressionLevel {
    /// A user-facing label for the create dialog.
    pub fn label(self) -> &'static str {
        match self {
            CompressionLevel::Store => "Store (no compression)",
            CompressionLevel::Fast => "Fast",
            CompressionLevel::Normal => "Normal",
            CompressionLevel::Maximum => "Maximum",
        }
    }
}
