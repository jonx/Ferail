//! Reusable two-tone scrub slider used by compact Ferail toolbars.
//!
//! This deliberately has no thumb: the bright segment is the current value
//! and the dim segment is the remaining range. Hosts retain their own value
//! and drag state, which lets them decide when a live scrub should be
//! persisted (icon size) or merely recomputed in memory (Similar Images).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Bounds, Div, ElementId, InteractiveElement as _, ParentElement as _, Pixels, Stateful,
    Styled as _, div, px, relative,
};
use gpui_component::ActiveTheme as _;

pub(crate) const TRACK_HIT_HEIGHT: f32 = 20.0;
const TRACK_HEIGHT: f32 = 6.0;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ScrubRange {
    min: f32,
    max: f32,
    step: f32,
}

impl ScrubRange {
    pub(crate) const fn new(min: f32, max: f32, step: f32) -> Self {
        Self { min, max, step }
    }

    pub(crate) fn fraction(self, value: f32) -> f32 {
        if self.max <= self.min {
            return 0.0;
        }
        ((value.clamp(self.min, self.max) - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
    }

    pub(crate) fn value_at(self, bounds: Bounds<Pixels>, x: Pixels) -> Option<f32> {
        let width = bounds.size.width.as_f32();
        if width <= 0.0 || self.max <= self.min || self.step <= 0.0 {
            return None;
        }
        let fraction = ((x.as_f32() - bounds.origin.x.as_f32()) / width).clamp(0.0, 1.0);
        let value = self.min + fraction * (self.max - self.min);
        Some(((value / self.step).round() * self.step).clamp(self.min, self.max))
    }
}

/// Paint the shared track. The caller supplies width/flex sizing, bounds
/// capture, and interaction callbacks because those policies differ between
/// a persisted global preference and a tab-local result control.
pub(crate) fn track(
    id: impl Into<ElementId>,
    fraction: f32,
    disabled: bool,
    cx: &App,
) -> Stateful<Div> {
    let filled = if disabled {
        cx.theme().muted_foreground.opacity(0.35)
    } else {
        cx.theme().foreground
    };
    let rest = cx.theme().muted_foreground.opacity(0.25);

    div()
        .id(id)
        .relative()
        .flex_shrink_0()
        .h(px(TRACK_HIT_HEIGHT))
        .when(!disabled, |this| this.cursor_pointer())
        .child(
            div()
                .absolute()
                .top(px(TRACK_HIT_HEIGHT / 2.0 - TRACK_HEIGHT / 2.0))
                .left_0()
                .right_0()
                .h(px(TRACK_HEIGHT))
                .rounded_full()
                .bg(rest)
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left_0()
                        .w(relative(fraction.clamp(0.0, 1.0)))
                        // Keep a visible round nub at the minimum without
                        // introducing a separate draggable thumb.
                        .min_w(px(TRACK_HEIGHT))
                        .rounded_full()
                        .bg(filled),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    #[test]
    fn range_maps_track_positions_and_snaps() {
        let range = ScrubRange::new(0.0, 12.0, 1.0);
        let bounds = Bounds::new(point(px(20.0), px(0.0)), size(px(120.0), px(20.0)));

        assert_eq!(range.value_at(bounds, px(20.0)), Some(0.0));
        assert_eq!(range.value_at(bounds, px(75.0)), Some(6.0));
        assert_eq!(range.value_at(bounds, px(140.0)), Some(12.0));
        assert_eq!(range.fraction(6.0), 0.5);
    }
}
