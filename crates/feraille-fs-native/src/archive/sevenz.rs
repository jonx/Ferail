//! 7-Zip read path (read/extract only — no write in v1). A 7z file keeps its
//! file list in a header/footer, so the TOC is cheap to read without inflating
//! payloads. Header encryption means the list itself can require a password; we
//! surface that as [`ArchiveError::PasswordRequired`] so the workbench can
//! prompt and retry.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use feraille_archive::{ArchiveEntry, Toc};

use super::{
    ArchiveError, ArchiveSummary, CreateOptions, ExtractOptions, ExtractOutcome, PlannedItem,
    Selection,
};
use crate::file_ops::TransferProgress;

fn password_of(password: Option<&str>) -> sevenz_rust::Password {
    match password {
        Some(p) => sevenz_rust::Password::from(p),
        None => sevenz_rust::Password::empty(),
    }
}

pub(super) fn read_toc(archive: &Path, password: Option<&str>) -> Result<Toc, ArchiveError> {
    let reader =
        sevenz_rust::SevenZReader::open(archive, password_of(password)).map_err(map_sz_err)?;
    let mut entries = Vec::new();
    for f in &reader.archive().files {
        entries.push(ArchiveEntry {
            path: f.name().replace('\\', "/"),
            is_dir: f.is_directory(),
            uncompressed_size: Some(f.size()),
            compressed_size: None,
            mtime_unix: None,
            // Per-entry encryption in 7z is a property of the coder chain,
            // which this metadata pass does not resolve; content-password
            // failures surface at extraction time instead.
            encrypted: false,
        });
    }
    Ok(Toc {
        entries,
        needs_password: password.is_some(),
    })
}

pub(super) fn read_summary(archive: &Path) -> Result<ArchiveSummary, ArchiveError> {
    match read_toc(archive, None) {
        Ok(toc) => Ok(ArchiveSummary {
            file_count: Some(toc.file_count() as u32),
            root: toc.single_root().map(str::to_string),
            encrypted: toc.needs_password,
            total_uncompressed: toc.total_uncompressed(),
        }),
        // Header-encrypted: we know it is a 7z and that it is encrypted, which
        // is enough for a "7-Zip archive · encrypted" description.
        Err(ArchiveError::PasswordRequired) => Ok(ArchiveSummary {
            encrypted: true,
            ..ArchiveSummary::default()
        }),
        Err(e) => Err(e),
    }
}

pub(super) fn extract(
    archive: &Path,
    dest: &Path,
    sel: &Selection,
    opts: ExtractOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<ExtractOutcome, ArchiveError> {
    // 7z stores sizes in the header, but summing them here would mean a second
    // metadata pass; leave the bar indeterminate and count as we stream.
    progress.begin_transfer(0, 0);
    let mut reader =
        sevenz_rust::SevenZReader::open(archive, password_of(opts.password)).map_err(map_sz_err)?;

    let mut outcome = ExtractOutcome::default();
    // The backend closure can only surface its own error type, so our
    // ArchiveError (cancel, I/O, zip-slip) is captured out-of-band and the
    // closure returns `Ok(false)` to stop the walk.
    let mut captured: Option<ArchiveError> = None;
    let walk = reader.for_each_entries(|entry, rd| {
        if cancel.load(Ordering::Relaxed) {
            captured = Some(ArchiveError::Cancelled);
            return Ok(false);
        }
        let name = entry.name().replace('\\', "/");
        if !sel.includes(&name) {
            return Ok(true);
        }
        if entry.is_directory() {
            if let Some(safe) = super::safe_or_skip(&name, &mut outcome) {
                if let Err(e) = super::make_dir(dest, &safe, &mut outcome) {
                    captured = Some(e);
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        let Some(safe) = super::safe_or_skip(&name, &mut outcome) else {
            return Ok(true);
        };
        if let Err(e) = super::write_file(dest, &safe, &mut *rd, opts, progress, cancel, &mut outcome)
        {
            captured = Some(e);
            return Ok(false);
        }
        Ok(true)
    });

    if let Some(e) = captured {
        return Err(e);
    }
    walk.map_err(map_sz_err)?;
    Ok(outcome)
}

pub(super) fn create(
    items: &[PlannedItem],
    output: &Path,
    _opts: CreateOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<(), ArchiveError> {
    // Create-time password / level are not wired yet (the capability matrix
    // notes this); v1 uses the writer's default LZMA2 chain.
    let mut writer = sevenz_rust::SevenZWriter::create(output).map_err(map_sz_err)?;
    for item in items {
        super::check_cancel(cancel)?;
        let entry = sevenz_rust::SevenZArchiveEntry::from_path(&item.abs, item.rel.clone());
        if item.is_dir {
            writer
                .push_archive_entry(entry, None::<std::fs::File>)
                .map_err(map_sz_err)?;
        } else {
            progress.note_current(&item.abs);
            let file = std::fs::File::open(&item.abs)?;
            writer.push_archive_entry(entry, Some(file)).map_err(map_sz_err)?;
            progress.add_bytes(item.size);
            progress.add_items(1);
        }
    }
    writer.finish().map_err(ArchiveError::Io)?;
    Ok(())
}

fn map_sz_err(e: sevenz_rust::Error) -> ArchiveError {
    let msg = e.to_string();
    let lower = msg.to_ascii_lowercase();
    if lower.contains("password") || lower.contains("encrypt") {
        // The backend does not cleanly separate "missing" from "wrong" here;
        // treat an empty-password failure as "required" so the UI prompts.
        ArchiveError::PasswordRequired
    } else {
        ArchiveError::Codec(msg)
    }
}
