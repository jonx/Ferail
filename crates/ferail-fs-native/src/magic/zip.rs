//! ZIP-based format detection: generic ZIP, encrypted ZIP, Office
//! Open XML (.docx / .docm / .xlsx / .xlsm / .pptx / .pptm), Java JAR,
//! Android APK.
//!
//! Two-stage pipeline:
//!
//! 1. **[`sniff`] — header-only fast path.** Reads the first local
//!    file header (offset 0). If the very first entry is exactly
//!    `[Content_Types].xml` → Office (OOXML); if it starts with
//!    `META-INF/` → JAR / APK; otherwise → generic ZIP. **No
//!    substring walking** — that produces false positives when a
//!    ZIP entry uses data-descriptor mode (compressed_size = 0 in the
//!    local header) and the embedded payload is itself a ZIP. See
//!    the `DYNA_Datensatz.zip` regression below.
//!
//! 2. **[`refine_with_central_directory`] — authoritative pass.**
//!    The caller reads the last ~4 KB of the file and hands both
//!    buffers here. We find the End-of-Central-Directory record,
//!    walk the CD entries, and:
//!
//!    - Reclassify by examining real entry names (`[Content_Types].xml`,
//!      `xl/workbook.xml`, `word/document.xml`, `ppt/presentation.xml`,
//!      `vbaProject.bin`, `AndroidManifest.xml`, `*.class`).
//!    - Fill `file_count` from the EOCD record.
//!    - Fill `zip_root` when every CD entry sits under a single
//!      top-level directory.
//!
//! Ported from bfe-explorer (`crates/ferail-ui/src/magic/types.rs`):
//! `parse_zip_central_dir`, `analyze_zip_layout`, `extract_root_component`.

use super::types::{MagicInfo, MagicType};

/// Header-only classification. Pessimistic — the only positive
/// classification it makes from the header is the strong
/// `[Content_Types].xml` first-entry signal. Everything else returns
/// generic [`MagicType::Zip`] / [`MagicType::ZipEncrypted`] and lets
/// [`refine_with_central_directory`] correct it.
pub(super) fn sniff(buf: &[u8]) -> Option<MagicInfo> {
    if buf.len() < 30 || !buf.starts_with(b"PK\x03\x04") {
        return None;
    }

    let flags = u16::from_le_bytes([buf[6], buf[7]]);
    let is_encrypted = (flags & 0x0001) != 0;

    let name_len = u16::from_le_bytes([buf[26], buf[27]]) as usize;
    if buf.len() < 30 + name_len {
        return Some(zip_info(is_encrypted));
    }

    let first_name = match std::str::from_utf8(&buf[30..30 + name_len]) {
        Ok(s) => s,
        Err(_) => return Some(zip_info(is_encrypted)),
    };

    // Strong signal: OOXML containers always write `[Content_Types].xml`
    // as the first entry. Any other first-entry name is treated as a
    // plain ZIP for now — `refine_with_central_directory` upgrades it
    // later if the CD says otherwise.
    if first_name == "[Content_Types].xml" {
        let mut info = MagicInfo::new(MagicType::DocWord); // placeholder, CD walk picks the real one
        info.is_encrypted = is_encrypted;
        // We can't tell Word / Excel / PowerPoint from the first entry
        // name alone; mark as generic Zip until the CD walk refines.
        info.magic_type = MagicType::Zip;
        return Some(info);
    }

    Some(zip_info(is_encrypted))
}

fn zip_info(is_encrypted: bool) -> MagicInfo {
    let mut info = MagicInfo::new(if is_encrypted {
        MagicType::ZipEncrypted
    } else {
        MagicType::Zip
    });
    info.is_encrypted = is_encrypted;
    info
}

/// Mutate `info` in place using facts from the authoritative central
/// directory: refines the type (e.g. plain ZIP → Excel when CD has
/// `[Content_Types].xml` + `xl/workbook.xml`), fills file_count and
/// single-root-folder name where applicable.
///
/// `header_buf` is the first 4 KB of the file; `tail_buf` is the last
/// 4 KB (or the whole file if smaller). Either may be empty — the
/// function is best-effort; the caller's `info` is left unchanged if
/// the CD can't be parsed.
pub(super) fn refine_with_central_directory(
    info: &mut MagicInfo,
    header_buf: &[u8],
    tail_buf: &[u8],
    file_size: u64,
) {
    let Some(cd) = parse_central_directory(header_buf, tail_buf, file_size) else {
        return;
    };
    info.file_count = Some(cd.file_count);
    if let Some(root) = cd.root {
        info.zip_root = Some(root);
    }

    // Reclassify from the real entry list. The CD is authoritative —
    // it doesn't suffer from the data-descriptor walking bug because
    // CD records have fixed sizes and explicit `name_len`.
    let names = &cd.entry_names;
    let has_content_types = names.iter().any(|n| n == "[Content_Types].xml");
    let android_manifest = names.iter().any(|n| n == "AndroidManifest.xml");
    let has_class = names.iter().any(|n| n.ends_with(".class"));
    let has_meta_inf_manifest = names.iter().any(|n| n == "META-INF/MANIFEST.MF");
    let has_vba = names.iter().any(|n| {
        n == "word/vbaProject.bin" || n == "xl/vbaProject.bin" || n == "ppt/vbaProject.bin"
    });

    let is_zip_family = matches!(
        info.magic_type,
        MagicType::Zip | MagicType::ZipEncrypted
    );
    if !is_zip_family {
        return;
    }

    if has_content_types {
        let has_word = names.iter().any(|n| n.starts_with("word/"));
        let has_xl = names.iter().any(|n| n.starts_with("xl/"));
        let has_ppt = names.iter().any(|n| n.starts_with("ppt/"));
        if has_word {
            info.magic_type = if has_vba {
                MagicType::DocWordMacro
            } else {
                MagicType::DocWord
            };
            info.has_macros = has_vba;
            return;
        }
        if has_xl {
            info.magic_type = if has_vba {
                MagicType::DocExcelMacro
            } else {
                MagicType::DocExcel
            };
            info.has_macros = has_vba;
            return;
        }
        if has_ppt {
            info.magic_type = if has_vba {
                MagicType::DocPowerPointMacro
            } else {
                MagicType::DocPowerPoint
            };
            info.has_macros = has_vba;
            return;
        }
        // OOXML signature present but no recognized subtype → keep ZIP.
    }

    if android_manifest {
        info.magic_type = MagicType::AppApk;
    } else if has_meta_inf_manifest && has_class {
        info.magic_type = MagicType::AppJar;
    }
}

// ===========================================================================
// Central-directory walker
// ===========================================================================

/// What we extract from one walk of the central directory.
pub(super) struct CentralDirInfo {
    pub file_count: u32,
    /// Single-root-folder name when every CD entry shares one
    /// top-level path component, otherwise `None`.
    pub root: Option<String>,
    /// Up to [`MAX_CD_ENTRIES_TO_SCAN`] entry names (relative paths
    /// inside the archive).
    pub entry_names: Vec<String>,
}

const MAX_CD_ENTRIES_TO_SCAN: usize = 200;

/// Parse the End-of-Central-Directory record and the central directory
/// entries it points at. Returns `None` when the EOCD can't be found
/// in `tail_buf`.
///
/// `file_size` is the absolute size of the file `tail_buf` was read
/// from; passing it lets us map the EOCD's `cd_offset` field (an
/// absolute file offset) into the tail buffer so we walk **only the
/// outer** CD entries. Without this, the walker would also pick up
/// central-directory records from *nested* ZIP payloads (e.g. an
/// outer ZIP containing `.xlsx` files — the inner xlsx's CD sits in
/// the outer ZIP's compressed-data region and shares the same
/// `PK\x01\x02` signature). That was the original bfe-explorer bug.
///
/// Ported from bfe-explorer's `parse_zip_central_dir` + `analyze_zip_layout`
/// with the `cd_offset` guard added.
pub(super) fn parse_central_directory(
    header_buf: &[u8],
    tail_buf: &[u8],
    file_size: u64,
) -> Option<CentralDirInfo> {
    const EOCD_SIG: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    let eocd_pos = find_last_subsequence(tail_buf, &EOCD_SIG)?;
    if tail_buf.len() < eocd_pos + 22 {
        return None;
    }
    let eocd = &tail_buf[eocd_pos..];
    let total_entries = u16::from_le_bytes([eocd[10], eocd[11]]) as u32;
    let cd_offset = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as u64;

    // Map the absolute `cd_offset` into one of our buffers and walk
    // ONLY from there to the EOCD. Anything earlier in the tail
    // buffer is compressed payload (which may itself look like CD
    // records when the payload happens to be another ZIP).
    let tail_start = file_size.saturating_sub(tail_buf.len() as u64);
    let entry_names = if cd_offset >= tail_start {
        let start_in_tail = (cd_offset - tail_start) as usize;
        if start_in_tail < eocd_pos {
            walk_central_directory(&tail_buf[start_in_tail..eocd_pos])
        } else {
            Vec::new()
        }
    } else if (cd_offset as usize) < header_buf.len() {
        // Tiny archive: CD lives in the header buffer.
        walk_central_directory(&header_buf[cd_offset as usize..])
    } else {
        // CD lies between the header buffer and the tail buffer —
        // can't read it without a third I/O. Trust only the EOCD
        // count; layout / classification stay best-effort defaults.
        Vec::new()
    };

    let root = single_root(&entry_names);

    Some(CentralDirInfo {
        file_count: total_entries,
        root,
        entry_names,
    })
}

/// Walk central-directory headers (signature `PK\x01\x02`) and collect
/// entry names. Bounded by [`MAX_CD_ENTRIES_TO_SCAN`].
fn walk_central_directory(buf: &[u8]) -> Vec<String> {
    const CD_SIG: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
    let mut names = Vec::new();
    let mut pos = 0;
    while pos + 46 <= buf.len() && names.len() < MAX_CD_ENTRIES_TO_SCAN {
        let Some(off) = find_subsequence(&buf[pos..], &CD_SIG) else {
            break;
        };
        pos += off;
        if pos + 46 > buf.len() {
            break;
        }
        let entry = &buf[pos..];
        let name_len = u16::from_le_bytes([entry[28], entry[29]]) as usize;
        let extra_len = u16::from_le_bytes([entry[30], entry[31]]) as usize;
        let comment_len = u16::from_le_bytes([entry[32], entry[33]]) as usize;
        if pos + 46 + name_len > buf.len() {
            break;
        }
        if let Ok(name) = std::str::from_utf8(&entry[46..46 + name_len]) {
            names.push(name.to_string());
        }
        pos += 46 + name_len + extra_len + comment_len;
    }
    names
}

/// Return the single root-folder name if every entry shares one
/// top-level path component; otherwise `None`.
fn single_root(entry_names: &[String]) -> Option<String> {
    let mut roots: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for name in entry_names {
        let trimmed = name.trim_start_matches('/');
        if trimmed.is_empty() {
            continue;
        }
        let head = match trimmed.find('/') {
            Some(slash) => &trimmed[..slash],
            None => return None, // a file at root → not single-rooted
        };
        roots.insert(head);
        if roots.len() > 1 {
            return None;
        }
    }
    roots.into_iter().next().map(|s| s.to_string())
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn find_last_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal but valid-enough ZIP for the central-directory
    /// walker: local file headers, central directory headers, EOCD
    /// record. No compressed payload (all stored, sizes = 0).
    fn build_zip(names: &[&str]) -> Vec<u8> {
        let mut local = Vec::new();
        let mut central = Vec::new();
        let mut local_offsets: Vec<u32> = Vec::new();

        for name in names {
            let nb = name.as_bytes();

            // Local file header
            local_offsets.push(local.len() as u32);
            local.extend_from_slice(b"PK\x03\x04");
            local.extend_from_slice(&[0; 22]); // version..crc..sizes (zeros)
            local.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            local.extend_from_slice(&0u16.to_le_bytes()); // extra len
            local.extend_from_slice(nb);
        }

        for (i, name) in names.iter().enumerate() {
            let nb = name.as_bytes();
            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&[0; 24]); // versions..crc..sizes
            central.extend_from_slice(&(nb.len() as u16).to_le_bytes()); // name_len
            central.extend_from_slice(&0u16.to_le_bytes()); // extra_len
            central.extend_from_slice(&0u16.to_le_bytes()); // comment_len
            central.extend_from_slice(&[0; 8]); // disk, attrs
            central.extend_from_slice(&local_offsets[i].to_le_bytes());
            central.extend_from_slice(nb);
        }

        let cd_offset = local.len() as u32;
        let cd_size = central.len() as u32;
        let total = names.len() as u16;

        let mut zip = local;
        zip.extend_from_slice(&central);
        // EOCD
        zip.extend_from_slice(b"PK\x05\x06");
        zip.extend_from_slice(&0u16.to_le_bytes()); // disk
        zip.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
        zip.extend_from_slice(&total.to_le_bytes()); // entries this disk
        zip.extend_from_slice(&total.to_le_bytes()); // entries total
        zip.extend_from_slice(&cd_size.to_le_bytes());
        zip.extend_from_slice(&cd_offset.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes()); // comment len
        zip
    }

    fn classify(buf: &[u8]) -> MagicInfo {
        let mut info = sniff(buf).expect("zip detected");
        // Synthetic archives in these tests are small enough that the
        // whole file is both "header" and "tail"; file_size == buf.len().
        refine_with_central_directory(&mut info, buf, buf, buf.len() as u64);
        info
    }

    #[test]
    fn real_xlsx_detected_via_central_directory() {
        let zip = build_zip(&[
            "[Content_Types].xml",
            "_rels/.rels",
            "xl/workbook.xml",
            "xl/styles.xml",
            "xl/worksheets/sheet1.xml",
        ]);
        let info = classify(&zip);
        assert_eq!(info.magic_type, MagicType::DocExcel);
        assert_eq!(info.file_count, Some(5));
        // No single root: entries split across `xl/` and `_rels/`.
        assert_eq!(info.zip_root, None);
    }

    #[test]
    fn docx_with_macros_detected() {
        let zip = build_zip(&[
            "[Content_Types].xml",
            "_rels/.rels",
            "word/document.xml",
            "word/vbaProject.bin",
        ]);
        let info = classify(&zip);
        assert_eq!(info.magic_type, MagicType::DocWordMacro);
        assert!(info.has_macros);
    }

    #[test]
    fn plain_zip_with_xlsx_inside_is_zip_not_excel() {
        // Outer ZIP whose only entry is `something.xlsx` should be
        // classified as a plain ZIP — the inner xlsx's CD is *not*
        // the outer file's CD. This mirrors the real
        // `DYNA_Datensatz.zip` regression (data-descriptor mode +
        // nested ZIP payload).
        let zip = build_zip(&[
            "DYNA_Datensatz/ERROR_LS_DYNA.xlsx",
            "DYNA_Datensatz/Fragenkatalog.xlsx",
            "DYNA_Datensatz/Manual/notes.txt",
        ]);
        let info = classify(&zip);
        assert_eq!(info.magic_type, MagicType::Zip);
        assert_eq!(info.file_count, Some(3));
        assert_eq!(info.zip_root.as_deref(), Some("DYNA_Datensatz"));
    }

    #[test]
    fn apk_detected_from_android_manifest() {
        let zip = build_zip(&[
            "META-INF/MANIFEST.MF",
            "AndroidManifest.xml",
            "classes.dex",
        ]);
        let info = classify(&zip);
        assert_eq!(info.magic_type, MagicType::AppApk);
    }

    #[test]
    fn jar_detected_from_manifest_plus_class() {
        let zip = build_zip(&[
            "META-INF/MANIFEST.MF",
            "com/example/Main.class",
            "com/example/Util.class",
        ]);
        let info = classify(&zip);
        assert_eq!(info.magic_type, MagicType::AppJar);
    }

    #[test]
    fn header_only_sniff_treats_unknown_first_entry_as_zip() {
        let zip = build_zip(&["foo/bar.txt", "[Content_Types].xml", "xl/sheet.xml"]);
        // Without CD pass, first entry is `foo/bar.txt` → plain Zip
        // (the strong first-entry signal isn't there).
        let info = sniff(&zip).expect("zip detected");
        assert_eq!(info.magic_type, MagicType::Zip);
    }

    #[test]
    fn cd_pass_recovers_ooxml_when_content_types_not_first() {
        // Some producers write `[Content_Types].xml` non-first. The CD
        // walk still finds it.
        let zip = build_zip(&["foo/bar.txt", "[Content_Types].xml", "xl/workbook.xml"]);
        let info = classify(&zip);
        assert_eq!(info.magic_type, MagicType::DocExcel);
    }
}
