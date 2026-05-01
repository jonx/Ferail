//! Scrollbar — vertical thumb proportional to viewport/content. Iter-2
//! is always-visible (auto-hide is iter-3 polish). The host owns
//! `scroll_offset`; this primitive only paints + handles thumb drag.
//!
//! Spec: `specs/controls/02-primitives.md` §6.

use feraille_design::Tokens;
use feraille_render::{Point, Rect, Renderer};

pub struct Scrollbar {
    pub width: f32,
    pub min_thumb_height: f32,
    drag: Option<DragState>,
}

struct DragState {
    /// Pointer offset inside the thumb at drag start (so the thumb stays
    /// "pinned" relative to the cursor while dragging).
    thumb_local_y: f32,
}

impl Default for Scrollbar {
    fn default() -> Self {
        Self::new()
    }
}

impl Scrollbar {
    pub fn new() -> Self {
        Self { width: 10.0, min_thumb_height: 24.0, drag: None }
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn paint(
        &self,
        bounds: Rect,
        content_size: f32,
        viewport_size: f32,
        scroll_offset: f32,
        tokens: &Tokens,
        painter: &mut dyn Renderer,
    ) {
        if content_size <= viewport_size + 0.5 {
            return;
        }
        let thumb = self.thumb_rect(bounds, content_size, viewport_size, scroll_offset);
        // No track fill — Zed-style. `fg.secondary` reads cleanly on
        // both light and dark surfaces; `border.default` was too faint
        // on white.
        painter.fill_rect(thumb, tokens.fg.secondary);
    }

    fn thumb_rect(
        &self,
        bounds: Rect,
        content_size: f32,
        viewport_size: f32,
        scroll_offset: f32,
    ) -> Rect {
        let h_ratio = (viewport_size / content_size.max(1.0)).clamp(0.0, 1.0);
        let track_h = bounds.size.height;
        let thumb_h = (track_h * h_ratio).max(self.min_thumb_height).min(track_h);
        let max_scroll = (content_size - viewport_size).max(1e-3);
        let progress = (scroll_offset / max_scroll).clamp(0.0, 1.0);
        let max_thumb_y = (track_h - thumb_h).max(0.0);
        let thumb_y = bounds.top() + max_thumb_y * progress;
        let pad = 2.0_f32.min(self.width / 4.0);
        let thumb_x = bounds.left() + pad;
        let thumb_w = (self.width - 2.0 * pad).max(2.0);
        Rect::new(thumb_x, thumb_y, thumb_w, thumb_h)
    }

    /// Try to start a drag if `point` lies on the thumb. Returns whether
    /// the drag started.
    pub fn begin_drag_at(
        &mut self,
        bounds: Rect,
        point: Point,
        content_size: f32,
        viewport_size: f32,
        scroll_offset: f32,
    ) -> bool {
        if content_size <= viewport_size + 0.5 {
            return false;
        }
        let thumb = self.thumb_rect(bounds, content_size, viewport_size, scroll_offset);
        if thumb.contains(point) {
            self.drag = Some(DragState { thumb_local_y: point.y - thumb.top() });
            true
        } else {
            false
        }
    }

    pub fn end_drag(&mut self) {
        self.drag = None;
    }

    /// Given a current pointer y in DIPs, compute the new scroll_offset.
    /// Caller asserts `is_dragging()`.
    pub fn scroll_offset_for_drag(
        &self,
        bounds: Rect,
        client_y: f32,
        content_size: f32,
        viewport_size: f32,
    ) -> Option<f32> {
        let drag = self.drag.as_ref()?;
        let h_ratio = (viewport_size / content_size.max(1.0)).clamp(0.0, 1.0);
        let track_h = bounds.size.height;
        let thumb_h = (track_h * h_ratio).max(self.min_thumb_height).min(track_h);
        let max_thumb_y = (track_h - thumb_h).max(0.0);
        if max_thumb_y <= 0.0 {
            return Some(0.0);
        }
        let target_thumb_top = client_y - drag.thumb_local_y;
        let progress = ((target_thumb_top - bounds.top()) / max_thumb_y).clamp(0.0, 1.0);
        let max_scroll = (content_size - viewport_size).max(0.0);
        Some(progress * max_scroll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> Rect {
        Rect::new(0.0, 0.0, 10.0, 200.0)
    }

    #[test]
    fn no_thumb_when_content_fits() {
        let mut sb = Scrollbar::new();
        // begin_drag_at returns false when no overflow.
        assert!(!sb.begin_drag_at(track(), Point::new(5.0, 50.0), 100.0, 200.0, 0.0));
    }

    #[test]
    fn thumb_at_top_when_offset_zero() {
        let sb = Scrollbar::new();
        let thumb = sb.thumb_rect(track(), 1000.0, 200.0, 0.0);
        assert!((thumb.top() - 0.0).abs() < 0.01);
    }

    #[test]
    fn thumb_at_bottom_when_max_offset() {
        let sb = Scrollbar::new();
        let thumb = sb.thumb_rect(track(), 1000.0, 200.0, 800.0);
        // thumb.bottom() should equal track bottom.
        assert!((thumb.bottom() - 200.0).abs() < 0.5);
    }

    #[test]
    fn drag_to_end_yields_max_scroll() {
        let mut sb = Scrollbar::new();
        // Begin drag at top of thumb.
        let thumb_at_zero = sb.thumb_rect(track(), 1000.0, 200.0, 0.0);
        let started = sb.begin_drag_at(
            track(),
            Point::new(5.0, thumb_at_zero.top() + 1.0),
            1000.0,
            200.0,
            0.0,
        );
        assert!(started);
        // Drag pointer all the way to bottom.
        let new_offset = sb
            .scroll_offset_for_drag(track(), 200.0, 1000.0, 200.0)
            .unwrap();
        // max scroll = 800 (content - viewport).
        assert!((new_offset - 800.0).abs() < 1.0);
    }
}
