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

use crate::text::TextScale as _;
use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{ActiveTheme, Sizable as _, h_flex, v_flex};

use crate::tasks::{Outcome, TaskProgress, TaskRegistry};

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
    let theme_danger = theme.danger;
    let registry = tasks.borrow();

    // Only show tasks that have lived past SURFACE_DELAY — instant
    // clones never flicker a row in. (docs/features/FILE_OPS.md)
    let surfaced: Vec<&crate::tasks::ActiveTask> =
        registry.iter().filter(|t| t.is_surfaced()).collect();
    let header_text = if surfaced.is_empty() {
        tr!("Background tasks")
    } else {
        trn!("{n} background task", "{n} background tasks", surfaced.len())
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
                .text_scale_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme_fg)
                .child(header_text),
        );

    let body: AnyElement = if surfaced.is_empty() {
        div()
            .px_3()
            .py_3()
            .text_scale_xs()
            .text_color(theme_muted)
            .child(tr!("No active tasks."))
            .into_any_element()
    } else {
        let rows = surfaced
            .iter()
            .map(|t| {
                let label = SharedString::from(t.label.clone());
                let progress_text = match t.progress {
                    TaskProgress::Indeterminate => tr!("Running\u{2026}").to_string(),
                    TaskProgress::Determinate(p) => format!("{:.0}%", p * 100.0),
                };
                let elapsed = humanize_secs(t.started_at.elapsed().as_secs());
                // Rich transfer detail: counts · bytes · rate · ETA, plus
                // the file in flight. Only present for copy/move tasks.
                let detail: Option<String> = t.transfer.as_ref().map(transfer_detail);
                let current: Option<String> = t
                    .transfer
                    .as_ref()
                    .filter(|s| !s.current.is_empty())
                    .map(|s| s.current.clone());
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
                                    .text_scale_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme_fg)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_scale_xs()
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
                                    .text_scale_xs()
                                    .text_color(theme_muted)
                                    .child(SharedString::from(elapsed)),
                            ),
                    )
                    .when_some(detail, |this, d| {
                        this.child(
                            div()
                                .w_full()
                                .text_scale_xs()
                                .text_color(theme_muted)
                                .child(SharedString::from(d)),
                        )
                    })
                    .when_some(current, |this, name| {
                        this.child(
                            div()
                                .w_full()
                                .min_w_0()
                                .truncate()
                                .text_scale_xs()
                                .text_color(theme_muted.opacity(0.8))
                                .child(SharedString::from(name)),
                        )
                    })
            })
            .collect::<Vec<_>>();
        div()
            .id("task-panel-rows")
            .w_full()
            .max_h(px(280.0))
            .overflow_y_scroll()
            .child(v_flex().w_full().children(rows))
            .into_any_element()
    };

    // "Recent" — a dimmed list of the just-finished foreground tasks
    // (copies, moves, searches, scans, trashes). Omitted entirely when
    // there is no history so the panel doesn't grow an empty heading.
    let recent: Option<Div> = if registry.has_history() {
        let rows = registry
            .completed()
            .map(|c| {
                let (glyph, glyph_color) = match &c.outcome {
                    Outcome::Completed => ("\u{2713}", theme_muted),
                    Outcome::Cancelled => ("\u{2298}", theme_muted),
                    Outcome::Failed(_) => ("\u{26A0}", theme_danger),
                };
                // A failed task appends its reason after the label so the
                // user sees *why* without another surface.
                let label = match &c.outcome {
                    Outcome::Failed(msg) => {
                        tr!("{label} \u{2014} {detail}", label = c.label, detail = msg).to_string()
                    }
                    _ => c.label.clone(),
                };
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .py_1p5()
                    .px_3()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_scale_xs()
                            .text_color(glyph_color)
                            .child(SharedString::from(glyph)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_scale_xs()
                            .text_color(theme_muted)
                            .child(SharedString::from(label)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_scale_xs()
                            .text_color(theme_muted.opacity(0.7))
                            .child(SharedString::from(humanize_secs(c.elapsed.as_secs()))),
                    )
            })
            .collect::<Vec<_>>();
        Some(
            v_flex()
                .w_full()
                .border_t_1()
                .border_color(theme_border)
                .child(
                    div()
                        .px_3()
                        .py_1p5()
                        .text_scale_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme_muted)
                        .child(tr!("Recent")),
                )
                .child(
                    div()
                        .id("task-panel-recent")
                        .max_h(px(160.0))
                        .overflow_y_scroll()
                        .child(v_flex().w_full().children(rows)),
                ),
        )
    } else {
        None
    };

    let popover = v_flex()
        .absolute()
        // Sit just above the status bar. py_1 + text_xs there → ~22 DIPs;
        // 28 leaves a 6-DIP visual gap.
        .bottom(px(28.0))
        .left(px(8.0))
        // Wide enough that a transfer's full detail line ("5,423 of
        // 6,964 items · 8.0 GB of 164.2 GB · 51.9 MB/s · ~51m") fits on
        // one line instead of wrapping.
        .w(px(430.0))
        .rounded(theme_radius)
        .border_1()
        .border_color(theme_border)
        .bg(theme_bg)
        .shadow_lg()
        // Clicking inside the popover shouldn't bubble — the outer
        // shell uses on_mouse_down to dismiss when the click misses.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(header)
        .child(body)
        .when_some(recent, |this, r| this.child(r));

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
        // Skip a zero remainder — rounded ETAs land on whole minutes
        // and "51m" reads better than "51m 0s".
        match (s / 60, s % 60) {
            (m, 0) => format!("{m}m"),
            (m, s) => format!("{m}m {s}s"),
        }
    } else {
        match (s / 3600, (s % 3600) / 60) {
            (h, 0) => format!("{h}h"),
            (h, m) => format!("{h}h {m}m"),
        }
    }
}

fn humanize_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut s = b as f64;
    let mut u = 0;
    while s >= 1024.0 && u + 1 < UNITS.len() {
        s /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} {}", b, UNITS[u])
    } else {
        format!("{:.1} {}", s, UNITS[u])
    }
}

/// One-line breakdown for a transfer row: "1,204 of 3,418 · 4.2/19 GB ·
/// 320 MB/s · ~12s". Pieces that aren't meaningful yet (no rate while
/// ramping, no ETA right after an instant clone) are omitted.
fn transfer_detail(s: &crate::tasks::TransferStats) -> String {
    let mut parts: Vec<String> = Vec::new();
    if s.items_total > 0 {
        parts.push(
            trn!(
                "{done} of {n} item",
                "{done} of {n} items",
                s.items_total,
                done = s.items_done
            )
            .to_string(),
        );
    }
    if s.bytes_total > 0 {
        parts.push(
            tr!(
                "{done} of {total}",
                done = humanize_bytes(s.bytes_done),
                total = humanize_bytes(s.bytes_total)
            )
            .to_string(),
        );
    }
    if s.bytes_per_sec >= 1.0 {
        parts.push(format!("{}/s", humanize_bytes(s.bytes_per_sec as u64)));
    }
    if let Some(eta) = s.eta_secs {
        parts.push(format!("~{}", humanize_secs(eta)));
    }
    parts.join(" \u{00B7} ")
}
