//! Magic-byte file detection. Reads the first ~512 bytes of a file
//! and matches against a hand-curated table of patterns.
//!
//! This is a *small* port of the spirit of Ferail's magic module — not
//! the 104 KB type database. iter-3.8 covers ~30 common file types
//! (images, archives, executables, audio, video, code/data) by friendly
//! label. Iter-7 ports the full DB if it earns its keep.
//!
//! The detector is synchronous and pure (no allocation past the read
//! buffer + label clone). Callers should cache by `(path, mtime)`.

use std::path::Path;

const HEADER_BYTES: usize = 512;

/// One entry in the magic table. `offset` is where in the file's first
/// `HEADER_BYTES` to look; `pattern` is the bytes to match (`None` byte
/// = wildcard).
struct Magic {
    offset: usize,
    pattern: &'static [Option<u8>],
    label: &'static str,
}

const fn b(byte: u8) -> Option<u8> {
    Some(byte)
}

const ANY: Option<u8> = None;

/// Magic table. Order matters when patterns overlap — first match wins.
/// Keep this list short and high-confidence; we'd rather say "Unknown"
/// than misclassify.
static TABLE: &[Magic] = &[
    // Images
    Magic { offset: 0, pattern: &[b(0x89), b(b'P'), b(b'N'), b(b'G'), b(0x0D), b(0x0A), b(0x1A), b(0x0A)], label: "PNG image" },
    Magic { offset: 0, pattern: &[b(0xFF), b(0xD8), b(0xFF)], label: "JPEG image" },
    Magic { offset: 0, pattern: &[b(b'G'), b(b'I'), b(b'F'), b(b'8'), ANY, b(b'a')], label: "GIF image" },
    Magic { offset: 0, pattern: &[b(b'B'), b(b'M')], label: "BMP image" },
    Magic { offset: 0, pattern: &[b(b'R'), b(b'I'), b(b'F'), b(b'F'), ANY, ANY, ANY, ANY, b(b'W'), b(b'E'), b(b'B'), b(b'P')], label: "WebP image" },
    Magic { offset: 4, pattern: &[b(b'f'), b(b't'), b(b'y'), b(b'p'), b(b'h'), b(b'e'), b(b'i')], label: "HEIC image" },
    Magic { offset: 0, pattern: &[b(b'<'), b(b'?'), b(b'x'), b(b'm'), b(b'l')], label: "XML / SVG" },
    // Documents
    Magic { offset: 0, pattern: &[b(b'%'), b(b'P'), b(b'D'), b(b'F'), b(b'-')], label: "PDF document" },
    Magic { offset: 0, pattern: &[b(0xD0), b(0xCF), b(0x11), b(0xE0)], label: "MS Office (legacy)" },
    // Archives
    Magic { offset: 0, pattern: &[b(b'P'), b(b'K'), b(0x03), b(0x04)], label: "ZIP archive" },
    Magic { offset: 0, pattern: &[b(b'P'), b(b'K'), b(0x05), b(0x06)], label: "ZIP archive (empty)" },
    Magic { offset: 0, pattern: &[b(0x1F), b(0x8B)], label: "Gzip archive" },
    Magic { offset: 0, pattern: &[b(b'B'), b(b'Z'), b(b'h')], label: "Bzip2 archive" },
    Magic { offset: 0, pattern: &[b(0xFD), b(b'7'), b(b'z'), b(b'X'), b(b'Z')], label: "XZ archive" },
    Magic { offset: 0, pattern: &[b(b'7'), b(b'z'), b(0xBC), b(0xAF), b(0x27), b(0x1C)], label: "7z archive" },
    Magic { offset: 0, pattern: &[b(b'R'), b(b'a'), b(b'r'), b(b'!'), b(0x1A), b(0x07)], label: "RAR archive" },
    Magic { offset: 257, pattern: &[b(b'u'), b(b's'), b(b't'), b(b'a'), b(b'r')], label: "TAR archive" },
    Magic { offset: 0, pattern: &[b(0x28), b(0xB5), b(0x2F), b(0xFD)], label: "Zstandard archive" },
    // Executables
    Magic { offset: 0, pattern: &[b(0x7F), b(b'E'), b(b'L'), b(b'F')], label: "ELF executable" },
    Magic { offset: 0, pattern: &[b(0xCF), b(0xFA), b(0xED), b(0xFE)], label: "Mach-O 64-bit" },
    Magic { offset: 0, pattern: &[b(0xCE), b(0xFA), b(0xED), b(0xFE)], label: "Mach-O 32-bit" },
    Magic { offset: 0, pattern: &[b(0xCA), b(0xFE), b(0xBA), b(0xBE)], label: "Mach-O fat / Java class" },
    Magic { offset: 0, pattern: &[b(b'M'), b(b'Z')], label: "PE / DOS executable" },
    Magic { offset: 0, pattern: &[b(b'#'), b(b'!'), b(b'/')], label: "Shell script" },
    // Audio
    Magic { offset: 0, pattern: &[b(b'I'), b(b'D'), b(b'3')], label: "MP3 audio (ID3)" },
    Magic { offset: 0, pattern: &[b(0xFF), b(0xFB)], label: "MP3 audio" },
    Magic { offset: 0, pattern: &[b(b'O'), b(b'g'), b(b'g'), b(b'S')], label: "Ogg audio" },
    Magic { offset: 0, pattern: &[b(b'f'), b(b'L'), b(b'a'), b(b'C')], label: "FLAC audio" },
    Magic { offset: 0, pattern: &[b(b'R'), b(b'I'), b(b'F'), b(b'F'), ANY, ANY, ANY, ANY, b(b'W'), b(b'A'), b(b'V'), b(b'E')], label: "WAV audio" },
    // Video
    Magic { offset: 4, pattern: &[b(b'f'), b(b't'), b(b'y'), b(b'p'), b(b'M'), b(b'P'), b(b'4')], label: "MP4 video" },
    Magic { offset: 4, pattern: &[b(b'f'), b(b't'), b(b'y'), b(b'p'), b(b'i'), b(b's'), b(b'o')], label: "MP4 video (ISO)" },
    Magic { offset: 4, pattern: &[b(b'f'), b(b't'), b(b'y'), b(b'p'), b(b'q'), b(b't'), b(b' '), b(b' ')], label: "QuickTime movie" },
    Magic { offset: 0, pattern: &[b(0x1A), b(0x45), b(0xDF), b(0xA3)], label: "Matroska / WebM" },
    // Fonts
    Magic { offset: 0, pattern: &[b(b'O'), b(b'T'), b(b'T'), b(b'O')], label: "OpenType font" },
    Magic { offset: 0, pattern: &[b(0x00), b(0x01), b(0x00), b(0x00), b(0x00)], label: "TrueType font" },
    // Databases
    Magic { offset: 0, pattern: &[b(b'S'), b(b'Q'), b(b'L'), b(b'i'), b(b't'), b(b'e'), b(b' '), b(b'f')], label: "SQLite database" },
    // Plain text BOMs
    Magic { offset: 0, pattern: &[b(0xEF), b(0xBB), b(0xBF)], label: "UTF-8 text" },
    Magic { offset: 0, pattern: &[b(0xFF), b(0xFE)], label: "UTF-16 LE text" },
    Magic { offset: 0, pattern: &[b(0xFE), b(0xFF)], label: "UTF-16 BE text" },
];

/// Return the friendly label for a file's magic, or `None` if no entry
/// matched. Reads the first 512 bytes (or the full file if smaller).
/// Empty / unreadable files return `None`.
pub fn detect_magic(path: &Path) -> Option<&'static str> {
    let mut header = [0u8; HEADER_BYTES];
    let bytes_read = read_header(path, &mut header)?;
    let header = &header[..bytes_read];
    for entry in TABLE {
        if entry.offset + entry.pattern.len() > header.len() {
            continue;
        }
        let slice = &header[entry.offset..entry.offset + entry.pattern.len()];
        if slice
            .iter()
            .zip(entry.pattern.iter())
            .all(|(b, p)| matches!(p, None) || matches!(p, Some(v) if v == b))
        {
            return Some(entry.label);
        }
    }
    // Heuristic fallback: if the header is mostly printable ASCII / UTF-8,
    // call it text. ~95% of real "this file has no magic" cases.
    if looks_textual(header) {
        return Some("Plain text");
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

/// Treats the buffer as text iff it parses as valid UTF-8 (allowing the
/// last char to be cut at the buffer boundary) AND has very few control
/// characters. Catches plain text + Markdown + JSON + YAML + source code
/// across most real languages; rejects pseudorandom binary cleanly.
fn looks_textual(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let valid_end = match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(e) => e.valid_up_to(),
    };
    if valid_end < bytes.len().saturating_sub(4) || valid_end < 8 {
        return false;
    }
    let valid = &bytes[..valid_end];
    let mut control = 0_usize;
    for &b in valid {
        match b {
            0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F => control += 1,
            _ => {}
        }
    }
    control * 20 < valid.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_returns_none() {
        let mut buf = [0u8; HEADER_BYTES];
        // looks_textual on empty -> false; magic detection fails first on
        // read_header returning None for empty files via fs.
        assert!(!looks_textual(&buf[..0]));
    }

    #[test]
    fn ascii_text_detects_as_text() {
        let s = b"hello world\nthis is a plain text file with several lines\n";
        assert!(looks_textual(s));
    }

    #[test]
    fn random_bytes_dont_detect_as_text() {
        let bytes: Vec<u8> = (0..512u32).map(|i| (i * 7 + 13) as u8).collect();
        let textual = looks_textual(&bytes);
        // Mostly low / control bytes — should NOT look textual.
        assert!(!textual);
    }
}
