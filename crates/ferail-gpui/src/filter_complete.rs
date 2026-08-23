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
//! Prime directive: everything here is a lookup in a static table; the
//! provider returns `Task::ready` and never touches the filesystem.

use ferail_core::filter_expr::TOKEN_HELP;
use gpui::{Context, Task, Window};
use gpui_component::input::{CompletionProvider, InputState, Rope};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Position, Range as LspRange, TextEdit,
};

pub struct FilterCompletionProvider;

/// The word being typed: text from the last whitespace before the
/// cursor up to the cursor, with its UTF-16 start column (the menu's
/// replacement range needs LSP positions).
fn current_word(upto: &str) -> (&str, u32) {
    let start = upto
        .rfind(char::is_whitespace)
        .map(|i| i + upto[i..].chars().next().map_or(1, char::len_utf8))
        .unwrap_or(0);
    let col = upto[..start].encode_utf16().count() as u32;
    (&upto[start..], col)
}

fn item(
    label: String,
    detail: Option<String>,
    replace: LspRange,
    insert: String,
) -> CompletionItem {
    CompletionItem {
        label,
        detail,
        kind: Some(CompletionItemKind::KEYWORD),
        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
            range: replace,
            new_text: insert,
        })),
        ..Default::default()
    }
}

/// Build the suggestion list for the word at the cursor. Split out of
/// the trait impl for testability.
fn suggestions_for(upto: &str) -> Vec<CompletionItem> {
    let (word, word_col) = current_word(upto);
    let cursor_col = upto.encode_utf16().count() as u32;
    let replace_word = LspRange {
        start: Position::new(0, word_col),
        end: Position::new(0, cursor_col),
    };

    // `key:partial-value` → offer that key's example values.
    if let Some((key, partial)) = word.split_once(':') {
        let key_lower = format!("{}:", key.to_lowercase());
        let Some(help) = TOKEN_HELP.iter().find(|h| h.key == key_lower) else {
            return Vec::new();
        };
        let value_col = word_col + word[..word.len() - partial.len()].encode_utf16().count() as u32;
        return help
            .values
            .iter()
            .filter(|v| partial.is_empty() || v.to_lowercase().starts_with(&partial.to_lowercase()))
            .map(|v| {
                item(
                    format!("{key_lower}{v}"),
                    None,
                    LspRange {
                        start: Position::new(0, value_col),
                        end: Position::new(0, cursor_col),
                    },
                    // Trailing space closes this term and re-arms the
                    // menu for the next one.
                    format!("{v} "),
                )
            })
            .collect();
    }

    // Empty input → the full token cheat-sheet. Mid-word → keys the
    // word prefixes. A non-matching word yields nothing, which hides
    // the menu (plain text filtering stays undisturbed).
    let word_lower = word.to_lowercase();
    if word.is_empty() && !upto.is_empty() {
        // After a space mid-typing: stay quiet rather than popping the
        // full list between every word of a plain-text filter.
        return Vec::new();
    }
    TOKEN_HELP
        .iter()
        .filter(|h| word_lower.is_empty() || h.key.starts_with(&word_lower))
        .map(|h| {
            item(
                h.key.to_string(),
                Some(crate::i18n::tr_static(h.detail).to_string()),
                replace_word,
                h.key.to_string(),
            )
        })
        .collect()
}

impl CompletionProvider for FilterCompletionProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut Context<InputState>,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let full = text.to_string();
        let upto = full.get(..offset).unwrap_or(&full).to_string();
        Task::ready(Ok(CompletionResponse::Array(suggestions_for(&upto))))
    }

    fn is_completion_trigger(
        &self,
        _offset: usize,
        _new_text: &str,
        _cx: &mut Context<InputState>,
    ) -> bool {
        // Every edit re-queries; an empty result set hides the menu.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(upto: &str) -> Vec<String> {
        suggestions_for(upto).into_iter().map(|i| i.label).collect()
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
        let items = suggestions_for("big mod:t");
        let today = items.iter().find(|i| i.label == "mod:today").unwrap();
        let Some(CompletionTextEdit::Edit(edit)) = &today.text_edit else {
            panic!("expected edit");
        };
        // "big mod:" is 8 UTF-16 units; the replacement starts after
        // the colon and covers just the typed value prefix.
        assert_eq!(edit.range.start.character, 8);
        assert_eq!(edit.range.end.character, 9);
        assert_eq!(edit.new_text, "today ");
    }

    #[test]
    fn multibyte_prefix_columns_are_utf16() {
        // "é🙂 " before the word: 1 + 2 + 1 UTF-16 units.
        let items = suggestions_for("é🙂 loc");
        let Some(CompletionTextEdit::Edit(edit)) = &items[0].text_edit else {
            panic!("expected edit");
        };
        assert_eq!(edit.range.start.character, 4);
    }
}
