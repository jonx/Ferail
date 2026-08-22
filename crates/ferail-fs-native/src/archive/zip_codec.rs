//! ZIP read path. The zip central directory is a real manifest we can read
//! without inflating any payload, so both the full TOC and the bounded summary
//! come from one cheap pass — `by_index_raw` exposes every entry's metadata
//! (name, sizes, encrypted flag) and never needs the password, which is only
//! required to read entry *data* at extraction time.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ferail_archive::{ArchiveEntry, Toc};
use zip::ZipArchive;
use zip::result::ZipError;

use super::{
    ArchiveEditPlan, ArchiveError, ArchiveSummary, CreateOptions, ExtractOptions, ExtractOutcome,
    PlannedItem, Selection, SkipReason,
};
use crate::file_ops::TransferProgress;

fn open(archive: &Path) -> Result<ZipArchive<BufReader<File>>, ArchiveError> {
    let file = File::open(archive)?;
    ZipArchive::new(BufReader::new(file)).map_err(map_zip_err)
}

pub(super) fn read_toc(archive: &Path, _password: Option<&str>) -> Result<Toc, ArchiveError> {
    let mut zipf = open(archive)?;
    let count = zipf.len();
    let mut entries = Vec::with_capacity(count);
    let mut needs_password = false;
    for i in 0..count {
        // `by_index_raw` reads the entry's header metadata straight from the
        // parsed central directory — no decompression, no password.
        let entry = zipf.by_index_raw(i).map_err(map_zip_err)?;
        let encrypted = entry.encrypted();
        needs_password |= encrypted;
        entries.push(ArchiveEntry {
            path: entry.name().to_string(),
            is_dir: entry.is_dir(),
            uncompressed_size: Some(entry.size()),
            compressed_size: Some(entry.compressed_size()),
            // DOS date/time → unix seconds, so the Modified column shows the
            // real date instead of the epoch.
            mtime_unix: entry.last_modified().and_then(dos_datetime_to_unix),
            compression_method: Some(format!("{:?}", entry.compression())),
            checksum: Some(format!("CRC32 {:08X}", entry.crc32())),
            unix_mode: entry.unix_mode(),
            comment: (!entry.comment().is_empty()).then(|| entry.comment().to_string()),
            encrypted,
        });
    }
    Ok(Toc {
        entries,
        needs_password,
    })
}

pub(super) fn read_summary(archive: &Path) -> Result<ArchiveSummary, ArchiveError> {
    // Reuse the TOC read (cheap for zip) and derive the summary through the
    // pure model so the single-root / total rules stay in one place.
    let toc = read_toc(archive, None)?;
    Ok(ArchiveSummary {
        file_count: Some(toc.file_count() as u32),
        root: toc.single_root().map(str::to_string),
        encrypted: toc.needs_password,
        total_uncompressed: toc.total_uncompressed(),
    })
}

/// Convert a zip DOS timestamp to unix seconds.
///
/// The zip crate's own `OffsetDateTime` conversion sits behind its `time`
/// feature, which we don't enable, so we do the civil-date arithmetic here
/// (Howard Hinnant's `days_from_civil`). DOS timestamps carry no timezone; like
/// every other zip tool we read them as UTC.
pub(super) fn dos_datetime_to_unix(dt: zip::DateTime) -> Option<i64> {
    let (y, m, d) = (dt.year() as i64, dt.month() as i64, dt.day() as i64);
    if m == 0 || d == 0 {
        return None; // DOS "unset" timestamp
    }
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + dt.hour() as i64 * 3_600 + dt.minute() as i64 * 60 + dt.second() as i64)
}

/// Read one entry into memory, or `Ok(None)` when it exceeds `cap`.
pub(super) fn read_entry_bytes(
    archive: &Path,
    entry: &str,
    password: Option<&str>,
    cap: u64,
) -> Result<Option<Vec<u8>>, ArchiveError> {
    use std::io::Read as _;
    let mut zipf = open(archive)?;
    let count = zipf.len();
    for i in 0..count {
        let (name, is_dir, encrypted, size) = {
            let e = zipf.by_index_raw(i).map_err(map_zip_err)?;
            (e.name().to_string(), e.is_dir(), e.encrypted(), e.size())
        };
        if name != entry || is_dir {
            continue;
        }
        if size > cap {
            return Ok(None);
        }
        if encrypted && password.is_none() {
            return Err(ArchiveError::PasswordRequired);
        }
        let mut f = if encrypted {
            zipf.by_index_decrypt(i, password.unwrap_or_default().as_bytes())
                .map_err(map_zip_err)?
        } else {
            zipf.by_index(i).map_err(map_zip_err)?
        };
        let mut buf = Vec::with_capacity(size as usize);
        f.read_to_end(&mut buf)?;
        return Ok(Some(buf));
    }
    Ok(None)
}

/// Whether a unix mode marks a symlink (`S_IFLNK`).
fn is_symlink_mode(mode: Option<u32>) -> bool {
    mode.is_some_and(|m| m & 0o170000 == 0o120000)
}

pub(super) fn extract(
    archive: &Path,
    dest: &Path,
    sel: &Selection,
    opts: ExtractOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<ExtractOutcome, ArchiveError> {
    let mut zipf = open(archive)?;
    let count = zipf.len();

    // Pre-pass over the central directory (metadata only, no inflation) to size
    // the progress bar determinately — zip records every entry's size up front.
    let mut total_bytes = 0u64;
    let mut total_files = 0u64;
    for i in 0..count {
        let e = zipf.by_index_raw(i).map_err(map_zip_err)?;
        if !e.is_dir() && sel.includes(e.name()) {
            total_bytes += e.size();
            total_files += 1;
        }
    }
    progress.begin_transfer(total_bytes, total_files);

    let mut outcome = ExtractOutcome::default();
    for i in 0..count {
        super::check_cancel(cancel)?;

        // Read the metadata first (raw borrow, dropped before we reborrow to
        // read data) so we can decide selection / dir / symlink / encryption.
        let (name, is_dir, encrypted, unix_mode) = {
            let e = zipf.by_index_raw(i).map_err(map_zip_err)?;
            (
                e.name().to_string(),
                e.is_dir(),
                e.encrypted(),
                e.unix_mode(),
            )
        };
        if !sel.includes(&name) {
            continue;
        }
        let Some(safe) = super::safe_or_skip(&name, &mut outcome) else {
            continue;
        };
        if is_dir {
            super::make_dir(dest, &safe, &mut outcome)?;
            continue;
        }
        if is_symlink_mode(unix_mode) {
            outcome.skip(name, SkipReason::Symlink);
            continue;
        }
        if encrypted && opts.password.is_none() {
            return Err(ArchiveError::PasswordRequired);
        }

        let mut entry = if encrypted {
            zipf.by_index_decrypt(i, opts.password.unwrap_or_default().as_bytes())
                .map_err(map_zip_err)?
        } else {
            zipf.by_index(i).map_err(map_zip_err)?
        };
        super::write_file(
            dest,
            &safe,
            &mut entry,
            opts,
            progress,
            cancel,
            &mut outcome,
        )?;
    }
    Ok(outcome)
}

/// Build the per-entry write options from the create options: compression
/// method + level, plus AES-256 when a password is set. The password is
/// borrowed, so the options carry its lifetime (not `'static`).
fn entry_options(opts: CreateOptions<'_>) -> zip::write::FileOptions<'_, ()> {
    use zip::CompressionMethod;
    use zip::write::SimpleFileOptions;
    let mut o = SimpleFileOptions::default();
    o = match opts.level {
        ferail_archive::CompressionLevel::Store => o.compression_method(CompressionMethod::Stored),
        level => o
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(super::stream_level(level) as i64)),
    };
    if let Some(pw) = opts.password {
        o = o.with_aes_encryption(zip::AesMode::Aes256, pw);
    }
    o
}

pub(super) fn create(
    items: &[PlannedItem],
    output: &Path,
    opts: CreateOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<(), ArchiveError> {
    let mut zw = zip::ZipWriter::new(File::create(output)?);
    let entry_opts = entry_options(opts);
    for item in items {
        super::check_cancel(cancel)?;
        if item.is_dir {
            zw.add_directory(&item.rel, entry_opts)
                .map_err(map_zip_err)?;
        } else {
            zw.start_file(&item.rel, entry_opts).map_err(map_zip_err)?;
            progress.note_current(&item.abs);
            let src = File::open(&item.abs)?;
            super::copy_stream(src, &mut zw, progress, cancel)?;
            progress.add_items(1);
        }
    }
    zw.finish().map_err(map_zip_err)?;
    Ok(())
}

/// Append `items` to an existing zip **transactionally**.
///
/// The archive is never written in place: existing members are byte-copied
/// (never re-encoded) into a sibling temp file, the new items are appended
/// there, and only a validated result replaces the original atomically. An
/// in-place append is faster — it rewrites just the central directory — but a
/// cancellation, vanished source, read error, or full disk part-way through
/// would leave a truncated member inside the user's real archive while the
/// operation reported failure. Same contract as [`rewrite`]; this path exists
/// because "add these files" skips duplicates rather than failing on them.
///
/// Names already present are skipped: zip permits duplicates, but a second
/// record with the same name shadows the first rather than replacing it, and
/// extraction (which refuses to clobber) would then write the *original*
/// bytes — the opposite of what "add this file" means. The set grows as items
/// are written, so two dropped inputs that resolve to the same archive path
/// cannot produce duplicate records either.
pub(super) fn append(
    archive: &Path,
    items: &[super::PlannedItem],
    opts: CreateOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<super::AddOutcome, ArchiveError> {
    let expected = super::archive_stamp(archive)?;
    let mut names: std::collections::HashSet<String> = read_toc(archive, None)?
        .entries
        .into_iter()
        .map(|e| e.path.trim_end_matches('/').to_string())
        .collect();

    let mut source = open(archive)?;
    let source_comment = source.comment().to_vec();
    let temp = unique_sibling(archive, "add")?;
    let mut temp_guard = TempPath::new(temp.clone());
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let mut writer = zip::ZipWriter::new(output);
    writer.set_raw_comment(source_comment.into_boxed_slice());

    // Carry the original members over in their already-compressed form.
    let mut copied = 0usize;
    for index in 0..source.len() {
        super::check_cancel(cancel)?;
        let entry = source.by_index_raw(index).map_err(map_zip_err)?;
        writer.raw_copy_file(entry).map_err(map_zip_err)?;
        copied += 1;
    }

    let entry_opts = entry_options(opts);
    let mut outcome = super::AddOutcome::default();
    let mut written = 0usize;
    for item in items {
        super::check_cancel(cancel)?;
        let key = item.rel.trim_end_matches('/').to_string();
        if !names.insert(key) {
            // Directories that already exist are not worth reporting — only
            // files the user expected to land.
            if !item.is_dir {
                outcome.skipped_existing.push(item.rel.clone());
            }
            continue;
        }
        if item.is_dir {
            writer
                .add_directory(&item.rel, entry_opts)
                .map_err(map_zip_err)?;
        } else {
            writer.start_file(&item.rel, entry_opts).map_err(map_zip_err)?;
            progress.note_current(&item.abs);
            let src = File::open(&item.abs)?;
            super::copy_stream(src, &mut writer, progress, cancel)?;
            progress.add_items(1);
            outcome.added += 1;
        }
        written += 1;
    }

    let output = writer.finish().map_err(map_zip_err)?;
    output.sync_all()?;
    std::fs::set_permissions(&temp, std::fs::metadata(archive)?.permissions())?;
    copy_extended_attributes(archive, &temp)?;

    // Parse the central directory before the original is touched, so a
    // truncated or codec-broken result never replaces a good archive.
    let validated = read_toc(&temp, None)?;
    if validated.entries.len() != copied + written {
        return Err(ArchiveError::Corrupt(
            "the updated archive did not contain the expected entries".to_string(),
        ));
    }
    super::check_cancel(cancel)?;
    drop(source);
    if super::archive_stamp(archive)? != expected {
        return Err(ArchiveError::Codec(
            "the archive changed on disk while items were being added; reload it and try again"
                .to_string(),
        ));
    }
    atomic_replace(&temp, archive)?;
    temp_guard.keep();
    Ok(outcome)
}

/// Rewrite a zip from a staged edit journal. Unchanged members are copied in
/// their already-compressed form, preserving compression, encryption, extra
/// fields, timestamps, comments, modes, and checksums without ever decoding
/// their payloads.
pub(super) fn rewrite(
    archive: &Path,
    expected: super::ArchiveStamp,
    plan: &ArchiveEditPlan,
    opts: CreateOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<(), ArchiveError> {
    let mut additions: Vec<PlannedItem> = Vec::new();
    let mut addition_bytes = 0u64;
    for addition in &plan.additions {
        let (mut items, bytes) = super::plan_inputs(&[addition.source.as_path()])?;
        let destination = normalize_optional_dir(&addition.destination)?;
        for item in &mut items {
            item.rel = join_internal(&destination, &item.rel);
            item.rel = renamed_path(&item.rel, &plan.renames)?;
        }
        addition_bytes = addition_bytes.saturating_add(bytes);
        additions.extend(items);
    }

    let source_bytes = std::fs::metadata(archive)?.len();
    let item_total = additions.iter().filter(|item| !item.is_dir).count() as u64;
    progress.begin_transfer(source_bytes.saturating_add(addition_bytes), item_total);

    let mut source = open(archive)?;
    let source_comment = source.comment().to_vec();
    let temp = unique_sibling(archive, "edit")?;
    let mut temp_guard = TempPath::new(temp.clone());
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let mut writer = zip::ZipWriter::new(output);
    writer.set_raw_comment(source_comment.into_boxed_slice());

    let mut names = std::collections::HashSet::<String>::new();
    let mut source_accounted = 0u64;
    for index in 0..source.len() {
        super::check_cancel(cancel)?;
        let entry = source.by_index_raw(index).map_err(map_zip_err)?;
        let original = entry.name().to_string();
        if path_matches_any(&original, &plan.removals) {
            let compressed = entry.compressed_size();
            source_accounted = source_accounted.saturating_add(compressed);
            progress.add_bytes(compressed);
            continue;
        }
        let renamed = renamed_path(&original, &plan.renames)?;
        let collision_key = renamed.trim_end_matches('/').to_string();
        if !names.insert(collision_key.clone()) {
            return Err(ArchiveError::Codec(format!(
                "more than one entry would be named {collision_key}"
            )));
        }
        let compressed = entry.compressed_size();
        source_accounted = source_accounted.saturating_add(compressed);
        writer
            .raw_copy_file_rename(entry, renamed)
            .map_err(map_zip_err)?;
        progress.add_bytes(compressed);
    }
    // Central-directory records and local headers are part of the source
    // archive's physical size but not any member's compressed payload.
    progress.add_bytes(source_bytes.saturating_sub(source_accounted));

    let add_opts = entry_options(opts);
    for item in additions {
        super::check_cancel(cancel)?;
        let collision_key = item.rel.trim_end_matches('/').to_string();
        if !names.insert(collision_key.clone()) {
            return Err(ArchiveError::Codec(format!(
                "an entry named {collision_key} is already in the archive"
            )));
        }
        if item.is_dir {
            writer
                .add_directory(&item.rel, add_opts)
                .map_err(map_zip_err)?;
        } else {
            writer
                .start_file(&item.rel, add_opts)
                .map_err(map_zip_err)?;
            progress.note_current(&item.abs);
            let input = File::open(&item.abs)?;
            super::copy_stream(input, &mut writer, progress, cancel)?;
            progress.add_items(1);
        }
    }

    let output = writer.finish().map_err(map_zip_err)?;
    output.sync_all()?;
    std::fs::set_permissions(&temp, std::fs::metadata(archive)?.permissions())?;
    copy_extended_attributes(archive, &temp)?;

    // Parse the central directory before the original is touched. This catches
    // truncated output and writer/codec errors that only become visible when
    // the archive is reopened.
    let validated = read_toc(&temp, None)?;
    if validated.entries.len() != names.len() {
        return Err(ArchiveError::Corrupt(
            "the rewritten archive did not contain the expected entries".to_string(),
        ));
    }
    super::check_cancel(cancel)?;
    drop(source);
    if super::archive_stamp(archive)? != expected {
        return Err(ArchiveError::Codec(
            "the archive changed on disk while changes were being saved; reload it before saving"
                .to_string(),
        ));
    }
    atomic_replace(&temp, archive)?;
    temp_guard.keep();
    Ok(())
}

#[cfg(unix)]
fn copy_extended_attributes(source: &Path, destination: &Path) -> Result<(), ArchiveError> {
    for name in xattr::list(source)? {
        if let Some(value) = xattr::get(source, &name)? {
            xattr::set(destination, &name, &value)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn copy_extended_attributes(_source: &Path, _destination: &Path) -> Result<(), ArchiveError> {
    Ok(())
}

fn normalize_optional_dir(path: &str) -> Result<String, ArchiveError> {
    if path.trim_matches('/').is_empty() {
        return Ok(String::new());
    }
    ferail_archive::safe_relative_path(path)
        .map_err(|_| ArchiveError::Codec(format!("unsafe archive destination: {path}")))
}

fn join_internal(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{parent}/{}", child.trim_start_matches('/'))
    }
}

fn normalized(path: &str) -> &str {
    path.trim_matches('/')
}

fn path_is_at_or_below(path: &str, root: &str) -> bool {
    let path = normalized(path);
    let root = normalized(root);
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn path_matches_any(path: &str, roots: &[String]) -> bool {
    roots.iter().any(|root| path_is_at_or_below(path, root))
}

fn renamed_path(original: &str, renames: &[super::ArchiveRename]) -> Result<String, ArchiveError> {
    let trailing_slash = original.ends_with('/');
    let path = normalized(original);
    let mut best: Option<(&str, &str)> = None;
    for rename in renames {
        if path_is_at_or_below(path, &rename.from)
            && best.is_none_or(|(from, _)| normalized(&rename.from).len() > from.len())
        {
            best = Some((normalized(&rename.from), normalized(&rename.to)));
        }
    }
    let Some((from, to)) = best else {
        return Ok(original.to_string());
    };
    let safe_to = ferail_archive::safe_relative_path(to)
        .map_err(|_| ArchiveError::Codec(format!("unsafe archive name: {to}")))?;
    let suffix = path.strip_prefix(from).unwrap_or_default();
    let mut result = format!("{safe_to}{suffix}");
    if trailing_slash {
        result.push('/');
    }
    Ok(result)
}

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn unique_sibling(archive: &Path, purpose: &str) -> Result<PathBuf, ArchiveError> {
    let parent = archive
        .parent()
        .ok_or_else(|| ArchiveError::Codec("the archive has no parent directory".to_string()))?;
    let leaf = archive
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive.zip".to_string());
    for _ in 0..100 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{leaf}.ferail-{purpose}-{}-{sequence}.tmp",
            std::process::id()
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(ArchiveError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a temporary archive name",
    )))
}

struct TempPath {
    path: Option<PathBuf>,
}

impl TempPath {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn keep(&mut self) {
        self.path = None;
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(not(windows))]
fn atomic_replace(temp: &Path, archive: &Path) -> Result<(), ArchiveError> {
    std::fs::rename(temp, archive)?;
    Ok(())
}

#[cfg(windows)]
fn atomic_replace(temp: &Path, archive: &Path) -> Result<(), ArchiveError> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::Win32::Storage::FileSystem::{REPLACEFILE_WRITE_THROUGH, ReplaceFileW};
    use windows::core::PCWSTR;

    let archive_w: Vec<u16> = archive.as_os_str().encode_wide().chain(Some(0)).collect();
    let temp_w: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both UTF-16 buffers are NUL-terminated and live for the call;
    // no optional backup/exclusion pointers are supplied.
    unsafe {
        ReplaceFileW(
            PCWSTR(archive_w.as_ptr()),
            PCWSTR(temp_w.as_ptr()),
            PCWSTR::null(),
            REPLACEFILE_WRITE_THROUGH,
            None,
            None,
        )
        .map_err(|error| ArchiveError::Io(std::io::Error::other(error)))?;
    }
    Ok(())
}

fn map_zip_err(e: ZipError) -> ArchiveError {
    match e {
        ZipError::Io(io) => ArchiveError::Io(io),
        ZipError::InvalidPassword => ArchiveError::WrongPassword,
        ZipError::UnsupportedArchive(msg) => {
            // The zip crate reports a missing password as an unsupported-archive
            // error with this sentinel message.
            if msg == ZipError::PASSWORD_REQUIRED {
                ArchiveError::PasswordRequired
            } else {
                ArchiveError::Codec(msg.to_string())
            }
        }
        ZipError::InvalidArchive(msg) => ArchiveError::Corrupt(msg.to_string()),
        other => ArchiveError::Codec(other.to_string()),
    }
}
