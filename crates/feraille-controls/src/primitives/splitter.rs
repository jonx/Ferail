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

/// Hit zone for begin-drag and hover detection. Generous enough that
/// users don't have to aim — Finder uses ~10 DIPs.
const HIT_WIDTH: f32 = 10.0;
/// Visible rule width when idle.
const RULE_WIDTH: f32 = 1.0;
/// Visible rule width while dragging — slightly thicker so it reads as
/// "active" without becoming visually heavy.
const RULE_WIDTH_DRAGGING: f32 = 2.0;
/// Diameter of each grab-handle dot. Bigger than the original 2 DIPs
/// so the handle reads as grabbable from arm's length.
const HANDLE_DOT: f32 = 3.0;
/// Vertical spacing between handle dots, centre-to-centre.
const HANDLE_DOT_GAP: f32 = 4.0;
/// Number of grab-handle dots stacked vertically. 5 reads clearly as a
/// drag-affordance without monopolising the visual field.
const HANDLE_DOT_COUNT: i32 = 5;

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
    /// Hovered: full hit-zone gets a faint fill so the grabbable area
    /// is obvious, plus a stronger rule colour and a grab-handle dot
    /// stack in the vertical centre.
    /// Dragging: thicker rule + accent-coloured handle.
    pub fn paint(&self, position: f32, container: Rect, tokens: &Tokens, painter: &mut dyn Renderer) {
        let active = self.is_dragging() || self.hovered;

        // Hover affordance: a faint fill over the full hit-zone width
        // so the user sees the grabbable area, not just the rule.
        if self.hovered && !self.is_dragging() {
            let mut tint = tokens.bg.layer3;
            tint.a = 180; // semi-translucent so it reads as overlay, not chrome
            painter.fill_rect(
                Rect::new(
                    position - HIT_WIDTH / 2.0,
                    container.top(),
                    HIT_WIDTH,
                    container.size.height,
                ),
                tint,
            );
        }

        let rule_w = if self.is_dragging() {
            RULE_WIDTH_DRAGGING
        } else {
            RULE_WIDTH
        };
        let rule_color = if self.is_dragging() {
            tokens.border.focus
        } else if self.hovered {
            tokens.border.default
        } else {
            tokens.border.subtle
        };
        painter.fill_rect(
            Rect::new(position - rule_w / 2.0, container.top(), rule_w, container.size.height),
            rule_color,
        );

        if active {
            let handle_color = if self.is_dragging() {
                tokens.border.focus
            } else {
                tokens.fg.secondary
            };
            let cy = container.top() + container.size.height / 2.0;
            let total_h =
                (HANDLE_DOT_COUNT - 1) as f32 * HANDLE_DOT_GAP + HANDLE_DOT;
            let mut dy = -total_h / 2.0 + HANDLE_DOT / 2.0;
            for _ in 0..HANDLE_DOT_COUNT {
                painter.fill_rect(
                    Rect::new(
                        position - HANDLE_DOT / 2.0,
                        cy + dy - HANDLE_DOT / 2.0,
                        HANDLE_DOT,
                        HANDLE_DOT,
                    ),
                    handle_color,
                );
                dy += HANDLE_DOT_GAP;
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
    fn hit_rect_centred_on_position() {
        let r = Splitter::hit_rect(200.0, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert!((r.size.width - HIT_WIDTH).abs() < 0.01);
        assert!((r.left() - (200.0 - HIT_WIDTH / 2.0)).abs() < 0.01);
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
