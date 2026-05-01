//! TextInput — single-line edit. Iter-2.5 minimum-viable: typing,
//! cursor movement, backspace/delete, Enter to submit, Esc to cancel,
//! paste (host fires `handle_text` with clipboard contents).
//!
//! Deferred to iter-3: selection, double/triple-click, IME composition,
//! caret blink animation. The current caret is solid (no blink) — fine
//! for the breadcrumb edit-mode use case.
//!
//! Spec: `specs/controls/02-primitives.md` §4.

use feraille_design::{FontWeight, Tokens};
use feraille_render::{Point, Rect, Renderer, TextStyle};

#[derive(Clone, Debug)]
pub enum TextInputEvent {
    Submit(String),
    Cancel,
}

#[derive(Clone, Copy, Debug)]
pub enum TextInputKey {
    Backspace,
    Delete,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    Enter,
    Escape,
}

pub struct TextInput {
    chars: Vec<char>,
    cursor: usize,
}

impl TextInput {
    pub fn new(initial: &str) -> Self {
        let chars: Vec<char> = initial.chars().collect();
        let cursor = chars.len();
        Self { chars, cursor }
    }

    pub fn value(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn set_value(&mut self, s: &str) {
        self.chars = s.chars().collect();
        self.cursor = self.chars.len();
    }

    /// Insert text at the cursor (control chars filtered out).
    pub fn handle_text(&mut self, text: &str) {
        for c in text.chars() {
            if c.is_control() {
                continue;
            }
            self.chars.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    pub fn handle_key(&mut self, key: TextInputKey) -> Option<TextInputEvent> {
        match key {
            TextInputKey::Backspace if self.cursor > 0 => {
                self.chars.remove(self.cursor - 1);
                self.cursor -= 1;
                None
            }
            TextInputKey::Delete if self.cursor < self.chars.len() => {
                self.chars.remove(self.cursor);
                None
            }
            TextInputKey::ArrowLeft => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                None
            }
            TextInputKey::ArrowRight => {
                if self.cursor < self.chars.len() {
                    self.cursor += 1;
                }
                None
            }
            TextInputKey::Home => {
                self.cursor = 0;
                None
            }
            TextInputKey::End => {
                self.cursor = self.chars.len();
                None
            }
            TextInputKey::Enter => Some(TextInputEvent::Submit(self.value())),
            TextInputKey::Escape => Some(TextInputEvent::Cancel),
            TextInputKey::Backspace | TextInputKey::Delete => None,
        }
    }

    pub fn paint(
        &self,
        bounds: Rect,
        focused: bool,
        tokens: &Tokens,
        painter: &mut dyn Renderer,
    ) {
        painter.fill_rect(bounds, tokens.bg.layer1);
        let border_color = if focused { tokens.border.focus } else { tokens.border.default };
        painter.stroke_rect(bounds, 1.0, border_color);

        let value: String = self.chars.iter().collect();
        let text_y = bounds.top() + (bounds.size.height - tokens.text.md) / 2.0 - 1.0;
        let text_x = bounds.left() + 8.0;
        painter.draw_text(
            Point::new(text_x, text_y),
            &value,
            TextStyle {
                size: tokens.text.md,
                weight: FontWeight::Regular,
                color: tokens.fg.primary,
            },
        );

        if focused {
            let prefix: String = self.chars[..self.cursor].iter().collect();
            let prefix_w = approx_text_width(&prefix, tokens.text.md);
            let caret_x = text_x + prefix_w;
            let caret_y = bounds.top() + 6.0;
            let caret_h = (bounds.size.height - 12.0).max(2.0);
            painter.fill_rect(
                Rect::new(caret_x, caret_y, 1.5, caret_h),
                tokens.fg.primary,
            );
        }
    }
}

fn approx_text_width(s: &str, size: f32) -> f32 {
    s.chars().count() as f32 * size * 0.55
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_and_backspace() {
        let mut t = TextInput::new("");
        t.handle_text("abc");
        assert_eq!(t.value(), "abc");
        assert!(matches!(t.handle_key(TextInputKey::Backspace), None));
        assert_eq!(t.value(), "ab");
    }

    #[test]
    fn enter_submits() {
        let mut t = TextInput::new("hello");
        match t.handle_key(TextInputKey::Enter) {
            Some(TextInputEvent::Submit(s)) => assert_eq!(s, "hello"),
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn esc_cancels() {
        let mut t = TextInput::new("anything");
        assert!(matches!(t.handle_key(TextInputKey::Escape), Some(TextInputEvent::Cancel)));
    }

    #[test]
    fn cursor_navigation() {
        let mut t = TextInput::new("ab");
        t.handle_key(TextInputKey::ArrowLeft);
        t.handle_text("X");
        assert_eq!(t.value(), "aXb");
    }
}
