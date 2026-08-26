//! Streaming checksum-manifest parsing and verification.
//!
//! Manifest paths are untrusted. On Unix, every component is opened relative
//! to an already-open directory with `O_NOFOLLOW`; other platforms reject
//! reparse/symlink components and re-check canonical containment.

use std::fs::{File, Metadata};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crc32fast::Hasher as Crc32;
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest as _, Sha224, Sha256, Sha384, Sha512};

use ferail_core::text_encoding::{decode_cp437, decode_text, TextEncoding};

const READ_BUFFER_SIZE: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigestAlgorithm {
    Crc32,
    Md5,
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
}

impl DigestAlgorithm {
    pub fn label(self) -> &'static str {
        match self {
            Self::Crc32 => "CRC32",
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA-1",
            Self::Sha224 => "SHA-224",
            Self::Sha256 => "SHA-256",
            Self::Sha384 => "SHA-384",
            Self::Sha512 => "SHA-512",
        }
    }

    pub fn is_legacy(self) -> bool {
        matches!(self, Self::Crc32 | Self::Md5 | Self::Sha1)
    }

    fn from_hex_len(len: usize) -> Option<Self> {
        match len {
            8 => Some(Self::Crc32),
            32 => Some(Self::Md5),
            40 => Some(Self::Sha1),
            56 => Some(Self::Sha224),
            64 => Some(Self::Sha256),
            96 => Some(Self::Sha384),
            128 => Some(Self::Sha512),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestFormat {
    Sfv,
    Gnu,
    Bsd,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestEntry {
    pub name: String,
    pub expected: String,
    pub algorithm: DigestAlgorithm,
    pub line: u64,
}

#[derive(Clone, Debug)]
pub struct Manifest {
    pub source: PathBuf,
    pub format: ManifestFormat,
    pub entries: Vec<ManifestEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestError {
    NotText,
    Empty,
    Malformed { line: u64, reason: &'static str },
    MixedAlgorithms,
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotText => f.write_str("manifest is not decodable text"),
            Self::Empty => f.write_str("manifest has no checksum entries"),
            Self::Malformed { line, reason } => write!(f, "line {line}: {reason}"),
            Self::MixedAlgorithms => f.write_str("manifest mixes checksum algorithms"),
        }
    }
}

impl std::error::Error for ManifestError {}

pub fn parse_manifest(source: PathBuf, bytes: &[u8]) -> Result<Manifest, ManifestError> {
    let mut decoded = decode_text(bytes).ok_or(ManifestError::NotText)?;
    let format = {
        let first_data = decoded
            .text
            .lines()
            .enumerate()
            .map(|(index, line)| (index as u64 + 1, line.trim_end_matches('\r')))
            .find(|(_, line)| !line.trim().is_empty() && !line.trim_start().starts_with([';', '#']))
            .ok_or(ManifestError::Empty)?;
        if parse_sfv_entry(first_data.0, first_data.1).is_ok() {
            ManifestFormat::Sfv
        } else if first_data.1.contains(") = ") {
            ManifestFormat::Bsd
        } else {
            ManifestFormat::Gnu
        }
    };

    // OEM code pages are common in historical SFV files. Generic legacy text
    // deliberately prefers Latin-1 to avoid corrupting prose, but SFV's
    // ecosystem convention gives CP437 the stronger claim here.
    if format == ManifestFormat::Sfv
        && decoded.encoding == TextEncoding::Latin1
        && std::str::from_utf8(bytes).is_err()
    {
        decoded.text = decode_cp437(bytes);
        decoded.encoding = TextEncoding::Cp437;
    }
    let meaningful: Vec<(u64, &str)> = decoded
        .text
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line = line.trim_end_matches('\r');
            (!line.trim().is_empty()).then_some((index as u64 + 1, line))
        })
        .collect();

    let mut entries = Vec::new();
    for (line_number, line) in meaningful {
        let trimmed = line.trim_start();
        if (format == ManifestFormat::Sfv && trimmed.starts_with(';'))
            || (format != ManifestFormat::Sfv && trimmed.starts_with('#'))
        {
            continue;
        }
        let entry = match format {
            ManifestFormat::Sfv => parse_sfv_entry(line_number, line),
            ManifestFormat::Gnu => parse_gnu_entry(line_number, line),
            ManifestFormat::Bsd => parse_bsd_entry(line_number, line),
        }?;
        entries.push(entry);
    }
    if entries.is_empty() {
        return Err(ManifestError::Empty);
    }
    if entries
        .iter()
        .any(|entry| entry.algorithm != entries[0].algorithm)
    {
        return Err(ManifestError::MixedAlgorithms);
    }
    Ok(Manifest {
        source,
        format,
        entries,
    })
}

fn parse_sfv_entry(line: u64, text: &str) -> Result<ManifestEntry, ManifestError> {
    let text = text.trim_end();
    let split = text
        .rfind(char::is_whitespace)
        .ok_or(ManifestError::Malformed {
            line,
            reason: "expected filename and CRC32",
        })?;
    let name = text[..split].trim_end();
    let expected = text[split..].trim();
    if name.is_empty() || expected.len() != 8 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ManifestError::Malformed {
            line,
            reason: "expected filename followed by eight hexadecimal CRC32 digits",
        });
    }
    Ok(ManifestEntry {
        name: name.to_owned(),
        expected: expected.to_ascii_lowercase(),
        algorithm: DigestAlgorithm::Crc32,
        line,
    })
}

fn parse_gnu_entry(line: u64, text: &str) -> Result<ManifestEntry, ManifestError> {
    let (escaped, text) = match text.strip_prefix('\\') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let digest_end = text.find(' ').ok_or(ManifestError::Malformed {
        line,
        reason: "expected digest, mode marker and filename",
    })?;
    let expected = &text[..digest_end];
    let algorithm =
        DigestAlgorithm::from_hex_len(expected.len()).ok_or(ManifestError::Malformed {
            line,
            reason: "unsupported digest length",
        })?;
    if !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ManifestError::Malformed {
            line,
            reason: "digest contains a non-hexadecimal character",
        });
    }
    let rest = &text[digest_end + 1..];
    let Some((&mode, raw_name)) = rest.as_bytes().split_first() else {
        return Err(ManifestError::Malformed {
            line,
            reason: "missing mode marker and filename",
        });
    };
    if !matches!(mode, b' ' | b'*') || raw_name.is_empty() {
        return Err(ManifestError::Malformed {
            line,
            reason: "mode marker must be a space or an asterisk",
        });
    }
    let raw_name = std::str::from_utf8(raw_name).map_err(|_| ManifestError::Malformed {
        line,
        reason: "filename is not valid decoded text",
    })?;
    let name = if escaped {
        unescape_gnu_name(raw_name, line)?
    } else {
        raw_name.to_owned()
    };
    Ok(ManifestEntry {
        name,
        expected: expected.to_ascii_lowercase(),
        algorithm,
        line,
    })
}

fn unescape_gnu_name(raw: &str, line: u64) -> Result<String, ManifestError> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            _ => {
                return Err(ManifestError::Malformed {
                    line,
                    reason: "invalid GNU filename escape",
                });
            }
        }
    }
    Ok(out)
}

fn parse_bsd_entry(line: u64, text: &str) -> Result<ManifestEntry, ManifestError> {
    let open = text.find(" (").ok_or(ManifestError::Malformed {
        line,
        reason: "expected ALGORITHM (filename) = digest",
    })?;
    let close = text.rfind(") = ").ok_or(ManifestError::Malformed {
        line,
        reason: "expected ALGORITHM (filename) = digest",
    })?;
    if close <= open + 2 {
        return Err(ManifestError::Malformed {
            line,
            reason: "filename is empty",
        });
    }
    let expected = text[close + 4..].trim();
    let algorithm =
        DigestAlgorithm::from_hex_len(expected.len()).ok_or(ManifestError::Malformed {
            line,
            reason: "unsupported digest length",
        })?;
    let declared = text[..open].trim().replace('-', "").to_ascii_uppercase();
    if declared != algorithm.label().replace('-', "").to_ascii_uppercase()
        || !expected.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ManifestError::Malformed {
            line,
            reason: "algorithm and digest do not agree",
        });
    }
    Ok(ManifestEntry {
        name: text[open + 2..close].to_owned(),
        expected: expected.to_ascii_lowercase(),
        algorithm,
        line,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntryOutcome {
    Ok,
    Mismatch { actual: String },
    Missing,
    Unreadable { reason: String },
    UnsafePath { reason: &'static str },
    UnavailablePlaceholder,
    ChangedWhileReading,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyOutcome {
    pub entry: ManifestEntry,
    pub outcome: EntryOutcome,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VerifyProgress {
    pub entry_index: u64,
    pub entry_count: u64,
    pub file_bytes_done: u64,
    pub file_bytes_total: u64,
}

#[derive(Clone, Debug, Default)]
pub struct VerifyReport {
    pub outcomes: Vec<VerifyOutcome>,
    pub cancelled: bool,
    pub format: Option<ManifestFormat>,
}

pub fn verify_manifest(
    manifest: &Manifest,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(VerifyProgress),
) -> VerifyReport {
    if cancel.load(Ordering::Relaxed) {
        return VerifyReport {
            outcomes: Vec::new(),
            cancelled: true,
            format: Some(manifest.format),
        };
    }
    let root = manifest.source.parent().unwrap_or_else(|| Path::new("."));
    let canonical_root = match std::fs::canonicalize(root) {
        Ok(path) => path,
        Err(error) => {
            return VerifyReport {
                outcomes: manifest
                    .entries
                    .iter()
                    .cloned()
                    .map(|entry| VerifyOutcome {
                        entry,
                        outcome: EntryOutcome::Unreadable {
                            reason: error.to_string(),
                        },
                    })
                    .collect(),
                cancelled: false,
                format: Some(manifest.format),
            };
        }
    };
    let count = manifest.entries.len() as u64;
    let mut report = VerifyReport {
        format: Some(manifest.format),
        ..VerifyReport::default()
    };
    for (index, entry) in manifest.entries.iter().cloned().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let relative = match safe_relative_path(&entry.name, manifest.format) {
            Ok(path) => path,
            Err(reason) => {
                report.outcomes.push(VerifyOutcome {
                    entry,
                    outcome: EntryOutcome::UnsafePath { reason },
                });
                continue;
            }
        };
        let target = canonical_root.join(&relative);
        if crate::is_cloud_placeholder(&target) {
            report.outcomes.push(VerifyOutcome {
                entry,
                outcome: EntryOutcome::UnavailablePlaceholder,
            });
            continue;
        }
        let (mut file, before) = match open_beneath(&canonical_root, &relative) {
            Ok(value) => value,
            Err(OpenError::Missing) => {
                report.outcomes.push(VerifyOutcome {
                    entry,
                    outcome: EntryOutcome::Missing,
                });
                continue;
            }
            Err(OpenError::Unsafe(reason)) => {
                report.outcomes.push(VerifyOutcome {
                    entry,
                    outcome: EntryOutcome::UnsafePath { reason },
                });
                continue;
            }
            Err(OpenError::Io(error)) => {
                report.outcomes.push(VerifyOutcome {
                    entry,
                    outcome: EntryOutcome::Unreadable {
                        reason: error.to_string(),
                    },
                });
                continue;
            }
        };
        let total = before.len();
        let actual = hash_stream(&mut file, entry.algorithm, cancel, |done| {
            on_progress(VerifyProgress {
                entry_index: index as u64,
                entry_count: count,
                file_bytes_done: done,
                file_bytes_total: total,
            });
        });
        let outcome = match actual {
            Ok(None) => {
                report.cancelled = true;
                EntryOutcome::Cancelled
            }
            Err(error) => EntryOutcome::Unreadable {
                reason: error.to_string(),
            },
            Ok(Some(actual)) => {
                if !path_still_names_open_file(&canonical_root, &relative, &file, &before) {
                    EntryOutcome::ChangedWhileReading
                } else if actual.eq_ignore_ascii_case(&entry.expected) {
                    EntryOutcome::Ok
                } else {
                    EntryOutcome::Mismatch { actual }
                }
            }
        };
        report.outcomes.push(VerifyOutcome { entry, outcome });
        if report.cancelled {
            break;
        }
    }
    report
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenerateFormat {
    Sfv,
    Sha256Sums,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GenerateReport {
    pub entries_written: u64,
    pub cancelled: bool,
}

/// Generate a manifest from root-relative regular files. The destination is
/// published with a same-directory hard link, which is atomic and refuses to
/// replace an existing file. A cancellation/failure only removes the private
/// temporary inode.
pub fn generate_manifest(
    root: &Path,
    output: &Path,
    relative_files: &[PathBuf],
    format: GenerateFormat,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(VerifyProgress),
) -> io::Result<GenerateReport> {
    if output.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "checksum manifest already exists",
        ));
    }
    let canonical_root = std::fs::canonicalize(root)?;
    let output_parent = output.parent().unwrap_or(root);
    std::fs::create_dir_all(output_parent)?;
    let temp = output_parent.join(format!(
        ".ferail-checksum-{}-{}.tmp",
        std::process::id(),
        next_temp_id()
    ));
    let mut cleanup = TempCleanup(Some(temp.clone()));
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let mut written = 0u64;

    for (index, relative) in relative_files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(GenerateReport {
                entries_written: written,
                cancelled: true,
            });
        }
        let name = relative.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "filename is not Unicode")
        })?;
        let manifest_format = if format == GenerateFormat::Sfv {
            ManifestFormat::Sfv
        } else {
            ManifestFormat::Gnu
        };
        let safe = safe_relative_path(name, manifest_format)
            .map_err(|reason| io::Error::new(io::ErrorKind::InvalidInput, reason))?;
        if format == GenerateFormat::Sfv && name.contains(['\r', '\n']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SFV cannot represent a filename containing CR or LF",
            ));
        }
        let (mut file, metadata) = open_beneath(&canonical_root, &safe).map_err(open_error_io)?;
        let algorithm = match format {
            GenerateFormat::Sfv => DigestAlgorithm::Crc32,
            GenerateFormat::Sha256Sums => DigestAlgorithm::Sha256,
        };
        let Some(actual) = hash_stream(&mut file, algorithm, cancel, |done| {
            on_progress(VerifyProgress {
                entry_index: index as u64,
                entry_count: relative_files.len() as u64,
                file_bytes_done: done,
                file_bytes_total: metadata.len(),
            });
        })?
        else {
            return Ok(GenerateReport {
                entries_written: written,
                cancelled: true,
            });
        };
        match format {
            GenerateFormat::Sfv => {
                writeln!(
                    writer,
                    "{} {}\r",
                    name.replace('/', "\\"),
                    actual.to_ascii_uppercase()
                )?;
            }
            GenerateFormat::Sha256Sums => {
                let (marker, encoded) = escape_gnu_name(name);
                writeln!(writer, "{marker}{actual} *{encoded}")?;
            }
        }
        written += 1;
    }
    writer.sync_all()?;
    drop(writer);
    // Atomic no-clobber publication. Both names are in one directory, so the
    // hard link cannot cross devices. Leaving the temp cleanup armed ensures
    // every failure path removes only Ferail's private file.
    std::fs::hard_link(&temp, output)?;
    std::fs::remove_file(&temp)?;
    cleanup.0 = None;
    Ok(GenerateReport {
        entries_written: written,
        cancelled: false,
    })
}

fn escape_gnu_name(name: &str) -> (&'static str, String) {
    if !name.contains(['\\', '\r', '\n']) {
        return ("", name.to_owned());
    }
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    ("\\", out)
}

fn open_error_io(error: OpenError) -> io::Error {
    match error {
        OpenError::Missing => io::Error::new(io::ErrorKind::NotFound, "input file is missing"),
        OpenError::Unsafe(reason) => io::Error::new(io::ErrorKind::PermissionDenied, reason),
        OpenError::Io(error) => error,
    }
}

fn next_temp_id() -> u64 {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

struct TempCleanup(Option<PathBuf>);

impl Drop for TempCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub fn safe_relative_path(name: &str, format: ManifestFormat) -> Result<PathBuf, &'static str> {
    if name.is_empty() || name.contains(['\0', '\r', '\n']) {
        return Err("empty or control-character filename");
    }
    let normalized = if format == ManifestFormat::Sfv || cfg!(windows) {
        name.replace('\\', "/")
    } else {
        name.to_owned()
    };
    if normalized.starts_with('/')
        || normalized.starts_with("//")
        || normalized.as_bytes().get(1) == Some(&b':')
    {
        return Err("absolute, UNC or drive path");
    }
    let mut result = PathBuf::new();
    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir => return Err("parent traversal"),
            Component::RootDir | Component::Prefix(_) => return Err("absolute path"),
        }
    }
    if result.as_os_str().is_empty() {
        Err("empty filename")
    } else {
        Ok(result)
    }
}

enum DigestState {
    Crc32(Crc32),
    Md5(Md5),
    Sha1(Sha1),
    Sha224(Sha224),
    Sha256(Sha256),
    Sha384(Sha384),
    Sha512(Sha512),
}

impl DigestState {
    fn new(algorithm: DigestAlgorithm) -> Self {
        match algorithm {
            DigestAlgorithm::Crc32 => Self::Crc32(Crc32::new()),
            DigestAlgorithm::Md5 => Self::Md5(Md5::new()),
            DigestAlgorithm::Sha1 => Self::Sha1(Sha1::new()),
            DigestAlgorithm::Sha224 => Self::Sha224(Sha224::new()),
            DigestAlgorithm::Sha256 => Self::Sha256(Sha256::new()),
            DigestAlgorithm::Sha384 => Self::Sha384(Sha384::new()),
            DigestAlgorithm::Sha512 => Self::Sha512(Sha512::new()),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Crc32(state) => state.update(bytes),
            Self::Md5(state) => state.update(bytes),
            Self::Sha1(state) => state.update(bytes),
            Self::Sha224(state) => state.update(bytes),
            Self::Sha256(state) => state.update(bytes),
            Self::Sha384(state) => state.update(bytes),
            Self::Sha512(state) => state.update(bytes),
        }
    }

    fn finish(self) -> String {
        match self {
            Self::Crc32(state) => format!("{:08x}", state.finalize()),
            Self::Md5(state) => format!("{:x}", state.finalize()),
            Self::Sha1(state) => format!("{:x}", state.finalize()),
            Self::Sha224(state) => format!("{:x}", state.finalize()),
            Self::Sha256(state) => format!("{:x}", state.finalize()),
            Self::Sha384(state) => format!("{:x}", state.finalize()),
            Self::Sha512(state) => format!("{:x}", state.finalize()),
        }
    }
}

fn hash_stream(
    reader: &mut File,
    algorithm: DigestAlgorithm,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64),
) -> io::Result<Option<String>> {
    let mut state = DigestState::new(algorithm);
    let mut buffer = vec![0u8; READ_BUFFER_SIZE];
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        state.update(&buffer[..read]);
        done = done.saturating_add(read as u64);
        on_progress(done);
    }
    Ok(Some(state.finish()))
}

enum OpenError {
    Missing,
    Unsafe(&'static str),
    Io(io::Error),
}

#[cfg(unix)]
fn open_beneath(root: &Path, relative: &Path) -> Result<(File, Metadata), OpenError> {
    use std::ffi::CString;
    use std::os::fd::{FromRawFd, RawFd};
    use std::os::unix::ffi::OsStrExt;

    fn open_at(directory: RawFd, name: &std::ffi::CStr, flags: i32) -> Result<RawFd, OpenError> {
        // SAFETY: directory is an owned live fd and name is NUL-terminated.
        let fd = unsafe { libc::openat(directory, name.as_ptr(), flags) };
        if fd >= 0 {
            return Ok(fd);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ENOENT) => Err(OpenError::Missing),
            Some(libc::ELOOP) | Some(libc::ENOTDIR) => {
                Err(OpenError::Unsafe("symlink or non-directory component"))
            }
            _ => Err(OpenError::Io(error)),
        }
    }

    let root_name = CString::new(root.as_os_str().as_bytes())
        .map_err(|_| OpenError::Unsafe("NUL in root path"))?;
    // SAFETY: root_name is a valid C string.
    let mut directory = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if directory < 0 {
        return Err(OpenError::Io(io::Error::last_os_error()));
    }
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            // SAFETY: directory is owned by this function.
            unsafe { libc::close(directory) };
            return Err(OpenError::Unsafe("non-normal path component"));
        };
        let name = match CString::new(name.as_bytes()) {
            Ok(name) => name,
            Err(_) => {
                // SAFETY: directory is owned by this function.
                unsafe { libc::close(directory) };
                return Err(OpenError::Unsafe("NUL in path component"));
            }
        };
        let last = index + 1 == components.len();
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if last { 0 } else { libc::O_DIRECTORY };
        let next = match open_at(directory, &name, flags) {
            Ok(fd) => fd,
            Err(error) => {
                // SAFETY: directory is owned by this function.
                unsafe { libc::close(directory) };
                return Err(error);
            }
        };
        // SAFETY: directory is owned and no longer used.
        unsafe { libc::close(directory) };
        directory = next;
    }
    // SAFETY: the final descriptor is uniquely owned here.
    let file = unsafe { File::from_raw_fd(directory) };
    let metadata = file.metadata().map_err(OpenError::Io)?;
    if !metadata.is_file() {
        return Err(OpenError::Unsafe("manifest entry is not a regular file"));
    }
    Ok((file, metadata))
}

#[cfg(not(unix))]
fn open_beneath(root: &Path, relative: &Path) -> Result<(File, Metadata), OpenError> {
    let mut target = root.to_path_buf();
    for component in relative.components() {
        target.push(component.as_os_str());
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) if metadata_is_link(&metadata) => {
                return Err(OpenError::Unsafe("symlink or reparse component"));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(OpenError::Missing);
            }
            Err(error) => return Err(OpenError::Io(error)),
            _ => {}
        }
    }
    let canonical = std::fs::canonicalize(&target).map_err(OpenError::Io)?;
    if !canonical.starts_with(root) {
        return Err(OpenError::Unsafe("resolved path escapes manifest root"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    configure_no_follow(&mut options);
    let file = options.open(&canonical).map_err(OpenError::Io)?;
    #[cfg(windows)]
    ensure_windows_handle_beneath(&file, root)?;
    let metadata = file.metadata().map_err(OpenError::Io)?;
    if metadata_is_link(&metadata) {
        return Err(OpenError::Unsafe("symlink or reparse-point entry"));
    }
    if !metadata.is_file() {
        return Err(OpenError::Unsafe("manifest entry is not a regular file"));
    }
    Ok((file, metadata))
}

#[cfg(windows)]
fn metadata_is_link(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(all(not(unix), not(windows)))]
fn metadata_is_link(metadata: &Metadata) -> bool {
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

#[cfg(windows)]
fn ensure_windows_handle_beneath(file: &File, root: &Path) -> Result<(), OpenError> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED, VOLUME_NAME_DOS,
    };

    let handle = HANDLE(file.as_raw_handle() as isize);
    let mut wide = vec![0u16; 512];
    loop {
        let length = unsafe {
            GetFinalPathNameByHandleW(handle, &mut wide, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS)
        } as usize;
        if length == 0 {
            return Err(OpenError::Io(io::Error::last_os_error()));
        }
        if length < wide.len() {
            let resolved = PathBuf::from(OsString::from_wide(&wide[..length]));
            // A case mismatch is rejected rather than normalized loosely: a
            // Windows directory may opt into case sensitivity. False
            // negatives are safe; accepting a sibling is not.
            return if resolved.starts_with(root) {
                Ok(())
            } else {
                Err(OpenError::Unsafe("opened handle escapes manifest root"))
            };
        }
        wide.resize(length.saturating_add(1), 0);
    }
}

fn same_file_state(before: &Metadata, after: &Metadata) -> bool {
    before.len() == after.len() && before.modified().ok() == after.modified().ok()
}

#[cfg(unix)]
fn path_still_names_open_file(
    root: &Path,
    relative: &Path,
    file: &File,
    before: &Metadata,
) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(after_handle) = file.metadata() else {
        return false;
    };
    let Ok((_, after_path)) = open_beneath(root, relative) else {
        return false;
    };
    same_file_state(before, &after_handle)
        && before.dev() == after_path.dev()
        && before.ino() == after_path.ino()
}

#[cfg(windows)]
fn path_still_names_open_file(
    root: &Path,
    relative: &Path,
    file: &File,
    before: &Metadata,
) -> bool {
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    fn identity(file: &File) -> Option<(u32, u64)> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe {
            GetFileInformationByHandle(HANDLE(file.as_raw_handle() as isize), &mut info).ok()?;
        }
        Some((
            info.dwVolumeSerialNumber,
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        ))
    }

    let Ok(after_handle) = file.metadata() else {
        return false;
    };
    let Ok((reopened, _)) = open_beneath(root, relative) else {
        return false;
    };
    same_file_state(before, &after_handle) && identity(file) == identity(&reopened)
}

#[cfg(all(not(unix), not(windows)))]
fn path_still_names_open_file(
    root: &Path,
    relative: &Path,
    file: &File,
    before: &Metadata,
) -> bool {
    let Ok(after_handle) = file.metadata() else {
        return false;
    };
    let Ok((_, after_path)) = open_beneath(root, relative) else {
        return false;
    };
    same_file_state(before, &after_handle) && same_file_state(before, &after_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(path: &str) -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../test-data/sidecars/generated")
                .join(path),
        )
        .unwrap()
    }

    fn scratch() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ferail-sidecar-verify-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn parses_sfv_gnu_bsd_and_rejects_mixed() {
        let sfv = parse_manifest(
            PathBuf::from("release.sfv"),
            &fixture("manifests/release.sfv"),
        )
        .unwrap();
        assert_eq!(sfv.format, ManifestFormat::Sfv);
        assert_eq!(sfv.entries.len(), 4);
        assert_eq!(sfv.entries[1].name, "file with spaces.txt");
        assert_eq!(sfv.entries[2].name, "unicodé.txt");

        let gnu = parse_manifest(
            PathBuf::from("SHA256SUMS"),
            &fixture("manifests/SHA256SUMS"),
        )
        .unwrap();
        assert_eq!(gnu.format, ManifestFormat::Gnu);
        assert!(gnu.entries.iter().any(|entry| entry.name.contains('\n')));
        assert!(gnu.entries.iter().any(|entry| entry.name.contains('\\')));

        let bsd = parse_manifest(PathBuf::from("BSD"), &fixture("manifests/BSD-SHA256")).unwrap();
        assert_eq!(bsd.format, ManifestFormat::Bsd);
        assert_eq!(bsd.entries[0].algorithm, DigestAlgorithm::Sha256);

        assert_eq!(
            parse_manifest(PathBuf::from("mixed"), &fixture("manifests/MIXEDSUMS")).unwrap_err(),
            ManifestError::MixedAlgorithms
        );
    }

    #[test]
    fn rejects_lexical_escapes_but_accepts_sfv_subdirectories() {
        assert_eq!(
            safe_relative_path("subdir\\file.bin", ManifestFormat::Sfv).unwrap(),
            PathBuf::from("subdir/file.bin")
        );
        for name in [
            "../secret",
            "..\\secret",
            "/etc/passwd",
            "C:\\Windows\\win.ini",
            "//server/share",
        ] {
            assert!(
                safe_relative_path(name, ManifestFormat::Sfv).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn verifies_crc32_and_sha256_and_reports_mismatch_missing() {
        let root = scratch();
        std::fs::write(root.join("ok.bin"), b"123456789").unwrap();
        let sfv_bytes = b"ok.bin CBF43926\nmissing.bin 00000000\nok.bin 00000000\n";
        let manifest_path = root.join("fixture.sfv");
        std::fs::write(&manifest_path, sfv_bytes).unwrap();
        let manifest = parse_manifest(manifest_path, sfv_bytes).unwrap();
        let report = verify_manifest(&manifest, &AtomicBool::new(false), |_| {});
        assert!(matches!(report.outcomes[0].outcome, EntryOutcome::Ok));
        assert!(matches!(report.outcomes[1].outcome, EntryOutcome::Missing));
        assert!(matches!(
            report.outcomes[2].outcome,
            EntryOutcome::Mismatch { .. }
        ));

        let expected = "15e2b0d3c33891ebb0f1ef609ec419420c20e320ce94c65fbc8c3312448eb225";
        let sha = format!("{expected} *ok.bin\n");
        let parsed = parse_manifest(root.join("SHA256SUMS"), sha.as_bytes()).unwrap();
        let report = verify_manifest(&parsed, &AtomicBool::new(false), |_| {});
        assert!(matches!(report.outcomes[0].outcome, EntryOutcome::Ok));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn never_follows_a_symlink_entry() {
        use std::os::unix::fs::symlink;
        let root = scratch();
        let outside = root.parent().unwrap().join("ferail-sidecar-outside-secret");
        std::fs::write(&outside, b"secret").unwrap();
        symlink(&outside, root.join("link.bin")).unwrap();
        let bytes = b"link.bin 5CA2E8E5\n";
        let source = root.join("unsafe.sfv");
        std::fs::write(&source, bytes).unwrap();
        let manifest = parse_manifest(source, bytes).unwrap();
        let report = verify_manifest(&manifest, &AtomicBool::new(false), |_| {});
        assert!(matches!(
            report.outcomes[0].outcome,
            EntryOutcome::UnsafePath { .. }
        ));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_file(outside);
    }

    #[test]
    fn pre_cancelled_verification_stops_before_opening() {
        let manifest = Manifest {
            source: PathBuf::from("does-not-matter.sfv"),
            format: ManifestFormat::Sfv,
            entries: vec![ManifestEntry {
                name: "file".into(),
                expected: "00000000".into(),
                algorithm: DigestAlgorithm::Crc32,
                line: 1,
            }],
        };
        let report = verify_manifest(&manifest, &AtomicBool::new(true), |_| {});
        assert!(report.cancelled);
        assert!(report.outcomes.is_empty());
    }

    #[test]
    fn hostile_fixture_never_resolves_outside_its_manifest_folder() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-data/sidecars/generated/security-root/child/unsafe.sfv");
        let manifest = parse_manifest(source, &fixture("security-root/child/unsafe.sfv")).unwrap();
        let report = verify_manifest(&manifest, &AtomicBool::new(false), |_| {});
        assert!(matches!(report.outcomes[0].outcome, EntryOutcome::Ok));
        assert!(report.outcomes[1..]
            .iter()
            .all(|item| matches!(item.outcome, EntryOutcome::UnsafePath { .. })));
    }

    #[test]
    fn generation_round_trips_and_never_clobbers() {
        let root = scratch();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("alpha.bin"), b"123456789").unwrap();
        std::fs::write(root.join("sub/file with spaces.txt"), b"hello\n").unwrap();
        let files = vec![
            PathBuf::from("alpha.bin"),
            PathBuf::from("sub/file with spaces.txt"),
        ];

        for (format, output_name) in [
            (GenerateFormat::Sfv, "release.sfv"),
            (GenerateFormat::Sha256Sums, "SHA256SUMS"),
        ] {
            let output = root.join(output_name);
            let generated = generate_manifest(
                &root,
                &output,
                &files,
                format,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
            assert_eq!(generated.entries_written, 2);
            let parsed = parse_manifest(output.clone(), &std::fs::read(&output).unwrap()).unwrap();
            let verified = verify_manifest(&parsed, &AtomicBool::new(false), |_| {});
            assert!(verified
                .outcomes
                .iter()
                .all(|item| matches!(item.outcome, EntryOutcome::Ok)));
            let error = generate_manifest(
                &root,
                &output,
                &files,
                format,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
