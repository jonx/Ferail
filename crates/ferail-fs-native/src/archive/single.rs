//! Single-member compressors: `.gz`, `.bz2`, `.xz` on their own (not wrapping a
//! tar). These hold exactly one logical file, so the TOC is always a single
//! entry. The uncompressed size is not recorded in a form we can trust cheaply
//! (gzip's footer size is mod 2^32; bz2/xz store nothing up front), so it is
//! left unknown rather than guessed — decompressing to measure would violate
//! the bounded-read contract.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use ferail_archive::{ArchiveEntry, Format, Toc};

use super::{
    ArchiveError, ArchiveSummary, CreateOptions, ExtractOptions, ExtractOutcome, PlannedItem,
    Selection,
};
use crate::file_ops::TransferProgress;

/// The logical name of the one member: the archive's filename with the
/// compression suffix stripped (`report.csv.gz` → `report.csv`). Falls back to
/// the whole leaf if the suffix is somehow absent.
fn member_name(archive: &Path, format: Format) -> String {
    let leaf = archive
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dot_ext = format!(".{}", format.canonical_extension());
    match leaf
        .to_ascii_lowercase()
        .strip_suffix(&dot_ext)
        .map(|stripped| leaf[..stripped.len()].to_string())
    {
        Some(name) if !name.is_empty() => name,
        _ => leaf,
    }
}

pub(super) fn read_toc(archive: &Path, format: Format) -> Result<Toc, ArchiveError> {
    // Confirm the file exists / is readable so an opened-but-missing archive
    // reports honestly, but do not decompress.
    let _ = std::fs::metadata(archive)?;
    Ok(Toc {
        entries: vec![ArchiveEntry {
            path: member_name(archive, format),
            is_dir: false,
            uncompressed_size: None,
            compressed_size: std::fs::metadata(archive).ok().map(|m| m.len()),
            mtime_unix: None,
            encrypted: false,
        }],
        needs_password: false,
    })
}

pub(super) fn read_summary(archive: &Path, _format: Format) -> Result<ArchiveSummary, ArchiveError> {
    let _ = std::fs::metadata(archive)?;
    Ok(ArchiveSummary {
        file_count: Some(1),
        root: None,
        encrypted: false,
        total_uncompressed: None,
    })
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
    let name = member_name(archive, format);
    let mut outcome = ExtractOutcome::default();
    if !sel.includes(&name) {
        return Ok(outcome);
    }
    let Some(safe) = super::safe_or_skip(&name, &mut outcome) else {
        return Ok(outcome);
    };
    // Uncompressed size is unknown up front, so progress stays indeterminate.
    progress.begin_transfer(0, 1);
    let file = File::open(archive)?;
    let reader: Box<dyn Read> = match format {
        Format::Gzip => Box::new(flate2::read::GzDecoder::new(file)),
        Format::Bzip2 => Box::new(bzip2::read::BzDecoder::new(file)),
        #[cfg(not(target_os = "aros"))]
        Format::Xz => Box::new(xz2::read::XzDecoder::new(file)),
        #[cfg(target_os = "aros")]
        Format::Xz => return Err(ArchiveError::Codec(super::XZ_UNAVAILABLE.into())),
        other => {
            return Err(ArchiveError::Codec(format!(
                "not a single-member format: {}",
                other.label()
            )))
        }
    };
    super::write_file(dest, &safe, reader, opts, progress, cancel, &mut outcome)?;
    Ok(outcome)
}

pub(super) fn create(
    format: Format,
    items: &[PlannedItem],
    output: &Path,
    opts: CreateOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<(), ArchiveError> {
    // A single-member compressor holds exactly one file — no directories, no
    // second input.
    let files: Vec<&PlannedItem> = items.iter().filter(|i| !i.is_dir).collect();
    if files.len() != 1 {
        return Err(ArchiveError::Codec(format!(
            "{} compresses exactly one file, got {}",
            format.label(),
            files.len()
        )));
    }
    let src = File::open(&files[0].abs)?;
    let out = File::create(output)?;
    let level = super::stream_level(opts.level);
    progress.note_current(&files[0].abs);
    match format {
        Format::Gzip => {
            let mut enc = flate2::write::GzEncoder::new(out, flate2::Compression::new(level));
            super::copy_stream(src, &mut enc, progress, cancel)?;
            enc.finish()?;
        }
        Format::Bzip2 => {
            let mut enc = bzip2::write::BzEncoder::new(out, bzip2::Compression::new(level.max(1)));
            super::copy_stream(src, &mut enc, progress, cancel)?;
            enc.finish()?;
        }
        #[cfg(not(target_os = "aros"))]
        Format::Xz => {
            let mut enc = xz2::write::XzEncoder::new(out, level);
            super::copy_stream(src, &mut enc, progress, cancel)?;
            enc.finish()?;
        }
        #[cfg(target_os = "aros")]
        Format::Xz => return Err(ArchiveError::Codec(super::XZ_UNAVAILABLE.into())),
        other => {
            return Err(ArchiveError::Codec(format!(
                "not a single-member format: {}",
                other.label()
            )))
        }
    }
    progress.add_items(1);
    Ok(())
}
