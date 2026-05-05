//! Toast — transient bottom-right notification.
//!
//! Surfaces user-visible messages that previously only went to stderr
//! (rename failure, trash failure, create_dir failure, etc.). A toast
//! lives for `LIFETIME`, fades over `FADE_OUT` at the tail, and stacks
//! upward so the most recent toast sits at the bottom.
//!
//! Renderer-agnostic and tokenized. Animation is host-driven: call
//! `next_wakeup` to learn when the next redraw should happen, and call
//! `prune` at paint time to drop expired toasts.

use std::time::{Duration, Instant};

use feraille_design::{FontWeight, Tokens};
use feraille_render::{Point, Rect, Renderer, TextStyle};

/// How long a toast stays fully visible before starting to fade.
pub const LIFETIME: Duration = Duration::from_millis(3600);
/// Tail fade duration. The total visible time is `LIFETIME + FADE_OUT`.
pub const FADE_OUT: Duration = Duration::from_millis(400);

const TOAST_W: f32 = 320.0;
const TOAST_H: f32 = 44.0;
const MARGIN: f32 = 12.0;
const STACK_GAP: f32 = 8.0;
const ACCENT_W: f32 = 3.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToastKind {
    Info,
    Error,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub kind: ToastKind,
    pub message: String,
    born: Instant,
}

impl Toast {
    pub fn new(kind: ToastKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            born: Instant::now(),
        }
    }

    /// 0.0..=1.0; below 1.0 means we're inside the fade-out tail.
    pub fn opacity(&self, now: Instant) -> f32 {
        let age = now.saturating_duration_since(self.born);
        if age <= LIFETIME {
            1.0
        } else {
            let into_fade = age - LIFETIME;
            if into_fade >= FADE_OUT {
                0.0
            } else {
                1.0 - (into_fade.as_secs_f32() / FADE_OUT.as_secs_f32())
            }
        }
    }

    /// Has the toast finished fading and should be removed?
    pub fn is_expired(&self, now: Instant) -> bool {
        self.born.elapsed() >= LIFETIME + FADE_OUT
            || now.saturating_duration_since(self.born) >= LIFETIME + FADE_OUT
    }
}

/// Bounded queue of active toasts. Adding past `MAX_VISIBLE` rotates
/// the oldest out so the newest one is always shown.
const MAX_VISIBLE: usize = 4;

#[derive(Clone, Debug, Default)]
pub struct ToastStack {
    toasts: Vec<Toast>,
}

impl ToastStack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, toast: Toast) {
        self.toasts.push(toast);
        let overflow = self.toasts.len().saturating_sub(MAX_VISIBLE);
        if overflow > 0 {
            self.toasts.drain(0..overflow);
        }
    }

    pub fn prune(&mut self, now: Instant) {
        self.toasts.retain(|t| !t.is_expired(now));
    }

    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.toasts.len()
    }

    /// When the host should next request a redraw to drive fade-out, or
    /// `None` if no toasts are active.
    pub fn next_wakeup(&self, now: Instant) -> Option<Instant> {
        self.toasts
            .iter()
            .map(|t| t.born + LIFETIME + FADE_OUT)
            .filter(|deadline| *deadline > now)
            .min()
    }

    /// Paint the active toasts inside `bounds` (typically the file-pane
    /// rect). Stack from the bottom-right corner upward.
    pub fn paint(&self, bounds: Rect, tokens: &Tokens, painter: &mut dyn Renderer) {
        let now = Instant::now();
        let right = bounds.right() - MARGIN;
        let mut bottom = bounds.bottom() - MARGIN;
        for toast in self.toasts.iter().rev() {
            let opacity = toast.opacity(now);
            if opacity <= 0.0 {
                continue;
            }
            let rect = Rect::new(right - TOAST_W, bottom - TOAST_H, TOAST_W, TOAST_H);
            paint_one(toast, rect, opacity, tokens, painter);
            bottom -= TOAST_H + STACK_GAP;
            if bottom < bounds.top() {
                break;
            }
        }
    }
}

fn paint_one(
    toast: &Toast,
    rect: Rect,
    opacity: f32,
    tokens: &Tokens,
    painter: &mut dyn Renderer,
) {
    let bg = with_alpha(tokens.bg.layer3, opacity);
    let border = with_alpha(tokens.border.default, opacity);
    let accent = with_alpha(
        match toast.kind {
            ToastKind::Info => tokens.accent.fill,
            ToastKind::Error => tokens.status.danger,
        },
        opacity,
    );
    let fg = with_alpha(tokens.fg.primary, opacity);

    painter.fill_rect(rect, bg);
    painter.stroke_rect(rect, 1.0, border);
    painter.fill_rect(
        Rect::new(rect.left(), rect.top(), ACCENT_W, rect.size.height),
        accent,
    );

    let text_x = rect.left() + ACCENT_W + tokens.space.md;
    let text_y = rect.top() + (rect.size.height - tokens.text.md) / 2.0 - 1.0;
    painter.draw_text(
        Point::new(text_x, text_y),
        &toast.message,
        TextStyle {
            size: tokens.text.md,
            weight: FontWeight::Regular,
            color: fg,
        },
    );
}

fn with_alpha(c: feraille_design::Color, opacity: f32) -> feraille_design::Color {
    let opacity = opacity.clamp(0.0, 1.0);
    feraille_design::Color {
        a: ((c.a as f32) * opacity).round() as u8,
        ..c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opacity_full_during_lifetime() {
        let t = Toast::new(ToastKind::Info, "hi");
        let now = t.born + Duration::from_millis(100);
        assert_eq!(t.opacity(now), 1.0);
    }

    #[test]
    fn opacity_zero_after_fade() {
        let t = Toast::new(ToastKind::Error, "boom");
        let now = t.born + LIFETIME + FADE_OUT + Duration::from_millis(50);
        assert!(t.is_expired(now));
        assert_eq!(t.opacity(now), 0.0);
    }

    #[test]
    fn stack_caps_at_max_visible() {
        let mut stack = ToastStack::new();
        for i in 0..(MAX_VISIBLE + 3) {
            stack.push(Toast::new(ToastKind::Info, format!("toast {i}")));
        }
        assert_eq!(stack.len(), MAX_VISIBLE);
    }

    #[test]
    fn prune_drops_expired() {
        let mut stack = ToastStack::new();
        let mut t = Toast::new(ToastKind::Info, "stale");
        t.born = Instant::now() - LIFETIME - FADE_OUT - Duration::from_millis(10);
        stack.toasts.push(t);
        stack.push(Toast::new(ToastKind::Info, "fresh"));
        stack.prune(Instant::now());
        assert_eq!(stack.len(), 1);
    }
}
