//! "What's Locking This?": name the processes holding files open, and
//! offer to close them.
//!
//! The same Restart-Manager diagnostics the failed-transfer toast uses
//! (`inspect_locked_retry`), surfaced as a first-class dialog reachable
//! from the context menu, before an operation fails, not only after.
//! Folder and volume targets are expanded by a capped background walk
//! ([`platform_shell::processes_using_tree`]), so the answer is honest
//! about being a sample on huge trees (`LockScan::truncated`).
//!
//! Everything that can block: the walk, the RM process enumeration, the
//! graceful-then-forced close: runs on the background executor; results
//! land through entity updates guarded by a generation counter (Prime
//! Directive). Closing invalidates any in-flight scan and always rescans
//! afterwards, so the list shows fresh truth, not an optimistic edit.

use std::path::PathBuf;

use gpui::{
    App, AppContext as _, Context, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogClose, DialogFooter},
    h_flex,
    progress::Progress,
    v_flex,
};

use super::{Shell, ShowLockHolders, ShowLockHoldersAtContext};
use crate::text::TextScale as _;

/// Walk cap for folder/volume targets: bounds the scan on huge trees.
/// Matches the Windows shell's own `volume_busy_processes` cap.
const MAX_SCAN_FILES: usize = 4096;

enum Phase {
    Scanning,
    /// A force-close is in flight; a rescan follows automatically.
    Closing,
    Ready(crate::platform_shell::LockScan),
}

struct LockInfoView {
    /// Display-ready subject: a file name, a volume name, or "{n} items".
    target_label: SharedString,
    paths: Vec<PathBuf>,
    phase: Phase,
    /// Bumped by every new scan/close; results carrying an older value
    /// are dropped (the user already moved the dialog on).
    generation: u64,
    error: Option<SharedString>,
}

impl LockInfoView {
    fn new(target_label: SharedString, paths: Vec<PathBuf>) -> Self {
        Self {
            target_label,
            paths,
            phase: Phase::Scanning,
            generation: 0,
            error: None,
        }
    }

    /// Kick a fresh background scan; the previous one (if any) is
    /// invalidated by the generation bump.
    fn start_scan(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.phase = Phase::Scanning;
        cx.notify();
        let generation = self.generation;
        let paths = self.paths.clone();
        cx.spawn(async move |this, cx| {
            let scan = cx
                .background_executor()
                .spawn(async move {
                    crate::platform_shell::processes_using_tree(&paths, MAX_SCAN_FILES)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.generation == generation {
                    view.phase = Phase::Ready(scan);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Ask the given processes to close (graceful Restart-Manager pass,
    /// then a forced kill for survivors), then rescan: whatever the
    /// outcome, the list must show what actually holds the files now.
    fn close_processes(&mut self, pids: Vec<u32>, cx: &mut Context<Self>) {
        self.generation += 1;
        self.phase = Phase::Closing;
        self.error = None;
        cx.notify();
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { crate::platform_shell::force_close_processes(&pids) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.generation != generation {
                    return;
                }
                if let Err(detail) = result {
                    view.error = Some(tr!(
                        "Some programs couldn’t be closed: {detail}",
                        detail = detail
                    ));
                }
                view.start_scan(cx);
            });
        })
        .detach();
    }

    fn banner(&self, message: SharedString, color: gpui::Hsla, cx: &App) -> gpui::Div {
        div()
            .w_full()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .bg(color.opacity(0.10))
            .border_1()
            .border_color(color.opacity(0.45))
            .text_scale_sm()
            .text_color(color)
            .child(message)
    }
}

impl Render for LockInfoView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let view = cx.entity();

        let body = match &self.phase {
            Phase::Scanning | Phase::Closing => v_flex()
                .w_full()
                .gap_2()
                .child(div().text_scale_sm().child(match self.phase {
                    Phase::Closing => tr!("Closing programs…"),
                    _ => tr!("Checking which programs have files open…"),
                }))
                .child(
                    Progress::new("lock-scan-progress")
                        .small()
                        .loading(true)
                        .value(0.),
                )
                .into_any_element(),
            Phase::Ready(scan) if scan.holders.is_empty() => {
                let mut col = v_flex().w_full().gap_2().child(self.banner(
                    tr!("No program is locking this."),
                    cx.theme().success,
                    cx,
                ));
                if scan.truncated {
                    col = col.child(div().text_scale_xs().text_color(muted).child(trn!(
                        "Only {n} file was checked: a lock outside that sample would not show.",
                        "Only the first {n} files were checked: a lock outside that sample would not show.",
                        scan.scanned
                    )));
                }
                col.into_any_element()
            }
            Phase::Ready(scan) => {
                let mut col = v_flex().w_full().gap_2();
                for holder in &scan.holders {
                    let pid = holder.pid;
                    let close_view = view.clone();
                    col = col.child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .px_3()
                            .py_1p5()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().secondary.opacity(0.5))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_scale_sm()
                                    .truncate()
                                    .child(holder.name.clone()),
                            )
                            .child(
                                div()
                                    .text_scale_xs()
                                    .text_color(muted)
                                    .child(tr!("PID {pid}", pid = pid)),
                            )
                            .child(
                                Button::new(("close-holder", pid as usize))
                                    .label(tr!("Close"))
                                    .outline()
                                    .small()
                                    .on_click(move |_, _, cx| {
                                        close_view.update(cx, |view, cx| {
                                            view.close_processes(vec![pid], cx);
                                        });
                                    }),
                            ),
                    );
                }
                if scan.truncated {
                    col = col.child(div().text_scale_xs().text_color(muted).child(trn!(
                        "Only {n} file was checked: a lock outside that sample would not show.",
                        "Only the first {n} files were checked: a lock outside that sample would not show.",
                        scan.scanned
                    )));
                }
                col = col.child(
                    div()
                        .text_scale_xs()
                        .text_color(cx.theme().warning)
                        .child(tr!(
                            "Closing a program this way can discard its unsaved changes."
                        )),
                );
                col.into_any_element()
            }
        };

        let mut root = v_flex()
            .w_full()
            .gap_3()
            .py_2()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_scale_xs().text_color(muted).child(tr!("Target")))
                    .child(div().text_scale_sm().child(self.target_label.clone())),
            )
            .child(body);

        if let Some(error) = &self.error {
            root = root.child(
                div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().danger.opacity(0.08))
                    .text_scale_sm()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }

        // Action row: Close All only when there is something to close.
        let mut actions = h_flex().gap_2().pt_1();
        if let Phase::Ready(scan) = &self.phase {
            if !scan.holders.is_empty() {
                let pids: Vec<u32> = scan.holders.iter().map(|h| h.pid).collect();
                let close_all_view = view.clone();
                actions = actions.child(
                    Button::new("close-all-holders")
                        .label(tr!("Close All"))
                        .danger()
                        .small()
                        .on_click(move |_, _, cx| {
                            let pids = pids.clone();
                            close_all_view.update(cx, |view, cx| {
                                view.close_processes(pids, cx);
                            });
                        }),
                );
            }
            let rescan_view = view.clone();
            actions = actions.child(
                Button::new("rescan-holders")
                    .label(tr!("Scan Again"))
                    .outline()
                    .small()
                    .on_click(move |_, _, cx| {
                        rescan_view.update(cx, |view, cx| view.start_scan(cx));
                    }),
            );
            root = root.child(actions);
        }

        root
    }
}

impl Shell {
    /// File-list row menu: diagnose the resolved selection.
    pub(crate) fn on_show_lock_holders(
        &mut self,
        _: &ShowLockHolders,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("What's Locking This");
        let paths: Vec<PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        let label: SharedString = match paths.as_slice() {
            [] => return,
            [path] => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
                .into(),
            many => trn!("{n} item", "{n} items", many.len()),
        };
        open_lock_dialog(label, paths, window, cx);
    }

    /// Sidebar volume rows: diagnose everything under the right-clicked
    /// volume (`context_target`): the pre-emptive "why won't it eject".
    pub(crate) fn on_show_lock_holders_at_context(
        &mut self,
        _: &ShowLockHoldersAtContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("What's Blocking Eject");
        let Some(path) = self.context_target.take() else {
            return;
        };
        let label: SharedString = self
            .mounted_volume_name(&path)
            .unwrap_or_else(|| {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string())
            })
            .into();
        open_lock_dialog(label, vec![path], window, cx);
    }
}

fn open_lock_dialog(
    target_label: SharedString,
    paths: Vec<PathBuf>,
    window: &mut Window,
    cx: &mut App,
) {
    let state = cx.new(|_| LockInfoView::new(target_label, paths));
    state.update(cx, |view, cx| view.start_scan(cx));
    let state_for_dialog = state.clone();
    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .title(tr!("What’s Locking This?"))
            .w(px(520.))
            .child(state_for_dialog.clone())
            .footer(DialogFooter::new().child(div().w(px(96.)).child(
                DialogClose::new().child(Button::new("lock-info-done").label(tr!("Done")).small()),
            )))
    })
}
