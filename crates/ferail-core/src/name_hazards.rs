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
    /// A combining diacritical mark with nothing sensible to combine with:
    /// riding on whitespace/punctuation/nothing, or stacked Zalgo-style.
    /// Marks on a letter are ordinary accents (macOS stores names in NFD,
    /// so every "é" on disk is `e` + U+0301) and are not flagged.
    CombiningMark,
}

impl HazardKind {
    /// Short human description for the segment's tooltip prefix — a msgid;
    /// translate at the display site with `ferail_core::i18n::tr_raw`.
    pub fn summary(self) -> &'static str {
        match self {
            HazardKind::LeadingSpace => msgid!("Leading whitespace"),
            HazardKind::TrailingSpace => msgid!("Trailing whitespace"),
            HazardKind::UnusualWhitespace => msgid!("Unusual whitespace"),
            HazardKind::ZeroWidth => msgid!("Zero-width character"),
            HazardKind::Control => msgid!("Control character"),
            HazardKind::Bidi => msgid!("Bidirectional override"),
            HazardKind::Homoglyph => msgid!("Look-alike character"),
            HazardKind::CombiningMark => msgid!("Combining mark"),
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
    let lead_end = chars
        .iter()
        .position(|c| !c.is_whitespace())
        .unwrap_or(chars.len());
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

    let mixed = mixed_script_flags(&chars);
    // Combining-mark context: a mark is anchored (benign) when it rides on a
    // letter and at most one other mark already sits on that base — the NFD
    // shapes real accents take (Vietnamese stacks two, e.g. "ế"). Anything
    // else — a mark on whitespace/punctuation/nothing, or a Zalgo pile — is
    // stray and stays flagged.
    let mut base_is_letter = false;
    let mut stacked_marks = 0usize;
    for (i, &c) in chars.iter().enumerate() {
        let in_lead = i < lead_end;
        let in_trail = i >= trail_start && trail_start < chars.len();
        let mark_anchored = if is_combining_mark(c as u32) {
            stacked_marks += 1;
            base_is_letter && stacked_marks <= 2
        } else {
            base_is_letter = c.is_alphabetic();
            stacked_marks = 0;
            false
        };
        match classify(c, in_lead, in_trail, mixed[i], mark_anchored) {
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

/// Per-char homoglyph context: `true` when the char's alphabetic token
/// mixes ASCII Latin letters with confusable non-ASCII letters — a
/// UTS #39-style mixed-script heuristic. An all-Cyrillic or all-Greek
/// word is just a name in that language; unconditionally flagging it
/// painted a Russian user's entire Documents folder with the
/// deceptive-character highlight, destroying the signal. The attack
/// shape is "раypal": lookalikes hidden *inside* an otherwise-Latin
/// word, so only mixed tokens light up.
fn mixed_script_flags(chars: &[char]) -> Vec<bool> {
    let mut flags = vec![false; chars.len()];
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i].is_alphabetic() {
            i += 1;
        }
        let token = &chars[start..i];
        let has_ascii = token.iter().any(|c| c.is_ascii_alphabetic());
        let has_confusable = token.iter().any(|c| confusable(*c).is_some());
        if has_ascii && has_confusable {
            for flag in &mut flags[start..i] {
                *flag = true;
            }
        }
    }
    flags
}

/// True for the common combining diacritical ranges (an approximation
/// without a Unicode DB).
fn is_combining_mark(cp: u32) -> bool {
    matches!(cp, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// Classify a single character. `in_lead`/`in_trail` mark the leading and
/// trailing whitespace zones so a normal interior space stays plain.
/// `in_mixed_token` gates the homoglyph check (see
/// [`mixed_script_flags`]). `mark_anchored` is true when a combining mark
/// rides on a letter as an ordinary accent (see [`analyze`]).
fn classify(
    c: char,
    in_lead: bool,
    in_trail: bool,
    in_mixed_token: bool,
    mark_anchored: bool,
) -> Option<NameSegment> {
    let cp = c as u32;

    // Bidirectional controls — the headline trick (reverses gpj.exe ⇒ exe.jpg).
    if matches!(cp, 0x202A..=0x202E | 0x2066..=0x2069 | 0x200E | 0x200F) {
        return Some(hazard(
            c,
            HazardKind::Bidi,
            bidi_name(cp),
            Some(visible_codepoint(cp)),
        ));
    }
    // Zero-width / word joiners / BOM.
    if matches!(cp, 0x200B | 0x200C | 0x200D | 0x2060 | 0xFEFF) {
        return Some(hazard(
            c,
            HazardKind::ZeroWidth,
            zero_width_name(cp),
            Some(visible_codepoint(cp)),
        ));
    }
    // Control characters (C0 except the whitespace handled below, C1, DEL).
    if (cp < 0x20 && !c.is_whitespace()) || cp == 0x7F || (0x80..=0x9F).contains(&cp) {
        return Some(hazard(
            c,
            HazardKind::Control,
            tr!(
                "Control character (U+{code})",
                code = format_args!("{cp:04X}")
            )
            .into_string(),
            Some(format!("⟨{cp:04X}⟩")),
        ));
    }
    // Whitespace: leading/trailing of any kind, or interior non-ASCII space.
    if c.is_whitespace() {
        if in_lead {
            return Some(hazard(
                c,
                HazardKind::LeadingSpace,
                whitespace_name(cp),
                Some(ws_glyph(c)),
            ));
        }
        if in_trail {
            return Some(hazard(
                c,
                HazardKind::TrailingSpace,
                whitespace_name(cp),
                Some(ws_glyph(c)),
            ));
        }
        if c != ' ' {
            return Some(hazard(
                c,
                HazardKind::UnusualWhitespace,
                whitespace_name(cp),
                Some(ws_glyph(c)),
            ));
        }
        return None; // a plain interior space is fine
    }
    // Combining marks: an accent on a letter is ordinary NFD text (kept in
    // the plain run so it shapes with its base, e.g. "congé"); only stray
    // marks — on whitespace/punctuation/nothing, or Zalgo stacks — flag.
    if is_combining_mark(cp) {
        if mark_anchored {
            return None;
        }
        return Some(hazard(
            c,
            HazardKind::CombiningMark,
            tr!(
                "Stray combining mark (U+{code})",
                code = format_args!("{cp:04X}")
            )
            .into_string(),
            Some(visible_codepoint(cp)),
        ));
    }
    // Homoglyphs — non-ASCII letters that mimic an ASCII one, flagged
    // only inside a token that also carries ASCII Latin letters.
    if in_mixed_token {
        if let Some((ascii, script)) = confusable(c) {
            return Some(hazard(
                c,
                HazardKind::Homoglyph,
                tr!(
                    "{script} '{c}' (U+{code}) — looks like '{ascii}'",
                    script = crate::i18n::tr_raw(script),
                    c = c,
                    code = format_args!("{cp:04X}"),
                    ascii = ascii
                )
                .into_string(),
                None,
            ));
        }
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
        0x20 => msgid!("space"),
        0x09 => msgid!("tab"),
        0x0A => msgid!("newline"),
        0x0D => msgid!("carriage return"),
        0xA0 => msgid!("no-break space"),
        0x2007 => msgid!("figure space"),
        0x202F => msgid!("narrow no-break space"),
        0x3000 => msgid!("ideographic space"),
        0x2000..=0x200A => msgid!("Unicode space"),
        _ => msgid!("whitespace"),
    };
    tr!(
        "{which} (U+{code})",
        which = crate::i18n::tr_raw(which),
        code = format_args!("{cp:04X}")
    )
    .into_string()
}

fn zero_width_name(cp: u32) -> String {
    let which = match cp {
        0x200B => msgid!("zero-width space"),
        0x200C => msgid!("zero-width non-joiner"),
        0x200D => msgid!("zero-width joiner"),
        0x2060 => msgid!("word joiner"),
        0xFEFF => msgid!("byte-order mark"),
        _ => msgid!("zero-width character"),
    };
    tr!(
        "{which} (U+{code})",
        which = crate::i18n::tr_raw(which),
        code = format_args!("{cp:04X}")
    )
    .into_string()
}

fn bidi_name(cp: u32) -> String {
    let which = match cp {
        0x202A => msgid!("left-to-right embedding"),
        0x202B => msgid!("right-to-left embedding"),
        0x202C => msgid!("pop directional formatting"),
        0x202D => msgid!("left-to-right override"),
        0x202E => msgid!("right-to-left override"),
        0x2066 => msgid!("left-to-right isolate"),
        0x2067 => msgid!("right-to-left isolate"),
        0x2068 => msgid!("first strong isolate"),
        0x2069 => msgid!("pop directional isolate"),
        0x200E => msgid!("left-to-right mark"),
        0x200F => msgid!("right-to-left mark"),
        _ => msgid!("bidirectional control"),
    };
    tr!(
        "{which} (U+{code})",
        which = crate::i18n::tr_raw(which),
        code = format_args!("{cp:04X}")
    )
    .into_string()
}

/// Map a known confusable codepoint to (ascii lookalike, script name).
/// Curated rather than exhaustive — covers the common Cyrillic/Greek/
/// fullwidth attacks (e.g. "раypal").
fn confusable(c: char) -> Option<(char, &'static str)> {
    // Fullwidth Latin (U+FF21..FF3A, U+FF41..FF5A).
    if ('\u{FF21}'..='\u{FF3A}').contains(&c) {
        return Some((
            (c as u32 - 0xFF21 + b'A' as u32).try_into().ok()?,
            msgid!("Fullwidth"),
        ));
    }
    if ('\u{FF41}'..='\u{FF5A}').contains(&c) {
        return Some((
            (c as u32 - 0xFF41 + b'a' as u32).try_into().ok()?,
            msgid!("Fullwidth"),
        ));
    }
    let mapped = match c {
        // Cyrillic lowercase lookalikes.
        'а' => ('a', msgid!("Cyrillic")),
        'е' => ('e', msgid!("Cyrillic")),
        'о' => ('o', msgid!("Cyrillic")),
        'р' => ('p', msgid!("Cyrillic")),
        'с' => ('c', msgid!("Cyrillic")),
        'у' => ('y', msgid!("Cyrillic")),
        'х' => ('x', msgid!("Cyrillic")),
        'і' => ('i', msgid!("Cyrillic")),
        'ј' => ('j', msgid!("Cyrillic")),
        'ѕ' => ('s', msgid!("Cyrillic")),
        'ԁ' => ('d', msgid!("Cyrillic")),
        'һ' => ('h', msgid!("Cyrillic")),
        'ո' => ('n', msgid!("Armenian")),
        // Cyrillic uppercase lookalikes.
        'А' => ('A', msgid!("Cyrillic")),
        'В' => ('B', msgid!("Cyrillic")),
        'Е' => ('E', msgid!("Cyrillic")),
        'К' => ('K', msgid!("Cyrillic")),
        'М' => ('M', msgid!("Cyrillic")),
        'Н' => ('H', msgid!("Cyrillic")),
        'О' => ('O', msgid!("Cyrillic")),
        'Р' => ('P', msgid!("Cyrillic")),
        'С' => ('C', msgid!("Cyrillic")),
        'Т' => ('T', msgid!("Cyrillic")),
        'Х' => ('X', msgid!("Cyrillic")),
        // Greek lookalikes.
        'ο' => ('o', msgid!("Greek")),
        'α' => ('a', msgid!("Greek")),
        'ν' => ('v', msgid!("Greek")),
        'ρ' => ('p', msgid!("Greek")),
        'τ' => ('t', msgid!("Greek")),
        'ι' => ('i', msgid!("Greek")),
        'κ' => ('k', msgid!("Greek")),
        'Α' => ('A', msgid!("Greek")),
        'Β' => ('B', msgid!("Greek")),
        'Ε' => ('E', msgid!("Greek")),
        'Ο' => ('O', msgid!("Greek")),
        'Ρ' => ('P', msgid!("Greek")),
        'Τ' => ('T', msgid!("Greek")),
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
        assert_eq!(
            analyze("  hi.txt")[0].hazard,
            Some(HazardKind::LeadingSpace)
        );
    }

    #[test]
    fn bidi_override_flagged() {
        // "photo_\u{202E}gpj.exe" renders as "photo_exe.jpg".
        let name = "photo_\u{202E}gpj.exe";
        let hit = analyze(name)
            .into_iter()
            .find(|s| s.hazard == Some(HazardKind::Bidi));
        assert!(hit.is_some());
        assert!(hit
            .unwrap()
            .label
            .unwrap()
            .contains("right-to-left override"));
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
    fn all_cyrillic_name_is_clean() {
        // A normal Russian document name: single-script tokens are a
        // language, not an attack (extension token "pdf" is pure
        // ASCII, stem is pure Cyrillic — never mixed).
        assert!(!has_hazards("Договор аренды.pdf"));
        assert!(!has_hazards("Отчёт_2024.docx"));
        // Greek too.
        assert!(!has_hazards("Σημειώσεις.txt"));
    }

    #[test]
    fn mixed_token_still_flagged() {
        // Cyrillic 'о' hidden inside a Latin word — the actual attack.
        assert!(has_hazards("inv\u{043E}ice.pdf"));
    }

    #[test]
    fn nfd_accents_are_clean() {
        // macOS stores names in NFD: "congé" arrives as "conge\u{301}".
        assert!(!has_hazards("Lorraine conge\u{301}.txt"));
        // The mark stays inside the plain run so it shapes with its base.
        let segs = analyze("conge\u{301}.txt");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "conge\u{301}.txt");
        // Vietnamese stacks two marks per letter: "ế" = e + circumflex + acute.
        assert!(!has_hazards("tie\u{302}\u{301}ng.txt"));
        // Precomposed (NFC) input was never flagged and still isn't.
        assert!(!has_hazards("Lorraine congé.txt"));
    }

    #[test]
    fn stray_combining_marks_flagged() {
        // A mark riding on a space — nothing legitimate combines there.
        assert!(analyze("inv \u{0301}oice.pdf")
            .iter()
            .any(|s| s.hazard == Some(HazardKind::CombiningMark)));
        // A mark at the very start of the name.
        assert!(has_hazards("\u{0301}virus.exe"));
        // A Zalgo pile: three-plus marks on one base.
        assert!(has_hazards("z\u{0300}\u{0301}\u{0302}alg.txt"));
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
