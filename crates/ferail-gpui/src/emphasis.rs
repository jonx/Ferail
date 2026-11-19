//! Names set in italics inside a plain-string label.
//!
//! A task label such as "Copying *report.pdf* to *Desktop*…" travels as
//! one `String` through the task registry, so the emphasis rides along as
//! a pair of invisible markers around each name: U+2068 FIRST STRONG
//! ISOLATE and U+2069 POP DIRECTIONAL ISOLATE. They are the Unicode
//! characters meant for exactly this, an embedded name of unknown
//! direction, so a label that reaches a log or the clipboard unrendered
//! still reads correctly. [`styled`] draws the marked spans in italics
//! and drops the markers; [`plain`] only drops them.
//!
//! Marking a name replaces quoting it: quote characters differ by
//! language (“…”, « … », „…“) and a translation used to mix two styles in
//! one label.

use gpui::{FontStyle, HighlightStyle, SharedString, StyledText};

const OPEN: char = '\u{2068}';
const CLOSE: char = '\u{2069}';

/// Wraps `name` so [`styled`] sets it in italics.
pub fn name(name: impl AsRef<str>) -> String {
    let name = name.as_ref();
    let mut out = String::with_capacity(name.len() + 2 * OPEN.len_utf8());
    out.push(OPEN);
    out.push_str(name);
    out.push(CLOSE);
    out
}

/// `text` without its emphasis markers.
pub fn plain(text: &str) -> String {
    text.chars().filter(|c| *c != OPEN && *c != CLOSE).collect()
}

/// `text` with every marked span set in italics and the markers removed.
/// Text without markers comes back as a plain run, so any label can go
/// through here.
pub fn styled(text: &str) -> StyledText {
    let (stripped, spans) = split(text);
    let italic = HighlightStyle {
        font_style: Some(FontStyle::Italic),
        ..HighlightStyle::default()
    };
    StyledText::new(SharedString::from(stripped))
        .with_highlights(spans.into_iter().map(|range| (range, italic)))
}

/// The marker-free text and the byte ranges of its emphasized spans. An
/// unclosed span runs to the end; a stray close is ignored.
fn split(text: &str) -> (String, Vec<std::ops::Range<usize>>) {
    let mut out = String::with_capacity(text.len());
    let mut spans = Vec::new();
    let mut start: Option<usize> = None;
    for c in text.chars() {
        match c {
            OPEN => {
                if start.is_none() {
                    start = Some(out.len());
                }
            }
            CLOSE => {
                if let Some(s) = start.take() {
                    if s < out.len() {
                        spans.push(s..out.len());
                    }
                }
            }
            c => out.push(c),
        }
    }
    if let Some(s) = start {
        if s < out.len() {
            spans.push(s..out.len());
        }
    }
    (out, spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_marked_names() {
        let label = format!("Copie de {} dans {}…", name("a.tc"), name("Desktop"));
        let (text, spans) = split(&label);
        assert_eq!(text, "Copie de a.tc dans Desktop…");
        assert_eq!(&text[spans[0].clone()], "a.tc");
        assert_eq!(&text[spans[1].clone()], "Desktop");
        assert_eq!(plain(&label), text);
    }

    #[test]
    fn unmarked_and_malformed_text() {
        assert_eq!(split("Sizing 4 folders…"), ("Sizing 4 folders…".into(), vec![]));
        let (text, spans) = split("x\u{2069}y\u{2068}zz");
        assert_eq!(text, "xyzz");
        assert_eq!(spans, vec![2..4]);
    }
}
