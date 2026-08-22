//! LHA / LZH codec — read and extract.
//!
//! LHarc is the archive format of the Amiga world: essentially all of Aminet
//! ships as `.lha`, and AmigaOS-family systems (AROS included) treat it the
//! way the rest of the world treats zip. It is also common on retro MS-DOS and
//! X68000 collections, so this is a cross-platform format, not an AROS-only
//! one — the codec is identical on every target.
//!
//! Backed by [`delharc`], a pure-Rust decoder covering `-lh0-` (stored)
//! through `-lh7-`, the older `-lz*-` methods, and `-lhd-` directory entries.
//! It is a *decoder only*: there is no LHA writer here, which is why
//! [`Format::Lha`] reports `can_create: false` in the capability matrix.
//!
//! # Streaming shape
//!
//! LHA has no central directory. Each member is a header immediately followed
//! by its compressed data, so listing the contents means walking the whole
//! file, exactly like tar. [`read_toc`] therefore streams headers (skipping
//! payloads) and [`super::read_summary`] must not call it for a column label.
//!
//! # `no_std` delharc
//!
//! The dependency is taken with `default-features = false` so that its `std`
//! feature — which pulls `chrono/clock`, and with it platform time APIs that
//! AROS has no arm for — stays off. The cost is that delharc then speaks its
//! own [`delharc::stub_io::Read`] rather than [`std::io::Read`], so this
//! module carries two small bridges: [`StdReader`] adapts a std reader *into*
//! delharc, and [`Decoded`] adapts delharc's decoded output back *out* to std.

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use delharc::LhaDecodeReader;
use delharc::stub_io::Read as DelharcRead;

use ferail_archive::{ArchiveEntry, Toc};

use super::{ArchiveError, ExtractOptions, ExtractOutcome, Selection, SkipReason};
use crate::file_ops::TransferProgress;

/// Adapts a [`std::io::Read`] into the trait delharc wants when built without
/// its `std` feature. Mirrors delharc's own blanket std impl, including the
/// `Interrupted` retry.
struct StdReader<R>(R);

impl<R: io::Read> DelharcRead for StdReader<R> {
    type Error = io::Error;

    fn unexpected_eof() -> Self::Error {
        io::Error::new(io::ErrorKind::UnexpectedEof, "failed to fill whole buffer")
    }

    fn read_all(&mut self, mut buf: &mut [u8]) -> Result<usize, Self::Error> {
        let orig_len = buf.len();
        while !buf.is_empty() {
            match self.0.read(buf) {
                Ok(0) => break,
                Ok(n) => buf = &mut buf[n..],
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(orig_len - buf.len())
    }
}

/// Adapts the decoded bytes of the *current* member back to [`std::io::Read`],
/// so extraction reuses the shared [`super::write_file`] path (and with it the
/// progress, cancellation and overwrite policy every other format gets).
struct Decoded<'a, R: io::Read>(&'a mut LhaDecodeReader<StdReader<R>>);

impl<R: io::Read> io::Read for Decoded<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0
            .read_all(buf)
            .map_err(|e| io::Error::other(format!("LHA decode failed: {e:?}")))
    }
}

/// Buffered: delharc's bit reader pulls small chunks, so an unbuffered `File`
/// would turn every member into thousands of syscalls.
fn open(archive: &Path) -> Result<LhaDecodeReader<StdReader<io::BufReader<File>>>, ArchiveError> {
    let file = File::open(archive)?;
    LhaDecodeReader::new(StdReader(io::BufReader::new(file)))
        .map_err(|e| ArchiveError::Corrupt(format!("not a readable LHA archive: {e:?}")))
}

/// Normalize a stored path to the `/`-separated form the rest of the archive
/// layer expects. `parse_pathname_to_str` already joins with `/` and handles
/// the Amiga OS-type quirk (NUL-terminated filenames), so this only has to
/// deal with archives that stored DOS separators inside a single component.
fn entry_path(header: &delharc::header::LhaHeader) -> String {
    header.parse_pathname_to_str().replace('\\', "/")
}

fn unix_mode(header: &delharc::header::LhaHeader) -> Option<u32> {
    header.iter_extra().find_map(|extra| match extra {
        [delharc::header::ext::EXT_HEADER_UNIX_PERM, lo, hi, ..] => {
            Some(u16::from_le_bytes([*lo, *hi]) as u32)
        }
        _ => None,
    })
}

fn is_symlink(header: &delharc::header::LhaHeader) -> bool {
    unix_mode(header).is_some_and(|mode| mode & 0o170000 == 0o120000)
}

pub(super) fn read_toc(archive: &Path) -> Result<Toc, ArchiveError> {
    let mut reader = open(archive)?;
    let mut entries = Vec::new();
    loop {
        let header = reader.header();
        let path = entry_path(header);
        if !path.is_empty() {
            entries.push(ArchiveEntry {
                path,
                is_dir: header.is_directory() && !is_symlink(header),
                uncompressed_size: Some(header.original_size),
                compressed_size: Some(header.compressed_size),
                // `last_modified` is already a Unix timestamp for level-2/3
                // headers; delharc converts the MS-DOS stamp of level-0/1
                // headers into the same scale when it parses them.
                mtime_unix: Some(header.last_modified as i64),
                compression_method: header
                    .compression_method()
                    .ok()
                    .map(|method| method.to_string()),
                checksum: Some(format!("CRC16 {:04X}", header.file_crc)),
                unix_mode: unix_mode(header),
                comment: None,
                // LHA has no encryption in any method we decode.
                encrypted: false,
            });
        }
        if !reader
            .next_file()
            .map_err(|e| ArchiveError::Corrupt(format!("{e:?}")))?
        {
            break;
        }
    }
    Ok(Toc {
        entries,
        needs_password: false,
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
    // No central directory, so the totals are unknown up front — same
    // indeterminate progress the tar family uses.
    progress.begin_transfer(0, 0);
    let mut reader = open(archive)?;
    let mut outcome = ExtractOutcome::default();
    loop {
        super::check_cancel(cancel)?;
        let header = reader.header();
        let path = entry_path(header);
        let is_link = is_symlink(header);
        let is_dir = header.is_directory() && !is_link;
        // A method we cannot decode must not silently produce a truncated
        // file: skip the member and tell the user which one.
        let supported = reader.is_decoder_supported();

        if !path.is_empty() && sel.includes(&path) {
            if is_link {
                outcome.skip(path, SkipReason::Symlink);
            } else if is_dir {
                if let Some(safe) = super::safe_or_skip(&path, &mut outcome) {
                    super::make_dir(dest, &safe, &mut outcome)?;
                }
            } else if !supported {
                outcome.skip(path, SkipReason::UnsupportedMethod);
            } else if let Some(safe) = super::safe_or_skip(&path, &mut outcome) {
                super::write_file(
                    dest,
                    &safe,
                    &mut Decoded(&mut reader),
                    opts,
                    progress,
                    cancel,
                    &mut outcome,
                )?;
            }
        }

        if !reader
            .next_file()
            .map_err(|e| ArchiveError::Corrupt(format!("{e:?}")))?
        {
            break;
        }
    }
    Ok(outcome)
}
