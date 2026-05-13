//! Drawing atoms shared across controls and dialogs.
//!
//! The soft renderer (`feraille-render::soft`) only exposes rectangular
//! primitives. These helpers compose curves from rectangles — a center
//! band plus quarter-circle caps approximated row-by-row as
//! `√(r² − dy²)`-wide horizontal slabs. At the radii we use (≤16 DIPs
//! for cards, ≤12 for controls) it reads as a smooth curve at 1× and 2×
//! display scale.
//!
//! Naming follows the design brief's atomic vocabulary: every UI surface
//! in the app is a composition of `paint_card`, `paint_separator`,
//! `fill_rounded_rect`, and `fill_circle`. No raw `fill_rect` for
//! anything that should look rounded.

use feraille_design::{Color, Tokens};
use feraille_render::{Point, Rect, Renderer};

/// Fill a rounded rect. Radius is clamped to half of the smaller edge,
/// so passing a very large radius produces a capsule.
pub fn fill_rounded_rect(
    renderer: &mut dyn Renderer,
    rect: Rect,
    radius: f32,
    color: Color,
) {
    let r = radius.min(rect.size.width / 2.0).min(rect.size.height / 2.0);
    if r <= 0.5 {
        renderer.fill_rect(rect, color);
        return;
    }
    // Center vertical band — the rect minus its top and bottom corner caps.
    renderer.fill_rect(
        Rect::new(
            rect.left(),
            rect.top() + r,
            rect.size.width,
            rect.size.height - 2.0 * r,
        ),
        color,
    );
    // Top and bottom horizontal bands between the corner inset.
    renderer.fill_rect(
        Rect::new(rect.left() + r, rect.top(), rect.size.width - 2.0 * r, r),
        color,
    );
    renderer.fill_rect(
        Rect::new(
            rect.left() + r,
            rect.bottom() - r,
            rect.size.width - 2.0 * r,
            r,
        ),
        color,
    );
    // Four quarter-circle corner caps. Each row gets stretched to the
    // curve width for that row.
    let steps = r.ceil() as i32;
    for i in 0..steps {
        let y = i as f32;
        let dy = r - y - 0.5;
        let dx = (r * r - dy * dy).max(0.0).sqrt();
        renderer.fill_rect(
            Rect::new(rect.left() + r - dx, rect.top() + y, dx, 1.0),
            color,
        );
        renderer.fill_rect(Rect::new(rect.right() - r, rect.top() + y, dx, 1.0), color);
        renderer.fill_rect(
            Rect::new(rect.left() + r - dx, rect.bottom() - 1.0 - y, dx, 1.0),
            color,
        );
        renderer.fill_rect(
            Rect::new(rect.right() - r, rect.bottom() - 1.0 - y, dx, 1.0),
            color,
        );
    }
}

/// Stroke a rounded rect by painting a slightly larger border-coloured
/// rounded fill, then the inner fill on top. Cheaper than tracing the
/// outline on this renderer and matches the "border is decorative,
/// not structural" aesthetic from the design tokens.
pub fn stroke_rounded_rect(
    renderer: &mut dyn Renderer,
    rect: Rect,
    radius: f32,
    width: f32,
    border: Color,
    fill: Color,
) {
    fill_rounded_rect(renderer, rect, radius, border);
    let inset = Rect::new(
        rect.left() + width,
        rect.top() + width,
        (rect.size.width - 2.0 * width).max(0.0),
        (rect.size.height - 2.0 * width).max(0.0),
    );
    fill_rounded_rect(renderer, inset, (radius - width).max(0.0), fill);
}

/// Filled circle. Same row-by-row strategy as the corner caps but
/// mirrored over the horizontal axis.
pub fn fill_circle(
    renderer: &mut dyn Renderer,
    center: Point,
    radius: f32,
    color: Color,
) {
    if radius <= 0.5 {
        return;
    }
    let r = radius;
    let steps = r.ceil() as i32;
    for i in 0..(steps * 2) {
        let y = i as f32;
        let dy = y - r + 0.5;
        let dx = (r * r - dy * dy).max(0.0).sqrt();
        if dx <= 0.0 {
            continue;
        }
        renderer.fill_rect(
            Rect::new(center.x - dx, center.y - r + y, dx * 2.0, 1.0),
            color,
        );
    }
}

/// Stroke a circle by painting a slightly larger filled disc, then a
/// 1-DIP-inset filled disc on top with the inner fill colour.
pub fn stroke_circle(
    renderer: &mut dyn Renderer,
    center: Point,
    radius: f32,
    width: f32,
    border: Color,
    fill: Color,
) {
    fill_circle(renderer, center, radius, border);
    fill_circle(renderer, center, (radius - width).max(0.0), fill);
}

/// The canonical card: `radius.md` rounded fill on `bg.layer2` with a
/// 1-DIP `border.subtle` outline. Every grouped settings surface, every
/// preview tile, every popover body should be a `paint_card` — never a
/// raw `fill_rect`.
pub fn paint_card(renderer: &mut dyn Renderer, tokens: &Tokens, rect: Rect) {
    stroke_rounded_rect(
        renderer,
        rect,
        tokens.radius.md,
        1.0,
        tokens.border.subtle,
        tokens.bg.layer2,
    );
}

/// 1-DIP hairline separator between rows inside a card. The card's
/// own border draws the outer perimeter, so this fills only the inner
/// span between two rows.
pub fn paint_separator(
    renderer: &mut dyn Renderer,
    tokens: &Tokens,
    y: f32,
    left: f32,
    right: f32,
) {
    renderer.fill_rect(
        Rect::new(left, y, (right - left).max(0.0), 1.0),
        tokens.border.subtle,
    );
}

/// Vertical centre of `text_size_px` inside `rect`. Returns the y at
/// which to call `draw_text` so the visual centre of the glyph row sits
/// at the rect's vertical centre. Hand-tuned `-1.0` matches the small
/// optical-baseline adjustment used everywhere else in the app.
pub fn text_y_center(rect: Rect, text_size_px: f32) -> f32 {
    rect.top() + (rect.size.height - text_size_px) / 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use feraille_design::Color;

    /// Tiny mock renderer that just records the fill_rect calls so we can
    /// verify the helpers don't degenerate into nothing for sane inputs.
    struct CountingRenderer {
        calls: usize,
    }

    impl Renderer for CountingRenderer {
        fn viewport(&self) -> feraille_render::Size {
            feraille_render::Size::new(1000.0, 1000.0)
        }
        fn scale_factor(&self) -> f32 {
            1.0
        }
        fn fill_rect(&mut self, _rect: Rect, _color: Color) {
            self.calls += 1;
        }
        fn stroke_rect(&mut self, _rect: Rect, _w: f32, _color: Color) {}
        fn draw_text(
            &mut self,
            _pos: Point,
            _t: &str,
            _s: feraille_render::TextStyle,
        ) {
        }
        fn measure_text(
            &self,
            _t: &str,
            _s: feraille_render::TextStyle,
        ) -> feraille_render::Size {
            feraille_render::Size::new(0.0, 0.0)
        }
        fn draw_bitmap(&mut self, _r: Rect, _b: &feraille_render::Bitmap) {}
        fn push_clip(&mut self, _r: Rect) {}
        fn pop_clip(&mut self) {}
    }

    #[test]
    fn rounded_rect_with_zero_radius_emits_one_fill() {
        let mut r = CountingRenderer { calls: 0 };
        fill_rounded_rect(&mut r, Rect::new(0.0, 0.0, 100.0, 40.0), 0.0, Color::rgb(0, 0, 0));
        assert_eq!(r.calls, 1);
    }

    #[test]
    fn rounded_rect_with_radius_emits_multiple_fills() {
        let mut r = CountingRenderer { calls: 0 };
        fill_rounded_rect(&mut r, Rect::new(0.0, 0.0, 100.0, 40.0), 8.0, Color::rgb(0, 0, 0));
        // Three bands + 4*8 = 32 quarter-circle rows = 35 minimum.
        assert!(r.calls >= 35);
    }

    #[test]
    fn text_y_center_is_within_rect() {
        let rect = Rect::new(10.0, 100.0, 200.0, 32.0);
        let y = text_y_center(rect, 14.0);
        assert!(y >= rect.top() - 2.0 && y + 14.0 <= rect.bottom() + 2.0);
    }
}
