//! Reusable completion state for Ferail's compact, single-line inputs.
//!
//! gpui-component's LSP completion overlay belongs to its document editor.
//! Search and address fields use the ordinary `InputState` and keep only this
//! small presentation model beside them. Providers remain responsible for
//! producing suggestions (synchronously for filter tokens, on a worker for
//! filesystem paths).

use std::ops::Range;

use gpui::{Entity, SharedString, Window};
use gpui_component::input::InputState;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SingleLineSuggestion {
    pub label: SharedString,
    pub detail: Option<SharedString>,
    /// UTF-8 byte range in the current input value.
    pub replacement: Range<usize>,
    pub insertion: SharedString,
}

#[derive(Clone, Debug, Default)]
pub struct SingleLineSuggestions {
    items: Vec<SingleLineSuggestion>,
    selected: usize,
}

impl SingleLineSuggestions {
    pub fn replace(&mut self, items: Vec<SingleLineSuggestion>) {
        self.items = items;
        self.selected = 0;
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.selected = 0;
    }

    pub fn items(&self) -> &[SingleLineSuggestion] {
        &self.items
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn selected(&self) -> Option<&SingleLineSuggestion> {
        self.items.get(self.selected)
    }

    pub fn is_open(&self) -> bool {
        !self.items.is_empty()
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        self.selected = if delta < 0 {
            self.selected
                .checked_sub(delta.unsigned_abs())
                .unwrap_or(self.items.len() - 1)
        } else {
            (self.selected + delta as usize) % self.items.len()
        };
    }
}

/// Apply one suggestion as one undoable replacement and leave the caret just
/// after the inserted text. The provider's byte range is clipped defensively
/// so a stale asynchronous result can never panic.
pub fn apply_suggestion(
    input: &Entity<InputState>,
    suggestion: &SingleLineSuggestion,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    input.update(cx, |state, cx| {
        let old = state.value().to_string();
        let start = suggestion.replacement.start.min(old.len());
        let end = suggestion.replacement.end.min(old.len()).max(start);
        if !old.is_char_boundary(start) || !old.is_char_boundary(end) {
            return;
        }
        let mut next = old;
        next.replace_range(start..end, suggestion.insertion.as_ref());
        let caret = start + suggestion.insertion.len();
        state.replace_all(next, window, cx);
        state.set_selected_range(caret..caret, cx);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_wraps_in_both_directions() {
        let item = |label: &'static str| SingleLineSuggestion {
            label: label.into(),
            detail: None,
            replacement: 0..0,
            insertion: label.into(),
        };
        let mut state = SingleLineSuggestions::default();
        state.replace(vec![item("a"), item("b"), item("c")]);
        state.move_by(-1);
        assert_eq!(state.selected().unwrap().label.as_ref(), "c");
        state.move_by(1);
        assert_eq!(state.selected().unwrap().label.as_ref(), "a");
    }
}
