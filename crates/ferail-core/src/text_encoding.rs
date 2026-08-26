//! Bounded, dependency-free decoding helpers shared by magic sniffing and
//! previews. The caller owns read limits; this module only interprets bytes.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Cp437,
    Latin1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedText {
    pub text: String,
    pub encoding: TextEncoding,
}

/// Decode a plausible text buffer without allowing arbitrary binary data
/// through the legacy single-byte fallback.
pub fn decode_text(bytes: &[u8]) -> Option<DecodedText> {
    if bytes.is_empty() {
        return Some(DecodedText {
            text: String::new(),
            encoding: TextEncoding::Utf8,
        });
    }
    if bytes.starts_with(&[0xff, 0xfe]) {
        return decode_utf16(&bytes[2..], true);
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        return decode_utf16(&bytes[2..], false);
    }
    if bytes.contains(&0) {
        return None;
    }

    let utf8 = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    if let Ok(text) = std::str::from_utf8(utf8) {
        return Some(DecodedText {
            text: text.to_owned(),
            encoding: TextEncoding::Utf8,
        });
    }
    if !looks_like_single_byte_text(bytes) {
        return None;
    }

    if looks_like_cp437_art(bytes) {
        Some(DecodedText {
            text: decode_cp437(bytes),
            encoding: TextEncoding::Cp437,
        })
    } else {
        Some(DecodedText {
            text: bytes.iter().map(|&byte| char::from(byte)).collect(),
            encoding: TextEncoding::Latin1,
        })
    }
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> Option<DecodedText> {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        units.push(if little_endian {
            u16::from_le_bytes([chunk[0], chunk[1]])
        } else {
            u16::from_be_bytes([chunk[0], chunk[1]])
        });
    }
    // A prefix read can split the final code unit. Ignore that single byte,
    // just as the UTF-8 preview tolerates a split final codepoint.
    let text = String::from_utf16(&units).ok()?;
    Some(DecodedText {
        text,
        encoding: if little_endian {
            TextEncoding::Utf16Le
        } else {
            TextEncoding::Utf16Be
        },
    })
}

/// DOS code page 437. ASCII is identical; the upper half is indexed through
/// the canonical Unicode mapping below.
pub fn decode_cp437(bytes: &[u8]) -> String {
    const HIGH: [char; 128] = [
        'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ',
        'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú',
        'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡',
        '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟',
        '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘',
        '┌', '█', '▄', '▌', '▐', '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ',
        '∞', 'φ', 'ε', '∩', '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²',
        '■', '\u{a0}',
    ];
    bytes
        .iter()
        .map(|&byte| {
            if byte < 0x80 {
                char::from(byte)
            } else {
                HIGH[(byte - 0x80) as usize]
            }
        })
        .collect()
}

/// Conservative score for OEM text art. False means only "not confident":
/// callers must retain the normal text fallback.
pub fn looks_like_cp437_art(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(4096)];
    let drawing = sample
        .iter()
        .filter(|&&byte| matches!(byte, 0xb0..=0xdf | 0xfe))
        .count();
    if drawing < 8 || drawing * 100 < sample.len().max(1) * 2 {
        return false;
    }

    let mut long_runs = 0;
    let mut line_drawing = 0;
    for line in sample.split(|&byte| byte == b'\n').take(100) {
        let mut run_byte = 0;
        let mut run_len = 0;
        let mut best = 0;
        for &byte in line {
            if byte == run_byte {
                run_len += 1;
            } else {
                run_byte = byte;
                run_len = 1;
            }
            if matches!(byte, 0xb0..=0xdf | 0xfe) {
                line_drawing += 1;
                best = best.max(run_len);
            }
        }
        if best >= 4 {
            long_runs += 1;
        }
    }
    long_runs >= 1 && line_drawing >= 8
}

/// Unicode-side art score, used after UTF-8/UTF-16 decoding.
pub fn looks_like_text_art(text: &str) -> bool {
    let mut drawing = 0;
    let mut art_lines = 0;
    for line in text.lines().take(100) {
        let count = line
            .chars()
            .filter(|&ch| {
                matches!(
                    ch,
                    '\u{2500}'..='\u{257f}' | '\u{2580}'..='\u{259f}' | '■'
                )
            })
            .count();
        drawing += count;
        if count >= 4 {
            art_lines += 1;
        }
    }
    drawing >= 8 && art_lines >= 2
}

pub fn looks_like_single_byte_text(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(512)];
    let printable = sample
        .iter()
        .filter(|&&byte| {
            byte.is_ascii_graphic()
                || matches!(byte, b' ' | b'\n' | b'\r' | b'\t' | 0x1b)
                || byte >= 0xa0
        })
        .count();
    printable * 100 / sample.len().max(1) >= 85
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnsiColor {
    Standard(u8),
    Bright(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AnsiStyle {
    pub foreground: Option<AnsiColor>,
    pub background: Option<AnsiColor>,
    pub bold: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnsiSpan {
    pub range: std::ops::Range<usize>,
    pub style: AnsiStyle,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AnsiDocument {
    pub text: String,
    pub spans: Vec<AnsiSpan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AnsiCell {
    ch: char,
    style: AnsiStyle,
}

impl Default for AnsiCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: AnsiStyle::default(),
        }
    }
}

/// Render the harmless layout and SGR colour subset of ANSI terminal text
/// into a bounded document. OSC/DCS and unknown escape families are consumed
/// as data and never reach a terminal/widget.
pub fn render_ansi(text: &str, max_columns: usize, max_rows: usize) -> AnsiDocument {
    if !text.contains('\u{1b}') {
        return AnsiDocument {
            text: text.to_owned(),
            spans: Vec::new(),
        };
    }
    let max_columns = max_columns.max(1);
    let max_rows = max_rows.max(1);
    let chars: Vec<char> = text.chars().collect();
    let mut canvas: Vec<Vec<AnsiCell>> = vec![Vec::new()];
    let mut style = AnsiStyle::default();
    let (mut row, mut column, mut index) = (0usize, 0usize, 0usize);

    while index < chars.len() && row < max_rows {
        let ch = chars[index];
        index += 1;
        match ch {
            '\u{1b}' => {
                let Some(&family) = chars.get(index) else {
                    break;
                };
                index += 1;
                match family {
                    '[' => {
                        let start = index;
                        while index < chars.len()
                            && !(('\u{40}'..='\u{7e}').contains(&chars[index]))
                        {
                            index += 1;
                        }
                        let Some(&command) = chars.get(index) else {
                            break;
                        };
                        let params: String = chars[start..index].iter().collect();
                        index += 1;
                        let numbers: Vec<usize> = params
                            .trim_start_matches('?')
                            .split(';')
                            .map(|part| part.parse().unwrap_or(0))
                            .collect();
                        let first = numbers.first().copied().unwrap_or(0);
                        match command {
                            'H' | 'f' => {
                                row = numbers.first().copied().unwrap_or(1).max(1) - 1;
                                column = numbers.get(1).copied().unwrap_or(1).max(1) - 1;
                            }
                            'A' => row = row.saturating_sub(first.max(1)),
                            'B' => row = row.saturating_add(first.max(1)),
                            'C' => column = column.saturating_add(first.max(1)),
                            'D' => column = column.saturating_sub(first.max(1)),
                            'G' => column = first.max(1) - 1,
                            'J' if first == 2 || first == 3 => {
                                canvas.clear();
                                canvas.push(Vec::new());
                                row = 0;
                                column = 0;
                            }
                            'K' => {
                                ensure_row(&mut canvas, row, max_rows);
                                if let Some(line) = canvas.get_mut(row) {
                                    match first {
                                        1 => {
                                            for cell in line.iter_mut().take(column + 1) {
                                                *cell = AnsiCell::default();
                                            }
                                        }
                                        2 => line.clear(),
                                        _ => line.truncate(column),
                                    }
                                }
                            }
                            'm' => apply_sgr(&numbers, &mut style),
                            // Every unsupported command remains inert.
                            _ => {}
                        }
                        row = row.min(max_rows - 1);
                        column = column.min(max_columns - 1);
                    }
                    // OSC: BEL or ST terminator. This includes hyperlinks and
                    // clipboard command 52, both intentionally discarded.
                    ']' => skip_control_string(&chars, &mut index, true),
                    // DCS/SOS/PM/APC: ST terminator.
                    'P' | 'X' | '^' | '_' => skip_control_string(&chars, &mut index, false),
                    _ => {}
                }
            }
            '\r' => column = 0,
            '\n' => {
                row += 1;
                column = 0;
                ensure_row(&mut canvas, row, max_rows);
            }
            '\t' => column = ((column / 8) + 1) * 8,
            ch if ch.is_control() => {}
            ch => {
                if column < max_columns {
                    ensure_row(&mut canvas, row, max_rows);
                    if let Some(line) = canvas.get_mut(row) {
                        if line.len() <= column {
                            line.resize(column + 1, AnsiCell::default());
                        }
                        line[column] = AnsiCell { ch, style };
                    }
                    column += 1;
                }
            }
        }
    }

    while canvas
        .last()
        .is_some_and(|line| line.iter().all(|cell| *cell == AnsiCell::default()))
    {
        canvas.pop();
    }
    let mut document = AnsiDocument::default();
    for (line_index, mut line) in canvas.into_iter().enumerate() {
        while line.last() == Some(&AnsiCell::default()) {
            line.pop();
        }
        if line_index > 0 {
            document.text.push('\n');
        }
        for cell in line {
            let start = document.text.len();
            document.text.push(cell.ch);
            let end = document.text.len();
            if cell.style == AnsiStyle::default() {
                continue;
            }
            let extends_last = document
                .spans
                .last()
                .is_some_and(|last| last.range.end == start && last.style == cell.style);
            if extends_last {
                document.spans.last_mut().expect("span exists").range.end = end;
            } else {
                document.spans.push(AnsiSpan {
                    range: start..end,
                    style: cell.style,
                });
            }
        }
    }
    document
}

pub fn render_ansi_plain(text: &str, max_columns: usize, max_rows: usize) -> String {
    render_ansi(text, max_columns, max_rows).text
}

fn apply_sgr(numbers: &[usize], style: &mut AnsiStyle) {
    let mut index = 0;
    while index < numbers.len() {
        let code = numbers[index];
        match code {
            0 => *style = AnsiStyle::default(),
            1 => style.bold = true,
            22 => style.bold = false,
            30..=37 => style.foreground = Some(AnsiColor::Standard((code - 30) as u8)),
            39 => style.foreground = None,
            40..=47 => style.background = Some(AnsiColor::Standard((code - 40) as u8)),
            49 => style.background = None,
            90..=97 => style.foreground = Some(AnsiColor::Bright((code - 90) as u8)),
            100..=107 => style.background = Some(AnsiColor::Bright((code - 100) as u8)),
            38 | 48 => {
                let target = if code == 38 {
                    &mut style.foreground
                } else {
                    &mut style.background
                };
                match numbers.get(index + 1).copied() {
                    Some(5) if numbers.get(index + 2).is_some() => {
                        *target = Some(AnsiColor::Indexed(numbers[index + 2].min(255) as u8));
                        index += 2;
                    }
                    Some(2) if numbers.get(index + 4).is_some() => {
                        *target = Some(AnsiColor::Rgb(
                            numbers[index + 2].min(255) as u8,
                            numbers[index + 3].min(255) as u8,
                            numbers[index + 4].min(255) as u8,
                        ));
                        index += 4;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        index += 1;
    }
}

fn ensure_row(canvas: &mut Vec<Vec<AnsiCell>>, row: usize, max_rows: usize) {
    if row < max_rows && canvas.len() <= row {
        canvas.resize_with(row + 1, Vec::new);
    }
}

fn skip_control_string(chars: &[char], index: &mut usize, bel_terminates: bool) {
    while *index < chars.len() {
        if bel_terminates && chars[*index] == '\u{7}' {
            *index += 1;
            return;
        }
        if chars[*index] == '\u{1b}' && chars.get(*index + 1) == Some(&'\\') {
            *index += 2;
            return;
        }
        *index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp437_box_art_decodes_and_scores() {
        let bytes = b"\xc9\xcd\xcd\xcd\xcd\xbb\r\n\xba TEST \xba\r\n\xc8\xcd\xcd\xcd\xcd\xbc\r\n";
        assert!(looks_like_cp437_art(bytes));
        assert!(decode_cp437(bytes).contains("╔════╗"));
        assert_eq!(decode_text(bytes).unwrap().encoding, TextEncoding::Cp437);
    }

    #[test]
    fn latin1_prose_is_not_cp437_art() {
        let bytes = b"\xc9T\xc9 \xc0 PARIS - \xc7A RESTE DE LA PROSE. \xc0\xc9\xc8\xc7 \xc0\xc9\xc8\xc7\r\n";
        assert!(!looks_like_cp437_art(bytes));
        assert_eq!(decode_text(bytes).unwrap().encoding, TextEncoding::Latin1);
    }

    #[test]
    fn utf16_bom_is_decoded_before_nul_rejection() {
        let bytes = [0xff, 0xfe, b'<', 0, b'M', 0, b's', 0, b'I', 0];
        let decoded = decode_text(&bytes).unwrap();
        assert_eq!(decoded.encoding, TextEncoding::Utf16Le);
        assert_eq!(decoded.text, "<MsI");
    }

    #[test]
    fn ansi_layout_is_rendered_but_osc_is_discarded() {
        let text =
            "\u{1b}[2J\u{1b}[H\u{1b}[31;1mtop\u{1b}[0m\u{1b}[3;5Hplaced\u{1b}]52;c;c2VjcmV0\u{7}";
        let rendered = render_ansi(text, 80, 20);
        assert_eq!(rendered.text, "top\n\n    placed");
        assert!(!rendered.text.contains("52"));
        assert!(!rendered.text.contains("secret"));
        assert_eq!(rendered.spans.len(), 1);
        assert_eq!(rendered.spans[0].range, 0..3);
        assert_eq!(
            rendered.spans[0].style.foreground,
            Some(AnsiColor::Standard(1))
        );
        assert!(rendered.spans[0].style.bold);
    }

    #[test]
    fn ansi_extended_colours_are_retained_as_styles() {
        let rendered = render_ansi(
            "\u{1b}[38;5;45mindexed\u{1b}[48;2;1;2;3mtrue\u{1b}[0m",
            80,
            20,
        );
        assert_eq!(rendered.text, "indexedtrue");
        assert_eq!(
            rendered.spans[0].style.foreground,
            Some(AnsiColor::Indexed(45))
        );
        assert_eq!(
            rendered.spans[1].style.background,
            Some(AnsiColor::Rgb(1, 2, 3))
        );
    }
}
