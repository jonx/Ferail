//! Image format parsers — extract width, height, and (where cheap)
//! alpha-channel presence.
//!
//! All dispatched formats keep their dimension fields in the first
//! few hundred bytes; everything here fits inside the 4 KB read budget.
//!
//! Ported from bfe-explorer's `sniff_image_info`, `sniff_png_info`,
//! `sniff_jpeg_info`, `sniff_gif_info`, `sniff_bmp_info`,
//! `sniff_webp_info`, `sniff_ico_info`, `sniff_tiff_info`.

use super::types::{MagicInfo, MagicType};

pub(super) fn sniff(buf: &[u8]) -> Option<MagicInfo> {
    if buf.len() >= 24 && buf.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some(sniff_png(buf));
    }
    if buf.len() >= 12 && buf.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(sniff_jpeg(buf));
    }
    if buf.len() >= 10 && buf.starts_with(b"GIF8") {
        return Some(sniff_gif(buf));
    }
    if buf.len() >= 26 && buf.starts_with(b"BM") {
        return Some(sniff_bmp(buf));
    }
    if buf.len() >= 30 && buf.starts_with(b"RIFF") && &buf[8..12] == b"WEBP" {
        return Some(sniff_webp(buf));
    }
    if buf.len() >= 22 && buf.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        return Some(sniff_ico(buf));
    }
    if buf.len() >= 8
        && (buf.starts_with(&[0x49, 0x49, 0x2a, 0x00])
            || buf.starts_with(&[0x4d, 0x4d, 0x00, 0x2a]))
    {
        return Some(sniff_tiff(buf));
    }
    // HEIC: ftyp box with brand "heic" (or "heix" / "mif1" with HEIC inside)
    if buf.len() >= 12 && &buf[4..8] == b"ftyp" {
        let brand = &buf[8..12];
        if brand == b"heic" || brand == b"heix" || brand == b"mif1" || brand == b"msf1" {
            return Some(MagicInfo::new(MagicType::Heic));
        }
    }
    None
}

/// PNG IHDR chunk lives immediately after the 8-byte signature:
/// length(4) + "IHDR"(4) + width(4 BE) + height(4 BE) + depth(1) + color_type(1) + ...
fn sniff_png(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::Png);
    if buf.len() < 26 || &buf[12..16] != b"IHDR" {
        return info;
    }
    let off = 16;
    info.width = Some(u32::from_be_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
    ]));
    info.height = Some(u32::from_be_bytes([
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ]));
    let color_type = buf[off + 9];
    // 4 = gray + alpha, 6 = RGBA
    info.has_alpha = color_type == 4 || color_type == 6;
    info
}

/// JPEG SOF (Start Of Frame) markers carry dimensions. Walk segments
/// from byte 2 forward, jumping by segment length until we hit one
/// of SOF0..=SOFn.
fn sniff_jpeg(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::Jpeg);
    let mut pos = 2;
    while pos + 10 < buf.len() {
        if buf[pos] != 0xff {
            pos += 1;
            continue;
        }
        let marker = buf[pos + 1];
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            // marker(2) + length(2) + precision(1) + height(2) + width(2)
            info.height = Some(u16::from_be_bytes([buf[pos + 5], buf[pos + 6]]) as u32);
            info.width = Some(u16::from_be_bytes([buf[pos + 7], buf[pos + 8]]) as u32);
            return info;
        }
        // SOI / EOI / RST markers — no length field.
        if marker == 0xd8 || marker == 0xd9 || (0xd0..=0xd7).contains(&marker) {
            pos += 2;
        } else if pos + 3 < buf.len() {
            let len = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) as usize;
            pos += 2 + len.max(2);
        } else {
            break;
        }
    }
    info
}

fn sniff_gif(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::Gif);
    info.width = Some(u16::from_le_bytes([buf[6], buf[7]]) as u32);
    info.height = Some(u16::from_le_bytes([buf[8], buf[9]]) as u32);
    info
}

/// BMP: BITMAPFILEHEADER(14) + BITMAPINFOHEADER. biWidth at +18,
/// biHeight at +22 (signed — negative means top-down DIB).
fn sniff_bmp(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::Bmp);
    let width = i32::from_le_bytes([buf[18], buf[19], buf[20], buf[21]]);
    let height = i32::from_le_bytes([buf[22], buf[23], buf[24], buf[25]]);
    info.width = Some(width.unsigned_abs());
    info.height = Some(height.unsigned_abs());
    if buf.len() >= 30 {
        let bit_count = u16::from_le_bytes([buf[28], buf[29]]);
        info.has_alpha = bit_count == 32;
    }
    info
}

/// WebP comes in three flavors — VP8, VP8L, VP8X — each with its
/// own dimension encoding.
fn sniff_webp(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::Webp);
    let chunk_type = &buf[12..16];

    if chunk_type == b"VP8 " {
        if buf.len() >= 30 && buf[23] == 0x9d && buf[24] == 0x01 && buf[25] == 0x2a {
            info.width = Some((u16::from_le_bytes([buf[26], buf[27]]) & 0x3fff) as u32);
            info.height = Some((u16::from_le_bytes([buf[28], buf[29]]) & 0x3fff) as u32);
        }
    } else if chunk_type == b"VP8L" && buf.len() >= 25 && buf[20] == 0x2f {
        let bits = u32::from_le_bytes([buf[21], buf[22], buf[23], buf[24]]);
        info.width = Some((bits & 0x3fff) + 1);
        info.height = Some(((bits >> 14) & 0x3fff) + 1);
        info.has_alpha = (bits >> 28) & 1 != 0;
    } else if chunk_type == b"VP8X" && buf.len() >= 30 {
        let flags = buf[20];
        info.has_alpha = (flags & 0x10) != 0;
        info.width =
            Some(((buf[24] as u32) | ((buf[25] as u32) << 8) | ((buf[26] as u32) << 16)) + 1);
        info.height =
            Some(((buf[27] as u32) | ((buf[28] as u32) << 8) | ((buf[29] as u32) << 16)) + 1);
    }
    info
}

/// ICO: u16 reserved + u16 type + u16 count + count×16-byte entries.
/// We scan up to 10 entries and pick the largest as the "primary".
fn sniff_ico(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::Ico);
    let count = u16::from_le_bytes([buf[4], buf[5]]) as usize;
    if count == 0 || buf.len() < 6 + count * 16 {
        return info;
    }
    let mut max_size = 0u64;
    for i in 0..count.min(10) {
        let entry = 6 + i * 16;
        // Width / height field of 0 means 256 in the spec.
        let w = if buf[entry] == 0 {
            256
        } else {
            buf[entry] as u32
        };
        let h = if buf[entry + 1] == 0 {
            256
        } else {
            buf[entry + 1] as u32
        };
        let size = w as u64 * h as u64;
        if size > max_size {
            max_size = size;
            info.width = Some(w);
            info.height = Some(h);
        }
    }
    info.has_alpha = true;
    info
}

/// TIFF: II*\0 (LE) or MM\0* (BE) + u32 IFD offset.
/// Each IFD entry is 12 bytes: tag(2) + type(2) + count(4) + value/offset(4).
fn sniff_tiff(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::Tiff);
    let is_le = buf[0] == 0x49;

    let read_u16 = |off: usize, buf: &[u8]| -> u16 {
        if is_le {
            u16::from_le_bytes([buf[off], buf[off + 1]])
        } else {
            u16::from_be_bytes([buf[off], buf[off + 1]])
        }
    };
    let read_u32 = |off: usize, buf: &[u8]| -> u32 {
        if is_le {
            u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
        } else {
            u32::from_be_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
        }
    };

    let ifd_offset = read_u32(4, buf) as usize;
    if ifd_offset + 2 > buf.len() {
        return info;
    }
    let num_entries = read_u16(ifd_offset, buf) as usize;
    let entries_start = ifd_offset + 2;

    for i in 0..num_entries.min(40) {
        let entry = entries_start + i * 12;
        if entry + 12 > buf.len() {
            break;
        }
        let tag = read_u16(entry, buf);
        let typ = read_u16(entry + 2, buf);
        let val_off = entry + 8;
        match tag {
            256 => {
                info.width = Some(if typ == 3 {
                    read_u16(val_off, buf) as u32
                } else {
                    read_u32(val_off, buf)
                });
            }
            257 => {
                info.height = Some(if typ == 3 {
                    read_u16(val_off, buf) as u32
                } else {
                    read_u32(val_off, buf)
                });
            }
            338 => {
                info.has_alpha = true;
            }
            _ => {}
        }
    }
    info
}
