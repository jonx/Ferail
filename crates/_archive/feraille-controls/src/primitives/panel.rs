//! Panel — modal-overlay chrome and tokenized layout helpers.
//!
//! `paint_dialog`, `paint_search`, `paint_properties` (in
//! `feraille-app`) all share the same chrome: optional semi-transparent
//! backdrop, then a `bg.layer1`-filled card with a 1-DIP `border.default`
//! stroke, then a content rect padded by `space.lg`. This module lifts
//! that into a single primitive so future panels (Settings, About,
//! command palette, …) inherit the look without re-implementing it.
//!
//! Renderer-agnostic and tokenized. No interaction state — modal panels
//! own input routing themselves.

use feraille_design::{Color, Tokens};
use feraille_render::{Rect, Renderer};

/// Layout description for a centred modal panel.
#[derive(Clone, Copy, Debug)]
pub struct ModalPanel {
    /// Viewport bounds (typically the window's content area). Used for
    /// centring and the optional backdrop fill.
    pub viewport: Rect,
    /// Panel width and height in DIPs.
    pub width: f32,
    pub height: f32,
    /// Vertical placement: `None` = centred; `Some(0.18)` = panel top
    /// at 18% down the viewport (search-style).
    pub top_offset_fraction: Option<f32>,
    /// Backdrop fill alpha (0–255). 0 = no backdrop.
    pub backdrop_alpha: u8,
    /// Inner padding inset from the panel edges. Use `tokens.space.lg`
    /// for dialogs and search; `tokens.space.xl` for properties.
    pub padding: f32,
}

impl ModalPanel {
    /// Compute the panel and inner body rects. Pure layout — no paint.
    pub fn compute(&self) -> (Rect, Rect) {
        let x =
            (self.viewport.left() + (self.viewport.size.width - self.width) / 2.0).round();
        let y = match self.top_offset_fraction {
            Some(frac) => (self.viewport.top() + self.viewport.size.height * frac).round(),
            None => {
                (self.viewport.top() + (self.viewport.size.height - self.height) / 2.0)
                    .round()
            }
        };
        let panel = Rect::new(x, y, self.width, self.height);
        let body = Rect::new(
            panel.left() + self.padding,
            panel.top() + self.padding,
            (panel.size.width - self.padding * 2.0).max(0.0),
            (panel.size.height - self.padding * 2.0).max(0.0),
        );
        (panel, body)
    }

    /// Paint the chrome (backdrop + frame) and return `(panel, body)`.
    /// The caller draws into `body`; `panel` is exposed so consumers
    /// can clip, draw dividers, or place footer text relative to the
    /// outer edge.
    pub fn paint(&self, tokens: &Tokens, painter: &mut dyn Renderer) -> (Rect, Rect) {
        let (panel, body) = self.compute();
        if self.backdrop_alpha > 0 {
            painter.fill_rect(
                self.viewport,
                Color::rgba(0, 0, 0, self.backdrop_alpha),
            );
        }
        painter.fill_rect(panel, tokens.bg.layer1);
        painter.stroke_rect(panel, 1.0, tokens.border.default);
        (panel, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> Rect {
        Rect::new(0.0, 0.0, 1000.0, 600.0)
    }

    #[test]
    fn centred_layout_centres() {
        let m = ModalPanel {
            viewport: vp(),
            width: 400.0,
            height: 200.0,
            top_offset_fraction: None,
            backdrop_alpha: 0,
            padding: 16.0,
        };
        let (panel, _) = m.compute();
        assert!((panel.left() - 300.0).abs() < 0.01);
        assert!((panel.top() - 200.0).abs() < 0.01);
    }

    #[test]
    fn top_offset_layout() {
        let m = ModalPanel {
            viewport: vp(),
            width: 400.0,
            height: 200.0,
            top_offset_fraction: Some(0.25),
            backdrop_alpha: 0,
            padding: 16.0,
        };
        let (panel, _) = m.compute();
        assert!((panel.top() - 150.0).abs() < 0.01);
    }

    #[test]
    fn body_is_padded() {
        let m = ModalPanel {
            viewport: vp(),
            width: 400.0,
            height: 200.0,
            top_offset_fraction: None,
            backdrop_alpha: 0,
            padding: 16.0,
        };
        let (panel, body) = m.compute();
        assert!((body.left() - (panel.left() + 16.0)).abs() < 0.01);
        assert!((body.right() - (panel.right() - 16.0)).abs() < 0.01);
        assert!((body.top() - (panel.top() + 16.0)).abs() < 0.01);
        assert!((body.bottom() - (panel.bottom() - 16.0)).abs() < 0.01);
    }
}
