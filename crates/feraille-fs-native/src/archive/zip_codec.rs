//! ZIP read path. The zip central directory is a real manifest we can read
//! without inflating any payload, so both the full TOC and the bounded summary
//! come from one cheap pass — `by_index_raw` exposes every entry's metadata
//! (name, sizes, encrypted flag) and never needs the password, which is only
//! required to read entry *data* at extraction time.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use feraille_archive::{ArchiveEntry, Toc};
use zip::result::ZipError;
use zip::ZipArchive;

use super::{
    ArchiveError, ArchiveSummary, CreateOptions, ExtractOptions, ExtractOutcome, PlannedItem,
    Selection, SkipReason,
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
            // Per-entry mtime decoding (DOS date/time → unix) is deferred; the
            // browse view falls back to a blank Modified cell until then.
            mtime_unix: None,
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
            (e.name().to_string(), e.is_dir(), e.encrypted(), e.unix_mode())
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
        super::write_file(dest, &safe, &mut entry, opts, progress, cancel, &mut outcome)?;
    }
    Ok(outcome)
}

/// Build the per-entry write options from the create options: compression
/// method + level, plus AES-256 when a password is set. The password is
/// borrowed, so the options carry its lifetime (not `'static`).
fn entry_options(opts: CreateOptions<'_>) -> zip::write::FileOptions<'_, ()> {
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;
    let mut o = SimpleFileOptions::default();
    o = match opts.level {
        feraille_archive::CompressionLevel::Store => o.compression_method(CompressionMethod::Stored),
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
            zw.add_directory(&item.rel, entry_opts).map_err(map_zip_err)?;
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
