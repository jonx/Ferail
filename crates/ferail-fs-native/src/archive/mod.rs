//! Archive codec layer: the bytes-move half of Ferail's archive support.
//!
//! `ferail-archive` owns the pure model: the [`Format`] enum, the capability
//! matrix, the [`Toc`] shape, and the zip-slip [`safe_relative_path`] guard.
//! This module is where those turn into real I/O: parsing an archive's table of
//! contents, extracting entries, and (later) creating new archives. It backs
//! both product surfaces: the quick-action Extract command and the embedded
//! archive workbench, plus the accurate compressed-file descriptions.
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
//! - [`read_toc`]: the **full** table of contents. Used when the archive is
//!   actually opened in the workbench. May stream the whole archive for
//!   tar-family formats (which have no central directory).
//! - [`read_summary`]: a **bounded** metadata read for the Description column.
//!   Cheap for formats with a directory record (zip, 7z) and for single-member
//!   compressors; deliberately format-level-only for tar-family so rendering a
//!   column label never stream-decompresses a multi-gigabyte tarball.

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
/// Fields are `None`/`false` when the fact was not cheaply available: a
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
/// workbench can react, most importantly, distinguish "this archive needs a
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
    /// The selected destination is not a creatable multi-file format.
    ConversionTargetUnsupported(Format),
    /// The selected writer cannot encrypt newly created archives.
    ConversionEncryptionUnsupported(Format),
    /// Extraction skipped members, so conversion stopped rather than silently
    /// producing an incomplete archive.
    ConversionUnsafeEntries(usize),
    /// The source stamp changed during conversion.
    ConversionSourceChanged,
    /// The requested output stem is empty or contains a path separator.
    ConversionInvalidName,
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArchiveError::UnsupportedFormat => write!(f, "unsupported archive format"),
            ArchiveError::PasswordRequired => {
                write!(f, "archive is encrypted; a password is required")
            }
            ArchiveError::WrongPassword => write!(f, "incorrect password"),
            ArchiveError::Corrupt(m) => write!(f, "archive is corrupt: {m}"),
            ArchiveError::Cancelled => write!(f, "cancelled"),
            ArchiveError::Io(e) => write!(f, "{e}"),
            ArchiveError::Codec(m) => write!(f, "{m}"),
            ArchiveError::ConversionTargetUnsupported(format) => {
                write!(
                    f,
                    "{} is not a multi-file conversion target",
                    format.label()
                )
            }
            ArchiveError::ConversionEncryptionUnsupported(format) => write!(
                f,
                "{} creation does not support password encryption",
                format.label()
            ),
            ArchiveError::ConversionUnsafeEntries(count) => write!(
                f,
                "conversion stopped because {count} archive entries could not be represented safely"
            ),
            ArchiveError::ConversionSourceChanged => write!(
                f,
                "the source archive changed while it was being converted; try again"
            ),
            ArchiveError::ConversionInvalidName => write!(
                f,
                "the converted archive name must be a single non-empty filename"
            ),
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
/// Lexical only (no I/O): the same rule the UI-thread context-menu builder uses.
pub fn format_of(path: &Path) -> Result<Format, ArchiveError> {
    let name = path.to_string_lossy();
    Format::from_path(&name).ok_or(ArchiveError::UnsupportedFormat)
}

/// Work out how to open `path` as an archive, by extension first and by
/// content second.
///
/// The extension is authoritative when it names a format we support (it also
/// distinguishes `.tar.gz` from a bare `.gz`, which magic bytes cannot). When
/// it says nothing, `.docx`, `.xlsx`, `.pptx`, `.jar`, `.apk`, `.ipa`, or a
/// file with no extension at all: we sniff the header, because every one of
/// those is a zip container underneath and is perfectly browsable.
///
/// Returns `None` for anything we can't open, so the caller can say so plainly
/// rather than showing an empty archive. Blocking (reads the file header), so
/// it runs off the UI thread like the rest of this module.
pub fn probe_format(path: &Path) -> Option<Format> {
    ferail_core::path_guard::assert_off_ui_thread("archive::probe_format");
    if let Ok(format) = format_of(path) {
        // Browsers commonly disambiguate duplicate downloads by inserting a
        // suffix before the final extension (`backup.tar (1).gz`). That loses
        // the lexical `.tar.gz` signal even though the decompressed stream is
        // still a tar archive. Inspect one decoded 512-byte header on this
        // worker path and promote such single-member compressors back to their
        // tar-family format, so the workbench shows the real inner contents.
        let promoted = match format {
            Format::Gzip if tarball::compressed_payload_is_tar(path, format) => Format::TarGz,
            Format::Bzip2 if tarball::compressed_payload_is_tar(path, format) => Format::TarBz2,
            Format::Xz if tarball::compressed_payload_is_tar(path, format) => Format::TarXz,
            _ => format,
        };
        return Some(promoted);
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
/// read, for tar-family formats that means streaming the entire archive, so
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

/// Read one entry's bytes into memory, up to `cap` bytes.
///
/// This is the no-disk path: text and images are decoded and drawn by our own
/// renderers, so their contents never need to be written out. Formats whose
/// preview only Quick Look can produce (PDF, Office, video) still have to be
/// staged to a file: see `ferail_fs_native::scratch`.
///
/// `Ok(None)` means the entry is larger than `cap`; the caller decides whether
/// to stage it instead. Blocking, so off the UI thread like the rest.
pub fn read_entry_bytes(
    archive: &Path,
    entry: &str,
    password: Option<&str>,
    cap: u64,
) -> Result<Option<Vec<u8>>, ArchiveError> {
    feraille_off_ui("archive::read_entry_bytes");
    let format = probe_format(archive).ok_or(ArchiveError::UnsupportedFormat)?;
    match format {
        Format::Zip => zip_codec::read_entry_bytes(archive, entry, password, cap),
        f if f.is_tar_family() => tarball::read_entry_bytes(archive, f, entry, cap),
        // 7z and the single-member compressors go through the staging path.
        _ => Ok(None),
    }
}

fn feraille_off_ui(what: &str) {
    ferail_core::path_guard::assert_off_ui_thread(what);
}

/// Read a bounded summary of `archive` for the Description column.
///
/// Cheap by contract: never decompresses payloads. Tar-family archives return
/// a format-only summary (no counts): see the module docs on the two read
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
    /// job: this flag only governs per-file clobbering within it.
    pub overwrite: bool,
}

/// Why an archive entry was not extracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The entry path failed the zip-slip guard (traversal / absolute / drive).
    UnsafePath,
    /// A symlink entry: skipped so a malicious link can never be created and
    /// then written through by a later entry.
    Symlink,
    /// A hard-link entry (tar): skipped for the same reason.
    HardLink,
    /// A device / fifo / other special entry we do not materialize.
    SpecialFile,
    /// The target already existed and `overwrite` was false.
    ExistingNotOverwritten,
    /// The entry uses a compression method this build cannot decode (LHA has
    /// a long tail of historical methods). Skipped rather than written
    /// truncated, so a partial file is never mistaken for a good one.
    UnsupportedMethod,
    /// A path already present below the extraction destination is a symlink
    /// (or Windows reparse point). Following it could redirect a safe archive
    /// name outside the destination, so the entry is not written.
    UnsafeDestinationLink,
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
    /// Top-level paths created under the destination (deduped): what the undo
    /// step removes and what "reveal" selects.
    pub created: Vec<PathBuf>,
    /// Count of regular files actually written.
    pub files_written: u64,
    /// Entries deliberately not written, with reasons: surfaced to the user
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

/// Cooperative cancellation check: codecs call this per entry (and per buffer
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
    if !ensure_dir_beneath(dest, safe_rel)? {
        outcome.skip(safe_rel.to_string(), SkipReason::UnsafeDestinationLink);
        return Ok(());
    }
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
    let mut file = match open_file_beneath(dest, safe_rel, opts.overwrite)? {
        DestinationFile::Ready(file) => file,
        DestinationFile::Exists => {
            outcome.skip(safe_rel.to_string(), SkipReason::ExistingNotOverwritten);
            return Ok(());
        }
        DestinationFile::UnsafeLink => {
            outcome.skip(safe_rel.to_string(), SkipReason::UnsafeDestinationLink);
            return Ok(());
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

enum DestinationFile {
    Ready(std::fs::File),
    Exists,
    UnsafeLink,
}

// On Unix, every component is resolved relative to an already-open directory
// descriptor. O_NOFOLLOW protects the final component and every directory
// hop, closing both the ordinary pre-existing-symlink hole and the rename/
// replacement race between a metadata check and the subsequent open.
#[cfg(unix)]
fn ensure_dir_beneath(dest: &Path, safe_rel: &str) -> Result<bool, ArchiveError> {
    Ok(unix_walk_dirs(dest, safe_rel)?.is_some())
}

#[cfg(unix)]
fn open_file_beneath(
    dest: &Path,
    safe_rel: &str,
    overwrite: bool,
) -> Result<DestinationFile, ArchiveError> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};

    let Some((parent, leaf)) = safe_rel.rsplit_once('/') else {
        return unix_open_leaf(unix_open_root(dest)?, safe_rel, overwrite);
    };
    let Some(parent_fd) = unix_walk_dirs(dest, parent)? else {
        return Ok(DestinationFile::UnsafeLink);
    };
    let name = CString::new(leaf.as_bytes()).map_err(|_| {
        ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "archive entry contains a NUL byte",
        ))
    })?;
    let flags = libc::O_WRONLY
        | libc::O_CREAT
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | if overwrite {
            libc::O_TRUNC
        } else {
            libc::O_EXCL
        };
    // SAFETY: `parent_fd` and the NUL-terminated component remain alive for
    // the call; the mode argument is supplied because O_CREAT is set.
    let raw = unsafe { libc::openat(parent_fd.as_raw_fd(), name.as_ptr(), flags, 0o666) };
    if raw >= 0 {
        // SAFETY: openat returned a new owned descriptor.
        return Ok(DestinationFile::Ready(unsafe {
            std::fs::File::from_raw_fd(raw)
        }));
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ELOOP) {
        return Ok(DestinationFile::UnsafeLink);
    }
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        return if unix_is_link_at(parent_fd.as_raw_fd(), &name)? {
            Ok(DestinationFile::UnsafeLink)
        } else {
            Ok(DestinationFile::Exists)
        };
    }
    Err(ArchiveError::Io(error))
}

#[cfg(unix)]
fn unix_open_leaf(
    parent_fd: std::os::fd::OwnedFd,
    leaf: &str,
    overwrite: bool,
) -> Result<DestinationFile, ArchiveError> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = CString::new(leaf.as_bytes()).map_err(|_| {
        ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "archive entry contains a NUL byte",
        ))
    })?;
    let flags = libc::O_WRONLY
        | libc::O_CREAT
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | if overwrite {
            libc::O_TRUNC
        } else {
            libc::O_EXCL
        };
    // SAFETY: see `open_file_beneath`; ownership transfers only on success.
    let raw = unsafe { libc::openat(parent_fd.as_raw_fd(), name.as_ptr(), flags, 0o666) };
    if raw >= 0 {
        return Ok(DestinationFile::Ready(unsafe {
            std::fs::File::from_raw_fd(raw)
        }));
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ELOOP) {
        return Ok(DestinationFile::UnsafeLink);
    }
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        return if unix_is_link_at(parent_fd.as_raw_fd(), &name)? {
            Ok(DestinationFile::UnsafeLink)
        } else {
            Ok(DestinationFile::Exists)
        };
    }
    Err(ArchiveError::Io(error))
}

#[cfg(unix)]
fn unix_open_root(dest: &Path) -> Result<std::os::fd::OwnedFd, ArchiveError> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(dest.as_os_str().as_bytes()).map_err(|_| {
        ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "extraction destination contains a NUL byte",
        ))
    })?;
    // SAFETY: `path` is a valid NUL-terminated OS path for this call.
    let raw = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw < 0 {
        return Err(ArchiveError::Io(std::io::Error::last_os_error()));
    }
    // SAFETY: open returned a new owned descriptor.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) })
}

#[cfg(unix)]
fn unix_walk_dirs(
    dest: &Path,
    safe_rel: &str,
) -> Result<Option<std::os::fd::OwnedFd>, ArchiveError> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};

    let mut dir = unix_open_root(dest)?;
    for component in safe_rel.split('/').filter(|part| !part.is_empty()) {
        let name = CString::new(component.as_bytes()).map_err(|_| {
            ArchiveError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "archive entry contains a NUL byte",
            ))
        })?;
        // SAFETY: descriptor and name are valid for this call.
        let made = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o777) };
        if made < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(ArchiveError::Io(error));
            }
        }
        // SAFETY: descriptor and name are valid. O_DIRECTORY rejects regular
        // files; O_NOFOLLOW rejects links, including a raced replacement.
        let raw = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if raw < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ELOOP)
                || error.raw_os_error() == Some(libc::ENOTDIR)
            {
                return Ok(None);
            }
            return Err(ArchiveError::Io(error));
        }
        // SAFETY: openat returned a new owned descriptor.
        dir = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    }
    Ok(Some(dir))
}

#[cfg(unix)]
fn unix_is_link_at(dir: std::os::fd::RawFd, name: &std::ffi::CStr) -> Result<bool, ArchiveError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: pointers are valid and `stat` is initialized on success.
    let rc = unsafe {
        libc::fstatat(
            dir,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc < 0 {
        return Err(ArchiveError::Io(std::io::Error::last_os_error()));
    }
    // SAFETY: fstatat succeeded.
    let stat = unsafe { stat.assume_init() };
    Ok(stat.st_mode & libc::S_IFMT == libc::S_IFLNK)
}

// Windows does not expose openat-style relative traversal through std. Walk
// one component at a time, reject every reparse point, and open the leaf with
// FILE_FLAG_OPEN_REPARSE_POINT so a raced replacement is not followed.
#[cfg(not(unix))]
fn ensure_dir_beneath(dest: &Path, safe_rel: &str) -> Result<bool, ArchiveError> {
    let mut current = dest.to_path_buf();
    for component in safe_rel.split('/').filter(|part| !part.is_empty()) {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata_is_link(&metadata) => return Ok(false),
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(ArchiveError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "a non-directory blocks the extraction path",
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(ArchiveError::Io(error)),
                }
                let metadata = std::fs::symlink_metadata(&current)?;
                if metadata_is_link(&metadata) {
                    return Ok(false);
                }
            }
            Err(error) => return Err(ArchiveError::Io(error)),
        }
    }
    Ok(true)
}

#[cfg(not(unix))]
fn open_file_beneath(
    dest: &Path,
    safe_rel: &str,
    overwrite: bool,
) -> Result<DestinationFile, ArchiveError> {
    let target = dest.join(safe_rel);
    let parent_rel = safe_rel.rsplit_once('/').map_or("", |(parent, _)| parent);
    if !ensure_dir_beneath(dest, parent_rel)? {
        return Ok(DestinationFile::UnsafeLink);
    }
    if let Ok(metadata) = std::fs::symlink_metadata(&target) {
        if metadata_is_link(&metadata) {
            return Ok(DestinationFile::UnsafeLink);
        }
        if !overwrite {
            return Ok(DestinationFile::Exists);
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(overwrite);
    if !overwrite {
        options.create_new(true);
    }
    configure_no_follow(&mut options);
    match options.open(target) {
        Ok(file) => Ok(DestinationFile::Ready(file)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(DestinationFile::Exists)
        }
        Err(error) => Err(ArchiveError::Io(error)),
    }
}

#[cfg(windows)]
fn metadata_is_link(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(all(not(unix), not(windows)))]
fn metadata_is_link(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn configure_no_follow(options: &mut std::fs::OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(all(not(unix), not(windows)))]
fn configure_no_follow(_options: &mut std::fs::OpenOptions) {}

// ===========================================================================
// Creation
// ===========================================================================

/// Options for creating an archive.
#[derive(Debug, Default, Clone, Copy)]
pub struct CreateOptions<'a> {
    /// Compression effort (mapped per-codec).
    pub level: CompressionLevel,
    /// Password, only honored by formats whose capability matrix allows it
    /// (zip); ignored otherwise.
    pub password: Option<&'a str>,
}

/// Options for converting one archive into a newly created archive.
#[derive(Debug, Clone, Copy)]
pub struct ConvertOptions<'a> {
    pub target: Format,
    pub level: CompressionLevel,
    pub input_password: Option<&'a str>,
    pub output_password: Option<&'a str>,
}

/// Result of a successful archive conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertOutcome {
    /// Final no-clobber destination selected by the worker.
    pub output: PathBuf,
    pub files_converted: u64,
}

/// One planned input, as it will appear inside the new archive.
pub(super) struct PlannedItem {
    /// Absolute source path on disk.
    pub abs: PathBuf,
    /// `/`-joined path inside the archive (rooted at each input's own name, so
    /// compressing `project/` yields `project/...` entries: Finder's
    /// keep-parent shape).
    pub rel: String,
    pub is_dir: bool,
    /// File size in bytes (0 for directories): used to size progress.
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

static CONVERT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static DRAG_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Materialize one archive member at the exact path requested by an OS file
/// promise.
///
/// Finder supplies `target` only after the user actually drops outside
/// Ferail. The member is first extracted into a private sibling directory,
/// then renamed into place, so a failed/cancelled decode never leaves a
/// plausible but truncated destination file. Because the staging directory is
/// on the destination filesystem, the final rename is atomic and works for
/// removable/network volumes without copying through the UI process's temp
/// directory.
pub fn materialize_archive_entry(
    archive: &Path,
    entry: &str,
    target: &Path,
    password: Option<&str>,
) -> Result<(), ArchiveError> {
    ferail_core::path_guard::assert_off_ui_thread("archive::materialize_archive_entry");
    let parent = target
        .parent()
        .ok_or_else(|| ArchiveError::Codec("the promised file has no parent directory".into()))?;
    let safe = safe_relative_path(entry)
        .map_err(|_| ArchiveError::Corrupt("unsafe promised archive path".into()))?;

    let stage = create_drag_stage(parent)?;
    let _stage_guard = ConversionTemp::directory(stage.clone());
    let progress = TransferProgress::new();
    let cancel = AtomicBool::new(false);
    let outcome = extract_entries(
        archive,
        &stage,
        &[entry],
        ExtractOptions {
            password,
            overwrite: false,
        },
        &progress,
        &cancel,
    )?;
    if !outcome.skipped.is_empty() {
        return Err(ArchiveError::Codec(format!(
            "the promised archive entry could not be extracted safely ({})",
            outcome.skipped.len()
        )));
    }
    let staged = stage.join(safe);
    if !staged.exists() {
        return Err(ArchiveError::Corrupt(format!(
            "archive entry was not materialized: {entry}"
        )));
    }
    publish_promised_entry(&staged, target)?;
    Ok(())
}

/// Publish without replacing an entry that appeared after Finder chose the
/// promise URL. `renameatx_np(RENAME_EXCL)` makes the check and rename one
/// atomic kernel operation on macOS, closing the same destination-name race
/// guarded elsewhere in the extraction pipeline.
#[cfg(target_os = "macos")]
fn publish_promised_entry(staged: &Path, target: &Path) -> Result<(), ArchiveError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    const AT_FDCWD: libc::c_int = -2;
    const RENAME_EXCL: u32 = 0x0000_0004;
    unsafe extern "C" {
        fn renameatx_np(
            from_fd: libc::c_int,
            from: *const libc::c_char,
            to_fd: libc::c_int,
            to: *const libc::c_char,
            flags: u32,
        ) -> libc::c_int;
    }

    let from = CString::new(staged.as_os_str().as_bytes()).map_err(|_| {
        ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "archive drag staging path contains a NUL byte",
        ))
    })?;
    let to = CString::new(target.as_os_str().as_bytes()).map_err(|_| {
        ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "archive drag destination contains a NUL byte",
        ))
    })?;
    // SAFETY: both C strings live through the call; AT_FDCWD makes the paths
    // absolute/working-directory relative exactly as std::fs would.
    if unsafe { renameatx_np(AT_FDCWD, from.as_ptr(), AT_FDCWD, to.as_ptr(), RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(ArchiveError::Io(std::io::Error::last_os_error()))
    }
}

#[cfg(not(target_os = "macos"))]
fn publish_promised_entry(staged: &Path, target: &Path) -> Result<(), ArchiveError> {
    if target.exists() {
        return Err(ArchiveError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "the promised destination already exists",
        )));
    }
    std::fs::rename(staged, target)?;
    Ok(())
}

fn create_drag_stage(parent: &Path) -> Result<PathBuf, ArchiveError> {
    for _ in 0..100 {
        let sequence = DRAG_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".ferail-archive-drag-{}-{sequence}",
            std::process::id()
        ));
        match std::fs::create_dir(&candidate) {
            Ok(()) => {
                scratch::set_private_permissions(&candidate, 0o700);
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(ArchiveError::Io(error)),
        }
    }
    Err(ArchiveError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate archive drag staging",
    )))
}

/// Convert `source` to a fresh archive beside it without modifying the source.
///
/// The conversion deliberately composes the existing guarded extraction and
/// creation paths. A private staging directory and partial output live on the
/// destination filesystem; the partial archive is reopened and validated,
/// then atomically published with a no-clobber hard link. Every temporary path
/// is removed on cancellation or error.
pub fn convert_archive(
    source: &Path,
    output_stem: &str,
    opts: ConvertOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<ConvertOutcome, ArchiveError> {
    ferail_core::path_guard::assert_off_ui_thread("archive::convert_archive");
    if !opts.target.capabilities().can_create || opts.target.is_single_member() {
        return Err(ArchiveError::ConversionTargetUnsupported(opts.target));
    }
    if opts.output_password.is_some() && !opts.target.capabilities().supports_create_password {
        return Err(ArchiveError::ConversionEncryptionUnsupported(opts.target));
    }
    let output_stem = validate_output_stem(output_stem)?;
    let parent = source
        .parent()
        .ok_or_else(|| ArchiveError::Codec("the source archive has no parent directory".into()))?;
    let before = archive_stamp(source)?;
    let source_toc = read_toc(source, opts.input_password)?;
    check_cancel(cancel)?;

    let stage = unique_conversion_path(parent, source, "stage", None)?;
    std::fs::create_dir(&stage)?;
    scratch::set_private_permissions(&stage, 0o700);
    let _stage_guard = ConversionTemp::directory(stage.clone());

    let extracted = extract_all(
        source,
        &stage,
        ExtractOptions {
            password: opts.input_password,
            overwrite: false,
        },
        progress,
        cancel,
    )?;
    if !extracted.skipped.is_empty() {
        return Err(ArchiveError::ConversionUnsafeEntries(
            extracted.skipped.len(),
        ));
    }
    if archive_stamp(source)? != before {
        return Err(ArchiveError::ConversionSourceChanged);
    }
    check_cancel(cancel)?;

    let mut top_level = Vec::new();
    for entry in std::fs::read_dir(&stage)? {
        top_level.push(entry?.path());
    }
    top_level.sort();
    let inputs: Vec<&Path> = top_level.iter().map(PathBuf::as_path).collect();
    // Keep the writer target inside the already-created private directory.
    // A predictable partial beside the source could be replaced with a
    // symlink between name selection and File::create by another local user.
    let partial = stage.join(format!(
        ".ferail-converted.{}",
        opts.target.canonical_extension()
    ));
    let partial_guard = ConversionTemp::file(partial.clone());
    create_archive(
        opts.target,
        &inputs,
        &partial,
        CreateOptions {
            level: opts.level,
            password: opts.output_password,
        },
        progress,
        cancel,
    )?;
    check_cancel(cancel)?;

    let converted_toc = read_toc(&partial, opts.output_password)?;
    if converted_toc.file_count() as u64 != extracted.files_written {
        return Err(ArchiveError::Corrupt(format!(
            "converted archive contains {} files; expected {}",
            converted_toc.file_count(),
            extracted.files_written
        )));
    }
    // Also prove that an empty source did not unexpectedly gain members.
    if source_toc.file_count() == 0 && converted_toc.file_count() != 0 {
        return Err(ArchiveError::Corrupt(
            "converted empty archive unexpectedly contains files".into(),
        ));
    }
    // Opened for writing purely to flush it: Windows' FlushFileBuffers needs a
    // handle with write access and fails the whole conversion with
    // ERROR_ACCESS_DENIED on the read-only handle `File::open` returns, while
    // Unix would happily fsync it.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&partial)?
        .sync_all()?;
    check_cancel(cancel)?;
    let output = publish_conversion(
        &partial,
        parent,
        &output_stem,
        opts.target.canonical_extension(),
    )?;
    std::fs::remove_file(&partial)?;
    drop(partial_guard);
    Ok(ConvertOutcome {
        output,
        files_converted: extracted.files_written,
    })
}

fn validate_output_stem(stem: &str) -> Result<String, ArchiveError> {
    let stem = stem.trim();
    if stem.is_empty()
        || stem == "."
        || stem == ".."
        || stem.contains('/')
        || stem.contains('\\')
        || stem.contains('\0')
    {
        return Err(ArchiveError::ConversionInvalidName);
    }
    Ok(stem.to_string())
}

fn unique_conversion_path(
    parent: &Path,
    source: &Path,
    purpose: &str,
    extension: Option<&str>,
) -> Result<PathBuf, ArchiveError> {
    let source_leaf = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".into());
    for _ in 0..100 {
        let sequence = CONVERT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let suffix = extension.map_or_else(String::new, |ext| format!(".{ext}"));
        let candidate = parent.join(format!(
            ".{source_leaf}.ferail-convert-{purpose}-{}-{sequence}{suffix}",
            std::process::id()
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(ArchiveError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate archive conversion staging",
    )))
}

fn publish_conversion(
    partial: &Path,
    parent: &Path,
    stem: &str,
    extension: &str,
) -> Result<PathBuf, ArchiveError> {
    for n in 1..=9999 {
        let leaf = if n == 1 {
            format!("{stem}.{extension}")
        } else {
            format!("{stem} {n}.{extension}")
        };
        let candidate = parent.join(leaf);
        match std::fs::hard_link(partial, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(ArchiveError::Io(error)),
        }
    }
    Err(ArchiveError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not choose an unused converted archive name",
    )))
}

struct ConversionTemp {
    path: PathBuf,
    directory: bool,
}

impl ConversionTemp {
    fn file(path: PathBuf) -> Self {
        Self {
            path,
            directory: false,
        }
    }

    fn directory(path: PathBuf) -> Self {
        Self {
            path,
            directory: true,
        }
    }
}

impl Drop for ConversionTemp {
    fn drop(&mut self) {
        if self.directory {
            let _ = std::fs::remove_dir_all(&self.path);
        } else {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// What an [`add_to_archive`] call did.
#[derive(Debug, Default)]
pub struct AddOutcome {
    /// Number of files written into the archive.
    pub added: u64,
    /// Entry paths that were already present and therefore left alone: a
    /// duplicate name would shadow the original rather than replace it, so we
    /// skip and report instead of silently corrupting the archive's meaning.
    pub skipped_existing: Vec<String>,
}

/// One filesystem item staged for addition to an existing archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveAddition {
    /// File or directory on disk. Directory contents are planned recursively
    /// on the worker when the edit is committed.
    pub source: PathBuf,
    /// Existing directory inside the archive, or the empty string for root.
    pub destination: String,
}

/// Rename or move one archive entry. Renaming a directory implicitly moves its
/// full subtree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRename {
    pub from: String,
    pub to: String,
}

/// The complete unsaved edit journal for one archive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveEditPlan {
    pub additions: Vec<ArchiveAddition>,
    /// File or directory roots to remove; a directory removes its subtree.
    pub removals: Vec<String>,
    pub renames: Vec<ArchiveRename>,
}

impl ArchiveEditPlan {
    pub fn is_empty(&self) -> bool {
        self.additions.is_empty() && self.removals.is_empty() && self.renames.is_empty()
    }

    pub fn change_count(&self) -> usize {
        self.additions.len() + self.removals.len() + self.renames.len()
    }
}

/// Cheap identity captured when the workbench loaded an archive. Save compares
/// it immediately before rewriting so an external modification is never
/// silently overwritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
}

impl ArchiveStamp {
    pub fn byte_len(self) -> u64 {
        self.len
    }
}

pub fn archive_stamp(path: &Path) -> Result<ArchiveStamp, ArchiveError> {
    ferail_core::path_guard::assert_off_ui_thread("archive::archive_stamp");
    let metadata = std::fs::metadata(path)?;
    Ok(ArchiveStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

/// Expand staged filesystem additions into virtual archive entries for the
/// workbench's unsaved projection. This performs the recursive walk on a
/// worker; render and drop-hover never stat or enumerate the sources.
pub fn inspect_archive_additions(
    additions: &[ArchiveAddition],
) -> Result<Vec<ferail_archive::ArchiveEntry>, ArchiveError> {
    ferail_core::path_guard::assert_off_ui_thread("archive::inspect_archive_additions");
    let mut entries = Vec::new();
    for addition in additions {
        let destination = if addition.destination.trim_matches('/').is_empty() {
            String::new()
        } else {
            safe_relative_path(&addition.destination).map_err(|_| {
                ArchiveError::Codec(format!(
                    "unsafe archive destination: {}",
                    addition.destination
                ))
            })?
        };
        let (items, _) = plan_inputs(&[addition.source.as_path()])?;
        for item in items {
            let path = if destination.is_empty() {
                item.rel
            } else {
                format!("{destination}/{}", item.rel.trim_start_matches('/'))
            };
            entries.push(ferail_archive::ArchiveEntry {
                path,
                is_dir: item.is_dir,
                uncompressed_size: (!item.is_dir).then_some(item.size),
                compressed_size: None,
                mtime_unix: None,
                compression_method: None,
                checksum: None,
                unix_mode: None,
                comment: None,
                encrypted: false,
            });
        }
    }
    Ok(entries)
}

/// Commit a staged edit journal transactionally.
///
/// The current implementation intentionally accepts plain `.zip` files only.
/// ZIP-based application/document packages are browseable through content
/// probing, but changing them can invalidate signatures or package structure.
pub fn commit_archive_edits(
    archive: &Path,
    expected: ArchiveStamp,
    plan: &ArchiveEditPlan,
    opts: CreateOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<(), ArchiveError> {
    ferail_core::path_guard::assert_off_ui_thread("archive::commit_archive_edits");
    if plan.is_empty() {
        return Ok(());
    }
    if Format::from_path(&archive.to_string_lossy()) != Some(Format::Zip) {
        return Err(ArchiveError::Codec(
            "this archive type is browse-only and cannot be edited safely".to_string(),
        ));
    }
    if archive_stamp(archive)? != expected {
        return Err(ArchiveError::Codec(
            "the archive changed on disk after it was opened; reload it before saving".to_string(),
        ));
    }
    zip_codec::rewrite(archive, expected, plan, opts, progress, cancel)
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

/// One filesystem name as a single zip/tar path component.
///
/// A local name is one component by construction (`file_name` never returns a
/// separator on its own platform), but on Unix it may legitimately *contain* a
/// backslash, and translating that to `/`, as this used to, invents structure:
/// the real file `..\payload` would become the archive entry `../payload`.
/// Ferail's extraction guard would reject that, but other tools' might not, so
/// the traversal must never be written in the first place. Backslashes are
/// therefore left alone, and anything that still fails the shared safety rule
/// (a component that is `.`/`..`, empty, or carries a separator or NUL) is
/// dropped rather than archived under a dangerous name.
fn archive_component(name: &str) -> String {
    if name.contains('/') || ferail_archive::safe_relative_path(name).is_err() {
        return String::new();
    }
    name.to_string()
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
            .map(|n| archive_component(&n.to_string_lossy()))
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
            continue; // never follow: avoids cycles and link-escape surprises
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
                let name = archive_component(&child.file_name().to_string_lossy());
                if name.is_empty() {
                    continue;
                }
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
        // Other special files (fifo/device) are not archivable: skip.
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
