//! Splitter — vertical drag handle dividing two horizontal panes.
//! 1-DIP visible rule, 6-DIP hit area (per spec §7 — visible rule and
//! hit-target deliberately differ to make grabbing reliable).
//!
//! Iter-2 ships vertical splitters only; horizontal splitter for
//! preview-pane-below-list lands later.
//!
//! Spec: `specs/controls/02-primitives.md` §7.

use feraille_design::Tokens;
use feraille_render::{Point, Rect, Renderer};

const HIT_WIDTH: f32 = 6.0;
const RULE_WIDTH: f32 = 1.0;

pub struct Splitter {
    /// Min position (DIPs from container left).
    pub min: f32,
    /// Max position (DIPs from container left).
    pub max: f32,
    drag: Option<DragState>,
}

struct DragState {
    /// Pointer offset within the hit zone at drag-start.
    pointer_offset: f32,
}

impl Splitter {
    pub fn new(min: f32, max: f32) -> Self {
        Self { min, max, drag: None }
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// 6-DIP-wide hit zone centred on `position`.
    pub fn hit_rect(position: f32, container: Rect) -> Rect {
        Rect::new(position - HIT_WIDTH / 2.0, container.top(), HIT_WIDTH, container.size.height)
    }

    /// Paint the 1-DIP rule at `position` over the full container height.
    pub fn paint(&self, position: f32, container: Rect, tokens: &Tokens, painter: &mut dyn Renderer) {
        let rule_x = position - RULE_WIDTH / 2.0;
        let color = if self.is_dragging() {
            tokens.border.focus
        } else {
            tokens.border.subtle
        };
        painter.fill_rect(
            Rect::new(rule_x, container.top(), RULE_WIDTH, container.size.height),
            color,
        );
    }

    pub fn begin_drag_at(&mut self, position: f32, container: Rect, point: Point) -> bool {
        let hit = Self::hit_rect(position, container);
        if hit.contains(point) {
            self.drag = Some(DragState { pointer_offset: point.x - position });
            true
        } else {
            false
        }
    }

    pub fn end_drag(&mut self) {
        self.drag = None;
    }

    /// Compute the new splitter position for a pointer at `point`, clamped
    /// to `[min, max]`. Returns `None` if not dragging.
    pub fn position_for_drag(&self, point: Point) -> Option<f32> {
        let drag = self.drag.as_ref()?;
        let raw = point.x - drag.pointer_offset;
        Some(raw.clamp(self.min, self.max))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_rect_is_six_dips_wide() {
        let r = Splitter::hit_rect(200.0, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert!((r.size.width - 6.0).abs() < 0.01);
        assert!((r.left() - 197.0).abs() < 0.01);
    }

    #[test]
    fn drag_clamps_to_min_max() {
        let mut s = Splitter::new(120.0, 480.0);
        assert!(s.begin_drag_at(
            200.0,
            Rect::new(0.0, 0.0, 800.0, 600.0),
            Point::new(200.0, 100.0),
        ));
        // Drag way left.
        let pos = s.position_for_drag(Point::new(50.0, 100.0)).unwrap();
        assert!((pos - 120.0).abs() < 0.01);
        // Drag way right.
        let pos = s.position_for_drag(Point::new(900.0, 100.0)).unwrap();
        assert!((pos - 480.0).abs() < 0.01);
    }
}
