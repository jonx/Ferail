//! OLE2 / CFBF (Compound File Binary Format) detection — the container
//! behind legacy Office documents (.doc / .xls / .ppt), MSI installers,
//! and **password-protected OOXML**: an encrypted .docx/.xlsx/.pptx is
//! not a ZIP at all but a CFBF file wrapping an `EncryptedPackage`
//! stream, which is why such files used to fall through every sniffer
//! into the "Binary" bucket.
//!
//! Two-stage pipeline, mirroring `magic::zip`:
//!
//! 1. **[`sniff`] — header-only.** The 8-byte CFBF magic at offset 0
//!    classifies the container as [`MagicType::OleCompound`]; the app
//!    can't be told from the header alone.
//!
//! 2. **[`refine_with_directory`] — one targeted read.** The header
//!    names the first directory sector; its entries name the main
//!    stream (`WordDocument`, `Workbook`, `PowerPoint Document`,
//!    `EncryptedPackage`), which pins the application. Best-effort:
//!    only the first directory sector is examined (the main stream is
//!    all but always among its 4–32 entries), and any parse failure
//!    leaves the generic compound-document classification.

use super::types::{MagicInfo, MagicType};

const CFBF_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Header-only classification: CFBF magic → generic compound document.
pub(super) fn sniff(buf: &[u8]) -> Option<MagicInfo> {
    // A valid CFBF header is exactly 512 bytes; shorter files are
    // truncated garbage we leave to the generic binary fallback.
    if buf.len() < 512 || !buf.starts_with(&CFBF_MAGIC) {
        return None;
    }
    Some(MagicInfo::new(MagicType::OleCompound))
}

/// Refine [`MagicType::OleCompound`] by naming the application from the
/// first directory sector. `read_at(absolute_offset, len)` performs the
/// one targeted read when that sector sits outside `header_buf`.
pub(super) fn refine_with_directory(
    info: &mut MagicInfo,
    header_buf: &[u8],
    read_at: &mut dyn FnMut(u64, usize) -> Option<Vec<u8>>,
) {
    if info.magic_type != MagicType::OleCompound || header_buf.len() < 512 {
        return;
    }
    // Sector size: 512 (v3, shift 9) or 4096 (v4, shift 12). Sector n
    // starts at byte (n + 1) << shift — the header occupies "sector -1".
    let sector_shift = u16::from_le_bytes([header_buf[30], header_buf[31]]);
    if !(7..=15).contains(&sector_shift) {
        return;
    }
    let sector_size = 1usize << sector_shift;
    let first_dir_sector =
        u32::from_le_bytes([header_buf[48], header_buf[49], header_buf[50], header_buf[51]]);
    if first_dir_sector >= 0xFFFF_FFFA {
        // ENDOFCHAIN / FREESECT / FAT markers — no directory to read.
        return;
    }
    let dir_offset = (first_dir_sector as u64 + 1) << sector_shift;

    let owned;
    let sector: &[u8] = if dir_offset as usize + sector_size <= header_buf.len() {
        &header_buf[dir_offset as usize..dir_offset as usize + sector_size]
    } else {
        match read_at(dir_offset, sector_size) {
            Some(buf) => {
                owned = buf;
                &owned
            }
            None => return,
        }
    };

    let mut app: Option<MagicType> = None;
    let mut has_macros = false;
    for entry in sector.chunks_exact(128) {
        // Entry name: UTF-16LE, length in bytes *including* the NUL.
        let name_len = u16::from_le_bytes([entry[64], entry[65]]) as usize;
        if !(2..=64).contains(&name_len) {
            continue;
        }
        let name: String = entry[..name_len - 2]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]) as u32)
            .map(|u| char::from_u32(u).unwrap_or('\u{FFFD}'))
            .collect();
        match name.as_str() {
            "WordDocument" => app = app.or(Some(MagicType::DocWordOle)),
            "Workbook" | "Book" => app = app.or(Some(MagicType::DocExcelOle)),
            "PowerPoint Document" => app = app.or(Some(MagicType::DocPowerPointOle)),
            // Password-protected OOXML: the real document is the
            // encrypted ZIP inside this stream. Which app made it is
            // unknowable without decrypting, so the type stays generic.
            "EncryptedPackage" => info.is_encrypted = true,
            // VBA storages across the legacy formats.
            "Macros" | "VBA" | "_VBA_PROJECT_CUR" | "_VBA_PROJECT" => has_macros = true,
            _ => {}
        }
    }
    if let Some(app) = app {
        info.magic_type = app;
    }
    info.has_macros = has_macros;
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal CFBF: 512-byte header (shift 9, directory at sector
    /// `dir_sector`) followed by `sector_count` 512-byte sectors, with
    /// the given stream names as directory entries in the dir sector.
    fn build_cfbf(dir_sector: u32, sector_count: usize, names: &[&str]) -> Vec<u8> {
        let mut file = vec![0u8; 512 + sector_count * 512];
        file[..8].copy_from_slice(&CFBF_MAGIC);
        file[30..32].copy_from_slice(&9u16.to_le_bytes());
        file[48..52].copy_from_slice(&dir_sector.to_le_bytes());

        let dir_off = 512 + dir_sector as usize * 512;
        for (i, name) in names.iter().enumerate() {
            let entry = &mut file[dir_off + i * 128..dir_off + (i + 1) * 128];
            let utf16: Vec<u8> = name
                .encode_utf16()
                .flat_map(|u| u.to_le_bytes())
                .collect();
            entry[..utf16.len()].copy_from_slice(&utf16);
            entry[64..66].copy_from_slice(&((utf16.len() as u16 + 2).to_le_bytes()));
        }
        file
    }

    fn classify(file: &[u8], header_window: usize) -> MagicInfo {
        let header = &file[..file.len().min(header_window)];
        let mut info = sniff(header).expect("cfbf detected");
        refine_with_directory(&mut info, header, &mut |off, len| {
            let start = off as usize;
            let end = (start + len).min(file.len());
            (start < end).then(|| file[start..end].to_vec())
        });
        info
    }

    #[test]
    fn legacy_powerpoint_detected() {
        let file = build_cfbf(0, 1, &["Root Entry", "Current User", "PowerPoint Document"]);
        let info = classify(&file, 4096);
        assert_eq!(info.magic_type, MagicType::DocPowerPointOle);
        assert!(!info.is_encrypted);
    }

    #[test]
    fn legacy_word_with_macros_detected() {
        let file = build_cfbf(0, 1, &["Root Entry", "WordDocument", "Macros"]);
        let info = classify(&file, 4096);
        assert_eq!(info.magic_type, MagicType::DocWordOle);
        assert!(info.has_macros);
    }

    #[test]
    fn encrypted_ooxml_detected_as_encrypted_office() {
        let file = build_cfbf(
            0,
            1,
            &["Root Entry", "EncryptionInfo", "EncryptedPackage"],
        );
        let info = classify(&file, 4096);
        assert_eq!(info.magic_type, MagicType::OleCompound);
        assert!(info.is_encrypted);
    }

    #[test]
    fn directory_outside_header_window_read_via_read_at() {
        // Directory at sector 20 → byte offset 10752, past the 4 KB
        // header window, so classification needs the targeted read.
        let file = build_cfbf(20, 24, &["Root Entry", "Workbook"]);
        let info = classify(&file, 4096);
        assert_eq!(info.magic_type, MagicType::DocExcelOle);

        // Without the read the container classification remains.
        let header = &file[..4096];
        let mut degraded = sniff(header).expect("cfbf detected");
        refine_with_directory(&mut degraded, header, &mut |_, _| None);
        assert_eq!(degraded.magic_type, MagicType::OleCompound);
    }

    #[test]
    fn non_cfbf_rejected() {
        assert!(sniff(b"PK\x03\x04").is_none());
        assert!(sniff(&[0u8; 600]).is_none());
        let mut short = vec![0u8; 100];
        short[..8].copy_from_slice(&CFBF_MAGIC);
        assert!(sniff(&short).is_none());
    }
}
