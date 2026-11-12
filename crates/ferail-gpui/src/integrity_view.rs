//! Tab-local Verify Integrity report (docs/features/INTEGRITY.md), hosted by
//! the Verify result surface.
//!
//! The walk and the hashing run on the background executor
//! ([`ferail_fs_native::integrity::verify_integrity`]); the worker bumps
//! lock-free counters and appends findings to a bounded shared list, and the
//! view samples both on its own clock. Rendering reads the snapshot only.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferail_core::counts::format_count;
use ferail_fs_native::integrity::{
    CorruptFile, IntegrityCounts, IntegrityProgress, verify_integrity,
};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    progress::Progress,
    v_flex,
};

use crate::dupe_cache::DbHashCache;
use crate::tasks::{TaskId, TaskKind, TaskRegistry};
use crate::text::{IconScale as _, TextScale as _};

/// Corrupted files kept for the list. The counts go on past it.
const MAX_LISTED: usize = 10_000;
/// How often the view samples the worker.
const TICK: Duration = Duration::from_millis(150);

enum Phase {
    Running,
    Done,
    Cancelled,
    Failed(SharedString),
}

pub struct IntegrityView {
    roots: Vec<PathBuf>,
    db: Option<Arc<Mutex<ferail_meta::MetadataDb>>>,
    phase: Phase,
    cancel: Arc<AtomicBool>,
    progress: Arc<IntegrityProgress>,
    shared_found: Arc<Mutex<Vec<CorruptFile>>>,
    /// Render-side snapshots of the two above.
    counts: IntegrityCounts,
    found: Arc<Vec<CorruptFile>>,
    generation: u64,
    scroll: UniformListScrollHandle,
    tasks: Rc<RefCell<TaskRegistry>>,
    task_id: Option<TaskId>,
}

impl IntegrityView {
    pub fn new(
        roots: Vec<PathBuf>,
        db: Option<Arc<Mutex<ferail_meta::MetadataDb>>>,
        tasks: Rc<RefCell<TaskRegistry>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            roots,
            db,
            phase: Phase::Running,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(IntegrityProgress::default()),
            shared_found: Arc::new(Mutex::new(Vec::new())),
            counts: IntegrityCounts::default(),
            found: Arc::new(Vec::new()),
            generation: 0,
            scroll: UniformListScrollHandle::new(),
            tasks,
            task_id: None,
        };
        view.start(cx);
        view
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(task_id) = self.task_id.take() {
            self.tasks.borrow_mut().end(task_id);
        }
        self.cancel = Arc::new(AtomicBool::new(false));
        self.progress = Arc::new(IntegrityProgress::default());
        self.shared_found = Arc::new(Mutex::new(Vec::new()));
        self.counts = IntegrityCounts::default();
        self.found = Arc::new(Vec::new());
        self.phase = Phase::Running;

        let Some(db) = self.db.clone() else {
            self.phase = Phase::Failed(tr!(
                "The metadata database is not available, so there are no recorded checksums to compare with."
            ));
            cx.notify();
            return;
        };
        self.task_id = Some(self.tasks.borrow_mut().begin_with_cancel(
            TaskKind::Checksum,
            tr!("Verifying integrity\u{2026}"),
            self.cancel.clone(),
        ));

        let roots = self.roots.clone();
        let cancel = self.cancel.clone();
        let progress = self.progress.clone();
        let found = self.shared_found.clone();
        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            let worker_cancel = cancel.clone();
            cx.background_executor()
                .spawn(async move {
                    let cache = DbHashCache::new(db, ferail_core::now_unix());
                    verify_integrity(&roots, &cache, &progress, &worker_cancel, |corrupt| {
                        if let Ok(mut list) = found.lock() {
                            if list.len() < MAX_LISTED {
                                list.push(corrupt);
                            }
                        }
                    });
                })
                .await;
            let _ = weak.update(cx, |view, cx| {
                if view.generation != generation {
                    return;
                }
                view.sample();
                view.phase = if cancel.load(Ordering::Relaxed) {
                    Phase::Cancelled
                } else {
                    Phase::Done
                };
                if let Some(task_id) = view.task_id.take() {
                    view.tasks.borrow_mut().end(task_id);
                }
                cx.notify();
            });
        })
        .detach();

        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let keep_going = weak
                    .update(cx, |view, cx| {
                        if view.generation != generation || !matches!(view.phase, Phase::Running) {
                            return false;
                        }
                        view.sample();
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
        cx.notify();
    }

    /// Copy the worker's counters and any new findings into the render
    /// snapshot. The list is cloned only when it grew.
    fn sample(&mut self) {
        self.counts = self.progress.snapshot();
        if let Ok(list) = self.shared_found.lock() {
            if list.len() != self.found.len() {
                self.found = Arc::new(list.clone());
            }
        }
    }

    fn summary_strip(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let c = &self.counts;
        let item = |text: SharedString, color: Hsla| div().text_color(color).child(text);
        h_flex()
            .w_full()
            .flex_wrap()
            .px_4()
            .py_2()
            .gap_x_4()
            .gap_y_1()
            .text_scale_xs()
            .border_b_1()
            .border_color(theme.border)
            .child(item(
                tr!("{n} verified OK", n = format_count(c.verified)),
                theme.success,
            ))
            .child(item(
                tr!("{n} corrupted", n = format_count(c.corrupted)),
                if c.corrupted > 0 {
                    theme.danger
                } else {
                    theme.muted_foreground
                },
            ))
            .child(item(
                tr!("{n} modified since last check", n = format_count(c.modified)),
                theme.muted_foreground,
            ))
            .child(item(
                tr!("{n} baseline recorded", n = format_count(c.baseline)),
                theme.muted_foreground,
            ))
            .child(item(
                tr!("{n} unreadable", n = format_count(c.unreadable)),
                if c.unreadable > 0 {
                    theme.warning
                } else {
                    theme.muted_foreground
                },
            ))
            .when(c.excluded > 0, |strip| {
                strip.child(item(
                    tr!(
                        "{n} not checked: excluded in Privacy settings",
                        n = format_count(c.excluded)
                    ),
                    theme.muted_foreground,
                ))
            })
    }

    fn corrupted_list(&self, cx: &Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let foreground = cx.theme().foreground;
        let found = self.found.clone();
        let rows = found.len();
        let list = uniform_list("integrity-results", rows, move |range, _, _| {
            range
                .filter_map(|ix| {
                    let item = found.get(ix)?;
                    let name = item
                        .path
                        .file_name()
                        .map(|n| crate::private_mode::present_leaf_str(&n.to_string_lossy(), false))
                        .unwrap_or_default();
                    let folder = item
                        .path
                        .parent()
                        .map(crate::private_mode::present_path)
                        .unwrap_or_default();
                    let target = item.path.clone();
                    Some(
                        h_flex()
                            .h(px(40.))
                            .w_full()
                            .items_center()
                            .px_4()
                            .gap_3()
                            .border_b_1()
                            .border_color(muted.opacity(0.15))
                            .child(
                                div()
                                    .w(px(90.))
                                    .flex_none()
                                    .text_scale_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(danger)
                                    .child(tr!("Corrupted")),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().truncate().text_color(foreground).child(name))
                                    .child(
                                        div()
                                            .truncate()
                                            .text_scale_xs()
                                            .text_color(muted)
                                            .child(folder),
                                    ),
                            )
                            .child(
                                div()
                                    .w(px(90.))
                                    .flex_none()
                                    .text_right()
                                    .text_scale_xs()
                                    .text_color(muted)
                                    .child(ferail_fs_native::humanize_bytes(item.size)),
                            )
                            .child(
                                Button::new(("integrity-select", ix))
                                    .small()
                                    .ghost()
                                    .icon(Icon::empty().path("icons/folder-open.svg"))
                                    .tooltip(tr!("Select in Ferail"))
                                    .on_click(move |_, _, cx| {
                                        crate::shell::reveal_path_in_app(cx, target.clone());
                                    }),
                            ),
                    )
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.scroll)
        .flex_1();
        let shown = self.found.len() as u64;
        v_flex()
            .size_full()
            .child(list)
            .when(self.counts.corrupted > shown, |this| {
                this.child(
                    div()
                        .px_4()
                        .py_2()
                        .text_scale_xs()
                        .text_color(muted)
                        .child(tr!(
                            "Showing the first {shown} of {total} corrupted files.",
                            shown = format_count(shown),
                            total = format_count(self.counts.corrupted)
                        )),
                )
            })
            .into_any_element()
    }
}

impl Drop for IntegrityView {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(task_id) = self.task_id.take() {
            self.tasks.borrow_mut().end(task_id);
        }
    }
}

impl Render for IntegrityView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let running = matches!(self.phase, Phase::Running);
        let entity = cx.entity();

        let header = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_3()
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
                            .path("icons/hard-drive.svg")
                            .icon_px(20.)
                            .text_color(cx.theme().foreground),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(
                                div()
                                    .text_scale_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(tr!("Verify Integrity")),
                            )
                            .child(div().text_scale_xs().text_color(muted).child(tr!(
                                "Finds files whose contents changed while their size and modification date stayed the same. Only files hashed before can be checked: by a verified copy, Find Duplicates, Compare Folders or an earlier Verify Integrity."
                            ))),
                    ),
            )
            .child(if running {
                Button::new("integrity-cancel")
                    .small()
                    .label(tr!("Cancel"))
                    .on_click({
                        let cancel = self.cancel.clone();
                        move |_, _, _cx| {
                            cancel.store(true, Ordering::Relaxed);
                        }
                    })
            } else {
                Button::new("integrity-rerun")
                    .small()
                    .ghost()
                    .icon(Icon::empty().path("icons/nav/refresh.svg"))
                    .tooltip(tr!("Verify again"))
                    .disabled(self.db.is_none())
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |view, cx| view.start(cx));
                    })
            });

        let centered = || {
            v_flex()
                .flex_1()
                .w_full()
                .items_center()
                .justify_center()
                .gap_2()
                .px_6()
                .text_center()
        };
        let body = match &self.phase {
            Phase::Failed(error) => centered()
                .child(
                    div()
                        .text_color(cx.theme().danger)
                        .child(tr!("Verification failed")),
                )
                .child(div().text_scale_sm().text_color(muted).child(error.clone()))
                .into_any_element(),
            _ if !self.found.is_empty() => self.corrupted_list(cx),
            Phase::Running => centered()
                .child(div().child(tr!(
                    "Checked {n} files ({size})",
                    n = format_count(self.counts.files_checked),
                    size = ferail_fs_native::humanize_bytes(self.counts.bytes_hashed)
                )))
                .child(
                    div()
                        .w(px(360.))
                        .child(Progress::new("integrity-progress").loading(true)),
                )
                .into_any_element(),
            Phase::Cancelled => centered()
                .child(div().child(tr!("Cancelled.")))
                .child(div().text_scale_sm().text_color(muted).child(tr!(
                    "No corruption found in the files checked before cancelling."
                )))
                .into_any_element(),
            Phase::Done => {
                let c = &self.counts;
                let nothing_known = c.verified == 0 && c.modified == 0 && c.baseline > 0;
                centered()
                    .child(
                        div()
                            .text_color(cx.theme().success)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(tr!("No corruption found.")),
                    )
                    .when(nothing_known, |this| {
                        this.child(div().text_scale_sm().text_color(muted).child(tr!(
                            "None of these files had been hashed before, so nothing could be compared yet. Their checksums are recorded now, and the next Verify Integrity will check them."
                        )))
                    })
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .text_scale_xs()
            .child(header)
            .child(self.summary_strip(cx))
            .child(body)
    }
}
