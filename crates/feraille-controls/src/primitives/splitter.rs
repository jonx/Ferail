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
    hovered: bool,
}

struct DragState {
    /// Pointer offset within the hit zone at drag-start.
    pointer_offset: f32,
}

impl Splitter {
    pub fn new(min: f32, max: f32) -> Self {
        Self { min, max, drag: None, hovered: false }
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn is_hovered(&self) -> bool {
        self.hovered
    }

    /// 6-DIP-wide hit zone centred on `position`.
    pub fn hit_rect(position: f32, container: Rect) -> Rect {
        Rect::new(position - HIT_WIDTH / 2.0, container.top(), HIT_WIDTH, container.size.height)
    }

    /// Update hover state from the latest cursor position. Returns true
    /// if the hover state changed (caller can request redraw on change).
    /// Pass `None` when the cursor leaves the window or the splitter's
    /// container.
    pub fn update_hover(&mut self, position: f32, container: Rect, point: Option<Point>) -> bool {
        let next = point
            .map(|p| Self::hit_rect(position, container).contains(p))
            .unwrap_or(false);
        let changed = next != self.hovered;
        self.hovered = next;
        changed
    }

    /// Paint the rule at `position` over the full container height.
    /// Idle: 1-DIP subtle rule.
    /// Hovered or dragging: stronger colour + a small grab-handle of
    /// three dots in the vertical centre so the user can see where to
    /// click. Spec §7 keeps the visible rule narrow; the dots are an
    /// affordance hint rather than a wider rule.
    pub fn paint(&self, position: f32, container: Rect, tokens: &Tokens, painter: &mut dyn Renderer) {
        let rule_x = position - RULE_WIDTH / 2.0;
        let active = self.is_dragging() || self.hovered;
        let rule_color = if self.is_dragging() {
            tokens.border.focus
        } else if self.hovered {
            tokens.border.default
        } else {
            tokens.border.subtle
        };
        painter.fill_rect(
            Rect::new(rule_x, container.top(), RULE_WIDTH, container.size.height),
            rule_color,
        );
        if active {
            let handle_color = if self.is_dragging() {
                tokens.border.focus
            } else {
                tokens.fg.secondary
            };
            let cy = container.top() + container.size.height / 2.0;
            let dot = 2.0_f32;
            for i in -1..=1 {
                let dy = i as f32 * (dot + 3.0);
                painter.fill_rect(
                    Rect::new(position - dot / 2.0, cy + dy - dot / 2.0, dot, dot),
                    handle_color,
                );
            }
        }
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
