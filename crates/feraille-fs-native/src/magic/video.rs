//! Video container parsers: MP4 / MOV / AVI / MKV / WebM. We extract
//! only `has_video` / `has_audio` track presence; codec details and
//! durations would need following box chains into the file body,
//! which we don't do on a 4 KB budget.
//!
//! Ported from bfe-explorer's `sniff_video_info`, `sniff_mp4_info`,
//! `sniff_avi_info`, `sniff_mkv_info`.

use super::types::{MagicInfo, MagicType};

pub(super) fn sniff(buf: &[u8]) -> Option<MagicInfo> {
    if buf.len() >= 12 && &buf[4..8] == b"ftyp" {
        // HEIC was already claimed by image.rs — only return MP4/MOV here.
        let brand = &buf[8..12];
        if brand == b"heic" || brand == b"heix" || brand == b"mif1" || brand == b"msf1" {
            return None;
        }
        return Some(sniff_mp4(buf));
    }
    if buf.len() >= 12 && buf.starts_with(b"RIFF") && &buf[8..12] == b"AVI " {
        return Some(sniff_avi(buf));
    }
    if buf.len() >= 4 && buf.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return Some(sniff_mkv(buf));
    }
    // ASF (WMA/WMV): the 16-byte header-object GUID, stored little-endian.
    if buf.len() >= 16 && buf.starts_with(&ASF_HEADER_GUID) {
        return Some(sniff_asf(buf));
    }
    None
}

/// ASF header-object GUID (`75B22630-668E-11CF-A6D9-00AA0062CE6C`) in the
/// little-endian byte order ASF stores it.
const ASF_HEADER_GUID: [u8; 16] = [
    0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];

/// ASF: distinguish WMA (audio) from WMV (video) by scanning the header for
/// the stream-type GUIDs. Both share the container, so this is how the label
/// is decided; without a match (e.g. the stream header sits past the 4 KB
/// read) it stays a generic "Windows Media".
fn sniff_asf(buf: &[u8]) -> MagicInfo {
    // Stream-type GUIDs, little-endian as stored.
    const AUDIO_GUID: [u8; 16] = [
        0x40, 0x9E, 0x69, 0xF8, 0x4D, 0x5B, 0xCF, 0x11, 0xA8, 0xFD, 0x00, 0x80, 0x5F, 0x5C, 0x44,
        0x2B,
    ];
    const VIDEO_GUID: [u8; 16] = [
        0xC0, 0xEF, 0x19, 0xBC, 0x4D, 0x5B, 0xCF, 0x11, 0xA8, 0xFD, 0x00, 0x80, 0x5F, 0x5C, 0x44,
        0x2B,
    ];
    let mut info = MagicInfo::new(MagicType::Asf);
    info.has_audio = buf.windows(16).any(|w| w == AUDIO_GUID);
    info.has_video = buf.windows(16).any(|w| w == VIDEO_GUID);
    info
}

/// MP4/MOV box structure. Recurses (in a flat loop) through container
/// boxes (`moov`/`trak`/`mdia`/`minf`/`stbl`) looking for `hdlr`
/// entries whose handler_type is `vide` or `soun`.
fn sniff_mp4(buf: &[u8]) -> MagicInfo {
    let brand = &buf[8..12];
    let mt = if brand == b"qt  " {
        MagicType::Mov
    } else {
        MagicType::Mp4
    };
    let mut info = MagicInfo::new(mt);

    let mut pos = 0usize;
    while pos + 8 < buf.len() {
        let box_size = u32::from_be_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]])
            as usize;
        if box_size < 8 || pos + box_size > buf.len() {
            break;
        }
        let box_type = &buf[pos + 4..pos + 8];

        if box_type == b"hdlr" && pos + 20 <= buf.len() {
            // hdlr layout: version+flags(4) + pre_defined(4) +
            // handler_type(4) + ...   handler_type is at +16 from
            // box start (i.e. +8 from box_type).
            let handler = &buf[pos + 16..pos + 20];
            if handler == b"vide" {
                info.has_video = true;
            } else if handler == b"soun" {
                info.has_audio = true;
            }
        }

        if matches!(box_type, b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl") {
            pos += 8;
        } else {
            pos += box_size.max(8);
        }
    }

    // An audio-only MPEG-4 file is an M4A/AAC/ALAC track (Apple Music, iTunes)
    // — label it as audio, not "MP4 video", so the Format column is right and
    // it doesn't trip the mismatch alert against a `.m4a`/`.m4b` extension.
    // The `ftyp` brand is the strong signal; the handler-box scan (`soun` but
    // no `vide`) covers files that declare a generic brand.
    let audio_brand = matches!(brand, b"M4A " | b"M4B " | b"M4P ");
    if audio_brand || (info.has_audio && !info.has_video) {
        info.magic_type = MagicType::M4a;
        info.has_audio = true;
        info.has_video = false;
        return info;
    }

    if !info.has_video && !info.has_audio {
        info.has_video = true;
        info.has_audio = true;
    }
    info
}

fn sniff_avi(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::Avi);
    let mut pos = 12usize;
    while pos + 12 < buf.len() {
        let id = &buf[pos..pos + 4];
        let size =
            u32::from_le_bytes([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]]) as usize;
        if id == b"strh" && pos + 12 <= buf.len() {
            let fcc = &buf[pos + 8..pos + 12];
            if fcc == b"vids" {
                info.has_video = true;
            } else if fcc == b"auds" {
                info.has_audio = true;
            }
        }
        if info.has_video && info.has_audio {
            break;
        }
        if size == 0 {
            break;
        }
        pos = pos.saturating_add(8).saturating_add(size);
    }
    if !info.has_video && !info.has_audio {
        info.has_video = true;
        info.has_audio = true;
    }
    info
}

/// MKV / WebM: distinguish by looking for the literal byte sequence
/// "webm" anywhere in the first 64 bytes. Not 100% accurate but
/// matches bfe-explorer's heuristic.
fn sniff_mkv(buf: &[u8]) -> MagicInfo {
    let mut mt = MagicType::Mkv;
    let scan_end = buf.len().min(64);
    for i in 0..scan_end.saturating_sub(4) {
        if &buf[i..i + 4] == b"webm" {
            mt = MagicType::Webm;
            break;
        }
    }
    let mut info = MagicInfo::new(mt);
    // We don't parse MKV tracks (EBML variable-length encoding is
    // complex). Assume both — accurate for nearly all real files.
    info.has_video = true;
    info.has_audio = true;
    info
}
