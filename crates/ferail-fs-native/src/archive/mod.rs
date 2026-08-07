//! Archive codec layer: the bytes-move half of Ferail's archive support.
//!
//! `ferail-archive` owns the pure model — the [`Format`] enum, the capability
//! matrix, the [`Toc`] shape, and the zip-slip [`safe_relative_path`] guard.
//! This module is where those turn into real I/O: parsing an archive's table of
//! contents, extracting entries, and (later) creating new archives. It backs
//! both product surfaces — the quick-action Extract command and the embedded
//! archive workbench — plus the accurate compressed-file descriptions.
//!
//! # Prime Directive
//!
//! Every public entry point here does blocking I/O and MUST run off the UI
//! thread; each asserts that in debug builds via
//! [`ferail_core::path_guard::assert_off_ui_thread`], the same tripwire the
//! enumeration and magic paths use. Callers schedule this work on the
//! background executor (see `Shell::spawn_file_op`) and report back through
//! entity updates.
//!
//! # Two read modes
//!
//! Reading is split by cost, because formats differ in how expensive their
//! metadata is:
//!
//! - [`read_toc`] — the **full** table of contents. Used when the archive is
//!   actually opened in the workbench. May stream the whole archive for
//!   tar-family formats (which have no central directory).
//! - [`read_summary`] — a **bounded** metadata read for the Description column.
//!   Cheap for formats with a directory record (zip, 7z) and for single-member
//!   compressors; deliberately format-level-only for tar-family so rendering a
//!   column label never stream-decompresses a multi-gigabyte tarball.

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use ferail_archive::{safe_relative_path, CompressionLevel, Format, Toc};

use crate::file_ops::TransferProgress;

mod lha;
pub mod scratch;
mod sevenz;
mod single;
mod tarball;
mod zip_codec;

#[cfg(test)]
mod tests;

/// A bounded, cheap-to-read summary of an archive, for the Description column.
///
/// Fields are `None`/`false` when the fact was not cheaply available — a
/// tar-family archive reports its [`Format`] but no `file_count`, because
/// counting would mean decompressing the whole stream.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArchiveSummary {
    /// Number of entries, when the format records it in a directory we can
    /// read without inflating payloads.
    pub file_count: Option<u32>,
    /// Single shared top-level folder, when every entry lives under one.
    pub root: Option<String>,
    /// Whether the archive (or any entry) is encrypted.
    pub encrypted: bool,
    /// Total uncompressed size when every entry reports it cheaply.
    pub total_uncompressed: Option<u64>,
}

/// Errors from the archive codec layer. Typed rather than stringly so the
/// workbench can react — most importantly, distinguish "this archive needs a
/// password" (prompt the user) from a genuine failure.
#[derive(Debug)]
pub enum ArchiveError {
    /// The path's extension is not a supported archive format.
    UnsupportedFormat,
    /// The archive or a requested entry is encrypted and no password was
    /// supplied. The workbench prompts and retries.
    PasswordRequired,
    /// A password was supplied but rejected.
    WrongPassword,
    /// The archive is malformed or truncated.
    Corrupt(String),
    /// The operation was cancelled cooperatively via the cancel flag.
    Cancelled,
    /// Underlying filesystem I/O error.
    Io(std::io::Error),
    /// A codec backend reported an error we do not model more precisely.
    Codec(String),
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArchiveError::UnsupportedFormat => write!(f, "unsupported archive format"),
            ArchiveError::PasswordRequired => write!(f, "archive is encrypted; a password is required"),
            ArchiveError::WrongPassword => write!(f, "incorrect password"),
            ArchiveError::Corrupt(m) => write!(f, "archive is corrupt: {m}"),
            ArchiveError::Cancelled => write!(f, "cancelled"),
            ArchiveError::Io(e) => write!(f, "{e}"),
            ArchiveError::Codec(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

impl From<std::io::Error> for ArchiveError {
    fn from(e: std::io::Error) -> Self {
        ArchiveError::Io(e)
    }
}

/// Detect an archive's [`Format`] from its path, or [`ArchiveError::UnsupportedFormat`].
/// Lexical only (no I/O) — the same rule the UI-thread context-menu builder uses.
pub fn format_of(path: &Path) -> Result<Format, ArchiveError> {
    let name = path.to_string_lossy();
    Format::from_path(&name).ok_or(ArchiveError::UnsupportedFormat)
}

/// Work out how to open `path` as an archive, by extension first and by
/// content second.
///
/// The extension is authoritative when it names a format we support (it also
/// distinguishes `.tar.gz` from a bare `.gz`, which magic bytes cannot). When
/// it says nothing — `.docx`, `.xlsx`, `.pptx`, `.jar`, `.apk`, `.ipa`, or a
/// file with no extension at all — we sniff the header, because every one of
/// those is a zip container underneath and is perfectly browsable.
///
/// Returns `None` for anything we can't open, so the caller can say so plainly
/// rather than showing an empty archive. Blocking (reads the file header), so
/// it runs off the UI thread like the rest of this module.
pub fn probe_format(path: &Path) -> Option<Format> {
    ferail_core::path_guard::assert_off_ui_thread("archive::probe_format");
    if let Ok(format) = format_of(path) {
        return Some(format);
    }
    // Content fallback: the magic table already classifies the zip-based
    // document and app-package containers.
    let info = crate::magic::detect_magic_info(path)?;
    use crate::magic::MagicType;
    match info.magic_type {
        // Every OOXML document, JAR, APK and plain zip is a zip container.
        MagicType::Zip
        | MagicType::ZipEncrypted
        | MagicType::DocWord
        | MagicType::DocWordMacro
        | MagicType::DocExcel
        | MagicType::DocExcelMacro
        | MagicType::DocPowerPoint
        | MagicType::DocPowerPointMacro
        | MagicType::AppJar
        | MagicType::AppApk => Some(Format::Zip),
        MagicType::SevenZip => Some(Format::SevenZ),
        MagicType::Tar => Some(Format::Tar),
        MagicType::Lha => Some(Format::Lha),
        MagicType::Gzip => Some(Format::Gzip),
        MagicType::Xz => Some(Format::Xz),
        MagicType::Bzip2 => Some(Format::Bzip2),
        // Recognised as an archive, but not one this engine can read.
        _ => None,
    }
}

/// Read the full table of contents of `archive`.
///
/// `password` is used for encrypted zip/7z; pass `None` first and retry with a
/// password if this returns [`ArchiveError::PasswordRequired`]. Runs the whole
/// read — for tar-family formats that means streaming the entire archive, so
/// this belongs off the UI thread.
pub fn read_toc(archive: &Path, password: Option<&str>) -> Result<Toc, ArchiveError> {
    ferail_core::path_guard::assert_off_ui_thread("archive::read_toc");
    // Content-aware: a `.docx` has no archive extension but is a zip.
    let format = probe_format(archive).ok_or(ArchiveError::UnsupportedFormat)?;
    match format {
        Format::Zip => zip_codec::read_toc(archive, password),
        Format::SevenZ => sevenz::read_toc(archive, password),
        Format::Lha => lha::read_toc(archive),
        f if f.is_tar_family() => tarball::read_toc(archive, f),
        f => single::read_toc(archive, f),
    }
}

/// Read a bounded summary of `archive` for the Description column.
///
/// Cheap by contract: never decompresses payloads. Tar-family archives return
/// a format-only summary (no counts) — see the module docs on the two read
/// modes.
pub fn read_summary(archive: &Path) -> Result<ArchiveSummary, ArchiveError> {
    ferail_core::path_guard::assert_off_ui_thread("archive::read_summary");
    match format_of(archive)? {
        Format::Zip => zip_codec::read_summary(archive),
        Format::SevenZ => sevenz::read_summary(archive),
        f if f.is_tar_family() => Ok(ArchiveSummary::default()),
        f => single::read_summary(archive, f),
    }
}

// ===========================================================================
// Extraction
// ===========================================================================

/// Options for an extract operation.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExtractOptions<'a> {
    /// Password for encrypted zip/7z entries.
    pub password: Option<&'a str>,
    /// When true, existing files at the destination are overwritten; when
    /// false, they are left in place and recorded as skipped. The destination
    /// *folder* choice (smart wrap, `" 2"` collision suffix) is the caller's
    /// job — this flag only governs per-file clobbering within it.
    pub overwrite: bool,
}

/// Why an archive entry was not extracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The entry path failed the zip-slip guard (traversal / absolute / drive).
    UnsafePath,
    /// A symlink entry — skipped so a malicious link can never be created and
    /// then written through by a later entry.
    Symlink,
    /// A hard-link entry (tar) — skipped for the same reason.
    HardLink,
    /// A device / fifo / other special entry we do not materialize.
    SpecialFile,
    /// The target already existed and `overwrite` was false.
    ExistingNotOverwritten,
    /// The entry uses a compression method this build cannot decode (LHA has
    /// a long tail of historical methods). Skipped rather than written
    /// truncated, so a partial file is never mistaken for a good one.
    UnsupportedMethod,
}

/// One entry that extraction chose not to write, with the reason.
#[derive(Debug, Clone)]
pub struct SkippedEntry {
    pub path: String,
    pub reason: SkipReason,
}

/// The result of an extract operation.
#[derive(Debug, Default)]
pub struct ExtractOutcome {
    /// Top-level paths created under the destination (deduped) — what the undo
    /// step removes and what "reveal" selects.
    pub created: Vec<PathBuf>,
    /// Count of regular files actually written.
    pub files_written: u64,
    /// Entries deliberately not written, with reasons — surfaced to the user
    /// so a skipped file is never silently lost.
    pub skipped: Vec<SkippedEntry>,
}

impl ExtractOutcome {
    fn skip(&mut self, path: String, reason: SkipReason) {
        self.skipped.push(SkippedEntry { path, reason });
    }
}

/// Which entries to extract.
pub(crate) enum Selection {
    /// Every entry in the archive.
    All,
    /// Only entries matching one of these stored paths, or living under one of
    /// them as a directory prefix (so selecting a folder pulls its subtree).
    Subset(Vec<String>),
}

impl Selection {
    fn includes(&self, entry_path: &str) -> bool {
        match self {
            Selection::All => true,
            Selection::Subset(paths) => paths.iter().any(|p| {
                let p = p.trim_end_matches('/');
                let e = entry_path.trim_end_matches('/');
                e == p || e.strip_prefix(p).is_some_and(|rest| rest.starts_with('/'))
            }),
        }
    }
}

/// Extract every entry of `archive` into `dest`.
pub fn extract_all(
    archive: &Path,
    dest: &Path,
    opts: ExtractOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<ExtractOutcome, ArchiveError> {
    ferail_core::path_guard::assert_off_ui_thread("archive::extract_all");
    dispatch_extract(archive, dest, &Selection::All, opts, progress, cancel)
}

/// Extract a cherry-picked subset of `archive` into `dest`. Each string in
/// `entries` is a stored entry path (as it appears in the [`Toc`]); a directory
/// path pulls its whole subtree.
pub fn extract_entries(
    archive: &Path,
    dest: &Path,
    entries: &[&str],
    opts: ExtractOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<ExtractOutcome, ArchiveError> {
    ferail_core::path_guard::assert_off_ui_thread("archive::extract_entries");
    let subset = Selection::Subset(entries.iter().map(|s| s.to_string()).collect());
    dispatch_extract(archive, dest, &subset, opts, progress, cancel)
}

fn dispatch_extract(
    archive: &Path,
    dest: &Path,
    sel: &Selection,
    opts: ExtractOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<ExtractOutcome, ArchiveError> {
    let format = probe_format(archive).ok_or(ArchiveError::UnsupportedFormat)?;
    match format {
        Format::Zip => zip_codec::extract(archive, dest, sel, opts, progress, cancel),
        Format::SevenZ => sevenz::extract(archive, dest, sel, opts, progress, cancel),
        Format::Lha => lha::extract(archive, dest, sel, opts, progress, cancel),
        f if f.is_tar_family() => tarball::extract(archive, f, dest, sel, opts, progress, cancel),
        f => single::extract(archive, f, dest, sel, opts, progress, cancel),
    }
}

// --- shared extraction primitives (used by every codec) --------------------

/// Cooperative cancellation check — codecs call this per entry (and per buffer
/// for large files).
pub(super) fn check_cancel(cancel: &AtomicBool) -> Result<(), ArchiveError> {
    if cancel.load(Ordering::Relaxed) {
        Err(ArchiveError::Cancelled)
    } else {
        Ok(())
    }
}

/// Validate an entry path through the zip-slip guard. On rejection the entry is
/// recorded as skipped and `None` is returned so the caller moves on.
pub(super) fn safe_or_skip(entry_path: &str, outcome: &mut ExtractOutcome) -> Option<String> {
    match safe_relative_path(entry_path) {
        Ok(rel) => Some(rel),
        Err(_) => {
            outcome.skip(entry_path.to_string(), SkipReason::UnsafePath);
            None
        }
    }
}

fn note_created_root(dest: &Path, safe_rel: &str, outcome: &mut ExtractOutcome) {
    if let Some(top) = safe_rel.split('/').next() {
        let p = dest.join(top);
        if !outcome.created.contains(&p) {
            outcome.created.push(p);
        }
    }
}

/// Create `dest/safe_rel` as a directory (and its parents).
pub(super) fn make_dir(
    dest: &Path,
    safe_rel: &str,
    outcome: &mut ExtractOutcome,
) -> Result<(), ArchiveError> {
    std::fs::create_dir_all(dest.join(safe_rel))?;
    note_created_root(dest, safe_rel, outcome);
    Ok(())
}

/// Stream `reader` into `dest/safe_rel`, creating parent directories, honoring
/// `overwrite`, updating `progress`, and honoring `cancel` between buffers.
pub(super) fn write_file<R: Read>(
    dest: &Path,
    safe_rel: &str,
    mut reader: R,
    opts: ExtractOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
    outcome: &mut ExtractOutcome,
) -> Result<(), ArchiveError> {
    let target = dest.join(safe_rel);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = if opts.overwrite {
        std::fs::File::create(&target)?
    } else {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                outcome.skip(safe_rel.to_string(), SkipReason::ExistingNotOverwritten);
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
    };
    progress.note_current(&target);
    let mut buf = [0u8; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        progress.add_bytes(n as u64);
    }
    progress.add_items(1);
    outcome.files_written += 1;
    note_created_root(dest, safe_rel, outcome);
    Ok(())
}

// ===========================================================================
// Creation
// ===========================================================================

/// Options for creating an archive.
#[derive(Debug, Default, Clone, Copy)]
pub struct CreateOptions<'a> {
    /// Compression effort (mapped per-codec).
    pub level: CompressionLevel,
    /// Password — only honored by formats whose capability matrix allows it
    /// (zip); ignored otherwise.
    pub password: Option<&'a str>,
}

/// One planned input, as it will appear inside the new archive.
pub(super) struct PlannedItem {
    /// Absolute source path on disk.
    pub abs: PathBuf,
    /// `/`-joined path inside the archive (rooted at each input's own name, so
    /// compressing `project/` yields `project/...` entries — Finder's
    /// keep-parent shape).
    pub rel: String,
    pub is_dir: bool,
    /// File size in bytes (0 for directories) — used to size progress.
    pub size: u64,
}

/// Create an archive of `format` at `output` from `inputs` (files and/or
/// directories on disk). Directories are walked recursively; **symlinks are
/// skipped** so a cyclic link can never send the walk into a loop.
pub fn create_archive(
    format: Format,
    inputs: &[&Path],
    output: &Path,
    opts: CreateOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<PathBuf, ArchiveError> {
    ferail_core::path_guard::assert_off_ui_thread("archive::create_archive");
    if !format.capabilities().can_create {
        return Err(ArchiveError::Codec(format!(
            "{} archives cannot be created",
            format.label()
        )));
    }

    let (items, total_bytes) = plan_inputs(inputs)?;
    let file_count = items.iter().filter(|i| !i.is_dir).count() as u64;
    progress.begin_transfer(total_bytes, file_count);

    if format.is_single_member() {
        single::create(format, &items, output, opts, progress, cancel)?;
    } else if format.is_tar_family() {
        tarball::create(format, &items, output, opts, progress, cancel)?;
    } else if format == Format::SevenZ {
        sevenz::create(&items, output, opts, progress, cancel)?;
    } else {
        zip_codec::create(&items, output, opts, progress, cancel)?;
    }
    Ok(output.to_path_buf())
}

/// What an [`add_to_archive`] call did.
#[derive(Debug, Default)]
pub struct AddOutcome {
    /// Number of files written into the archive.
    pub added: u64,
    /// Entry paths that were already present and therefore left alone — a
    /// duplicate name would shadow the original rather than replace it, so we
    /// skip and report instead of silently corrupting the archive's meaning.
    pub skipped_existing: Vec<String>,
}

/// Add `inputs` to an **existing** archive, in place.
///
/// Only formats whose capability row allows in-place editing are accepted
/// (zip today). Tar-family archives are append-only streams with no central
/// directory and 7z has no incremental write path, so those return an error
/// rather than silently rewriting the whole file behind the user's back.
pub fn add_to_archive(
    archive: &Path,
    inputs: &[&Path],
    opts: CreateOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<AddOutcome, ArchiveError> {
    ferail_core::path_guard::assert_off_ui_thread("archive::add_to_archive");
    let format = format_of(archive)?;
    if !format.capabilities().can_edit_in_place {
        return Err(ArchiveError::Codec(format!(
            "{} archives can't be modified in place",
            format.label()
        )));
    }

    let (items, total_bytes) = plan_inputs(inputs)?;
    let file_count = items.iter().filter(|i| !i.is_dir).count() as u64;
    progress.begin_transfer(total_bytes, file_count);
    zip_codec::append(archive, &items, opts, progress, cancel)
}

/// Walk `inputs` into a flat list of planned entries. Uses `symlink_metadata`
/// (no follow) so symlinks are classified and skipped rather than traversed.
fn plan_inputs(inputs: &[&Path]) -> Result<(Vec<PlannedItem>, u64), ArchiveError> {
    let mut items = Vec::new();
    let mut total = 0u64;
    // Stack of (absolute path, archive-relative path).
    let mut stack: Vec<(PathBuf, String)> = Vec::new();
    for input in inputs {
        let base = input
            .file_name()
            .map(|n| n.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if base.is_empty() {
            continue;
        }
        stack.push((input.to_path_buf(), base));
    }

    while let Some((abs, rel)) = stack.pop() {
        let meta = std::fs::symlink_metadata(&abs)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            continue; // never follow — avoids cycles and link-escape surprises
        }
        if ft.is_dir() {
            items.push(PlannedItem {
                abs: abs.clone(),
                rel: rel.clone(),
                is_dir: true,
                size: 0,
            });
            for child in std::fs::read_dir(&abs)? {
                let child = child?;
                let name = child.file_name().to_string_lossy().replace('\\', "/");
                stack.push((child.path(), format!("{rel}/{name}")));
            }
        } else if ft.is_file() {
            total = total.saturating_add(meta.len());
            items.push(PlannedItem {
                abs,
                rel,
                is_dir: false,
                size: meta.len(),
            });
        }
        // Other special files (fifo/device) are not archivable — skip.
    }
    Ok((items, total))
}

/// Map the four named effort steps to a 0–9 backend level for the stream
/// codecs (deflate / gzip / xz). bzip2 has no level 0, so its caller clamps.
pub(super) fn stream_level(level: CompressionLevel) -> u32 {
    match level {
        CompressionLevel::Store => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Normal => 6,
        CompressionLevel::Maximum => 9,
    }
}

/// Stream `reader` into `writer`, updating progress bytes and honoring cancel
/// between buffers. Used by the zip and single-member creators, which control
/// their own copy (tar's `Builder` copies internally).
pub(super) fn copy_stream<R: Read, W: Write>(
    mut reader: R,
    writer: &mut W,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<(), ArchiveError> {
    let mut buf = [0u8; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        progress.add_bytes(n as u64);
    }
    Ok(())
}
