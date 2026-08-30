//! Tar-family read path: plain `.tar` and the compressed `.tar.gz` / `.tar.bz2`
//! / `.tar.xz`. Tar has no central directory: the only way to know what is
//! inside is to walk the whole stream, so there is deliberately **no**
//! bounded-summary path here (the dispatcher returns a format-only summary for
//! these). [`read_toc`] streams the entire archive, which is why it is a
//! full-mode, off-UI-thread operation.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use ferail_archive::{ArchiveEntry, Format, Toc};

use super::{
    ArchiveError, CreateOptions, ExtractOptions, ExtractOutcome, PlannedItem, Selection, SkipReason,
};
use crate::file_ops::TransferProgress;

/// Wrap the archive file in the right streaming decompressor for `format`.
fn decoded_reader(archive: &Path, format: Format) -> Result<Box<dyn Read>, ArchiveError> {
    let file = File::open(archive)?;
    let reader: Box<dyn Read> = match format {
        Format::Tar => Box::new(file),
        Format::TarGz => Box::new(flate2::read::GzDecoder::new(file)),
        Format::TarBz2 => Box::new(bzip2::read::BzDecoder::new(file)),
        Format::TarXz => Box::new(xz2::read::XzDecoder::new(file)),
        // The dispatcher only routes tar-family formats here.
        other => {
            return Err(ArchiveError::Codec(format!(
                "not a tar-family format: {}",
                other.label()
            )));
        }
    };
    Ok(reader)
}

/// Whether a lexically single-member compressor actually wraps a tar stream.
///
/// Only one decoded tar block is read. This is deliberately called from
/// `probe_format`, which already has the off-UI-thread tripwire; it never runs
/// from rendering or context-menu construction.
pub(super) fn compressed_payload_is_tar(archive: &Path, format: Format) -> bool {
    let Ok(file) = File::open(archive) else {
        return false;
    };
    let mut reader: Box<dyn Read> = match format {
        Format::Gzip => Box::new(flate2::read::GzDecoder::new(file)),
        Format::Bzip2 => Box::new(bzip2::read::BzDecoder::new(file)),
        Format::Xz => Box::new(xz2::read::XzDecoder::new(file)),
        _ => return false,
    };
    let mut block = [0u8; 512];
    reader.read_exact(&mut block).is_ok() && looks_like_tar_header(&block)
}

/// Validate the first tar header using both its name and checksum. Checking
/// only `ustar` would miss old v7 tar files; checking only printable bytes
/// would misclassify arbitrary compressed data.
fn looks_like_tar_header(block: &[u8; 512]) -> bool {
    if block[..100].iter().all(|b| *b == 0) {
        return false;
    }
    let stored = parse_tar_octal(&block[148..156]);
    let Some(stored) = stored else { return false };
    let unsigned_sum: u64 = block
        .iter()
        .enumerate()
        .map(|(ix, byte)| {
            if (148..156).contains(&ix) {
                b' ' as u64
            } else {
                *byte as u64
            }
        })
        .sum();
    stored == unsigned_sum
}

fn parse_tar_octal(bytes: &[u8]) -> Option<u64> {
    let text = bytes
        .iter()
        .copied()
        .skip_while(|b| *b == b' ' || *b == 0)
        .take_while(|b| (b'0'..=b'7').contains(b))
        .collect::<Vec<_>>();
    (!text.is_empty())
        .then(|| std::str::from_utf8(&text).ok())
        .flatten()
        .and_then(|s| u64::from_str_radix(s, 8).ok())
}

pub(super) fn read_toc(archive: &Path, format: Format) -> Result<Toc, ArchiveError> {
    let reader = decoded_reader(archive, format)?;
    let mut ar = tar::Archive::new(reader);
    let mut entries = Vec::new();
    for entry in ar.entries()? {
        let entry = entry?;
        let header = entry.header();
        let is_dir = header.entry_type().is_dir();
        let path = entry
            .path()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if path.is_empty() {
            continue;
        }
        entries.push(ArchiveEntry {
            path,
            is_dir,
            uncompressed_size: header.size().ok(),
            compressed_size: None, // per-member compressed size is not meaningful for a tar stream
            mtime_unix: header.mtime().ok().map(|m| m as i64),
            compression_method: None,
            checksum: header.cksum().ok().map(|sum| format!("TAR {:06o}", sum)),
            unix_mode: header.mode().ok(),
            comment: None,
            encrypted: false, // tar has no encryption
        });
    }
    Ok(Toc {
        entries,
        needs_password: false,
    })
}

/// Read one entry into memory, or `Ok(None)` when it exceeds `cap` or is
/// absent. Tar has no index, so this streams until it finds the entry.
pub(super) fn read_entry_bytes(
    archive: &Path,
    format: Format,
    entry: &str,
    cap: u64,
) -> Result<Option<Vec<u8>>, ArchiveError> {
    let reader = decoded_reader(archive, format)?;
    let mut ar = tar::Archive::new(reader);
    for e in ar.entries()? {
        let mut e = e?;
        let path = e
            .path()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if path != entry || !e.header().entry_type().is_file() {
            continue;
        }
        if e.header().size().unwrap_or(u64::MAX) > cap {
            return Ok(None);
        }
        let mut buf = Vec::new();
        e.read_to_end(&mut buf)?;
        return Ok(Some(buf));
    }
    Ok(None)
}

pub(super) fn extract(
    archive: &Path,
    format: Format,
    dest: &Path,
    sel: &Selection,
    opts: ExtractOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<ExtractOutcome, ArchiveError> {
    // Tar has no directory, so totals are unknown up front: leave the progress
    // indeterminate (total 0) and count items as they stream.
    progress.begin_transfer(0, 0);
    let reader = decoded_reader(archive, format)?;
    let mut ar = tar::Archive::new(reader);
    let mut outcome = ExtractOutcome::default();
    for entry in ar.entries()? {
        super::check_cancel(cancel)?;
        let mut entry = entry?;
        let etype = entry.header().entry_type();
        let path = entry
            .path()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if path.is_empty() || !sel.includes(&path) {
            continue;
        }
        if etype.is_dir() {
            if let Some(safe) = super::safe_or_skip(&path, &mut outcome) {
                super::make_dir(dest, &safe, &mut outcome)?;
            }
            continue;
        }
        // Skip anything that is not a plain file so a malicious link/device is
        // never created; the zip-slip guard then makes lexical joins safe.
        if etype.is_symlink() {
            outcome.skip(path, SkipReason::Symlink);
            continue;
        }
        if etype.is_hard_link() {
            outcome.skip(path, SkipReason::HardLink);
            continue;
        }
        if !etype.is_file() {
            outcome.skip(path, SkipReason::SpecialFile);
            continue;
        }
        let Some(safe) = super::safe_or_skip(&path, &mut outcome) else {
            continue;
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

/// Append every planned item into `tb` and return the inner writer (the still-
/// open compression encoder) so the caller can `finish()` it explicitly,
/// dropping a boxed encoder would only flush best-effort and swallow errors.
fn build_tar<W: Write>(
    mut tb: tar::Builder<W>,
    items: &[PlannedItem],
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<W, ArchiveError> {
    for item in items {
        super::check_cancel(cancel)?;
        if item.is_dir {
            tb.append_dir(&item.rel, &item.abs)?;
        } else {
            progress.note_current(&item.abs);
            let mut f = File::open(&item.abs)?;
            // append_file builds the header from the file's own metadata and
            // copies the bytes; we credit progress by the known size after.
            tb.append_file(&item.rel, &mut f)?;
            progress.add_bytes(item.size);
            progress.add_items(1);
        }
    }
    Ok(tb.into_inner()?)
}

pub(super) fn create(
    format: Format,
    items: &[PlannedItem],
    output: &Path,
    opts: CreateOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<(), ArchiveError> {
    let file = File::create(output)?;
    let level = super::stream_level(opts.level);
    match format {
        Format::Tar => {
            build_tar(tar::Builder::new(file), items, progress, cancel)?;
        }
        Format::TarGz => {
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::new(level));
            let enc = build_tar(tar::Builder::new(enc), items, progress, cancel)?;
            enc.finish()?;
        }
        Format::TarBz2 => {
            // bzip2 has no level 0: clamp Store up to the minimum.
            let enc = bzip2::write::BzEncoder::new(file, bzip2::Compression::new(level.max(1)));
            let enc = build_tar(tar::Builder::new(enc), items, progress, cancel)?;
            enc.finish()?;
        }
        Format::TarXz => {
            let enc = xz2::write::XzEncoder::new(file, level);
            let enc = build_tar(tar::Builder::new(enc), items, progress, cancel)?;
            enc.finish()?;
        }
        other => {
            return Err(ArchiveError::Codec(format!(
                "not a tar-family format: {}",
                other.label()
            )));
        }
    }
    Ok(())
}
