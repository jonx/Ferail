//! FocusRing — keyboard-focus visual painted as an *overlay* by the host
//! control as the last paint layer. Stateless; pure function.
//!
//! Spec: `specs/controls/02-primitives.md` §11.

use feraille_design::Tokens;
use feraille_render::{Rect, Renderer};

/// 2-DIP inset stroke in `border.focus`, painted entirely inside `rect`
/// so gaining focus does not shift layout.
pub fn paint(rect: Rect, tokens: &Tokens, painter: &mut dyn Renderer) {
    if rect.size.width <= 4.0 || rect.size.height <= 4.0 {
        return;
    }
    painter.stroke_rect(rect, 2.0, tokens.border.focus);
}
