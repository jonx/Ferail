//! Filename hazard analysis: surface the characters malware and phishing use
//! to disguise a file's true name — leading/trailing or unusual whitespace,
//! zero-width and control characters, bidirectional overrides (the classic
//! "exe→txt" trick), Unicode homoglyphs (Cyrillic/Greek/fullwidth letters
//! that mimic ASCII), and stray combining marks.
//!
//! [`analyze`] splits a name into [`NameSegment`]s: runs of ordinary text
//! interleaved with single flagged characters. Each flagged segment carries a
//! human label (for a tooltip) and a visible substitute (`render`) so an
//! otherwise-invisible character can actually be shown. This crate is data
//! only — the UI decides how to color and lay the segments out.

/// What makes a character suspicious in a filename.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HazardKind {
    /// Whitespace before the first visible character.
    LeadingSpace,
    /// Whitespace after the last visible character.
    TrailingSpace,
    /// Internal whitespace that isn't a plain ASCII space (tab, NBSP, the
    /// Unicode space zoo) — visually a space but not one.
    UnusualWhitespace,
    /// Zero-width space / joiner / non-joiner / BOM / word-joiner.
    ZeroWidth,
    /// C0/C1 control character or DEL.
    Control,
    /// Bidirectional control (RLO/LRO/RLI/PDF/…) — reorders the visible text.
    Bidi,
    /// A non-ASCII letter that mimics an ASCII/Latin one (homoglyph).
    Homoglyph,
    /// A combining diacritical mark riding on the previous character.
    CombiningMark,
}

impl HazardKind {
    /// Short human description for the segment's tooltip prefix.
    pub fn summary(self) -> &'static str {
        match self {
            HazardKind::LeadingSpace => "Leading whitespace",
            HazardKind::TrailingSpace => "Trailing whitespace",
            HazardKind::UnusualWhitespace => "Unusual whitespace",
            HazardKind::ZeroWidth => "Zero-width character",
            HazardKind::Control => "Control character",
            HazardKind::Bidi => "Bidirectional override",
            HazardKind::Homoglyph => "Look-alike character",
            HazardKind::CombiningMark => "Combining mark",
        }
    }
}

/// One run of the filename. `hazard` is `None` for ordinary text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NameSegment {
    /// The literal characters this segment covers.
    pub text: String,
    pub hazard: Option<HazardKind>,
    /// Tooltip detail, e.g. `Cyrillic 'а' (U+0430) — looks like Latin 'a'`.
    pub label: Option<String>,
    /// A visible stand-in for an otherwise invisible/ambiguous character,
    /// e.g. `␣`, `⇥`, `⟨ZWSP⟩`. `None` means render `text` as-is.
    pub render: Option<String>,
}

impl NameSegment {
    fn plain(text: String) -> Self {
        Self {
            text,
            hazard: None,
            label: None,
            render: None,
        }
    }
}

/// True if `name` contains any flagged character.
pub fn has_hazards(name: &str) -> bool {
    analyze(name).iter().any(|s| s.hazard.is_some())
}

/// Split `name` into plain runs and individually-flagged hazard characters.
pub fn analyze(name: &str) -> Vec<NameSegment> {
    let chars: Vec<char> = name.chars().collect();
    // Boundaries of leading / trailing whitespace runs.
    let lead_end = chars.iter().position(|c| !c.is_whitespace()).unwrap_or(chars.len());
    let trail_start = chars
        .iter()
        .rposition(|c| !c.is_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);

    let mut out: Vec<NameSegment> = Vec::new();
    let mut plain = String::new();
    let flush = |plain: &mut String, out: &mut Vec<NameSegment>| {
        if !plain.is_empty() {
            out.push(NameSegment::plain(std::mem::take(plain)));
        }
    };

    for (i, &c) in chars.iter().enumerate() {
        let in_lead = i < lead_end;
        let in_trail = i >= trail_start && trail_start < chars.len();
        match classify(c, in_lead, in_trail) {
            Some(seg) => {
                flush(&mut plain, &mut out);
                out.push(seg);
            }
            None => plain.push(c),
        }
    }
    flush(&mut plain, &mut out);
    out
}

/// Classify a single character. `in_lead`/`in_trail` mark the leading and
/// trailing whitespace zones so a normal interior space stays plain.
fn classify(c: char, in_lead: bool, in_trail: bool) -> Option<NameSegment> {
    let cp = c as u32;

    // Bidirectional controls — the headline trick (reverses gpj.exe ⇒ exe.jpg).
    if matches!(cp, 0x202A..=0x202E | 0x2066..=0x2069 | 0x200E | 0x200F) {
        return Some(hazard(c, HazardKind::Bidi, bidi_name(cp), Some(visible_codepoint(cp))));
    }
    // Zero-width / word joiners / BOM.
    if matches!(cp, 0x200B | 0x200C | 0x200D | 0x2060 | 0xFEFF) {
        return Some(hazard(c, HazardKind::ZeroWidth, zero_width_name(cp), Some(visible_codepoint(cp))));
    }
    // Control characters (C0 except the whitespace handled below, C1, DEL).
    if (cp < 0x20 && !c.is_whitespace()) || cp == 0x7F || (0x80..=0x9F).contains(&cp) {
        return Some(hazard(
            c,
            HazardKind::Control,
            format!("Control character (U+{cp:04X})"),
            Some(format!("⟨{cp:04X}⟩")),
        ));
    }
    // Whitespace: leading/trailing of any kind, or interior non-ASCII space.
    if c.is_whitespace() {
        if in_lead {
            return Some(hazard(c, HazardKind::LeadingSpace, whitespace_name(cp), Some(ws_glyph(c))));
        }
        if in_trail {
            return Some(hazard(c, HazardKind::TrailingSpace, whitespace_name(cp), Some(ws_glyph(c))));
        }
        if c != ' ' {
            return Some(hazard(c, HazardKind::UnusualWhitespace, whitespace_name(cp), Some(ws_glyph(c))));
        }
        return None; // a plain interior space is fine
    }
    // Combining marks (common ranges; an approximation without a Unicode DB).
    if matches!(cp, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
    {
        return Some(hazard(
            c,
            HazardKind::CombiningMark,
            format!("Combining mark (U+{cp:04X})"),
            None,
        ));
    }
    // Homoglyphs — non-ASCII letters that mimic an ASCII one.
    if let Some((ascii, script)) = confusable(c) {
        return Some(hazard(
            c,
            HazardKind::Homoglyph,
            format!("{script} '{c}' (U+{cp:04X}) — looks like '{ascii}'"),
            None,
        ));
    }
    None
}

fn hazard(c: char, kind: HazardKind, label: String, render: Option<String>) -> NameSegment {
    NameSegment {
        text: c.to_string(),
        hazard: Some(kind),
        label: Some(label),
        render,
    }
}

fn visible_codepoint(cp: u32) -> String {
    format!("⟨U+{cp:04X}⟩")
}

fn ws_glyph(c: char) -> String {
    match c {
        '\t' => "⇥".into(),
        '\n' => "⏎".into(),
        '\r' => "⏎".into(),
        ' ' => "␣".into(),
        _ => format!("⟨U+{:04X}⟩", c as u32),
    }
}

fn whitespace_name(cp: u32) -> String {
    let which = match cp {
        0x20 => "space",
        0x09 => "tab",
        0x0A => "newline",
        0x0D => "carriage return",
        0xA0 => "no-break space",
        0x2007 => "figure space",
        0x202F => "narrow no-break space",
        0x3000 => "ideographic space",
        0x2000..=0x200A => "Unicode space",
        _ => "whitespace",
    };
    format!("{which} (U+{cp:04X})")
}

fn zero_width_name(cp: u32) -> String {
    let which = match cp {
        0x200B => "zero-width space",
        0x200C => "zero-width non-joiner",
        0x200D => "zero-width joiner",
        0x2060 => "word joiner",
        0xFEFF => "byte-order mark",
        _ => "zero-width character",
    };
    format!("{which} (U+{cp:04X})")
}

fn bidi_name(cp: u32) -> String {
    let which = match cp {
        0x202A => "left-to-right embedding",
        0x202B => "right-to-left embedding",
        0x202C => "pop directional formatting",
        0x202D => "left-to-right override",
        0x202E => "right-to-left override",
        0x2066 => "left-to-right isolate",
        0x2067 => "right-to-left isolate",
        0x2068 => "first strong isolate",
        0x2069 => "pop directional isolate",
        0x200E => "left-to-right mark",
        0x200F => "right-to-left mark",
        _ => "bidirectional control",
    };
    format!("{which} (U+{cp:04X})")
}

/// Map a known confusable codepoint to (ascii lookalike, script name).
/// Curated rather than exhaustive — covers the common Cyrillic/Greek/
/// fullwidth attacks (e.g. "раypal").
fn confusable(c: char) -> Option<(char, &'static str)> {
    // Fullwidth Latin (U+FF21..FF3A, U+FF41..FF5A).
    if ('\u{FF21}'..='\u{FF3A}').contains(&c) {
        return Some(((c as u32 - 0xFF21 + b'A' as u32).try_into().ok()?, "Fullwidth"));
    }
    if ('\u{FF41}'..='\u{FF5A}').contains(&c) {
        return Some(((c as u32 - 0xFF41 + b'a' as u32).try_into().ok()?, "Fullwidth"));
    }
    let mapped = match c {
        // Cyrillic lowercase lookalikes.
        'а' => ('a', "Cyrillic"),
        'е' => ('e', "Cyrillic"),
        'о' => ('o', "Cyrillic"),
        'р' => ('p', "Cyrillic"),
        'с' => ('c', "Cyrillic"),
        'у' => ('y', "Cyrillic"),
        'х' => ('x', "Cyrillic"),
        'і' => ('i', "Cyrillic"),
        'ј' => ('j', "Cyrillic"),
        'ѕ' => ('s', "Cyrillic"),
        'ԁ' => ('d', "Cyrillic"),
        'һ' => ('h', "Cyrillic"),
        'ո' => ('n', "Armenian"),
        // Cyrillic uppercase lookalikes.
        'А' => ('A', "Cyrillic"),
        'В' => ('B', "Cyrillic"),
        'Е' => ('E', "Cyrillic"),
        'К' => ('K', "Cyrillic"),
        'М' => ('M', "Cyrillic"),
        'Н' => ('H', "Cyrillic"),
        'О' => ('O', "Cyrillic"),
        'Р' => ('P', "Cyrillic"),
        'С' => ('C', "Cyrillic"),
        'Т' => ('T', "Cyrillic"),
        'Х' => ('X', "Cyrillic"),
        // Greek lookalikes.
        'ο' => ('o', "Greek"),
        'α' => ('a', "Greek"),
        'ν' => ('v', "Greek"),
        'ρ' => ('p', "Greek"),
        'τ' => ('t', "Greek"),
        'ι' => ('i', "Greek"),
        'κ' => ('k', "Greek"),
        'Α' => ('A', "Greek"),
        'Β' => ('B', "Greek"),
        'Ε' => ('E', "Greek"),
        'Ο' => ('O', "Greek"),
        'Ρ' => ('P', "Greek"),
        'Τ' => ('T', "Greek"),
        _ => return None,
    };
    Some(mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ascii_is_clean() {
        assert!(!has_hazards("invoice_2024.pdf"));
        assert!(!has_hazards("My Document v2.txt")); // interior spaces are fine
        let segs = analyze("report.pdf");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].hazard, None);
    }

    #[test]
    fn trailing_space_flagged() {
        let segs = analyze("invoice.pdf ");
        assert_eq!(segs.last().unwrap().hazard, Some(HazardKind::TrailingSpace));
    }

    #[test]
    fn leading_space_flagged() {
        assert_eq!(analyze("  hi.txt")[0].hazard, Some(HazardKind::LeadingSpace));
    }

    #[test]
    fn bidi_override_flagged() {
        // "photo_\u{202E}gpj.exe" renders as "photo_exe.jpg".
        let name = "photo_\u{202E}gpj.exe";
        let hit = analyze(name).into_iter().find(|s| s.hazard == Some(HazardKind::Bidi));
        assert!(hit.is_some());
        assert!(hit.unwrap().label.unwrap().contains("right-to-left override"));
    }

    #[test]
    fn zero_width_flagged() {
        assert!(analyze("inv\u{200B}oice.exe")
            .iter()
            .any(|s| s.hazard == Some(HazardKind::ZeroWidth)));
    }

    #[test]
    fn cyrillic_homoglyph_flagged() {
        // "раypal" — first two letters are Cyrillic.
        let segs = analyze("\u{0440}\u{0430}ypal.exe");
        let homos: Vec<_> = segs
            .iter()
            .filter(|s| s.hazard == Some(HazardKind::Homoglyph))
            .collect();
        assert_eq!(homos.len(), 2);
        assert!(homos[0].label.as_ref().unwrap().contains("Cyrillic"));
    }

    #[test]
    fn tab_is_unusual_whitespace() {
        assert!(analyze("a\tb.txt")
            .iter()
            .any(|s| s.hazard == Some(HazardKind::UnusualWhitespace)));
    }

    #[test]
    fn segments_reconstruct_the_name() {
        let name = " pаypal\u{200B}.exe ";
        let rebuilt: String = analyze(name).iter().map(|s| s.text.as_str()).collect();
        assert_eq!(rebuilt, name);
    }
}
