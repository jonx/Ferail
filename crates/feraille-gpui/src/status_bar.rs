//! Bottom-of-window status bar — task count + progress indicator.
//!
//! Harvest Stage 5.b. Replaces the soft-renderer status arm in
//! `feraille-app/src/main.rs`. Reads from `Shell::tasks` (a shared
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

use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{ActiveTheme, Sizable as _, h_flex};

use crate::tasks::{TaskProgress, TaskRegistry};

/// Render the status bar row. `entries` is the active tab's entry
/// count; `tasks` is the shared task registry. `simulated_progress`
/// is `Some(_)` only when the `--simulate-progress` CLI flag is set
/// (used to visualise the strip in screenshots without spinning up
/// real work). `on_toggle_task_panel` fires when the user clicks the
/// task region or progress strip — `None` when the host doesn't want
/// the panel (screenshots, etc.).
pub fn render(
    entries: usize,
    tasks: &Rc<RefCell<TaskRegistry>>,
    simulated_progress: Option<f32>,
    on_toggle_task_panel: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    show_hidden: bool,
    on_toggle_hidden: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    cx: &mut App,
) -> Div {
    let theme = cx.theme();
    let registry = tasks.borrow();

    // Left side: entry count.
    let count_label = if entries == 1 {
        "1 item".to_string()
    } else {
        format!("{} items", entries)
    };

    // Middle: task summary.
    let task_label = if let Some(_p) = simulated_progress {
        Some(SharedString::from("Simulating progress\u{2026}"))
    } else if registry.is_empty() {
        None
    } else if registry.len() == 1 {
        registry
            .primary()
            .map(|t| SharedString::from(t.label.clone()))
    } else {
        Some(SharedString::from(format!(
            "{} tasks running",
            registry.len()
        )))
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

    let bar = h_flex()
        .w_full()
        .flex_shrink_0()
        .items_center()
        .gap_4()
        .px_3()
        .py_1()
        .border_t_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .text_xs()
        .text_color(theme.muted_foreground)
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
        );

    bar
}

fn task_label_none(registry: &TaskRegistry, simulated_progress: Option<f32>) -> bool {
    registry.is_empty() && simulated_progress.is_none()
}

fn compute_progress(registry: &TaskRegistry, simulated_progress: Option<f32>) -> (bool, bool, f32) {
    if let Some(p) = simulated_progress {
        if p < 0.0 {
            return (true, true, 0.0);
        }
        return (true, false, p.clamp(0.0, 1.0));
    }
    if registry.is_empty() {
        return (false, false, 0.0);
    }
    let any_indeterminate = registry
        .iter()
        .any(|t| matches!(t.progress, TaskProgress::Indeterminate));
    if any_indeterminate {
        return (true, true, 0.0);
    }
    // All determinate — show the primary task's fraction.
    let fraction = match registry.primary().map(|t| t.progress) {
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
