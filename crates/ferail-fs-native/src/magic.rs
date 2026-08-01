//! Magic-byte file detection. Reads the first 4 KB of a file and
//! dispatches to per-format parsers that extract structured metadata
//! (bitness, arch, dimensions, channels, sample rate, etc.).
//!
//! Public API:
//!
//! - [`detect_magic`] returns a friendly label string for the Format
//!   column. Same return shape as the pre-Description detector — all
//!   existing callers keep working unchanged.
//! - [`detect_magic_info`] returns the full structured [`MagicInfo`]
//!   from which the Description column is rendered.
//!
//! Reading 4 KB instead of 512 is required to give the per-format
//! parsers enough buffer to find their metadata fields (JPEG SOF
//! markers can sit after EXIF; PE optional headers + CLR data dirs
//! land around 0x200; ZIP local file headers iterate through the
//! buffer for Office macro detection). The cost is negligible — one
//! disk block on most filesystems.
//!
//! Ported from bfe-explorer (`crates/ferail-ui/src/magic/`); see
//! [docs/features/MAGIC_DESCRIPTION.md] for design notes.

use std::path::Path;

mod audio;
mod exe;
mod image;
mod text;
pub mod types;
mod video;
mod zip;

pub use types::{CpuArch, ElfOs, MagicInfo, MagicType, PeSubsystem};

const HEADER_BYTES: usize = 4096;

/// Return a friendly label for the file's content, or `None` if no
/// match. Wraps [`detect_magic_info`] for callers that only want the
/// label string (the Format column, icon classification, file
/// category).
pub fn detect_magic(path: &Path) -> Option<&'static str> {
    let info = detect_magic_info(path)?;
    let label = info.magic_type.display_name();
    if label.is_empty() {
        None
    } else {
        Some(label)
    }
}

/// Return full structured info derived from the file's first ~4 KB,
/// plus — for ZIP-based types — a second 4 KB read at the file tail
/// to walk the central directory and fill `file_count` / `zip_root` /
/// reclassify into Office / JAR / APK as appropriate.
///
/// `None` only for empty or unreadable files. The tail read is
/// skipped silently when it fails (the header-only classification
/// remains).
pub fn detect_magic_info(path: &Path) -> Option<MagicInfo> {
    ferail_core::path_guard::assert_off_ui_thread("detect_magic_info");
    let mut header = [0u8; HEADER_BYTES];
    let n_header = read_header(path, &mut header)?;
    let mut info = sniff_bytes_info(&header[..n_header]);

    if is_zip_family(info.magic_type) {
        let mut tail = [0u8; HEADER_BYTES];
        if let Some((n_tail, file_size)) = read_tail_into(path, &mut tail) {
            zip::refine_with_central_directory(
                &mut info,
                &header[..n_header],
                &tail[..n_tail],
                file_size,
            );
        }
    } else if info.magic_type == MagicType::SevenZip {
        // 7z keeps its file list in a footer we can read without inflating
        // payloads, so the Description gains a count / root / encrypted flag
        // the same way ZIP does. Best-effort — a failure (e.g. a header-
        // encrypted archive with no counts) leaves the bare "7-Zip archive"
        // label, and `read_summary` still reports `encrypted` in that case.
        // Other archive families are intentionally *not* enriched here: gzip
        // /bzip2/xz single members have a trivial count of one, and tar-family
        // counts would require streaming the whole archive (the bounded-read
        // cost guard — see `archive::read_summary`).
        if let Ok(s) = crate::archive::read_summary(path) {
            info.file_count = info.file_count.or(s.file_count);
            info.zip_root = info.zip_root.take().or(s.root);
            info.is_encrypted |= s.encrypted;
        }
    }
    Some(info)
}

fn is_zip_family(mt: MagicType) -> bool {
    matches!(
        mt,
        MagicType::Zip
            | MagicType::ZipEncrypted
            | MagicType::DocWord
            | MagicType::DocWordMacro
            | MagicType::DocExcel
            | MagicType::DocExcelMacro
            | MagicType::DocPowerPoint
            | MagicType::DocPowerPointMacro
            | MagicType::AppJar
            | MagicType::AppApk
    )
}

/// Read the last [`HEADER_BYTES`] of `path` into `buf`. Returns
/// `(bytes_read, total_file_size)` on success. The file_size value is
/// what the central-directory parser needs to map EOCD's `cd_offset`
/// (an absolute file offset) into the tail buffer.
fn read_tail_into(path: &Path, buf: &mut [u8; HEADER_BYTES]) -> Option<(usize, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.seek(SeekFrom::End(0)).ok()?;
    if len == 0 {
        return None;
    }
    let want = (buf.len() as u64).min(len);
    f.seek(SeekFrom::End(-(want as i64))).ok()?;
    let mut total = 0usize;
    while total < want as usize {
        match f.read(&mut buf[total..want as usize]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(_) => return None,
        }
    }
    if total == 0 {
        None
    } else {
        Some((total, len))
    }
}

/// Pure dispatch over an already-read buffer. Useful for tests and
/// for callers that batch their I/O elsewhere.
pub fn sniff_bytes_info(buf: &[u8]) -> MagicInfo {
    if buf.is_empty() {
        return MagicInfo::new(MagicType::Unknown);
    }

    // 1. Executables — full structured parse.
    if let Some(info) = exe::sniff(buf) {
        return info;
    }

    // 2. ZIP-based (Office / JAR / APK / generic).
    if let Some(info) = zip::sniff(buf) {
        return info;
    }

    // 3. Images — extract dimensions + alpha.
    if let Some(info) = image::sniff(buf) {
        return info;
    }

    // 4. Audio — channels / sample rate / duration where cheap.
    if let Some(info) = audio::sniff(buf) {
        return info;
    }

    // 5. Video containers — has_video / has_audio.
    if let Some(info) = video::sniff(buf) {
        return info;
    }

    // 6. Signature-table fast path for remaining binary formats that
    //    don't need structured parsing.
    if let Some(mt) = sniff_signature_table(buf) {
        return MagicInfo::new(mt);
    }

    // 7. Text / script heuristic (shebang, UTF-16, XML/HTML/JSON/INI/...)
    if let Some(info) = text::sniff(buf) {
        return info;
    }

    // 8. Binary fallback.
    let sample = &buf[..buf.len().min(512)];
    let printable = sample
        .iter()
        .filter(|&&b| {
            b.is_ascii_graphic() || b == b' ' || b == b'\n' || b == b'\r' || b == b'\t'
        })
        .count();
    if printable * 100 / sample.len().max(1) < 85 {
        return MagicInfo::new(MagicType::Binary);
    }
    MagicInfo::new(MagicType::Unknown)
}

/// Plain byte-pattern lookup for formats where the dispatcher hasn't
/// already claimed the buffer. Wildcards aren't expressed here — every
/// pattern is a literal byte run at a fixed offset.
fn sniff_signature_table(buf: &[u8]) -> Option<MagicType> {
    const fn b_(c: u8) -> u8 {
        c
    }
    struct Sig {
        magic: MagicType,
        offset: usize,
        bytes: &'static [u8],
    }
    static SIGS: &[Sig] = &[
        Sig {
            magic: MagicType::Pdf,
            offset: 0,
            bytes: b"%PDF-",
        },
        Sig {
            magic: MagicType::Rar,
            offset: 0,
            bytes: b"Rar!\x1a\x07",
        },
        Sig {
            magic: MagicType::SevenZip,
            offset: 0,
            bytes: b"7z\xbc\xaf\x27\x1c",
        },
        Sig {
            magic: MagicType::Gzip,
            offset: 0,
            bytes: &[0x1f, 0x8b],
        },
        Sig {
            magic: MagicType::Xz,
            offset: 0,
            bytes: &[0xfd, b_(b'7'), b_(b'z'), b_(b'X'), b_(b'Z')],
        },
        Sig {
            magic: MagicType::Bzip2,
            offset: 0,
            bytes: b"BZh",
        },
        Sig {
            magic: MagicType::Zstd,
            offset: 0,
            bytes: &[0x28, 0xb5, 0x2f, 0xfd],
        },
        Sig {
            magic: MagicType::Sqlite,
            offset: 0,
            bytes: b"SQLite format 3\0",
        },
        Sig {
            magic: MagicType::Lnk,
            offset: 0,
            bytes: &[
                0x4c, 0x00, 0x00, 0x00, 0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
            ],
        },
        // TAR: "ustar" magic at offset 257
        Sig {
            magic: MagicType::Tar,
            offset: 257,
            bytes: b"ustar",
        },
        // LHA/LZH: the method id sits at offset 2 as `-xxN-`, so there is one
        // signature per method rather than a single pattern. Covers the
        // methods delharc decodes: `-lh0-` (stored) through `-lh7-`, the
        // directory marker `-lhd-`, and the older `-lz*-` family.
        Sig {
            magic: MagicType::Lha,
            offset: 2,
            bytes: b"-lh0-",
        },
        Sig {
            magic: MagicType::Lha,
            offset: 2,
            bytes: b"-lh1-",
        },
        Sig {
            magic: MagicType::Lha,
            offset: 2,
            bytes: b"-lh4-",
        },
        Sig {
            magic: MagicType::Lha,
            offset: 2,
            bytes: b"-lh5-",
        },
        Sig {
            magic: MagicType::Lha,
            offset: 2,
            bytes: b"-lh6-",
        },
        Sig {
            magic: MagicType::Lha,
            offset: 2,
            bytes: b"-lh7-",
        },
        Sig {
            magic: MagicType::Lha,
            offset: 2,
            bytes: b"-lhd-",
        },
        Sig {
            magic: MagicType::Lha,
            offset: 2,
            bytes: b"-lzs-",
        },
        Sig {
            magic: MagicType::Lha,
            offset: 2,
            bytes: b"-lz4-",
        },
        Sig {
            magic: MagicType::Lha,
            offset: 2,
            bytes: b"-lz5-",
        },
        // Fonts
        Sig {
            magic: MagicType::OpenType,
            offset: 0,
            bytes: b"OTTO",
        },
        Sig {
            magic: MagicType::TrueType,
            offset: 0,
            bytes: &[0x00, 0x01, 0x00, 0x00, 0x00],
        },
    ];

    for sig in SIGS {
        if buf.len() >= sig.offset + sig.bytes.len()
            && &buf[sig.offset..sig.offset + sig.bytes.len()] == sig.bytes
        {
            return Some(sig.magic);
        }
    }
    None
}

fn read_header(path: &Path, buf: &mut [u8; HEADER_BYTES]) -> Option<usize> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut total = 0;
    loop {
        match f.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                if total == buf.len() {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    if total == 0 {
        None
    } else {
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buf_is_unknown() {
        let info = sniff_bytes_info(&[]);
        assert_eq!(info.magic_type, MagicType::Unknown);
    }

    #[test]
    fn pdf_signature_detected() {
        let mut buf = vec![0u8; 32];
        buf[..5].copy_from_slice(b"%PDF-");
        let info = sniff_bytes_info(&buf);
        assert_eq!(info.magic_type, MagicType::Pdf);
        assert_eq!(detect_magic_info_label(&info), "PDF document");
    }

    #[test]
    fn png_signature_with_dimensions() {
        // PNG sig + IHDR chunk with 320x200 RGBA (color type 6).
        let mut buf = vec![0u8; 64];
        buf[..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        // chunk length (13) + "IHDR"
        buf[8..12].copy_from_slice(&[0, 0, 0, 13]);
        buf[12..16].copy_from_slice(b"IHDR");
        buf[16..20].copy_from_slice(&320u32.to_be_bytes());
        buf[20..24].copy_from_slice(&200u32.to_be_bytes());
        buf[24] = 8; // bit depth
        buf[25] = 6; // color type RGBA
        let info = sniff_bytes_info(&buf);
        assert_eq!(info.magic_type, MagicType::Png);
        assert_eq!(info.width, Some(320));
        assert_eq!(info.height, Some(200));
        assert!(info.has_alpha);
    }

    #[test]
    fn elf_64bit_x64() {
        let mut buf = vec![0u8; 32];
        buf[..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        buf[4] = 2; // 64-bit
        buf[5] = 1; // LE
        buf[16] = 2; // ET_EXEC (LE)
        buf[18] = 0x3e; // EM_X86_64 (LE)
        let info = sniff_bytes_info(&buf);
        assert_eq!(info.magic_type, MagicType::ExeLinux);
        assert_eq!(info.is_64bit, Some(true));
        assert_eq!(info.arch, CpuArch::X64);
        assert_eq!(info.os, ElfOs::Unknown);
        assert!(!info.is_relocatable);
    }

    #[test]
    fn elf_aros_aarch64_relocatable() {
        // Real e_ident + e_type + e_machine of an AROS `exec.library`
        // (ELF 64-bit LSB relocatable, ARM aarch64, AROS Research OS):
        //   7f 45 4c 46 02 01 01 0f  01 00 00 00 00 00 00 00
        //   01 00 b7 00 ...
        let mut buf = vec![0u8; 24];
        buf[..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        buf[4] = 2; // EI_CLASS = 64-bit
        buf[5] = 1; // EI_DATA = LE
        buf[6] = 1; // EI_VERSION
        buf[7] = 0x0f; // EI_OSABI = ELFOSABI_AROS
        buf[16] = 1; // ET_REL (relocatable, LE)
        buf[18] = 0xb7; // EM_AARCH64 (LE)
        let info = sniff_bytes_info(&buf);
        assert_eq!(info.magic_type, MagicType::ExeLinux);
        assert_eq!(info.is_64bit, Some(true));
        assert_eq!(info.arch, CpuArch::Arm64);
        assert_eq!(info.os, ElfOs::Aros);
        assert!(info.is_relocatable);
        assert_eq!(
            info.description(),
            "ELF \u{b7} 64-bit \u{b7} relocatable \u{b7} ARM64 \u{b7} AROS"
        );
    }

    #[test]
    fn aiff_form_with_comm_chunk() {
        // FORM..AIFF with a COMM chunk: 2 channels, 88200 frames, 16-bit,
        // 44100 Hz. The sample rate is the 80-bit extended float whose
        // canonical bytes for 44100.0 are 40 0E AC 44 00 00 00 00 00 00.
        let mut buf = vec![0u8; 38];
        buf[0..4].copy_from_slice(b"FORM");
        buf[8..12].copy_from_slice(b"AIFF");
        buf[12..16].copy_from_slice(b"COMM");
        buf[16..20].copy_from_slice(&18u32.to_be_bytes()); // chunk size
        buf[20..22].copy_from_slice(&2u16.to_be_bytes()); // channels
        buf[22..26].copy_from_slice(&88_200u32.to_be_bytes()); // sample frames
        buf[26..28].copy_from_slice(&16u16.to_be_bytes()); // sample size
        buf[28..38].copy_from_slice(&[0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]);
        let info = sniff_bytes_info(&buf);
        assert_eq!(info.magic_type, MagicType::Aiff);
        assert_eq!(info.channels, Some(2));
        assert_eq!(info.sample_rate, Some(44_100));
        assert_eq!(info.duration_secs, Some(2));
        assert_eq!(info.description(), "AIFF \u{b7} stereo \u{b7} 44.1 kHz \u{b7} 00:02");
    }

    #[test]
    fn aifc_form_is_also_aiff() {
        // The compressed variant declares form type AIFC.
        let mut buf = vec![0u8; 12];
        buf[0..4].copy_from_slice(b"FORM");
        buf[8..12].copy_from_slice(b"AIFC");
        assert_eq!(sniff_bytes_info(&buf).magic_type, MagicType::Aiff);
    }

    #[test]
    fn m4a_audio_only_mp4_not_video() {
        // An `ftyp` box with the M4A brand: audio, not "MP4 video".
        let mut buf = vec![0u8; 24];
        buf[0..4].copy_from_slice(&16u32.to_be_bytes()); // ftyp box size
        buf[4..8].copy_from_slice(b"ftyp");
        buf[8..12].copy_from_slice(b"M4A "); // brand (trailing space)
        let info = sniff_bytes_info(&buf);
        assert_eq!(info.magic_type, MagicType::M4a);
        assert!(info.has_audio && !info.has_video);
        assert_eq!(detect_magic_info_label(&info), "M4A audio");
    }

    #[test]
    fn wma_asf_audio_detected_not_binary() {
        // ASF header GUID at offset 0, with the audio stream-type GUID later
        // in the header — a WMA. Must be "Windows Media Audio", not "Binary".
        let header = [
            0x30u8, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62,
            0xCE, 0x6C,
        ];
        let audio_guid = [
            0x40u8, 0x9E, 0x69, 0xF8, 0x4D, 0x5B, 0xCF, 0x11, 0xA8, 0xFD, 0x00, 0x80, 0x5F, 0x5C,
            0x44, 0x2B,
        ];
        let mut buf = vec![0u8; 128];
        buf[..16].copy_from_slice(&header);
        buf[64..80].copy_from_slice(&audio_guid);
        let info = sniff_bytes_info(&buf);
        assert_eq!(info.magic_type, MagicType::Asf);
        assert!(info.has_audio && !info.has_video);
        assert_eq!(detect_magic_info_label(&info), "Windows Media");
        assert_eq!(info.description(), "Windows Media Audio");
    }

    #[test]
    fn shebang_python() {
        let info = sniff_bytes_info(b"#!/usr/bin/env python3\nprint('hi')\n");
        assert_eq!(info.magic_type, MagicType::ScriptPython);
    }

    #[test]
    fn json_brace_prefix() {
        let info = sniff_bytes_info(b"{\n  \"key\": \"value\"\n}\n");
        assert_eq!(info.magic_type, MagicType::Json);
    }

    #[test]
    fn random_bytes_classify_as_binary() {
        let bytes: Vec<u8> = (0..512u32).map(|i| (i * 7 + 13) as u8).collect();
        let info = sniff_bytes_info(&bytes);
        assert_eq!(info.magic_type, MagicType::Binary);
    }

    #[test]
    fn detect_magic_returns_legacy_label() {
        let mut buf = vec![0u8; 16];
        buf[..5].copy_from_slice(b"%PDF-");
        let info = sniff_bytes_info(&buf);
        assert_eq!(info.magic_type.display_name(), "PDF document");
    }

    fn detect_magic_info_label(info: &MagicInfo) -> &'static str {
        info.magic_type.display_name()
    }
}
