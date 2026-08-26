//! Tab-local checksum-manifest verification report.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use ferail_fs_native::verify::{
    EntryOutcome, VerifyReport, parse_manifest, safe_relative_path, verify_manifest,
};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    progress::Progress,
    v_flex,
};

use crate::tasks::{TaskId, TaskKind, TaskRegistry};
use crate::text::{TextScale as _, TruncateMiddle as _};

enum Phase {
    Running,
    Ready(Arc<VerifyReport>),
    Failed(SharedString),
    Cancelled(Arc<VerifyReport>),
}

pub struct VerifyView {
    source: PathBuf,
    phase: Phase,
    cancel: Arc<AtomicBool>,
    progress_entry: Arc<AtomicU64>,
    progress_count: Arc<AtomicU64>,
    progress_bytes: Arc<AtomicU64>,
    progress_total: Arc<AtomicU64>,
    generation: u64,
    running: bool,
    show_refreshing: bool,
    problems_only: bool,
    visible_rows: Arc<Vec<usize>>,
    scroll: UniformListScrollHandle,
    tasks: Rc<RefCell<TaskRegistry>>,
    task_id: Option<TaskId>,
}

impl VerifyView {
    pub fn new(source: PathBuf, tasks: Rc<RefCell<TaskRegistry>>, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            source,
            phase: Phase::Running,
            cancel: Arc::new(AtomicBool::new(false)),
            progress_entry: Arc::new(AtomicU64::new(0)),
            progress_count: Arc::new(AtomicU64::new(0)),
            progress_bytes: Arc::new(AtomicU64::new(0)),
            progress_total: Arc::new(AtomicU64::new(0)),
            generation: 0,
            running: false,
            show_refreshing: false,
            problems_only: false,
            visible_rows: Arc::new(Vec::new()),
            scroll: UniformListScrollHandle::new(),
            tasks,
            task_id: None,
        };
        view.start(cx);
        view
    }

    pub fn source(&self) -> &PathBuf {
        &self.source
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let preserve_report = matches!(self.phase, Phase::Ready(_) | Phase::Cancelled(_));
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(task_id) = self.task_id.take() {
            self.tasks.borrow_mut().end(task_id);
        }
        self.cancel = Arc::new(AtomicBool::new(false));
        self.progress_entry = Arc::new(AtomicU64::new(0));
        self.progress_count = Arc::new(AtomicU64::new(0));
        self.progress_bytes = Arc::new(AtomicU64::new(0));
        self.progress_total = Arc::new(AtomicU64::new(0));
        self.running = true;
        self.show_refreshing = false;
        if !preserve_report {
            self.phase = Phase::Running;
            self.visible_rows = Arc::new(Vec::new());
        }

        self.task_id = Some(self.tasks.borrow_mut().begin_with_cancel(
            TaskKind::Checksum,
            tr!("Verifying checksum manifest…"),
            self.cancel.clone(),
        ));

        let source = self.source.clone();
        let cancel = self.cancel.clone();
        let entry = self.progress_entry.clone();
        let count = self.progress_count.clone();
        let bytes = self.progress_bytes.clone();
        let total = self.progress_total.clone();
        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let raw = std::fs::read(&source).map_err(|error| {
                        tr!("Could not read manifest: {detail}", detail = error)
                    })?;
                    let manifest = parse_manifest(source, &raw).map_err(|error| {
                        tr!("Could not parse manifest: {detail}", detail = error)
                    })?;
                    count.store(manifest.entries.len() as u64, Ordering::Relaxed);
                    Ok::<_, String>(verify_manifest(&manifest, &cancel, |progress| {
                        entry.store(progress.entry_index + 1, Ordering::Relaxed);
                        bytes.store(progress.file_bytes_done, Ordering::Relaxed);
                        total.store(progress.file_bytes_total, Ordering::Relaxed);
                    }))
                })
                .await;
            let _ = weak.update(cx, |view, cx| {
                if view.generation != generation {
                    return;
                }
                view.running = false;
                view.show_refreshing = false;
                match result {
                    Ok(report) if report.cancelled => {
                        view.phase = Phase::Cancelled(Arc::new(report));
                    }
                    Ok(report) => view.set_report(Arc::new(report)),
                    Err(error) => view.phase = Phase::Failed(error.into()),
                }
                if let Some(task_id) = view.task_id.take() {
                    match &view.phase {
                        Phase::Failed(error) => view
                            .tasks
                            .borrow_mut()
                            .end_failed(task_id, error.to_string()),
                        _ => view.tasks.borrow_mut().end(task_id),
                    }
                }
                cx.notify();
            });
        })
        .detach();

        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            let mut ticks = 0u8;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let keep_going = weak
                    .update(cx, |view, cx| {
                        if view.generation != generation || !view.running {
                            return false;
                        }
                        ticks = ticks.saturating_add(1);
                        if ticks >= 2 {
                            view.show_refreshing = true;
                        }
                        if let Some(task_id) = view.task_id {
                            let current = view.progress_entry.load(Ordering::Relaxed);
                            let count = view.progress_count.load(Ordering::Relaxed);
                            if count > 0 {
                                let file_fraction = {
                                    let total = view.progress_total.load(Ordering::Relaxed);
                                    if total == 0 {
                                        0.0
                                    } else {
                                        view.progress_bytes.load(Ordering::Relaxed) as f32
                                            / total as f32
                                    }
                                };
                                let fraction = (current.saturating_sub(1) as f32 + file_fraction)
                                    / count as f32;
                                view.tasks.borrow_mut().update(task_id, fraction);
                            }
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();
    }

    fn set_report(&mut self, report: Arc<VerifyReport>) {
        self.phase = Phase::Ready(report.clone());
        self.rebuild_visible(&report);
    }

    fn rebuild_visible(&mut self, report: &VerifyReport) {
        self.visible_rows = Arc::new(
            report
                .outcomes
                .iter()
                .enumerate()
                .filter_map(|(index, outcome)| {
                    (!self.problems_only || !matches!(outcome.outcome, EntryOutcome::Ok))
                        .then_some(index)
                })
                .collect(),
        );
    }

    fn toggle_problems(&mut self, cx: &mut Context<Self>) {
        self.problems_only = !self.problems_only;
        if let Phase::Ready(report) | Phase::Cancelled(report) = &self.phase {
            let report = report.clone();
            self.rebuild_visible(&report);
        }
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn counts(report: &VerifyReport) -> (usize, usize, usize, usize) {
        let mut ok = 0;
        let mut failed = 0;
        let mut missing = 0;
        let mut other = 0;
        for item in &report.outcomes {
            match item.outcome {
                EntryOutcome::Ok => ok += 1,
                EntryOutcome::Mismatch { .. } => failed += 1,
                EntryOutcome::Missing => missing += 1,
                _ => other += 1,
            }
        }
        (ok, failed, missing, other)
    }

    fn outcome_label(outcome: &EntryOutcome) -> SharedString {
        match outcome {
            EntryOutcome::Ok => tr!("OK"),
            EntryOutcome::Mismatch { .. } => tr!("Mismatch"),
            EntryOutcome::Missing => tr!("Missing"),
            EntryOutcome::Unreadable { .. } => tr!("Unreadable"),
            EntryOutcome::UnsafePath { .. } => tr!("Unsafe path"),
            EntryOutcome::UnavailablePlaceholder => tr!("Not downloaded"),
            EntryOutcome::ChangedWhileReading => tr!("Changed while reading"),
            EntryOutcome::Cancelled => tr!("Cancelled"),
        }
    }
}

impl Drop for VerifyView {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(task_id) = self.task_id.take() {
            self.tasks.borrow_mut().end(task_id);
        }
    }
}

impl Render for VerifyView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let title = self
            .source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| tr!("Checksum manifest").to_string());
        let entity = cx.entity();

        let header = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .min_w_0()
                    .gap_3()
                    .items_center()
                    .child(
                        svg()
                            .path("icons/check.svg")
                            .size(px(20.))
                            .text_color(cx.theme().success),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(
                                div()
                                    .text_scale_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(div().text_scale_xs().text_color(muted).child(tr!(
                                "Checksums compare bytes; they do not prove who published them."
                            ))),
                    ),
            )
            .child(if matches!(self.phase, Phase::Running) {
                Button::new("verify-cancel")
                    .small()
                    .label(tr!("Cancel"))
                    .on_click({
                        let cancel = self.cancel.clone();
                        move |_, _, _cx| {
                            cancel.store(true, Ordering::Relaxed);
                        }
                    })
            } else {
                Button::new("verify-rerun")
                    .small()
                    .ghost()
                    .icon(Icon::empty().path("icons/nav/refresh.svg"))
                    .tooltip(tr!("Verify again"))
                    .loading(self.show_refreshing)
                    .disabled(self.running)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |view, cx| view.start(cx));
                    })
            });

        let body = match &self.phase {
            Phase::Running => {
                let current = self.progress_entry.load(Ordering::Relaxed);
                let count = self.progress_count.load(Ordering::Relaxed);
                let done = self.progress_bytes.load(Ordering::Relaxed);
                let total = self.progress_total.load(Ordering::Relaxed);
                let fraction = if total == 0 {
                    0.0
                } else {
                    (done as f32 / total as f32).clamp(0.0, 1.0)
                };
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(div().child(if count == 0 {
                        tr!("Reading manifest…")
                    } else {
                        tr!(
                            "Verifying file {current} of {total}",
                            current = current,
                            total = count
                        )
                    }))
                    .child(
                        div().w(px(360.)).child(
                            Progress::new("verify-progress")
                                .loading(total == 0)
                                .value(fraction * 100.0),
                        ),
                    )
                    .into_any_element()
            }
            Phase::Failed(error) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(
                    div()
                        .text_color(cx.theme().danger)
                        .child(tr!("Verification failed")),
                )
                .child(div().text_scale_sm().text_color(muted).child(error.clone()))
                .into_any_element(),
            Phase::Ready(report) | Phase::Cancelled(report) => {
                let (ok, failed, missing, other) = Self::counts(report);
                let report = report.clone();
                let format = report.format;
                let algorithm = report
                    .outcomes
                    .first()
                    .map(|item| item.entry.algorithm.label())
                    .unwrap_or("checksum");
                let source_root = self
                    .source
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_path_buf();
                let indices = self.visible_rows.clone();
                let success = cx.theme().success;
                let danger = cx.theme().danger;
                let warning = cx.theme().warning;
                let foreground = cx.theme().foreground;
                let muted_row = muted;
                let mono_font = cx.theme().mono_font_family.clone();
                let rows = indices.len();
                let list = uniform_list("verify-results", rows, move |range, _, _| {
                    range
                        .filter_map(|visible| {
                            let item = report.outcomes.get(*indices.get(visible)?)?;
                            let (status_color, detail) = match &item.outcome {
                                EntryOutcome::Ok => (success, None),
                                EntryOutcome::Mismatch { actual } => {
                                    (danger, Some(format!("{}: {actual}", tr!("Actual"))))
                                }
                                EntryOutcome::Missing => (warning, None),
                                EntryOutcome::Unreadable { reason } => {
                                    (danger, Some(reason.clone()))
                                }
                                EntryOutcome::UnsafePath { reason } => {
                                    (danger, Some((*reason).to_string()))
                                }
                                EntryOutcome::UnavailablePlaceholder => (warning, None),
                                EntryOutcome::ChangedWhileReading => (warning, None),
                                EntryOutcome::Cancelled => (muted_row, None),
                            };
                            let selectable = matches!(
                                &item.outcome,
                                EntryOutcome::Mismatch { .. }
                                    | EntryOutcome::Unreadable { .. }
                                    | EntryOutcome::UnavailablePlaceholder
                                    | EntryOutcome::ChangedWhileReading
                            );
                            let target = selectable.then(|| {
                                format
                                    .and_then(|format| {
                                        safe_relative_path(&item.entry.name, format).ok()
                                    })
                                    .map(|relative| source_root.join(relative))
                            });
                            let action = match target.flatten() {
                                Some(target) => Button::new(("verify-select", visible))
                                    .small()
                                    .ghost()
                                    .icon(Icon::empty().path("icons/folder-open.svg"))
                                    .tooltip(tr!("Select in Ferail"))
                                    .on_click(move |_, _, cx| {
                                        crate::shell::reveal_path_in_app(cx, target.clone());
                                    })
                                    .into_any_element(),
                                None => div().w(px(28.)).flex_none().into_any_element(),
                            };
                            let expected: SharedString = item.entry.expected.clone().into();
                            Some(
                                h_flex()
                                    .h(px(36.))
                                    .w_full()
                                    .items_center()
                                    .px_4()
                                    .gap_3()
                                    .border_b_1()
                                    .border_color(muted_row.opacity(0.15))
                                    .child(
                                        div()
                                            .w(px(150.))
                                            .flex_none()
                                            .text_scale_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(status_color)
                                            .child(Self::outcome_label(&item.outcome)),
                                    )
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_color(foreground)
                                                    .child(item.entry.name.clone()),
                                            )
                                            .when_some(detail, |this, detail| {
                                                this.child(
                                                    div()
                                                        .truncate()
                                                        .text_scale_xs()
                                                        .text_color(muted_row)
                                                        .child(detail),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .w(px(190.))
                                            .flex_none()
                                            .truncate_middle()
                                            .text_right()
                                            .font_family(mono_font.clone())
                                            .text_scale_xs()
                                            .text_color(muted_row)
                                            .child(expected),
                                    )
                                    .child(action),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                .flex_1();
                v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .w_full()
                            .px_4()
                            .py_2()
                            .gap_4()
                            .text_scale_xs()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(div().text_color(success).child(tr!("{n} OK", n = ok)))
                            .child(
                                div()
                                    .text_color(danger)
                                    .child(tr!("{n} mismatches", n = failed)),
                            )
                            .child(
                                div()
                                    .text_color(warning)
                                    .child(tr!("{n} missing", n = missing)),
                            )
                            .child(div().text_color(muted).child(tr!("{n} other", n = other)))
                            .child(div().flex_1())
                            .child(
                                Button::new("verify-filter")
                                    .small()
                                    .ghost()
                                    .icon(Icon::empty().path(if self.problems_only {
                                        "icons/eye.svg"
                                    } else {
                                        "icons/eye-off.svg"
                                    }))
                                    .selected(self.problems_only)
                                    .tooltip(if self.problems_only {
                                        tr!("Show all")
                                    } else {
                                        tr!("Problems only")
                                    })
                                    .on_click(
                                        cx.listener(|view, _, _, cx| view.toggle_problems(cx)),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .h(px(30.))
                            .w_full()
                            .items_center()
                            .px_4()
                            .gap_3()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .text_scale_xxs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(muted)
                            .child(div().w(px(150.)).flex_none().child(tr!("Status")))
                            .child(div().flex_1().min_w_0().child(tr!("File")))
                            .child(
                                div()
                                    .w(px(190.))
                                    .flex_none()
                                    .text_right()
                                    .child(tr!("Expected ({algorithm})", algorithm = algorithm)),
                            )
                            .child(div().w(px(28.)).flex_none()),
                    )
                    .child(list)
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .text_scale_xs()
            .child(header)
            .child(body)
    }
}
