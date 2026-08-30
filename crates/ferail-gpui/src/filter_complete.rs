//! Filter-field token autocomplete.
//!
//! The filter box understands structured tokens (`size:>10mb`,
//! `mod:week`, `locked:yes`: see `ferail_core::filter_expr`). This
//! completion provider makes them discoverable: typing a prefix of a
//! token key offers the keys (`loc` → `locked:`), and once a key is
//! accepted the menu chains into its example values (`locked:` →
//! `yes` / `no`). Clearing the field to empty shows the whole token
//! list once, as a quiet cheat-sheet.
//!
//! Suggestions come from `filter_expr::TOKEN_HELP`: the same table
//! the parser's tests round-trip, so the menu can never advertise a
//! token the parser doesn't understand.
//!
//! Prime directive: everything here is a lookup in a static table and never
//! touches the filesystem. The returned byte ranges plug directly into the
//! normal single-line `InputState`.

use ferail_core::filter_expr::TOKEN_HELP;

/// Friendly spellings accepted by the parser but kept out of the compact help
/// table so the canonical vocabulary stays short.
const KEY_ALIASES: &[(&str, &str)] = &[("type:", "kind:")];

fn canonical_key(key: &str) -> &str {
    KEY_ALIASES
        .iter()
        .find_map(|(alias, canonical)| (*alias == key).then_some(*canonical))
        .unwrap_or(key)
}

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
        let lookup_key = canonical_key(&key_lower);
        let Some(help) = TOKEN_HELP.iter().find(|h| h.key == lookup_key) else {
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
    let matching_key_exists = TOKEN_HELP
        .iter()
        .any(|help| help.key.starts_with(&word_lower))
        || KEY_ALIASES
            .iter()
            .any(|(alias, _)| alias.starts_with(&word_lower));
    // A plain filename term is still useful search text. Once it no longer
    // resembles a token prefix, keep the syntax menu open and make every
    // choice append at the caret instead of replacing that search. This is
    // what lets `disk` become `disk size:>1mb` without selecting/deleting the
    // current query merely to make completion reappear.
    let append = !word.is_empty() && !matching_key_exists;
    let trailing_space = word.is_empty() && !upto.is_empty();
    let replacement = if append || trailing_space {
        cursor..cursor
    } else {
        word_start..cursor
    };
    let insertion_prefix = if append { " " } else { "" };
    let key_prefix = if append || trailing_space {
        ""
    } else {
        word_lower.as_str()
    };
    let canonical = TOKEN_HELP
        .iter()
        .filter(|h| key_prefix.is_empty() || h.key.starts_with(key_prefix))
        .map(|h| crate::single_line_complete::SingleLineSuggestion {
            label: h.key.into(),
            detail: Some(crate::i18n::tr_static(h.detail)),
            replacement: replacement.clone(),
            insertion: format!("{insertion_prefix}{}", h.key).into(),
        });
    let aliases = KEY_ALIASES
        .iter()
        .filter(|(alias, _)| key_prefix.is_empty() || alias.starts_with(key_prefix))
        .filter_map(|(alias, target)| {
            let help = TOKEN_HELP.iter().find(|help| help.key == *target)?;
            Some(crate::single_line_complete::SingleLineSuggestion {
                label: (*alias).into(),
                detail: Some(crate::i18n::tr_static(help.detail)),
                replacement: replacement.clone(),
                insertion: format!("{insertion_prefix}{alias}").into(),
            })
        });
    canonical.chain(aliases).collect()
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
        assert_eq!(labels("").len(), TOKEN_HELP.len() + KEY_ALIASES.len());
    }

    #[test]
    fn key_prefix_narrows() {
        assert_eq!(labels("loc"), vec!["locked:"]);
        assert_eq!(labels("report loc"), vec!["locked:"]);
        // Case-insensitive.
        assert_eq!(labels("LOC"), vec!["locked:"]);
        assert_eq!(labels("typ"), vec!["type:"]);
    }

    #[test]
    fn plain_search_keeps_appendable_filter_tokens_visible() {
        let items = single_line_suggestions("report", "report".len());
        assert_eq!(items.len(), TOKEN_HELP.len() + KEY_ALIASES.len());
        assert_eq!(items[0].replacement, 6..6);
        assert!(items[0].insertion.starts_with(' '));

        let after_space = single_line_suggestions("report ", "report ".len());
        assert_eq!(after_space.len(), TOKEN_HELP.len() + KEY_ALIASES.len());
        assert_eq!(after_space[0].replacement, 7..7);
        assert!(!after_space[0].insertion.starts_with(' '));
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
        assert_eq!(
            labels("type:"),
            vec!["type:folder", "type:file", "type:link"]
        );
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
