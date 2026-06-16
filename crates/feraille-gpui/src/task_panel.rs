//! Background-task panel — small popover that lists every in-flight
//! task with its label + progress. Toggled by clicking the task region
//! of the status bar (Stage 5.c). Positioned absolutely in the bottom-
//! left of the shell window, sitting just above the status bar.
//!
//! Tasks that carry a cooperative cancel flag (file transfers —
//! docs/features/FILE_OPS.md) get a ✕ button that flips it; the
//! worker notices at its next checkpoint. Everything else stays
//! read-only visibility — answering "what is the app doing right
//! now?".

use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{ActiveTheme, Sizable as _, h_flex, v_flex};

use crate::tasks::{TaskProgress, TaskRegistry};

/// Render the task-panel popover. Caller decides visibility — this
/// returns `None` when the popover should be hidden so the caller can
/// chain `.children(...)` directly.
pub fn render_if_open(open: bool, tasks: &Rc<RefCell<TaskRegistry>>, cx: &mut App) -> Option<Div> {
    if !open {
        return None;
    }
    // Snapshot every theme colour we need *before* iterating tasks —
    // the closure passed to `map` can't borrow `cx` again while the
    // outer reference to `theme` is alive.
    let theme = cx.theme();
    let theme_fg = theme.foreground;
    let theme_muted = theme.muted_foreground;
    let theme_border = theme.border;
    let theme_primary = theme.primary;
    let theme_bg = theme.background;
    let theme_radius = theme.radius;
    let registry = tasks.borrow();

    let header_text = if registry.is_empty() {
        "Background tasks".to_string()
    } else if registry.len() == 1 {
        "1 background task".to_string()
    } else {
        format!("{} background tasks", registry.len())
    };
    let header = h_flex()
        .w_full()
        .items_center()
        .gap_2()
        .px_3()
        .py_2p5()
        .border_b_1()
        .border_color(theme_border)
        .bg(theme_muted.opacity(0.06))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme_fg)
                .child(SharedString::from(header_text)),
        );

    let body: AnyElement = if registry.is_empty() {
        div()
            .px_3()
            .py_3()
            .text_xs()
            .text_color(theme_muted)
            .child(SharedString::from("No active tasks."))
            .into_any_element()
    } else {
        let rows = registry
            .iter()
            .map(|t| {
                let label = SharedString::from(t.label.clone());
                let progress_text = match t.progress {
                    TaskProgress::Indeterminate => "Running\u{2026}".to_string(),
                    TaskProgress::Determinate(p) => format!("{:.0}%", p * 100.0),
                };
                let elapsed = humanize_secs(t.started_at.elapsed().as_secs());
                // Cancel button for tasks that carry a cooperative
                // flag (docs/features/FILE_OPS.md). Flipping the flag
                // is the whole gesture — the worker notices at its
                // next checkpoint and ends the task itself.
                let cancel = t.cancel.clone().map(|flag| {
                    gpui_component::button::Button::new(("task-cancel", t.id.raw()))
                        .icon(gpui_component::Icon::empty().path("icons/close.svg"))
                        .xsmall()
                        .on_click(move |_, _, _| {
                            flag.store(true, std::sync::atomic::Ordering::Relaxed);
                        })
                });
                v_flex()
                    .w_full()
                    .gap_1p5()
                    .py_3()
                    .px_3()
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme_fg)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(theme_muted)
                                    .child(SharedString::from(progress_text)),
                            )
                            .when_some(cancel, |this, btn| this.child(btn)),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_2()
                            .child(progress_strip(t.progress, theme_border, theme_primary))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(theme_muted)
                                    .child(SharedString::from(elapsed)),
                            ),
                    )
            })
            .collect::<Vec<_>>();
        div()
            .id("task-panel-rows")
            .max_h(px(280.0))
            .overflow_y_scroll()
            .child(v_flex().w_full().children(rows))
            .into_any_element()
    };

    let popover = v_flex()
        .absolute()
        // Sit just above the status bar. py_1 + text_xs there → ~22 DIPs;
        // 28 leaves a 6-DIP visual gap.
        .bottom(px(28.0))
        .left(px(8.0))
        .w(px(340.0))
        .rounded(theme_radius)
        .border_1()
        .border_color(theme_border)
        .bg(theme_bg)
        .shadow_lg()
        // Clicking inside the popover shouldn't bubble — the outer
        // shell uses on_mouse_down to dismiss when the click misses.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(header)
        .child(body);

    Some(popover)
}

fn progress_strip(progress: TaskProgress, track_bg: gpui::Hsla, fill_bg: gpui::Hsla) -> Div {
    let fill_frac = match progress {
        TaskProgress::Indeterminate => 0.30,
        TaskProgress::Determinate(p) => p.clamp(0.0, 1.0),
    };
    div()
        .flex_1()
        .h(px(5.0))
        .rounded(px(2.5))
        .bg(track_bg.opacity(0.5))
        .child(
            div()
                .h_full()
                .w(relative(fill_frac))
                .rounded(px(2.5))
                .bg(fill_bg),
        )
}

fn humanize_secs(s: u64) -> String {
    if s < 60 {
        format!("{}s", s)
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}
