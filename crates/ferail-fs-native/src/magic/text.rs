//! Text and script heuristic. Runs after all binary-format dispatchers
//! return None: detects shebangs and their interpreter, UTF-16 BOMs,
//! XML / HTML / .reg / .url / INI / JSON via lightweight content
//! sniffing, and finally falls back to plain text on a printable-ratio
//! threshold.
//!
//! Ported from bfe-explorer's `sniff_text_heuristic`, `sniff_shebang`,
//! `looks_like_ini`, `looks_like_url_shortcut`.

use super::types::{MagicInfo, MagicType};

pub(super) fn sniff(buf: &[u8]) -> Option<MagicInfo> {
    if buf.is_empty() {
        return None;
    }

    // Shebang → script family
    if buf.len() >= 2 && buf[0] == b'#' && buf[1] == b'!' {
        let (mt, interp) = sniff_shebang(buf);
        let mut info = MagicInfo::new(mt);
        info.interpreter = interp;
        return Some(info);
    }

    // UTF-16 BOMs
    if buf.len() >= 2 && buf[0] == 0xff && buf[1] == 0xfe {
        // Could be UTF-16 LE plain text. Treat as plain text in v1;
        // the prior single-file detector returned "UTF-16 LE text"
        // for both — we keep that label.
        return Some(MagicInfo::new(MagicType::Utf16Le));
    }
    if buf.len() >= 2 && buf[0] == 0xfe && buf[1] == 0xff {
        return Some(MagicInfo::new(MagicType::Utf16Be));
    }

    // UTF-8 BOM (EF BB BF) is a strong hint of text — leave dispatch
    // to printable-ratio check below, but tag it.
    let has_utf8_bom = buf.len() >= 3 && buf[0] == 0xef && buf[1] == 0xbb && buf[2] == 0xbf;

    let sample = &buf[..buf.len().min(512)];
    let printable = sample
        .iter()
        .filter(|&&b| b.is_ascii_graphic() || b == b' ' || b == b'\n' || b == b'\r' || b == b'\t')
        .count();
    if printable * 100 / sample.len().max(1) < 85 {
        return None;
    }

    // Inspect the actual text (lossy decode).
    let text = String::from_utf8_lossy(sample);
    let trimmed = text.trim_start();

    if trimmed.starts_with("<?xml") {
        // SVG is XML; the existing flat-table label was "XML / SVG".
        // We can't cheaply distinguish — if the rest of the buffer
        // mentions "<svg" inside the prologue, prefer SVG.
        if text.contains("<svg") || text.contains("svg") {
            return Some(MagicInfo::new(MagicType::Svg));
        }
        return Some(MagicInfo::new(MagicType::Xml));
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
        return Some(MagicInfo::new(MagicType::Html));
    }
    if trimmed.starts_with("Windows Registry Editor") {
        return Some(MagicInfo::new(MagicType::TextReg));
    }
    if looks_like_url_shortcut(&text) {
        return Some(MagicInfo::new(MagicType::Url));
    }
    if looks_like_ini(&text) {
        return Some(MagicInfo::new(MagicType::TextIni));
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Some(MagicInfo::new(MagicType::Json));
    }
    if has_utf8_bom {
        return Some(MagicInfo::new(MagicType::Utf8Bom));
    }
    Some(MagicInfo::new(MagicType::TextPlain))
}

fn sniff_shebang(buf: &[u8]) -> (MagicType, Option<&'static str>) {
    let end = buf
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(buf.len().min(128));
    let line = String::from_utf8_lossy(&buf[2..end]).to_ascii_lowercase();
    if line.contains("bash") || line.contains("/sh") || line.contains(" sh") {
        (MagicType::ScriptBash, Some("bash"))
    } else if line.contains("python") {
        (MagicType::ScriptPython, Some("python"))
    } else if line.contains("perl") {
        (MagicType::ScriptPerl, Some("perl"))
    } else if line.contains("ruby") {
        (MagicType::ScriptRuby, Some("ruby"))
    } else if line.contains("node") {
        (MagicType::ScriptNode, Some("node"))
    } else {
        (MagicType::ScriptOther, None)
    }
}

/// INI: at least one `[section]` header and at least two `key=value`
/// lines among the first ~30 lines.
fn looks_like_ini(text: &str) -> bool {
    let mut has_section = false;
    let mut kv = 0;
    for line in text.lines().take(30) {
        let t = line.trim();
        if t.starts_with(';') || t.starts_with('#') {
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') && t.len() > 2 {
            has_section = true;
            continue;
        }
        if t.contains('=') && !t.starts_with('=') {
            kv += 1;
        }
    }
    has_section && kv >= 2
}

/// Windows .url file: starts with `[InternetShortcut]` section.
fn looks_like_url_shortcut(text: &str) -> bool {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().eq_ignore_ascii_case("[InternetShortcut]"))
        .unwrap_or(false)
}
