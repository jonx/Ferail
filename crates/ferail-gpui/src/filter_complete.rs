//! Filter-field token autocomplete.
//!
//! The filter box understands structured tokens (`size:>10mb`,
//! `mod:week`, `locked:yes` — see `ferail_core::filter_expr`). This
//! completion provider makes them discoverable: typing a prefix of a
//! token key offers the keys (`loc` → `locked:`), and once a key is
//! accepted the menu chains into its example values (`locked:` →
//! `yes` / `no`). Clearing the field to empty shows the whole token
//! list once, as a quiet cheat-sheet.
//!
//! Suggestions come from `filter_expr::TOKEN_HELP` — the same table
//! the parser's tests round-trip — so the menu can never advertise a
//! token the parser doesn't understand.
//!
//! Prime directive: everything here is a lookup in a static table and never
//! touches the filesystem. The returned byte ranges plug directly into the
//! normal single-line `InputState`.

use ferail_core::filter_expr::TOKEN_HELP;

/// The word being typed: text from the last whitespace before the
/// cursor up to the cursor, with its UTF-8 byte offset.
fn current_word(upto: &str) -> (&str, usize) {
    let start = upto
        .rfind(char::is_whitespace)
        .map(|i| i + upto[i..].chars().next().map_or(1, char::len_utf8))
        .unwrap_or(0);
    (&upto[start..], start)
}

/// Suggestions for the compact toolbar input. Ranges are UTF-8 byte ranges,
/// matching `InputState`; this deliberately avoids routing a one-line field
/// through the document editor's LSP/UTF-16 machinery.
pub fn single_line_suggestions(
    value: &str,
    cursor: usize,
) -> Vec<crate::single_line_complete::SingleLineSuggestion> {
    let cursor = cursor.min(value.len());
    if !value.is_char_boundary(cursor) {
        return Vec::new();
    }
    let upto = &value[..cursor];
    let (word, word_start) = current_word(upto);

    if let Some((key, partial)) = word.split_once(':') {
        let key_lower = format!("{}:", key.to_lowercase());
        let Some(help) = TOKEN_HELP.iter().find(|h| h.key == key_lower) else {
            return Vec::new();
        };
        let value_start = word_start + word.len() - partial.len();
        return help
            .values
            .iter()
            .filter(|v| partial.is_empty() || v.to_lowercase().starts_with(&partial.to_lowercase()))
            .map(|v| crate::single_line_complete::SingleLineSuggestion {
                label: format!("{key_lower}{v}").into(),
                detail: None,
                replacement: value_start..cursor,
                insertion: format!("{v} ").into(),
            })
            .collect();
    }

    let word_lower = word.to_lowercase();
    if word.is_empty() && !upto.is_empty() {
        return Vec::new();
    }
    TOKEN_HELP
        .iter()
        .filter(|h| word_lower.is_empty() || h.key.starts_with(&word_lower))
        .map(|h| crate::single_line_complete::SingleLineSuggestion {
            label: h.key.into(),
            detail: Some(crate::i18n::tr_static(h.detail)),
            replacement: word_start..cursor,
            insertion: h.key.into(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(upto: &str) -> Vec<String> {
        single_line_suggestions(upto, upto.len())
            .into_iter()
            .map(|i| i.label.to_string())
            .collect()
    }

    #[test]
    fn empty_input_lists_every_token() {
        assert_eq!(labels("").len(), TOKEN_HELP.len());
    }

    #[test]
    fn key_prefix_narrows() {
        assert_eq!(labels("loc"), vec!["locked:"]);
        assert_eq!(labels("report loc"), vec!["locked:"]);
        // Case-insensitive.
        assert_eq!(labels("LOC"), vec!["locked:"]);
    }

    #[test]
    fn non_matching_word_stays_quiet() {
        assert!(labels("report").is_empty());
        // Space after a word: no cheat-sheet mid-typing.
        assert!(labels("report ").is_empty());
    }

    #[test]
    fn key_chains_into_values() {
        assert_eq!(labels("locked:"), vec!["locked:yes", "locked:no"]);
        assert_eq!(labels("locked:y"), vec!["locked:yes"]);
        assert_eq!(
            labels("size:".trim()),
            vec!["size:>1mb", "size:>100mb", "size:<1mb", "size:1mb..100mb"]
        );
        // Freeform value key: nothing to offer, menu closes.
        assert!(labels("ext:").is_empty());
    }

    #[test]
    fn value_edit_replaces_only_the_value() {
        let items = single_line_suggestions("big mod:t", "big mod:t".len());
        let today = items.iter().find(|i| i.label == "mod:today").unwrap();
        // "big mod:" is 8 bytes; the replacement starts after
        // the colon and covers just the typed value prefix.
        assert_eq!(today.replacement, 8..9);
        assert_eq!(today.insertion.as_ref(), "today ");
    }

    #[test]
    fn multibyte_prefix_ranges_are_utf8_bytes() {
        // "é🙂 " before the word: 2 + 4 + 1 UTF-8 bytes.
        let value = "é🙂 loc";
        let items = single_line_suggestions(value, value.len());
        assert_eq!(items[0].replacement.start, 7);
    }
}
