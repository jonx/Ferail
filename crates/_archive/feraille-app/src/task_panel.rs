//! Task panel — small popover that lists every active background task.
//!
//! Anchored to the bottom-right of the viewport, just above the status
//! bar. Reads from `TaskRegistry` and paints one row per task: label,
//! progress bar (indeterminate fill or determinate fraction), optional
//! `[×]` cancel button. Hit-test returns either a cancel target or
//! whether the click is inside / outside the panel; the App layer
//! decides what to do with it.
//!
//! Stateless and renderer-agnostic. The App owns the open/closed flag.

use feraille_design::{FontWeight, Tokens};
use feraille_render::{Point as FPoint, Rect as FRect, Renderer, TextStyle};

use crate::tasks::{TaskId, TaskProgress, TaskRegistry};

/// Panel width in DIPs.
const PANEL_W: f32 = 320.0;
/// Per-task row height.
const ROW_H: f32 = 36.0;
/// Header strip above the rows.
const HEADER_H: f32 = 28.0;
/// Inset between panel edge and content.
const PADDING: f32 = 12.0;
/// Mirrors `STATUS_H` in `main.rs` — kept in sync by hand. The panel
/// floats above the status bar so the existing 2-DIP progress comet
/// remains visible underneath.
const STATUS_BAR_H: f32 = 24.0;
/// Gap between the bottom of the panel and the top of the status bar.
const BOTTOM_GAP: f32 = 4.0;
/// Hit area + glyph cell for the cancel button.
const CANCEL_SIZE: f32 = 20.0;
/// Per-row progress bar thickness.
const PROGRESS_BAR_H: f32 = 3.0;
/// Vertical inset for the progress bar within a row.
const PROGRESS_BAR_INSET: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitTest {
    /// Click was outside the panel — caller closes the panel.
    Outside,
    /// Click landed on the panel chrome but not on an actionable target —
    /// caller swallows the click without closing.
    Background,
    /// Click landed on a task's `[×]` button.
    Cancel(TaskId),
}

/// Compute the panel's outer rect for the given viewport and task count.
/// Always at least one row tall so the panel doesn't pop to zero size on
/// the last row's exit.
fn compute_rect(viewport: FRect, task_count: usize) -> FRect {
    let rows = task_count.max(1) as f32;
    let height = HEADER_H + ROW_H * rows + PADDING;
    let bottom = viewport.bottom() - STATUS_BAR_H - BOTTOM_GAP;
    let top = bottom - height;
    let right = viewport.right() - BOTTOM_GAP;
    let left = right - PANEL_W;
    FRect::new(left, top, PANEL_W, height)
}

/// Cancel-button rect for a row whose top-left corner is at `(panel_right, row_top)`.
fn cancel_rect(panel: FRect, row_top: f32) -> FRect {
    let cx = panel.right() - PADDING - CANCEL_SIZE / 2.0;
    let cy = row_top + ROW_H / 2.0;
    FRect::new(
        cx - CANCEL_SIZE / 2.0,
        cy - CANCEL_SIZE / 2.0,
        CANCEL_SIZE,
        CANCEL_SIZE,
    )
}

fn rect_contains(r: FRect, p: FPoint) -> bool {
    p.x >= r.left() && p.x < r.right() && p.y >= r.top() && p.y < r.bottom()
}

pub fn hit_test(viewport: FRect, tasks: &TaskRegistry, point: FPoint) -> HitTest {
    let panel = compute_rect(viewport, tasks.len());
    if !rect_contains(panel, point) {
        return HitTest::Outside;
    }
    let mut row_top = panel.top() + HEADER_H;
    for task in tasks.iter() {
        if task.cancellable {
            let btn = cancel_rect(panel, row_top);
            if rect_contains(btn, point) {
                return HitTest::Cancel(task.id);
            }
        }
        row_top += ROW_H;
    }
    HitTest::Background
}

pub fn paint(viewport: FRect, tasks: &TaskRegistry, tokens: &Tokens, painter: &mut dyn Renderer) {
    if tasks.is_empty() {
        return;
    }
    let panel = compute_rect(viewport, tasks.len());

    // Chrome — same look as ModalPanel, painted inline so we can
    // bottom-right-anchor without extending the primitive.
    painter.fill_rect(panel, tokens.bg.layer1);
    painter.stroke_rect(panel, 1.0, tokens.border.default);

    // Header
    let header = if tasks.len() == 1 {
        "1 task running".to_string()
    } else {
        format!("{} tasks running", tasks.len())
    };
    let header_style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::SemiBold,
        color: tokens.fg.primary,
    };
    painter.draw_text(
        FPoint::new(
            panel.left() + PADDING,
            panel.top() + (HEADER_H - tokens.text.sm) / 2.0 - 1.0,
        ),
        &header,
        header_style,
    );
    // Divider under header
    painter.fill_rect(
        FRect::new(
            panel.left() + PADDING,
            panel.top() + HEADER_H - 1.0,
            panel.size.width - PADDING * 2.0,
            1.0,
        ),
        tokens.border.subtle,
    );

    let label_style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::Regular,
        color: tokens.fg.primary,
    };
    let cancel_style = TextStyle {
        size: tokens.text.md,
        weight: FontWeight::Regular,
        color: tokens.fg.secondary,
    };

    let mut row_top = panel.top() + HEADER_H;
    for task in tasks.iter() {
        // Label
        painter.draw_text(
            FPoint::new(panel.left() + PADDING, row_top + 6.0),
            &task.label,
            label_style,
        );

        // Progress bar — leaves room on the right for the cancel button
        // when the task is cancellable, so the bar and the button never
        // overlap.
        let bar_right_inset = if task.cancellable {
            CANCEL_SIZE + 8.0
        } else {
            0.0
        };
        let bar = FRect::new(
            panel.left() + PADDING,
            row_top + ROW_H - PROGRESS_BAR_H - PROGRESS_BAR_INSET,
            (panel.size.width - PADDING * 2.0 - bar_right_inset).max(0.0),
            PROGRESS_BAR_H,
        );
        painter.fill_rect(bar, tokens.border.subtle);
        match task.progress {
            TaskProgress::Determinate(p) => {
                let fill_w = bar.size.width * p.clamp(0.0, 1.0);
                if fill_w > 0.0 {
                    painter.fill_rect(
                        FRect::new(bar.left(), bar.top(), fill_w, bar.size.height),
                        tokens.accent.fill,
                    );
                }
            }
            TaskProgress::Indeterminate => {
                painter.fill_rect(bar, tokens.accent.fill);
            }
        }

        // Cancel button (×) — centred in `cancel_rect`.
        if task.cancellable {
            let btn = cancel_rect(panel, row_top);
            let glyph = "\u{00D7}"; // ×
            let metrics = painter.measure_text(glyph, cancel_style);
            painter.draw_text(
                FPoint::new(
                    btn.left() + (btn.size.width - metrics.width) / 2.0,
                    btn.top() + (btn.size.height - tokens.text.md) / 2.0 - 1.0,
                ),
                glyph,
                cancel_style,
            );
        }
        row_top += ROW_H;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::TaskKind;

    fn vp() -> FRect {
        FRect::new(0.0, 0.0, 1000.0, 600.0)
    }

    #[test]
    fn click_outside_returns_outside() {
        let mut r = TaskRegistry::new();
        r.begin(TaskKind::Enumeration, "Reading folder…", true);
        let hit = hit_test(vp(), &r, FPoint::new(0.0, 0.0));
        assert_eq!(hit, HitTest::Outside);
    }

    #[test]
    fn click_inside_chrome_returns_background() {
        let mut r = TaskRegistry::new();
        r.begin(TaskKind::Enumeration, "Reading folder…", true);
        let panel = compute_rect(vp(), 1);
        // Click in the header strip.
        let p = FPoint::new(panel.left() + 20.0, panel.top() + 4.0);
        assert_eq!(hit_test(vp(), &r, p), HitTest::Background);
    }

    #[test]
    fn click_on_cancel_button_returns_cancel() {
        let mut r = TaskRegistry::new();
        let id = r.begin(TaskKind::Enumeration, "Reading folder…", true);
        let panel = compute_rect(vp(), 1);
        let btn = cancel_rect(panel, panel.top() + HEADER_H);
        let centre = FPoint::new(
            btn.left() + btn.size.width / 2.0,
            btn.top() + btn.size.height / 2.0,
        );
        assert_eq!(hit_test(vp(), &r, centre), HitTest::Cancel(id));
    }

    #[test]
    fn non_cancellable_row_swallows_click_as_background() {
        let mut r = TaskRegistry::new();
        r.begin(TaskKind::MagicPrefetch, "Indexing files…", false);
        let panel = compute_rect(vp(), 1);
        let btn = cancel_rect(panel, panel.top() + HEADER_H);
        let centre = FPoint::new(
            btn.left() + btn.size.width / 2.0,
            btn.top() + btn.size.height / 2.0,
        );
        // No cancel button drawn for non-cancellable tasks.
        assert_eq!(hit_test(vp(), &r, centre), HitTest::Background);
    }
}
