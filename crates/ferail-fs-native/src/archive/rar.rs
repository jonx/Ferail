//! RAR codec: read and extract (RAR 1.5 through RAR5).
//!
//! Backed by [`unrar_rs`], a pure-Rust decoder derived from the unRAR
//! source. It is a *decoder only*: RAR creation is proprietary and the unRAR
//! licence forbids re-creating the compressor, so [`Format::Rar`] reports
//! `can_create: false`. The licence paragraph ships in
//! `THIRD-PARTY-NOTICES.md`.
//!
//! UnRAR source code may be used in any software to handle RAR archives
//! without limitations free of charge, but cannot be used to develop RAR
//! (WinRAR) compatible archiver and to re-create RAR compression algorithm,
//! which is proprietary. Distribution of modified UnRAR source code in
//! separate form or as a part of other software is permitted, provided that
//! full text of this paragraph, starting from "UnRAR source code" words, is
//! included in license, or in documentation if license is not available, and
//! in source code comments of resulting package.
//!
//! # Volumes
//!
//! A multi-part set is opened from any part's path: the sibling volumes are
//! attached by name, in both the RAR3+ `name.part01.rar` scheme and the
//! older `name.rar`, `name.r00`, `name.r01` one. A member that spans a missing
//! volume fails with a message naming that volume.
//!
//! # Extraction safety
//!
//! Members go through the same zip-slip guard and descriptor-relative writes
//! as every other format ([`super::write_file_from`]). The library's own
//! `unpack_*` helpers, which write to paths themselves, are never used.
//! Symlinks and hard links are skipped and reported.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::UNIX_EPOCH;

use unrar_rs::{MemberInfo, RarArchive, RarError};

use ferail_archive::{ArchiveEntry, Toc};

use super::{ArchiveError, ExtractOptions, ExtractOutcome, Selection, SkipReason};
use crate::file_ops::TransferProgress;

fn map_err(error: RarError) -> ArchiveError {
    match error {
        RarError::EncryptedArchive | RarError::EncryptedMember { .. } => {
            ArchiveError::PasswordRequired
        }
        RarError::InvalidPassword | RarError::WrongPassword { .. } => ArchiveError::WrongPassword,
        RarError::MissingVolume { volume, .. } => ArchiveError::Codec(format!(
            "part {} of this multi-part RAR archive is missing",
            volume + 1
        )),
        other => ArchiveError::Corrupt(other.to_string()),
    }
}

fn open(archive: &Path, password: Option<&str>) -> Result<RarArchive, ArchiveError> {
    let first = first_volume(archive);
    let file = File::open(&first)?;
    let mut rar = match password {
        Some(password) => RarArchive::open_with_password(file, password),
        None => RarArchive::open(file),
    }
    .map_err(map_err)?;
    attach_sibling_volumes(&mut rar, &first);
    Ok(rar)
}

/// `name.partNN.rar` split into (`name`, digit width, part number).
fn part_scheme(leaf: &str) -> Option<(&str, usize, u32)> {
    let lower = leaf.to_ascii_lowercase();
    let body = lower.strip_suffix(".rar")?;
    let dot = body.rfind(".part")?;
    let digits = &body[dot + 5..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((&leaf[..dot], digits.len(), digits.parse().ok()?))
}

/// The path of volume 1 of the set `archive` belongs to: opening a later
/// part directly still lists and extracts the whole set.
fn first_volume(archive: &Path) -> PathBuf {
    let Some(leaf) = archive.file_name().and_then(|n| n.to_str()) else {
        return archive.to_path_buf();
    };
    if let Some((stem, width, number)) = part_scheme(leaf) {
        if number > 1 {
            let candidate = archive.with_file_name(format!("{stem}.part{:0width$}.rar", 1));
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    archive.to_path_buf()
}

/// Attach every sibling volume that exists, stopping at the first gap.
fn attach_sibling_volumes(rar: &mut RarArchive, first: &Path) {
    if !rar.more_volumes() {
        return;
    }
    let Some(leaf) = first.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let name_for: Box<dyn Fn(usize) -> String> = match part_scheme(leaf) {
        Some((stem, width, _)) => {
            let stem = stem.to_string();
            Box::new(move |volume| format!("{stem}.part{:0width$}.rar", volume + 1))
        }
        None => {
            let Some(stem) = leaf.get(..leaf.len().saturating_sub(4)) else {
                return;
            };
            let stem = stem.to_string();
            let upper = leaf.ends_with(".RAR");
            Box::new(move |volume| {
                let ext = if upper { "R" } else { "r" };
                format!("{stem}.{ext}{:02}", volume - 1)
            })
        }
    };
    for volume in 1..1000 {
        if rar.has_volume(volume) {
            continue;
        }
        let path = first.with_file_name(name_for(volume));
        let Ok(file) = File::open(&path) else {
            break;
        };
        rar.attach_volume_reader(volume, Box::new(file));
    }
}

/// Stored path in the `/`-separated form the archive layer expects. The raw
/// name is used, not the library's sanitized one, so the shared guard sees
/// and reports exactly what the archive stored.
fn entry_path(info: &MemberInfo) -> String {
    info.raw_name.replace('\\', "/")
}

fn mtime_unix(info: &MemberInfo) -> Option<i64> {
    let mtime = info.mtime?;
    match mtime.duration_since(UNIX_EPOCH) {
        Ok(after) => Some(after.as_secs() as i64),
        Err(before) => Some(-(before.duration().as_secs() as i64)),
    }
}

pub(super) fn read_toc(archive: &Path, password: Option<&str>) -> Result<Toc, ArchiveError> {
    let rar = open(archive, password)?;
    let mut needs_password = false;
    let entries = rar
        .entries()
        .filter_map(|info| {
            needs_password |= info.is_encrypted;
            let path = entry_path(&info);
            (!path.is_empty()).then(|| ArchiveEntry {
                is_dir: info.is_directory && !info.is_symlink,
                uncompressed_size: info.unpacked_size,
                compressed_size: Some(info.compressed_size),
                mtime_unix: mtime_unix(&info),
                compression_method: None,
                checksum: info.crc32.map(|crc| format!("CRC32 {crc:08X}")),
                unix_mode: None,
                comment: None,
                encrypted: info.is_encrypted,
                path,
            })
        })
        .collect();
    Ok(Toc {
        entries,
        needs_password,
    })
}

pub(super) fn extract(
    archive: &Path,
    dest: &Path,
    sel: &Selection,
    opts: ExtractOptions<'_>,
    progress: &TransferProgress,
    cancel: &AtomicBool,
) -> Result<ExtractOutcome, ArchiveError> {
    let mut rar = open(archive, opts.password)?;
    let infos: Vec<MemberInfo> = rar.entries().collect();
    let (bytes, items) = infos
        .iter()
        .filter(|info| !info.is_directory && sel.includes(&entry_path(info)))
        .fold((0u64, 0u64), |(bytes, items), info| {
            (bytes + info.unpacked_size.unwrap_or(0), items + 1)
        });
    progress.begin_transfer(bytes, items);
    let mut outcome = ExtractOutcome::default();
    // Index order: a solid archive decodes each member from the state the
    // previous one left, so members are visited front to back.
    for (index, info) in infos.iter().enumerate() {
        super::check_cancel(cancel)?;
        let path = entry_path(info);
        if path.is_empty() || !sel.includes(&path) {
            continue;
        }
        if info.is_symlink {
            outcome.skip(path, SkipReason::Symlink);
        } else if info.is_hardlink || info.is_file_copy {
            outcome.skip(path, SkipReason::HardLink);
        } else if info.is_directory {
            if let Some(safe) = super::safe_or_skip(&path, &mut outcome) {
                super::make_dir(dest, &safe, &mut outcome)?;
            }
        } else if let Some(safe) = super::safe_or_skip(&path, &mut outcome) {
            let encrypted = info.is_encrypted;
            super::write_file_from(dest, &safe, opts, progress, cancel, &mut outcome, |out| {
                let entry = rar.by_index(index).map_err(map_err)?;
                entry.copy_to(out).map(|_| ()).map_err(|error| {
                    // A cancel surfaces from the writer as an I/O error.
                    if super::check_cancel(cancel).is_err() {
                        return ArchiveError::Cancelled;
                    }
                    match map_err(error) {
                        // RAR4 stores no password check: a wrong password
                        // only shows up as undecodable data or a bad CRC.
                        ArchiveError::Corrupt(_) if encrypted && opts.password.is_some() => {
                            ArchiveError::WrongPassword
                        }
                        other => other,
                    }
                })
            })?;
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_scheme_parses_numbered_volumes() {
        assert_eq!(part_scheme("Movie.part01.rar"), Some(("Movie", 2, 1)));
        assert_eq!(part_scheme("a.b.PART007.RAR"), Some(("a.b", 3, 7)));
        assert_eq!(part_scheme("plain.rar"), None);
        assert_eq!(part_scheme("x.part.rar"), None);
    }
}
