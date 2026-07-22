//! Bottom-of-window status bar — task count + progress indicator.
//!
//! Reads from `Shell::tasks` (a shared
//! `Rc<RefCell<TaskRegistry>>`), so its text + progress always reflect
//! the live set of background jobs.
//!
//! Layout (left → right):
//! - "<N> item(s)" entry count for the active tab's listing.
//! - "Doing X…" when exactly one task is in flight (uses the task's
//!   label). When >1 task is in flight: "N tasks running".
//! - A thin progress strip on the right: indeterminate stripe when at
//!   least one task is `Indeterminate`, otherwise determinate fill at
//!   the latest task's fraction.
//!
//! Clicking the count region toggles the (future) task panel popover;
//! today it's a no-op placeholder — the popover lands in Stage 5.c
//! alongside the toast surface.

use crate::text::TextScale as _;
use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{ActiveTheme, Sizable as _, h_flex};

use crate::tasks::{TaskProgress, TaskRegistry};

/// Click-event callback the owning Shell hands to status-bar regions
/// (task area, progress strip).
pub type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Window-level action callback with no event payload (e.g. the
/// Show-Hidden switch, which carries its own state).
pub type ActionHandler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

/// Status-bar-local byte-size formatter. Mirrors the one in
/// disk_usage.rs (1 KB = 1024 B; 1 decimal place above KB).
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

/// Render the status bar row. `entries` is the active tab's entry
/// count; `tasks` is the shared task registry. `simulated_progress`
/// is `Some(_)` only when the `--simulate-progress` CLI flag is set
/// (used to visualise the strip in screenshots without spinning up
/// real work). `on_toggle_task_panel` fires when the user clicks the
/// task region or progress strip — `None` when the host doesn't want
/// the panel (screenshots, etc.).
/// Density-of-decisions metrics surfaced by the status bar
/// (Phase 8). Each field is precomputed by the Shell so the render
/// path doesn't recompute on every paint.
#[derive(Default, Clone)]
pub struct StatusMetrics {
    pub entries: usize,
    pub selected_count: usize,
    pub selected_size: u64,
    pub total_size: u64,
    pub free_bytes: Option<u64>,
    pub volume_name: Option<SharedString>,
}

pub fn render(
    metrics: StatusMetrics,
    tasks: &Rc<RefCell<TaskRegistry>>,
    simulated_progress: Option<f32>,
    on_toggle_task_panel: Option<ClickHandler>,
    show_hidden: bool,
    on_toggle_hidden: Option<ActionHandler>,
    cx: &mut App,
) -> Div {
    // Snapshot theme colours up-front — the later progress_strip
    // call takes `&mut App`, which would otherwise conflict with the
    // outstanding `theme` borrow inside `when_some(free_label, ...)`.
    let theme_border = cx.theme().border;
    let theme_secondary = cx.theme().secondary;
    let theme_muted_fg = cx.theme().muted_foreground;
    let registry = tasks.borrow();

    // Left side: item count + selected-count / -size when there is
    // a selection. Total visible size sits after the count so a
    // glance reveals "how heavy is this folder?" without selecting.
    let entries = metrics.entries;
    let count_label = if entries == 1 {
        format!("1 item \u{00B7} {}", humanize_bytes(metrics.total_size))
    } else if entries == 0 {
        "Empty folder".to_string()
    } else if metrics.selected_count > 0 {
        format!(
            "{} of {} selected \u{00B7} {}",
            metrics.selected_count,
            entries,
            humanize_bytes(metrics.selected_size),
        )
    } else {
        format!(
            "{} items \u{00B7} {}",
            entries,
            humanize_bytes(metrics.total_size)
        )
    };

    let free_label = match (metrics.free_bytes, metrics.volume_name) {
        (Some(b), Some(name)) => Some(format!("{} free on {}", humanize_bytes(b), name)),
        (Some(b), None) => Some(format!("{} free", humanize_bytes(b))),
        _ => None,
    };

    // Middle: task summary. Only surfaced tasks count — sub-perceptual
    // work (instant clones) begins and ends inside SURFACE_DELAY and
    // never flickers a label into view.
    let surfaced = registry.iter().filter(|t| t.is_surfaced()).count();
    let task_label = if let Some(_p) = simulated_progress {
        Some(SharedString::from("Simulating progress\u{2026}"))
    } else if surfaced == 0 {
        None
    } else if let Some(t) = registry.primary().filter(|t| t.is_surfaced()) {
        // The primary (foreground-preferring) task owns the line. With
        // exactly one surfaced task, or whenever the primary is a
        // foreground op, show its label + live rate/ETA. Otherwise just
        // count the ambient background work.
        if surfaced == 1 || t.kind.is_foreground() {
            Some(SharedString::from(label_with_rate(t)))
        } else {
            Some(SharedString::from(format!("{surfaced} tasks running")))
        }
    } else {
        Some(SharedString::from(format!("{surfaced} tasks running")))
    };

    // Right side: progress strip. Determinate fraction = the
    // primary task's fraction (or the simulated value). Anything
    // indeterminate flips the strip into the indeterminate mode.
    let (visible, indeterminate, fraction) = compute_progress(&registry, simulated_progress);

    let on_toggle = on_toggle_task_panel;
    // Returns AnyElement so the two branches (id'd Stateful<Div> vs.
    // plain Div) unify.
    let make_clickable = |d: Div, region_id: &'static str| -> AnyElement {
        if let Some(cb) = on_toggle.clone() {
            d.id(region_id)
                .cursor_pointer()
                .on_click(move |evt, window, cx| cb(evt, window, cx))
                .into_any_element()
        } else {
            d.into_any_element()
        }
    };

    h_flex()
        .w_full()
        .flex_shrink_0()
        .items_center()
        .gap_4()
        .px_3()
        .py_1()
        .border_t_1()
        .border_color(theme_border)
        .bg(theme_secondary)
        .text_scale_xs()
        .text_color(theme_muted_fg)
        .child(div().flex_shrink_0().child(count_label))
        .when_some(task_label, |this, label| {
            this.child(make_clickable(
                div().flex_1().min_w_0().truncate().child(label),
                "status-bar-task-label",
            ))
        })
        .when(task_label_none(&registry, simulated_progress), |this| {
            this.child(div().flex_1())
        })
        .when(visible, |this| {
            this.child(make_clickable(
                progress_strip(indeterminate, fraction, cx),
                "status-bar-progress",
            ))
        })
        // Phase 8: free-disk-space label sits between the task
        // summary and the Show-Hidden toggle. Only rendered when we
        // could query the volume info — non-macOS / sandboxed
        // builds skip it gracefully.
        .when_some(free_label, |this, label| {
            this.child(
                div()
                    .flex_shrink_0()
                    .text_color(theme_muted_fg.opacity(0.85))
                    .child(SharedString::from(label)),
            )
        })
        // Phase 7 user ask: Show-Hidden moved out of the toolbar
        // and lives here next to the count + task summary. View-mode
        // toggle belongs alongside the rest of the status-bar state.
        .child(div().flex_shrink_0().child("Show hidden"))
        .child(
            gpui_component::switch::Switch::new("status-bar-hidden-toggle")
                .checked(show_hidden)
                .xsmall()
                .when_some(on_toggle_hidden, |sw, cb| {
                    sw.on_click(move |_state, window, cx| {
                        // Switch's on_click hands us the new bool
                        // value; we don't need it here — Shell's
                        // toggle_hidden flips its own state from
                        // whatever the current Shell value is.
                        cb(window, cx);
                    })
                }),
        )
}

fn task_label_none(registry: &TaskRegistry, simulated_progress: Option<f32>) -> bool {
    !registry.iter().any(|t| t.is_surfaced()) && simulated_progress.is_none()
}

/// Compact label for the spotlight task: its own label, plus a live
/// "· 320 MB/s · ~12s" tail when it's a transfer with a known rate. The
/// full breakdown (counts, current file) lives in the task panel.
fn label_with_rate(task: &crate::tasks::ActiveTask) -> String {
    let mut s = task.label.clone();
    if let Some(t) = &task.transfer {
        if t.bytes_per_sec >= 1.0 {
            s.push_str(&format!(
                " \u{00B7} {}/s",
                humanize_bytes(t.bytes_per_sec as u64)
            ));
        }
        if let Some(eta) = t.eta_secs {
            s.push_str(&format!(" \u{00B7} ~{}", humanize_secs(eta)));
        }
    }
    s
}

fn humanize_secs(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
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

fn compute_progress(registry: &TaskRegistry, simulated_progress: Option<f32>) -> (bool, bool, f32) {
    if let Some(p) = simulated_progress {
        if p < 0.0 {
            return (true, true, 0.0);
        }
        return (true, false, p.clamp(0.0, 1.0));
    }
    // Only surfaced tasks drive the strip — an instant clone that lives
    // <150ms never paints a bar.
    if !registry.iter().any(|t| t.is_surfaced()) {
        return (false, false, 0.0);
    }
    let any_indeterminate = registry
        .iter()
        .filter(|t| t.is_surfaced())
        .any(|t| matches!(t.progress, TaskProgress::Indeterminate));
    if any_indeterminate {
        return (true, true, 0.0);
    }
    // All determinate — show the primary task's fraction.
    let fraction = match registry
        .primary()
        .filter(|t| t.is_surfaced())
        .map(|t| t.progress)
    {
        Some(TaskProgress::Determinate(p)) => p,
        _ => 0.0,
    };
    (true, false, fraction)
}

/// Status-bar progress strip — thin 120-DIP accent strip on the right.
/// Uses `gpui_component::Progress` so the indeterminate state gets
/// the library's built-in sliding animation (we used to paint a
/// static 30%-wide fill, which read as a stuck progress bar rather
/// than ongoing work, and at certain themes the track and fill
/// merged into one flat white line).
fn progress_strip(indeterminate: bool, fraction: f32, _cx: &mut App) -> Div {
    use gpui_component::{Sizable as _, progress::Progress};
    div().flex_shrink_0().w(px(120.0)).child(
        Progress::new("status-progress")
            .xsmall()
            .loading(indeterminate)
            .value(fraction.clamp(0.0, 1.0) * 100.0),
    )
}
