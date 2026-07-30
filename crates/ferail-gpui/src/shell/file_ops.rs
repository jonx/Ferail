use super::*;

/// Copy vs move for [`Shell::spawn_transfer_op`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransferMode {
    Copy,
    Move,
    /// Resolve in the worker per dnd-spec §3.6: same volume → Move,
    /// cross-volume → Copy. The drop handler can't stat on the UI
    /// thread, so drag-and-drop lands here unless a modifier forced
    /// the mode.
    Auto,
}

/// Callback run on the UI thread after a successful archive operation, for
/// surfaces whose state the directory reload doesn't refresh.
pub type ArchiveOpDone = Box<dyn FnOnce(&mut Shell, &mut Context<Shell>) + 'static>;

impl Shell {
    /// Cmd+C — write the selection's file URLs to the pasteboard.
    pub(super) fn on_copy_files(
        &mut self,
        _: &CopyFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        // `is_dir` comes from the cached FileEntry so the pasteboard
        // write never stats — a per-path stat on the main thread hangs
        // Cmd+C on a dead network mount (Prime Directive).
        let items: Vec<(PathBuf, bool)> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, entry, path)| {
                (
                    path,
                    matches!(entry.kind, ferail_core::EntryKind::Directory),
                )
            })
            .collect();
        if items.is_empty() {
            return;
        }
        let refs: Vec<(&std::path::Path, bool)> =
            items.iter().map(|(p, d)| (p.as_path(), *d)).collect();
        if !crate::platform_shell::clipboard_copy_file_urls(&refs) {
            window.push_notification(
                Notification::error("File clipboard isn't available on this platform yet."),
                cx,
            );
            return;
        }
        // A fresh Copy cancels any pending Cut.
        self.process.cut_marker.borrow_mut().clear();
        let msg = match items.as_slice() {
            [(single, _)] => format!(
                "Copied \u{201c}{}\u{201d}",
                single.file_name().unwrap_or_default().to_string_lossy()
            ),
            many => format!("Copied {} items", many.len()),
        };
        window.push_notification(Notification::success(msg), cx);
        cx.notify();
    }

    /// Cmd+X — copy the selection's URLs to the pasteboard and mark them
    /// so the next plain Paste *moves* them. The rows render dimmed
    /// while marked.
    pub(super) fn on_cut_files(
        &mut self,
        _: &CutFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        // Same no-stat contract as Copy (see on_copy_files).
        let items: Vec<(PathBuf, bool)> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, entry, path)| {
                (
                    path,
                    matches!(entry.kind, ferail_core::EntryKind::Directory),
                )
            })
            .collect();
        if items.is_empty() {
            return;
        }
        let refs: Vec<(&std::path::Path, bool)> =
            items.iter().map(|(p, d)| (p.as_path(), *d)).collect();
        if !crate::platform_shell::clipboard_copy_file_urls(&refs) {
            // Don't dim rows for a Cut that can never complete its
            // move — the stub platform has no file clipboard.
            window.push_notification(
                Notification::error("File clipboard isn't available on this platform yet."),
                cx,
            );
            return;
        }
        let paths: Vec<PathBuf> = items.into_iter().map(|(p, _)| p).collect();
        let msg = match paths.as_slice() {
            [single] => format!(
                "Cut \u{201c}{}\u{201d}",
                single.file_name().unwrap_or_default().to_string_lossy()
            ),
            many => format!("Cut {} items", many.len()),
        };
        *self.process.cut_marker.borrow_mut() = paths;
        window.push_notification(Notification::info(msg), cx);
        cx.notify();
    }

    /// Cmd+V — paste the pasteboard's files into this folder. A *move*
    /// when those exact items were Cut (clearing the mark), else a copy.
    pub(super) fn on_paste_files(
        &mut self,
        _: &PasteFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Paste");
        use gpui_component::notification::Notification;
        let sources = crate::platform_shell::clipboard_read_file_urls();
        if sources.is_empty() {
            window.push_notification(Notification::info("No files on the clipboard"), cx);
            return;
        }
        // Pasting the exact set that was Cut → move + clear the mark.
        let cut = self.process.cut_marker.borrow().clone();
        let is_cut_set = !cut.is_empty()
            && cut.len() == sources.len()
            && cut.iter().all(|p| sources.contains(p));
        let mode = if is_cut_set {
            self.process.cut_marker.borrow_mut().clear();
            cx.notify();
            TransferMode::Move
        } else {
            TransferMode::Copy
        };
        let dest = self.active_tab().current_dir.clone();
        self.spawn_transfer_op(sources, dest, mode, window, cx);
    }

    /// Cmd+Option+V — Finder's "Move Items Here".
    pub(super) fn on_move_paste_files(
        &mut self,
        _: &MovePasteFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Move Here (Paste)");
        self.paste_from_clipboard(TransferMode::Move, window, cx);
    }

    fn paste_from_clipboard(
        &mut self,
        mode: TransferMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        // Pasteboard read is a semantic event (action handler), same
        // boundary as Quick Look — never from render.
        let sources = crate::platform_shell::clipboard_read_file_urls();
        if sources.is_empty() {
            window.push_notification(Notification::info("No files on the clipboard"), cx);
            return;
        }
        let dest = self.active_tab().current_dir.clone();
        self.spawn_transfer_op(sources, dest, mode, window, cx);
    }

    /// Files dropped into the window (Finder drags, or our own rows
    /// dropped on a folder/the pane). Operation per dnd-spec §3.6:
    /// Option forces Copy, Cmd forces Move, otherwise Auto (worker
    /// resolves same-volume → Move, cross-volume → Copy). Dropping
    /// items where they already live is a no-op unless Option asks
    /// for duplicates.
    pub(crate) fn handle_external_drop(
        &mut self,
        paths: Vec<PathBuf>,
        dest: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let mods = window.modifiers();
        // Cmd+Option (Finder's alias-drag modifier): drop makes a Finder
        // alias for each source in `dest` instead of copying/moving.
        // Checked before the alt→Copy branch since alt is also set here.
        if mods.alt && mods.platform {
            let dest_for_op = dest.clone();
            let sources = paths.clone();
            self.spawn_file_op(
                dest,
                move || {
                    let mut created = Vec::new();
                    for src in &sources {
                        created.push(crate::platform_shell::make_alias_in(src, &dest_for_op)?);
                    }
                    Ok(created)
                },
                "Create alias",
                None,
                FileOpSuccessToast::None,
                FileOpUndo::None,
                window,
                cx,
            );
            return;
        }
        let mode = if mods.alt {
            TransferMode::Copy
        } else if mods.platform {
            TransferMode::Move
        } else {
            TransferMode::Auto
        };
        let same_dir = paths.iter().all(|s| s.parent() == Some(dest.as_path()));
        if same_dir && !mods.alt {
            return;
        }
        self.spawn_transfer_op(paths, dest, mode, window, cx);
    }

    /// Transfer OS paths *into* the folder at `row_ix` (dnd-spec §3.5).
    /// Shared by the list row and the icon-grid cell so both view modes
    /// resolve the destination and clear any pending spring-load the
    /// same way. Non-folder rows never call this — their drops fall
    /// through to the pane-background target's current-dir semantics.
    pub(crate) fn drop_onto_folder_row(
        &mut self,
        row_ix: usize,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.spring_load = None;
        if let Some(dest) = self.path_for_row(row_ix, cx) {
            self.handle_external_drop(paths, dest, window, cx);
        }
    }

    /// Spring-load bookkeeping shared by the list and grid: while a drag
    /// hovers a folder row, after a short dwell over the *same* row,
    /// drill into it so the user can drop deeper without releasing the
    /// drag. Only folder rows feed this, so the row's path is a
    /// directory by construction — no stat here.
    pub(crate) fn spring_load_hover(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        const SPRING_DWELL: std::time::Duration = std::time::Duration::from_millis(600);
        let now = std::time::Instant::now();
        match self.spring_load {
            Some((r, since)) if r == row_ix => {
                if now.duration_since(since) >= SPRING_DWELL {
                    self.spring_load = None;
                    if let Some(dest) = self.path_for_row(row_ix, cx) {
                        self.navigate(dest, cx);
                    }
                }
            }
            _ => self.spring_load = Some((row_ix, now)),
        }
    }

    /// The transfer worker (docs/features/FILE_OPS.md): plan on the
    /// background executor, raise the collision dialog if needed, run
    /// the engine with throttled progress into the task registry
    /// (cancellable from the task panel), then notify + reload +
    /// register undo.
    pub(crate) fn spawn_transfer_op(
        &mut self,
        sources: Vec<PathBuf>,
        dest: PathBuf,
        mode: TransferMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use ferail_fs_native::file_ops::{self as engine, CollisionPolicy};
        use gpui_component::button::ButtonVariants as _;
        use gpui_component::notification::Notification;

        let cancel = Arc::new(AtomicBool::new(false));
        let verb = match mode {
            TransferMode::Copy => "Copying",
            TransferMode::Move => "Moving",
            TransferMode::Auto => "Transferring",
        };
        let noun = match sources.as_slice() {
            [single] => format!(
                "\u{201c}{}\u{201d}",
                single.file_name().unwrap_or_default().to_string_lossy()
            ),
            many => format!("{} items", many.len()),
        };
        // "Moving “D4Mac” to “Backup”…" — the destination is part of the
        // label so the task panel answers *where to* without a hover.
        let dest_name = dest
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| dest.display().to_string());
        let base_label = format!("{verb} {noun} to \u{201c}{dest_name}\u{201d}\u{2026}");
        let task_id = self.process.tasks.borrow_mut().begin_with_cancel(
            crate::tasks::TaskKind::FileOp,
            base_label.clone(),
            cancel.clone(),
        );

        // Shared progress sink: the worker bumps lock-free atomic
        // counters on its hot path (no channel, no per-file allocation);
        // this sampler reads them on its own ~10 Hz clock and derives the
        // rate/ETA UI-side. The copy can never be slowed or stalled by
        // drawing its own progress, no matter how many files — the Prime
        // Directive, structurally.
        let prog = Arc::new(engine::TransferProgress::new());
        let done = Arc::new(AtomicBool::new(false));
        // The running-phase label, shared so the precheck can swap an
        // Auto drag-drop's generic "Transferring" for the resolved
        // "Moving"/"Copying" once it learns the volume relationship.
        let live_label = Arc::new(std::sync::Mutex::new(base_label.clone()));
        {
            let weak = cx.weak_entity();
            let prog = prog.clone();
            let done = done.clone();
            let base_label = base_label.clone();
            let live_label = live_label.clone();
            cx.spawn(async move |_this, cx| {
                // Rolling window of (t, bytes_done) samples (~6 s at the
                // 10 Hz tick). The displayed rate is the trimmed mean of
                // the window's per-sample rates, refreshed only once per
                // second: the progress bar stays 10 Hz-smooth, but the
                // rate/ETA text doesn't twitch on every repaint, and
                // one-tick extremes (instant-clone jumps, seek stalls)
                // fall out in the trim instead of whipsawing the number.
                let mut window: std::collections::VecDeque<(std::time::Instant, u64)> =
                    std::collections::VecDeque::new();
                let mut shown_rate: f64 = 0.0;
                let mut shown_eta: Option<u64> = None;
                let mut ticks: u32 = 0;
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(100))
                        .await;
                    if done.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let planning = prog.is_planning();
                    let bytes_done = prog.bytes_done();
                    let bytes_total = prog.bytes_total();
                    let now = std::time::Instant::now();
                    window.push_back((now, bytes_done));
                    while window
                        .front()
                        .is_some_and(|(t, _)| now.duration_since(*t).as_secs_f64() > 6.0)
                    {
                        window.pop_front();
                    }
                    ticks = ticks.wrapping_add(1);
                    if ticks % 10 == 0 {
                        shown_rate = trimmed_window_rate(&window);
                        shown_eta = if shown_rate > 1.0 && bytes_total > bytes_done {
                            Some(round_eta(
                                ((bytes_total - bytes_done) as f64 / shown_rate) as u64,
                            ))
                        } else {
                            None
                        };
                    }
                    let stats = crate::tasks::TransferStats {
                        bytes_done,
                        bytes_total,
                        items_done: prog.items_done(),
                        items_total: prog.items_total(),
                        bytes_per_sec: shown_rate,
                        eta_secs: shown_eta,
                        current: prog.current().to_string(),
                    };
                    let planned = prog.planned();
                    let label = if planning {
                        format!("{verb} \u{2014} preparing ({planned} items)\u{2026}")
                    } else {
                        live_label
                            .lock()
                            .map(|g| g.clone())
                            .unwrap_or_else(|_| base_label.clone())
                    };
                    let Some(shell) = weak.upgrade() else { break };
                    shell.update(cx, |this, cx| {
                        let mut reg = this.process.tasks.borrow_mut();
                        reg.set_label(task_id, label);
                        if !planning {
                            if bytes_total > 0 {
                                reg.update(task_id, bytes_done as f32 / bytes_total as f32);
                            }
                            reg.update_transfer(task_id, stats);
                        }
                        drop(reg);
                        cx.notify();
                    });
                }
            })
            .detach();
        }

        let win = window.window_handle();
        let process = self.process.clone();
        let weak = cx.weak_entity();
        let prog_run = prog.clone();
        let done_run = done.clone();
        let live_label_run = live_label.clone();
        let noun_run = noun.clone();
        let dest_name_run = dest_name.clone();
        cx.spawn(async move |_this, cx| {
            let end_task = |cx: &mut AsyncApp| {
                // Stop the sampler too, so a bailed-out op (plan error,
                // dialog cancel) doesn't leave it spinning.
                done_run.store(true, std::sync::atomic::Ordering::Relaxed);
                if let Some(shell) = weak.upgrade() {
                    shell.update(cx, |this, cx| {
                        this.process.tasks.borrow_mut().end(task_id);
                        cx.notify();
                    });
                }
            };

            // 1. Plan (walk + conflict scan) off the UI thread. The walk
            // ticks `prog`'s planning counter so the status bar can show
            // "Preparing — N items" instead of looking hung on a big tree.
            let plan = {
                let (s, d, c) = (sources.clone(), dest.clone(), cancel.clone());
                let p = prog_run.clone();
                cx.background_executor()
                    .spawn(async move { engine::plan_transfer(&s, &d, &p, &c) })
                    .await
            };
            let plan = match plan {
                Ok(p) => p,
                Err(e) => {
                    end_task(cx);
                    let _ = win.update(cx, |_, window, cx| {
                        window.push_notification(
                            Notification::error(format!("{verb} failed: {e}")),
                            cx,
                        );
                    });
                    return;
                }
            };

            // 1b. Free-space precheck + Auto-mode resolution (background:
            // same_volume stats + statvfs aren't UI-thread work). A
            // cross-volume transfer writes total_bytes onto the
            // destination volume — refuse up front rather than fail
            // mid-copy and strand a partial. Same-volume clone/rename
            // consume ~nothing, so they skip the check. Resolving the
            // volume relationship here also lets an Auto drag-drop
            // relabel from generic "Transferring" to "Moving"/"Copying".
            let (all_same_pre, space) = {
                let sources = plan.sources.clone();
                let dest_dir = plan.dest_dir.clone();
                let total = plan.total_bytes;
                cx.background_executor()
                    .spawn(async move {
                        let all_same = sources.iter().all(|s| engine::same_volume(s, &dest_dir));
                        let space = if all_same {
                            None
                        } else {
                            engine::available_space(&dest_dir).map(|avail| (avail, total))
                        };
                        (all_same, space)
                    })
                    .await
            };
            if mode == TransferMode::Auto {
                let resolved = if all_same_pre { "Moving" } else { "Copying" };
                if let Ok(mut g) = live_label_run.lock() {
                    *g = format!(
                        "{resolved} {noun_run} to \u{201c}{dest_name_run}\u{201d}\u{2026}"
                    );
                }
            }
            if let Some((avail, total)) = space {
                if total > avail {
                    end_task(cx);
                    let dest_name = dest
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| dest.display().to_string());
                    let _ = win.update(cx, |_, window, cx| {
                        window.push_notification(
                            Notification::error(format!(
                                "Not enough space on \u{201c}{dest_name}\u{201d} \u{2014} needs {}, only {} free",
                                ferail_fs_native::humanize_bytes(total),
                                ferail_fs_native::humanize_bytes(avail),
                            )),
                            cx,
                        );
                    });
                    return;
                }
            }

            // 2. Per-item collision resolution. Pasting next to the
            // originals obviously means "make me a copy" — no dialog.
            // Otherwise prompt once per conflicting top-level item, with
            // an "apply to the rest" shortcut that fills the remainder
            // with that choice. The map is keyed by source path; the
            // engine consults it per item (only on an actual collision).
            let same_dir = sources.iter().all(|s| s.parent() == Some(dest.as_path()));
            let conflicting: Vec<PathBuf> = if same_dir {
                Vec::new()
            } else {
                sources
                    .iter()
                    .filter(|s| {
                        s.file_name()
                            .map(|n| {
                                plan.conflicts.iter().any(|c| c == &plan.dest_dir.join(n))
                            })
                            .unwrap_or(false)
                    })
                    .cloned()
                    .collect()
            };
            let dest_label = dest
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| dest.display().to_string());
            let mut policies: std::collections::HashMap<PathBuf, CollisionPolicy> =
                std::collections::HashMap::new();
            let mut apply_rest: Option<CollisionPolicy> = None;
            let total = conflicting.len();
            for (i, src) in conflicting.iter().enumerate() {
                if let Some(p) = apply_rest {
                    policies.insert(src.clone(), p);
                    continue;
                }
                let remaining = total - i;
                let name = src
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| src.display().to_string());
                let dest_label = dest_label.clone();
                let (choice_tx, choice_rx) =
                    async_channel::bounded::<Option<(CollisionPolicy, bool)>>(1);
                // Shared "apply to rest" toggle, read when a policy
                // button is clicked. `window.refresh()` re-runs the
                // dialog's (re-rendered) build closure to show the check.
                let apply_all = std::rc::Rc::new(std::cell::Cell::new(false));
                let opened = win.update(cx, |_, window, cx| {
                    let tx = choice_tx.clone();
                    let flag = apply_all.clone();
                    let name = name.clone();
                    window.open_dialog(cx, move |dialog, _window, _cx| {
                        let tx_keep = tx.clone();
                        let tx_replace = tx.clone();
                        let tx_skip = tx.clone();
                        let tx_cancel = tx.clone();
                        let (f_keep, f_replace, f_skip, f_box) =
                            (flag.clone(), flag.clone(), flag.clone(), flag.clone());
                        let buttons = h_flex()
                            .gap_2()
                            .pt_2()
                            .child(
                                Button::new("collision-keep-both")
                                    .label("Keep Both")
                                    .primary()
                                    .small()
                                    .on_click(move |_, window, cx| {
                                        let _ = tx_keep.try_send(Some((
                                            CollisionPolicy::KeepBoth,
                                            f_keep.get(),
                                        )));
                                        window.close_dialog(cx);
                                    }),
                            )
                            .child(
                                Button::new("collision-replace")
                                    .label("Replace")
                                    .small()
                                    .on_click(move |_, window, cx| {
                                        let _ = tx_replace.try_send(Some((
                                            CollisionPolicy::Replace,
                                            f_replace.get(),
                                        )));
                                        window.close_dialog(cx);
                                    }),
                            )
                            .child(
                                Button::new("collision-skip")
                                    .label("Skip")
                                    .small()
                                    .on_click(move |_, window, cx| {
                                        let _ = tx_skip.try_send(Some((
                                            CollisionPolicy::Skip,
                                            f_skip.get(),
                                        )));
                                        window.close_dialog(cx);
                                    }),
                            );
                        let mut d = dialog
                            .title("An item already exists")
                            .child(div().text_scale_sm().child(format!(
                                "\u{201c}{name}\u{201d} already exists in \u{201c}{dest_label}\u{201d}."
                            )));
                        if remaining > 1 {
                            d = d.child(
                                gpui_component::checkbox::Checkbox::new("collision-apply-rest")
                                    .small()
                                    .label(format!(
                                        "Apply to the remaining {} item{}",
                                        remaining - 1,
                                        if remaining - 1 == 1 { "" } else { "s" }
                                    ))
                                    .checked(f_box.get())
                                    .on_click(move |checked, window, _cx| {
                                        f_box.set(*checked);
                                        window.refresh();
                                    }),
                            );
                        }
                        d.child(buttons).on_cancel(move |_, _, _| {
                            let _ = tx_cancel.try_send(None);
                            true
                        })
                    });
                });
                if opened.is_err() {
                    end_task(cx);
                    return;
                }
                // A dismissed dialog drops the senders → recv errors →
                // treated as cancel. Nothing can wedge the task open.
                match choice_rx.recv().await {
                    Ok(Some((p, all))) => {
                        policies.insert(src.clone(), p);
                        if all {
                            apply_rest = Some(p);
                        }
                    }
                    _ => {
                        end_task(cx);
                        return;
                    }
                }
            }

            // Hold off idle system sleep for the duration of the
            // byte-moving engine run, so the machine doesn't doze
            // mid-transfer and strand a half-written file
            // (docs/features/POWER.md). Reacting to WillSleep would be
            // too late — the engine can't checkpoint-and-resume — so we
            // prevent the sleep instead. None (assertion declined) just
            // means no guard; released right after the engine returns.
            let sleep_blocker =
                crate::platform_shell::prevent_idle_sleep(&format!("Ferail {verb}"));

            // 3. Run the engine on the background executor. The
            // same-volume answer rides along for move-undo
            // eligibility (stat — not allowed on the UI thread).
            let result = {
                let c = cancel.clone();
                let p = prog_run.clone();
                cx.background_executor()
                    .spawn(async move {
                        let all_same_volume = plan
                            .sources
                            .iter()
                            .all(|s| engine::same_volume(s, &plan.dest_dir));
                        // Per-source collision policy resolved by the
                        // dialog loop above; non-conflicting items never
                        // consult it (their dest doesn't exist).
                        let policy_for = |s: &std::path::Path| {
                            policies.get(s).copied().unwrap_or(CollisionPolicy::KeepBoth)
                        };
                        // dnd-spec §3.6: Auto resolves here, in the
                        // worker, where stat is allowed.
                        let effective = match mode {
                            TransferMode::Auto if all_same_volume => TransferMode::Move,
                            TransferMode::Auto => TransferMode::Copy,
                            m => m,
                        };
                        let outcome = match effective {
                            TransferMode::Move => engine::run_move(&plan, &policy_for, &p, &c),
                            _ => engine::run_copy(&plan, &policy_for, &p, &c),
                        };
                        outcome.map(|o| (o, all_same_volume, effective))
                    })
                    .await
            };
            // Engine finished — stop the sampler; the final state is set
            // by the completion block below. Drop the sleep guard now
            // that the byte-moving is done (the tail below is cheap UI).
            done_run.store(true, std::sync::atomic::Ordering::Relaxed);
            drop(sleep_blocker);

            // 4. Finish: end task, register undo, reload, notify.
            let mut surfaced = false;
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| {
                    surfaced = this
                        .process
                        .tasks
                        .borrow_mut()
                        .end_and_was_surfaced(task_id);
                    if let Ok((outcome, all_same_volume, effective)) = &result {
                        if !outcome.created.is_empty() {
                            match effective {
                                TransferMode::Move if *all_same_volume => {
                                    this.push_undo(UndoOp::MoveBack(outcome.created.clone()));
                                }
                                // Cross-volume (or mixed-volume) move:
                                // undo copies each item back and then
                                // deletes the moved copy. Only when the
                                // move replaced nothing — copy-back-
                                // undoing a replace would erase the sole
                                // remaining version's provenance.
                                TransferMode::Move if outcome.replaced == 0 => {
                                    this.push_undo(UndoOp::MoveBackCross(
                                        outcome.created.clone(),
                                    ));
                                }
                                TransferMode::Copy if outcome.replaced == 0 => {
                                    this.push_undo(UndoOp::RemoveCreated(
                                        outcome.created.iter().map(|(_, d)| d.clone()).collect(),
                                    ));
                                }
                                _ => {}
                            }
                        }
                    }
                    cx.notify();
                });
            }
            let mut reload = vec![dest.clone()];
            // Anything that may have moved needs its source parents
            // refreshed too (Auto may have resolved to Move; on error
            // a partial move may have happened — over-reloading is
            // harmless).
            if mode != TransferMode::Copy {
                for s in &sources {
                    if let Some(p) = s.parent() {
                        let p = p.to_path_buf();
                        if !reload.contains(&p) {
                            reload.push(p);
                        }
                    }
                }
            }
            Shell::broadcast_reload_for_process(&process, reload, cx);
            let _ = win.update(cx, |_, window, cx| {
                match &result {
                    Ok((outcome, _, effective)) => {
                        // Failures always surface — even for sub-150ms ops —
                        // as a per-item, classified report. The toast offers
                        // Copy (raw detail → clipboard), an in-process Retry of
                        // just the failed items, and Retry as administrator…
                        // when a permission denial could be fixed by elevating.
                        if outcome.has_failures() {
                            let op_noun = match effective {
                                TransferMode::Move => "Move",
                                _ => "Copy",
                            };
                            let summary = file_op_outcome_summary(op_noun, outcome);
                            // The failed top-level sources: those with a failure
                            // recorded against them (path equals or sits under
                            // the source). Excludes succeeded and skipped items.
                            let retry_sources: Vec<PathBuf> = sources
                                .iter()
                                .filter(|s| {
                                    let s: &PathBuf = s;
                                    outcome
                                        .failed
                                        .iter()
                                        .any(|f| f.path == *s || f.path.starts_with(s))
                                })
                                .cloned()
                                .collect();
                            if retry_sources.is_empty() {
                                window.push_notification(error_notification(summary), cx);
                            } else {
                                let elevation_recoverable = outcome
                                    .failed
                                    .iter()
                                    .any(|f| f.kind.is_elevation_recoverable());
                                // The exact locked paths (not top-level
                                // sources): the lock lookup needs the file the
                                // OS actually refused. Capped — a thousand
                                // locked items would all name the same app.
                                let locked: Vec<PathBuf> = outcome
                                    .failed
                                    .iter()
                                    .filter(|f| f.kind.is_lock())
                                    .take(16)
                                    .map(|f| f.path.clone())
                                    .collect();
                                let retry = crate::shell::TransferRetry {
                                    shell: weak.clone(),
                                    sources: retry_sources,
                                    dest: dest.clone(),
                                    mode: *effective,
                                    elevation_recoverable,
                                    locked,
                                };
                                window.push_notification(
                                    crate::shell::transfer_failure_notification(summary, retry),
                                    cx,
                                );
                            }
                            return;
                        }
                        if !surfaced && !outcome.cancelled && outcome.skipped == 0 {
                            return;
                        }
                        let done_verb = match effective {
                            TransferMode::Move => "Moved",
                            _ => "Copied",
                        };
                        let n = outcome.created.len();
                        let items = if n == 1 { "item" } else { "items" };
                        let mut msg = if outcome.cancelled {
                            format!(
                                "Cancelled \u{2014} {n} {items} {}",
                                done_verb.to_lowercase()
                            )
                        } else {
                            format!("{done_verb} {n} {items}")
                        };
                        if outcome.skipped > 0 {
                            msg.push_str(&format!(", {} skipped", outcome.skipped));
                        }
                        let note = if outcome.cancelled {
                            Notification::info(msg)
                        } else {
                            Notification::success(msg)
                        };
                        window.push_notification(note, cx);
                    }
                    Err(e) => {
                        window.push_notification(
                            Notification::error(format!("{verb} failed: {e}")),
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }

    /// Re-run the failed items of a transfer **with administrator privileges**.
    /// Serialises them into an `ElevatedOp` and runs the privileged worker
    /// (which re-execs this binary elevated — one OS auth prompt); reads back
    /// which items still failed and reports it. The whole thing runs off the UI
    /// thread because the auth dialog blocks.
    pub(crate) fn retry_transfer_elevated(
        &mut self,
        sources: Vec<PathBuf>,
        dest: PathBuf,
        mode: TransferMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;

        let is_move = matches!(mode, TransferMode::Move);
        let op = crate::elevation::ElevatedOp {
            is_move,
            dest_dir: dest.clone(),
            sources: sources.clone(),
        };
        let process = self.process.clone();
        let win = window.window_handle();
        cx.spawn(async move |_this, cx| {
            // Blocks on the OS auth dialog — keep it on the executor, never the
            // UI thread (Prime Directive).
            let result = cx
                .background_executor()
                .spawn(async move { crate::elevation::run_elevated_op(&op) })
                .await;

            // Refresh the destination, plus the moved-from parents on a move —
            // a partial elevated run may have changed either side.
            let mut reload = vec![dest.clone()];
            if is_move {
                for s in &sources {
                    if let Some(p) = s.parent() {
                        let p = p.to_path_buf();
                        if !reload.contains(&p) {
                            reload.push(p);
                        }
                    }
                }
            }
            Shell::broadcast_reload_for_process(&process, reload, cx);

            let _ = win.update(cx, |_, window, cx| match result {
                Ok(r) if r.failures.is_empty() => {
                    let items = if r.ok == 1 { "item" } else { "items" };
                    window.push_notification(
                        Notification::success(format!(
                            "Completed {} {items} as administrator",
                            r.ok
                        )),
                        cx,
                    );
                }
                Ok(r) => {
                    let mut msg = format!(
                        "As administrator: {} done \u{00b7} {} still failed",
                        r.ok,
                        r.failures.len()
                    );
                    for (kind, path) in r.failures.iter().take(4) {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        msg.push_str(&format!("\n\u{2022} {name} \u{2014} {}", kind.summary()));
                    }
                    window.push_notification(error_notification(msg), cx);
                }
                Err(e) if e == "cancelled" => {
                    window
                        .push_notification(Notification::info("Administrator retry cancelled"), cx);
                }
                Err(e) => {
                    window.push_notification(
                        error_notification(format!("Retry as administrator failed: {e}")),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// "What's using it?" — name the processes holding the locked items, then
    /// offer close-and-retry from a follow-up toast. The Restart Manager scan
    /// enumerates every process (seconds, not millis), so it runs on the
    /// background executor (Prime Directive).
    pub(crate) fn inspect_locked_retry(
        &mut self,
        retry: crate::shell::TransferRetry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::Sizable as _;
        use gpui_component::button::Button;
        use gpui_component::notification::Notification;

        let locked = retry.locked.clone();
        let win = window.window_handle();
        cx.spawn(async move |_this, cx| {
            let holders = cx
                .background_executor()
                .spawn(async move {
                    // Dedupe by pid — one app usually holds several items.
                    let mut seen: Vec<crate::platform_shell::LockingProcess> = Vec::new();
                    for path in &locked {
                        for lp in crate::platform_shell::processes_using(path) {
                            if !seen.iter().any(|s| s.pid == lp.pid) {
                                seen.push(lp);
                            }
                        }
                    }
                    seen.sort_by(|a, b| a.name.cmp(&b.name));
                    seen
                })
                .await;

            let _ = win.update(cx, |_, window, cx| {
                if holders.is_empty() {
                    // Whatever held it let go since the failure.
                    let r = retry.clone();
                    let note =
                        Notification::info("No process is holding these files anymore").action(
                            move |_, _, cx| {
                                let r = r.clone();
                                Button::new("retry-after-lock").label("Retry").small().on_click(
                                    cx.listener(move |note, _, window, cx| {
                                        let _ = r.shell.update(cx, |shell, cx| {
                                            shell.spawn_transfer_op(
                                                r.sources.clone(),
                                                r.dest.clone(),
                                                r.mode,
                                                window,
                                                cx,
                                            );
                                        });
                                        note.dismiss(window, cx);
                                    }),
                                )
                            },
                        );
                    window.push_notification(note, cx);
                    return;
                }
                let names = holders
                    .iter()
                    .map(|h| h.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let pids: Vec<u32> = holders.iter().map(|h| h.pid).collect();
                let r = retry.clone();
                let note = Notification::warning(format!("In use by {names}")).action(
                    move |_, _, cx| {
                        let r = r.clone();
                        let pids = pids.clone();
                        Button::new("close-and-retry")
                            .label("Close & retry")
                            .small()
                            .on_click(cx.listener(move |note, _, window, cx| {
                                let _ = r.shell.update(cx, |shell, cx| {
                                    shell.force_close_then_retry(
                                        r.clone(),
                                        pids.clone(),
                                        window,
                                        cx,
                                    );
                                });
                                note.dismiss(window, cx);
                            }))
                    },
                );
                window.push_notification(note, cx);
            });
        })
        .detach();
    }

    /// Close the named processes (graceful, then forced), then re-run the
    /// failed items. Blocking close runs on the background executor.
    pub(crate) fn force_close_then_retry(
        &mut self,
        retry: crate::shell::TransferRetry,
        pids: Vec<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let win = window.window_handle();
        cx.spawn(async move |_this, cx| {
            let closed = cx
                .background_executor()
                .spawn(async move { crate::platform_shell::force_close_processes(&pids) })
                .await;
            let _ = win.update(cx, |_, window, cx| match closed {
                Ok(()) => {
                    let _ = retry.shell.update(cx, |shell, cx| {
                        shell.spawn_transfer_op(
                            retry.sources.clone(),
                            retry.dest.clone(),
                            retry.mode,
                            window,
                            cx,
                        );
                    });
                }
                Err(e) => {
                    window.push_notification(
                        error_notification(format!("Couldn't close the apps: {e}")),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    pub(super) fn on_copy_path(
        &mut self,
        _: &CopyPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        let paths: Vec<PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        if paths.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(
            paths
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        let msg = if paths.len() == 1 {
            "Path copied to clipboard".to_string()
        } else {
            format!("{} paths copied to clipboard", paths.len())
        };
        window.push_notification(Notification::success(msg), cx);
    }

    /// Copy the *whole* visible list — every row in the active tab,
    /// not just the selection — as newline-joined full paths. Serves
    /// folder views, duplicate-finder results, and search results
    /// uniformly: all three feed the same table delegate, so iterating
    /// its rows and resolving each path from the cache (never the
    /// filesystem) covers them all.
    pub(super) fn on_copy_file_list(
        &mut self,
        _: &CopyFileList,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        let row_count = self.active_tab().table.read(cx).delegate().entries.len();
        let paths: Vec<PathBuf> = (0..row_count)
            .filter_map(|row| self.path_for_row(row, cx))
            .collect();
        if paths.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(
            paths
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        let msg = format!("Copied list of {} items to clipboard", paths.len());
        window.push_notification(Notification::success(msg), cx);
    }

    pub(super) fn on_reveal_in_finder(
        &mut self,
        _: &RevealInFinder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths: Vec<PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        if paths.is_empty() {
            return;
        }
        // FanOut: each path is revealed in its own platform file manager, so a
        // large selection is guarded behind a confirm.
        let count = paths.len();
        let plural = if count == 1 { "item" } else { "items" };
        let reveal_target = if cfg!(windows) { "Explorer" } else { "Finder" };
        let reveal_title = if cfg!(windows) {
            "Reveal in Explorer?"
        } else {
            "Reveal in Finder?"
        };
        self.confirm_fanout(
            count,
            reveal_title,
            format!("Reveal {count} {plural} in {reveal_target}?"),
            "Reveal",
            window,
            cx,
            move |_this, window, cx| {
                use gpui_component::notification::Notification;
                let msg = if paths.len() == 1 {
                    let name = paths[0]
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("item")
                        .to_string();
                    format!("Showing \u{201C}{}\u{201D} in {}", name, reveal_target)
                } else {
                    format!("Showing {} items in {}", paths.len(), reveal_target)
                };
                // Reveal is a process spawn on mac/win but a blocking
                // D-Bus round-trip per path on Linux — run the loop on
                // the background executor (Prime Directive).
                cx.background_spawn(async move {
                    for path in &paths {
                        crate::platform_shell::reveal_in_finder(path);
                    }
                })
                .detach();
                window.push_notification(Notification::info(msg), cx);
            },
        );
    }

    pub(super) fn on_reveal_context_path(
        &mut self,
        _: &RevealContextPath,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.context_target.take() else {
            return;
        };
        // Blocking D-Bus round-trip on Linux — worker, not UI thread.
        cx.background_spawn(async move {
            crate::platform_shell::reveal_in_finder(&path);
        })
        .detach();
    }

    pub(super) fn on_copy_context_path(
        &mut self,
        _: &CopyContextPath,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.context_target.take() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(
            path.to_string_lossy().into_owned(),
        ));
    }

    /// File-list "Open Terminal Here". Folder-only menu item; resolves
    /// the right-clicked (or selected) directory via the same path as
    /// `CopyPath`, then hands it to the platform shell.
    pub(super) fn on_open_terminal_here(
        &mut self,
        _: &OpenTerminalHere,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, _, path)) = self.action_entries_visible_order(cx).into_iter().next() else {
            return;
        };
        // Process spawn — worker, not UI thread (Prime Directive).
        cx.background_spawn(async move {
            crate::platform_shell::open_terminal(&path);
        })
        .detach();
    }

    /// Sidebar/tree/breadcrumb "Open Terminal Here". Resolves the
    /// right-clicked folder from `context_target` (mirrors
    /// `on_copy_context_path`).
    pub(super) fn on_open_terminal_at_context(
        &mut self,
        _: &OpenTerminalAtContext,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.context_target.take() else {
            return;
        };
        // Process spawn — worker, not UI thread (Prime Directive).
        cx.background_spawn(async move {
            crate::platform_shell::open_terminal(&path);
        })
        .detach();
    }

    /// Sidebar volume menu "Eject". Unmounts and ejects the right-
    /// clicked volume (`context_target`) on a worker — `unmountAndEject`
    /// can block while the system flushes/closes the device — then
    /// reports success or failure as a toast. The volume observer drops
    /// the row from the sidebar once the unmount lands.
    pub(super) fn on_eject_volume(
        &mut self,
        _: &EjectVolume,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.context_target.take() else {
            return;
        };
        self.eject_path(path, window, cx);
    }

    /// Eject the volume mounted at `path`. Shared by the context-menu
    /// "Eject" action and the trailing eject button drawn on ejectable
    /// volume rows.
    ///
    /// Finder parity: when the volume shares its physical device with
    /// other mounted volumes (a partitioned external disk, a
    /// multi-volume APFS container), ask first — eject just this
    /// volume, or every volume on the disk so it can be unplugged. The
    /// sibling lookup reads the cached sidebar volume list only; no
    /// I/O on the click path.
    pub(crate) fn eject_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::button::ButtonVariants as _;

        let name = self.volume_display_name(&path);
        let siblings = self.sibling_volumes_on_device(&path);
        if siblings.is_empty() {
            self.eject_volumes(vec![(path, name)], window, cx);
            return;
        }

        #[derive(Clone, Copy)]
        enum EjectChoice {
            One,
            All,
            Cancel,
        }
        let (choice_tx, choice_rx) = async_channel::bounded::<EjectChoice>(1);
        let total = siblings.len() + 1;
        let body = format!(
            "\u{201C}{name}\u{201D} is one of {total} volumes on its disk. \
             Do you want to eject \u{201C}{name}\u{201D} only, or all volumes on the disk?"
        );
        let only_label = format!("Eject \u{201C}{name}\u{201D} Only");
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let tx_all = choice_tx.clone();
            let tx_one = choice_tx.clone();
            let tx_cancel = choice_tx.clone();
            dialog
                .title("Eject")
                .child(div().text_scale_sm().child(body.clone()))
                .child(
                    h_flex()
                        .pt_2()
                        .gap_2()
                        .child(
                            Button::new("eject-all")
                                .label("Eject All")
                                .primary()
                                .small()
                                .on_click(move |_, window, cx| {
                                    let _ = tx_all.try_send(EjectChoice::All);
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("eject-one")
                                .label(only_label.clone())
                                .small()
                                .on_click(move |_, window, cx| {
                                    let _ = tx_one.try_send(EjectChoice::One);
                                    window.close_dialog(cx);
                                }),
                        ),
                )
                .on_cancel(move |_, _, _| {
                    let _ = tx_cancel.try_send(EjectChoice::Cancel);
                    true
                })
        });
        let weak = cx.weak_entity();
        let win = window.window_handle();
        cx.spawn(async move |_, cx| {
            let choice = match choice_rx.recv().await {
                Ok(EjectChoice::Cancel) | Err(_) => return,
                Ok(choice) => choice,
            };
            let _ = win.update(cx, |_, window, cx| {
                let Some(shell) = weak.upgrade() else { return };
                shell.update(cx, |shell, cx| {
                    let volumes = match choice {
                        EjectChoice::All => {
                            // Clicked volume last: the device powers
                            // down when its final volume unmounts.
                            let mut all = siblings.clone();
                            all.push((path.clone(), name.clone()));
                            all
                        }
                        _ => vec![(path.clone(), name.clone())],
                    };
                    shell.eject_volumes(volumes, window, cx);
                });
            });
        })
        .detach();
    }

    /// Display name for the volume mounted at `path`, from the cached
    /// sidebar volume list (fallback: the path's leaf).
    fn volume_display_name(&self, path: &Path) -> String {
        self.process
            .volumes
            .borrow()
            .iter()
            .find(|v| v.path == path)
            .map(|v| v.name.clone())
            .unwrap_or_else(|| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| path.display().to_string())
            })
    }

    /// Other mounted volumes sharing `path`'s physical device, as
    /// `(mount path, display name)` pairs from the cached sidebar
    /// volume list — no I/O. Removable volumes only: an internal-disk
    /// grouping must never be swept into an eject-all.
    fn sibling_volumes_on_device(&self, path: &Path) -> Vec<(PathBuf, String)> {
        let volumes = self.process.volumes.borrow();
        let Some(device) = volumes
            .iter()
            .find(|v| v.path == path)
            .and_then(|v| v.device_id.clone())
        else {
            return Vec::new();
        };
        volumes
            .iter()
            .filter(|v| {
                v.path != path && v.is_removable && v.device_id.as_deref() == Some(device.as_str())
            })
            .map(|v| (v.path.clone(), v.name.clone()))
            .collect()
    }

    /// Eject the given `(mount path, display name)` volumes on a
    /// worker (unmount/eject can block while the system flushes and
    /// closes the device). A single volume goes through
    /// `eject_volume`; several — the "Eject All" answer — go through
    /// `eject_device`, which unmounts every partition before powering
    /// the device down (a per-volume loop spuriously fails on macOS
    /// while sibling partitions are mounted). Failure toasts are
    /// enriched with the processes still holding files open on the
    /// volume — the usual reason an eject fails, and the part Finder's
    /// "disk is in use" alert never tells you. The volume observer
    /// drops ejected rows from the sidebar once the unmounts land.
    fn eject_volumes(
        &mut self,
        volumes: Vec<(PathBuf, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;

        /// Failed-eject toast: name who's blocking when we can (the
        /// actionable why), else fall back to the platform error.
        fn failure_message(busy: &[String], what: &str, err: &str) -> String {
            let apps = match busy {
                [] => return format!("Couldn’t eject {what}: {err}"),
                [a] => format!("{a} has"),
                [a, b] => format!("{a} and {b} have"),
                [head @ .., last] => format!("{} and {last} have", head.join(", ")),
            };
            format!("Couldn’t eject {what} — {apps} files open on it. Close them and try again.")
        }

        let win = window.window_handle();
        cx.spawn(async move |_this, cx| {
            let (ejected, failures) = cx
                .background_executor()
                .spawn(async move {
                    let mut ejected: Vec<String> = Vec::new();
                    let mut failures: Vec<String> = Vec::new();
                    match volumes.as_slice() {
                        [] => {}
                        [(path, name)] => match crate::platform_shell::eject_volume(path) {
                            Ok(()) => ejected.push(name.clone()),
                            Err(e) => {
                                let busy = crate::platform_shell::volume_busy_processes(path);
                                let what = format!("\u{201C}{name}\u{201D}");
                                failures.push(failure_message(&busy, &what, &e));
                            }
                        },
                        many => {
                            let paths: Vec<&Path> = many.iter().map(|(p, _)| p.as_path()).collect();
                            match crate::platform_shell::eject_device(&paths) {
                                Ok(()) => ejected.extend(many.iter().map(|(_, n)| n.clone())),
                                Err(e) => {
                                    // Whoever holds a file open on *any* of the
                                    // device's volumes blocks the whole eject.
                                    let mut busy: Vec<String> = many
                                        .iter()
                                        .flat_map(|(p, _)| {
                                            crate::platform_shell::volume_busy_processes(p)
                                        })
                                        .collect();
                                    busy.sort();
                                    busy.dedup();
                                    busy.truncate(5);
                                    failures.push(failure_message(&busy, "the disk", &e));
                                }
                            }
                        }
                    }
                    (ejected, failures)
                })
                .await;
            let _ = win.update(cx, |_, window, cx| {
                if failures.is_empty() && !ejected.is_empty() {
                    let msg = match ejected.as_slice() {
                        [name] => format!("Ejected \u{201C}{name}\u{201D}"),
                        names => format!("Ejected {} volumes", names.len()),
                    };
                    window.push_notification(Notification::info(msg), cx);
                }
                for msg in failures {
                    window.push_notification(Notification::error(msg), cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn on_open_context_in_new_tab(
        &mut self,
        _: &OpenContextInNewTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.context_target.take() else {
            return;
        };
        let id = self.process.fs.id_for_path(&path);
        self.process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(path.clone(), id);
        let tab = self.make_tab(path, id, window, cx);
        let insert_at = self.active + 1;
        self.tabs.insert(insert_at, tab);
        self.active = insert_at;
        let active_id = self.process.fs.id_for_path(&self.active_tab().current_dir);
        self.navigate_node(active_id, cx);
    }

    pub(super) fn on_new_folder_here(
        &mut self,
        _: &NewFolderHere,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(target) = self.context_target.take() {
            let saved = self.active_tab().current_dir.clone();
            self.active_tab_mut().current_dir = target;
            self.on_new_folder(&NewFolder, window, cx);
            self.active_tab_mut().current_dir = saved;
        } else {
            self.on_new_folder(&NewFolder, window, cx);
        }
    }

    fn toggle_tag_on_target(
        &mut self,
        color: ferail_core::commands::TagColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Tag xattrs are filesystem I/O — a large selection means one
        // read-modify-write per file, and any file on a dead network
        // mount blocks for the mount timeout. Prime Directive: collect
        // the paths here, run the toggles on the background executor,
        // then re-read the listing's tags on completion so the dot
        // chips repaint (the writes only touch disk, not the delegate's
        // parallel `tags` vec).
        let paths: Vec<PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        if paths.is_empty() {
            return;
        }
        let active = self.active;
        let win = window.window_handle();
        cx.spawn(async move |this, cx| {
            let (done, failures) = cx
                .background_executor()
                .spawn(async move {
                    let mut done = 0usize;
                    let mut failures: Vec<ferail_fs_native::file_ops::FileOpError> = Vec::new();
                    for path in &paths {
                        match crate::platform_shell::toggle_tag(path, color) {
                            Ok(()) => done += 1,
                            // Stringly-typed platform error — classify it
                            // so the report shares the one advice table.
                            Err(e) => failures.push(
                                ferail_fs_native::file_ops::FileOpError::other(
                                    path,
                                    super::classify_error_text(&e),
                                    e,
                                ),
                            ),
                        }
                    }
                    (done, failures)
                })
                .await;
            if !failures.is_empty() {
                crate::log_warn!(90, "tag toggle: {} item(s) failed", failures.len());
                // Quiet on success; failures surface through the same
                // structured "N of M · why" report as the other mutations.
                let summary = crate::shell::file_op_failure_report("Tag", done, 0, &failures);
                let _ = win.update(cx, |_, window, cx| {
                    window.push_notification(super::error_notification(summary), cx);
                });
            }
            let _ = this.update(cx, |this, cx| {
                let tab = if this.tabs.get(active).is_some() {
                    active
                } else {
                    this.active
                };
                this.refresh_file_list_tags_in_tab(tab, cx);
            });
        })
        .detach();
    }

    pub(super) fn on_toggle_tag_red(
        &mut self,
        _: &ToggleTagRed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(ferail_core::commands::TagColor::Red, window, cx);
    }

    pub(super) fn on_toggle_tag_orange(
        &mut self,
        _: &ToggleTagOrange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(ferail_core::commands::TagColor::Orange, window, cx);
    }

    pub(super) fn on_toggle_tag_yellow(
        &mut self,
        _: &ToggleTagYellow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(ferail_core::commands::TagColor::Yellow, window, cx);
    }

    pub(super) fn on_toggle_tag_green(
        &mut self,
        _: &ToggleTagGreen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(ferail_core::commands::TagColor::Green, window, cx);
    }

    pub(super) fn on_toggle_tag_blue(
        &mut self,
        _: &ToggleTagBlue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(ferail_core::commands::TagColor::Blue, window, cx);
    }

    pub(super) fn on_toggle_tag_purple(
        &mut self,
        _: &ToggleTagPurple,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(ferail_core::commands::TagColor::Purple, window, cx);
    }

    pub(super) fn on_toggle_tag_gray(
        &mut self,
        _: &ToggleTagGray,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(ferail_core::commands::TagColor::Gray, window, cx);
    }

    fn open_with_slot(&mut self, slot: usize, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        let Some(first) = paths.first() else { return };
        // Resolve the slot against the SAME warm cache the menu was
        // built from. Re-fetching here used to be both a sync
        // LaunchServices stall on action dispatch and a correctness
        // hazard: a fresh fetch can order candidates differently than
        // the list the user just looked at, silently opening the
        // wrong app for slot N. Cache mismatch (possible only if the
        // lead changed between menu open and click) is a no-op.
        let app_path: Option<PathBuf> = {
            let delegate = self.active_tab().table.read(cx).delegate();
            match &delegate.open_with_warm {
                Some((warm_path, cands)) if warm_path == first => {
                    cands.get(slot).map(|c| c.path.clone())
                }
                _ => {
                    crate::log_warn!(
                        90,
                        "open_with_slot {slot}: warm cache miss for {}; ignoring",
                        first.display()
                    );
                    None
                }
            }
        };
        if let Some(app) = app_path {
            // `open -a` waits for the app to check in (seconds on a
            // cold launch) — one batched invocation on the background
            // executor instead of N sequential waits on the UI thread.
            cx.background_spawn(async move {
                if let Err(e) = crate::platform_shell::open_with_app_many(&paths, &app) {
                    crate::log_warn!(90, "open with {}: {e}", app.display());
                }
            })
            .detach();
        }
    }

    /// Warm the Open With cache for the row the user just selected /
    /// right-clicked, so the context menu can build its submenu
    /// without any synchronous shell query (prime directive). Cheap
    /// no-op when the cache already holds this row's path.
    pub(super) fn warm_open_with_for_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        let Some(path) = self.path_for_row(row_ix, cx) else {
            return;
        };
        let table = self.active_tab().table.clone();
        if let Some((warm_path, _)) = &table.read(cx).delegate().open_with_warm {
            if *warm_path == path {
                return;
            }
        }
        crate::file_list::spawn_open_with_warm(table, path, cx);
    }

    pub(super) fn on_open_with_slot_0(
        &mut self,
        _: &OpenWithSlot0,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(0, cx);
    }

    pub(super) fn on_open_with_slot_1(
        &mut self,
        _: &OpenWithSlot1,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(1, cx);
    }

    pub(super) fn on_open_with_slot_2(
        &mut self,
        _: &OpenWithSlot2,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(2, cx);
    }

    pub(super) fn on_open_with_slot_3(
        &mut self,
        _: &OpenWithSlot3,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(3, cx);
    }

    pub(super) fn on_open_with_slot_4(
        &mut self,
        _: &OpenWithSlot4,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(4, cx);
    }

    pub(super) fn on_open_with_slot_5(
        &mut self,
        _: &OpenWithSlot5,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(5, cx);
    }

    pub(super) fn on_open_with_slot_6(
        &mut self,
        _: &OpenWithSlot6,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(6, cx);
    }

    pub(super) fn on_open_with_slot_7(
        &mut self,
        _: &OpenWithSlot7,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(7, cx);
    }

    pub(super) fn on_open_with_slot_8(
        &mut self,
        _: &OpenWithSlot8,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(8, cx);
    }

    pub(super) fn on_open_with_slot_9(
        &mut self,
        _: &OpenWithSlot9,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(9, cx);
    }

    pub(super) fn on_open_with_slot_10(
        &mut self,
        _: &OpenWithSlot10,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(10, cx);
    }

    pub(super) fn on_open_with_slot_11(
        &mut self,
        _: &OpenWithSlot11,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(11, cx);
    }

    pub(super) fn on_move_to_trash(
        &mut self,
        _: &MoveToTrash,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Move to Trash");
        use gpui_component::notification::Notification;
        let paths: Vec<PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        if paths.is_empty() {
            return;
        }
        let count = paths.len();
        let name = if count == 1 {
            paths[0]
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("item")
                .to_string()
        } else {
            format!("{count} items")
        };
        let cur = self.active_tab().current_dir.clone();
        // Bespoke worker (not spawn_file_op): trashItemAtURL reports
        // each item's resulting location inside the Trash [mac], and
        // those (original, trashed) pairs are exactly what Cmd+Z
        // needs to rename things back. Notification moved to
        // completion — the old one fired before the op ran.
        let process = self.process.clone();
        // Register the trash as a foreground task so slow / large deletes
        // (and Empty Trash on a network volume) get a visible, counted
        // row in the status bar + panel. Single-item trashes finish
        // inside SURFACE_DELAY and never flicker. (docs/features/FILE_OPS.md)
        let task_id = self.process.tasks.borrow_mut().begin(
            crate::tasks::TaskKind::FileOp,
            format!("Moving {name} to Trash"),
            false,
        );
        let weak = cx.weak_entity();
        let win = window.window_handle();
        cx.spawn(async move |_this, cx| {
            // Don't bail on the first failure: trash every item we can, and
            // collect the rest as classified `FileOpError`s so a permission
            // denial on one protected app doesn't strand the others and so we
            // can offer an elevated retry for just the recoverable ones.
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut pairs: Vec<(PathBuf, PathBuf)> = Vec::new();
                    let mut done = 0usize;
                    let mut failures: Vec<ferail_fs_native::file_ops::FileOpError> = Vec::new();
                    for path in &paths {
                        match ferail_fs_native::move_to_trash(path) {
                            Ok(Some(trashed)) => {
                                done += 1;
                                pairs.push((path.clone(), trashed));
                            }
                            // Trashed, but the resulting URL wasn't
                            // reported — done, just not undoable.
                            Ok(None) => done += 1,
                            Err(e) => failures.push(
                                ferail_fs_native::file_ops::FileOpError::from_io(&e, path),
                            ),
                        }
                    }
                    (pairs, done, failures)
                })
                .await;
            let (pairs, done, failures) = result;
            match failures.first() {
                Some(f) => process.tasks.borrow_mut().end_failed(task_id, f.to_string()),
                None => process.tasks.borrow_mut().end(task_id),
            }
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| {
                    if !pairs.is_empty() {
                        this.push_undo(UndoOp::TrashRestore(pairs.clone()));
                    }
                    cx.notify();
                });
            }
            Shell::broadcast_reload_for_process(&process, vec![cur], cx);
            let weak = weak.clone();
            let _ = win.update(cx, move |_, window, cx| {
                if failures.is_empty() {
                    window.push_notification(
                        Notification::info(format!("Moved \u{201C}{}\u{201D} to Trash", name)),
                        cx,
                    );
                    return;
                }
                // The structured "N of M · why" report shared with the
                // copy/move path; the raw OS detail rides along for the
                // Copy action.
                let summary =
                    crate::shell::file_op_failure_report("Move to Trash", done, 0, &failures);
                let detail = failures
                    .iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                // The items elevation could still trash — bare permission
                // denials (a root-owned app), not locked/missing ones.
                let recoverable: Vec<PathBuf> = failures
                    .iter()
                    .filter(|f| f.kind.is_elevation_recoverable())
                    .map(|f| f.path.clone())
                    .collect();
                let retry = crate::shell::TrashRetry {
                    shell: weak,
                    sources: recoverable,
                    delete: false,
                };
                window.push_notification(
                    crate::shell::trash_failure_notification(
                        summary.clone(),
                        format!("{summary}\n\n{detail}"),
                        retry,
                    ),
                    cx,
                );
            });
        })
        .detach();
    }

    /// Re-run the given items' trash (or permanent delete, when `delete`) with
    /// administrator rights: serialise them, run the elevated worker (one OS
    /// auth prompt — same osascript path copy/move uses), and report what
    /// landed. Elevated trashes move into the user's `~/.Trash` as root, so the
    /// item lands owned by root; we don't register Undo for them (restoring a
    /// root-owned item to a root-owned location would itself need elevation).
    pub(crate) fn retry_trash_elevated(
        &mut self,
        sources: Vec<PathBuf>,
        delete: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        let trash_dir = ferail_fs_native::home_trash_dir();
        let op = crate::elevation::ElevatedTrashOp {
            delete,
            trash_dir: trash_dir.clone(),
            sources: sources.clone(),
        };
        let process = self.process.clone();
        let win = window.window_handle();
        cx.spawn(async move |_this, cx| {
            // Blocks on the OS auth dialog — keep it off the UI thread.
            let result = cx
                .background_executor()
                .spawn(async move { crate::elevation::run_elevated_trash_op(&op) })
                .await;

            // Refresh the moved-from parents and, for a trash, the Trash itself.
            let mut reload = Vec::new();
            for s in &sources {
                if let Some(p) = s.parent() {
                    let p = p.to_path_buf();
                    if !reload.contains(&p) {
                        reload.push(p);
                    }
                }
            }
            if !delete && !reload.contains(&trash_dir) {
                reload.push(trash_dir);
            }
            Shell::broadcast_reload_for_process(&process, reload, cx);

            let total = sources.len();
            let _ = win.update(cx, move |_, window, cx| match result {
                Ok(r) if r.failed.is_empty() => {
                    let n = if delete { total } else { r.trashed.len() };
                    let items = if n == 1 { "item" } else { "items" };
                    let note = if delete {
                        format!("Deleted {n} {items} as administrator")
                    } else {
                        format!("Moved {n} {items} to Trash as administrator")
                    };
                    window.push_notification(Notification::success(note), cx);
                }
                Ok(r) => {
                    let done = total.saturating_sub(r.failed.len());
                    window.push_notification(
                        super::error_notification(format!(
                            "As administrator: {done} done \u{00b7} {} still failed",
                            r.failed.len()
                        )),
                        cx,
                    );
                }
                Err(e) if e == "cancelled" => {
                    window.push_notification(Notification::info("Administrator action cancelled"), cx);
                }
                Err(e) => {
                    window.push_notification(
                        super::error_notification(format!("Elevated action failed: {e}")),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// Permanently delete the selected items (no Trash) after a counted
    /// confirmation — a targeted Empty Trash. No undo. A permission denial on
    /// a protected item offers an elevated retry, exactly like Move to Trash.
    /// Bound to Option+Cmd+Delete [mac] / Shift+Delete [win/linux].
    pub(super) fn on_delete_immediately(
        &mut self,
        _: &DeleteImmediately,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Delete Immediately");
        use gpui_component::button::ButtonVariants as _;
        use gpui_component::notification::Notification;

        let paths: Vec<PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        if paths.is_empty() {
            return;
        }
        let count = paths.len();
        let name = if count == 1 {
            paths[0]
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("item")
                .to_string()
        } else {
            format!("{count} items")
        };
        let cur = self.active_tab().current_dir.clone();
        let process = self.process.clone();
        let win = window.window_handle();

        cx.spawn(async move |this, cx| {
            // Confirm first — this is the one delete with no undo.
            let (go_tx, go_rx) = async_channel::bounded::<bool>(1);
            let opened = win.update(cx, |_, window, cx| {
                let tx = go_tx.clone();
                let name = name.clone();
                window.open_dialog(cx, move |dialog, _window, _cx| {
                    let tx_go = tx.clone();
                    let tx_cancel = tx.clone();
                    let plural = if count == 1 { "item" } else { "items" };
                    let what = if count == 1 {
                        format!("\u{201C}{name}\u{201D}")
                    } else {
                        format!("{count} {plural}")
                    };
                    let body = format!("Permanently delete {what}? This can\u{2019}t be undone.");
                    dialog
                        .title("Delete Immediately?")
                        .child(div().text_scale_sm().child(body))
                        .child(
                            h_flex().pt_2().child(
                                Button::new("delete-now-go")
                                    .label("Delete")
                                    .danger()
                                    .small()
                                    .on_click(move |_, window, cx| {
                                        let _ = tx_go.try_send(true);
                                        window.close_dialog(cx);
                                    }),
                            ),
                        )
                        .on_cancel(move |_, _, _| {
                            let _ = tx_cancel.try_send(false);
                            true
                        })
                });
            });
            if opened.is_err() || !matches!(go_rx.recv().await, Ok(true)) {
                return;
            }

            let task_id = process.tasks.borrow_mut().begin(
                crate::tasks::TaskKind::FileOp,
                format!("Deleting {name}"),
                false,
            );
            let to_delete = paths;
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut deleted = 0usize;
                    let mut first_err: Option<String> = None;
                    let mut failed_perm: Vec<PathBuf> = Vec::new();
                    for p in &to_delete {
                        let removed = match std::fs::symlink_metadata(p) {
                            Ok(m) if m.is_dir() && !m.is_symlink() => std::fs::remove_dir_all(p),
                            Ok(_) => std::fs::remove_file(p),
                            Err(e) => Err(e),
                        };
                        match removed {
                            Ok(()) => deleted += 1,
                            Err(e) => {
                                if e.kind() == std::io::ErrorKind::PermissionDenied {
                                    failed_perm.push(p.clone());
                                }
                                if first_err.is_none() {
                                    first_err = Some(format!("{}: {e}", p.display()));
                                }
                            }
                        }
                    }
                    (deleted, first_err, failed_perm)
                })
                .await;
            let (deleted, first_err, failed_perm) = result;
            match &first_err {
                Some(e) => process.tasks.borrow_mut().end_failed(task_id, e.clone()),
                None => process.tasks.borrow_mut().end(task_id),
            }
            Shell::broadcast_reload_for_process(&process, vec![cur], cx);
            let _ = win.update(cx, move |_, window, cx| {
                let plural = if deleted == 1 { "item" } else { "items" };
                match first_err {
                    None => window.push_notification(
                        Notification::success(format!("Deleted {deleted} {plural}")),
                        cx,
                    ),
                    Some(e) => {
                        let detail = format!("Deleted {deleted} {plural} with errors: {e}");
                        let headline = if failed_perm.is_empty() {
                            detail.clone()
                        } else if failed_perm.len() == 1 {
                            "1 item needs administrator rights to delete.".to_string()
                        } else {
                            format!(
                                "{} items need administrator rights to delete.",
                                failed_perm.len()
                            )
                        };
                        let retry = crate::shell::TrashRetry {
                            shell: this,
                            sources: failed_perm,
                            delete: true,
                        };
                        window.push_notification(
                            crate::shell::trash_failure_notification(headline, detail, retry),
                            cx,
                        )
                    }
                }
            });
        })
        .detach();
    }

    /// Empty every trash this user can reach (`~/.Trash` + mounted
    /// volumes' `.Trashes/<uid>` [mac]) after an explicit, counted
    /// confirmation. Permanently destructive — the one file operation
    /// with no undo, which is why it confirms first.
    pub(super) fn on_empty_trash(
        &mut self,
        _: &EmptyTrash,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Empty Trash");
        use gpui_component::button::ButtonVariants as _;
        use gpui_component::notification::Notification;
        let process = self.process.clone();
        let win = window.window_handle();
        cx.spawn(async move |this, cx| {
            // Count first (background) so the confirmation says what
            // it's about to destroy.
            let preview = cx
                .background_executor()
                .spawn(async move {
                    let dirs = ferail_fs_native::trash_dirs();
                    let mut items = 0usize;
                    let mut unreadable = false;
                    for d in &dirs {
                        match std::fs::read_dir(d) {
                            Ok(rd) => items += rd.flatten().count(),
                            // TCC can deny Trash reads (e.g. dev runs
                            // from a terminal without Files & Folders
                            // access) — that's "unknown", not "empty".
                            Err(_) => unreadable = true,
                        }
                    }
                    (dirs, items, unreadable)
                })
                .await;
            let (dirs, items, unreadable) = preview;
            if items == 0 && !unreadable {
                let _ = win.update(cx, |_, window, cx| {
                    window.push_notification(Notification::info("Trash is already empty"), cx);
                });
                return;
            }
            let (go_tx, go_rx) = async_channel::bounded::<bool>(1);
            let opened = win.update(cx, |_, window, cx| {
                let tx = go_tx.clone();
                window.open_dialog(cx, move |dialog, _window, _cx| {
                    let tx_go = tx.clone();
                    let tx_cancel = tx.clone();
                    let plural = if items == 1 { "item" } else { "items" };
                    let body = if items > 0 {
                        format!("Permanently delete {items} {plural}? This can't be undone.")
                    } else {
                        // Count unknown (Trash unreadable right now).
                        "Permanently delete everything in the Trash? This can't be undone."
                            .to_string()
                    };
                    dialog
                        .title("Empty Trash?")
                        .child(div().text_scale_sm().child(body))
                        .child(
                            h_flex().pt_2().child(
                                Button::new("empty-trash-go")
                                    .label("Empty Trash")
                                    .danger()
                                    .small()
                                    .on_click(move |_, window, cx| {
                                        let _ = tx_go.try_send(true);
                                        window.close_dialog(cx);
                                    }),
                            ),
                        )
                        .on_cancel(move |_, _, _| {
                            let _ = tx_cancel.try_send(false);
                            true
                        })
                });
            });
            if opened.is_err() || !matches!(go_rx.recv().await, Ok(true)) {
                return;
            }
            // Now that the user has confirmed, surface the destruction as
            // a foreground task — emptying a full Trash (or one on a slow
            // volume) can take real time. (docs/features/FILE_OPS.md)
            let task_id = process.tasks.borrow_mut().begin(
                crate::tasks::TaskKind::FileOp,
                "Emptying Trash",
                false,
            );
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut deleted = 0usize;
                    // Every item that couldn't be removed, classified —
                    // feeds the shared "N of M · why" report, and its
                    // permission-denied subset is exactly what an
                    // elevated retry can finish (root-owned trash).
                    let mut failures: Vec<ferail_fs_native::file_ops::FileOpError> = Vec::new();
                    for d in &dirs {
                        let Ok(rd) = std::fs::read_dir(d) else {
                            continue;
                        };
                        for dirent in rd.flatten() {
                            let p = dirent.path();
                            let removed = match std::fs::symlink_metadata(&p) {
                                Ok(m) if m.is_dir() && !m.is_symlink() => {
                                    std::fs::remove_dir_all(&p)
                                }
                                Ok(_) => std::fs::remove_file(&p),
                                Err(e) => Err(e),
                            };
                            match removed {
                                Ok(()) => deleted += 1,
                                Err(e) => failures.push(
                                    ferail_fs_native::file_ops::FileOpError::from_io(&e, &p),
                                ),
                            }
                        }
                    }
                    (deleted, failures, dirs)
                })
                .await;
            let (deleted, failures, dirs) = result;
            match failures.first() {
                Some(f) => process.tasks.borrow_mut().end_failed(task_id, f.to_string()),
                None => process.tasks.borrow_mut().end(task_id),
            }
            // Trash contents changed under any tab browsing it.
            Shell::broadcast_reload_for_process(&process, dirs, cx);
            let _ = win.update(cx, move |_, window, cx| {
                let plural = if deleted == 1 { "item" } else { "items" };
                if failures.is_empty() {
                    if deleted == 0 && unreadable {
                        window.push_notification(
                            Notification::error(
                                "Couldn't read the Trash (permission denied). Grant Ferail \
                                 Files & Folders access and try again.",
                            ),
                            cx,
                        );
                    } else {
                        window.push_notification(
                            Notification::success(format!(
                                "Emptied Trash \u{2014} {deleted} {plural} deleted"
                            )),
                            cx,
                        );
                    }
                    return;
                }
                // Partial result through the same structured report the
                // copy/move path uses. Root-owned trash items can be
                // finished as admin; when nothing is recoverable this
                // falls back to the plain expandable/copyable toast.
                let summary =
                    crate::shell::file_op_failure_report("Empty Trash", deleted, 0, &failures);
                let detail = failures
                    .iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                let failed_perm: Vec<PathBuf> = failures
                    .iter()
                    .filter(|f| f.kind.is_elevation_recoverable())
                    .map(|f| f.path.clone())
                    .collect();
                let retry = crate::shell::TrashRetry {
                    shell: this,
                    sources: failed_perm,
                    delete: true,
                };
                window.push_notification(
                    crate::shell::trash_failure_notification(
                        summary.clone(),
                        format!("{summary}\n\n{detail}"),
                        retry,
                    ),
                    cx,
                );
            });
        })
        .detach();
    }

    /// Run `work` immediately when fanning out to `count` artifacts is
    /// small; otherwise pop a confirmation and run it only if the user
    /// proceeds. `work` re-enters the live Shell / Window / Context on
    /// confirm. Used to guard commands that each spawn a separate
    /// foreground artifact (a tab, a Get Info window, an app launch, a
    /// Finder reveal) so a stray 200-row selection asks first instead of
    /// opening 200 things. Batch commands that collapse to one operation
    /// (Compress, Move to Trash, Tags) never route through here.
    /// (docs/features/CONTEXT_MENU.md)
    // The fan-out confirmation dialog genuinely needs each of these inputs.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn confirm_fanout(
        &mut self,
        count: usize,
        title: &'static str,
        body: String,
        ok_label: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        work: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        /// At or above this many fan-out artifacts, confirm first.
        const FANOUT_CONFIRM_THRESHOLD: usize = 10;
        if count < FANOUT_CONFIRM_THRESHOLD {
            work(self, window, cx);
            return;
        }
        let (go_tx, go_rx) = async_channel::bounded::<bool>(1);
        let win = window.window_handle();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let tx_go = go_tx.clone();
            let tx_cancel = go_tx.clone();
            let body = body.clone();
            dialog
                .title(title)
                .child(div().text_sm().child(body))
                .child(
                    h_flex().pt_2().child(
                        Button::new("confirm-fanout-go")
                            .label(ok_label)
                            .small()
                            .on_click(move |_, window, cx| {
                                let _ = tx_go.try_send(true);
                                window.close_dialog(cx);
                            }),
                    ),
                )
                .on_cancel(move |_, _, _| {
                    let _ = tx_cancel.try_send(false);
                    true
                })
        });
        let weak = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            if !matches!(go_rx.recv().await, Ok(true)) {
                return;
            }
            let _ = win.update(cx, |_, window, cx| {
                if let Some(shell) = weak.upgrade() {
                    shell.update(cx, |shell, cx| work(shell, window, cx));
                }
            });
        })
        .detach();
    }

    pub fn trigger_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_rename_selected(&RenameSelected, window, cx);
    }

    pub fn trigger_bulk_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_bulk_rename_selected(&BulkRenameSelected, window, cx);
    }

    pub fn trigger_open_archive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_open_archive(&OpenAsArchive, window, cx);
    }

    pub fn trigger_new_archive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_new_archive(&NewArchive, window, cx);
    }

    pub fn trigger_new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_new_folder(&NewFolder, window, cx);
    }

    pub(super) fn on_new_folder(
        &mut self,
        _: &NewFolder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("New Folder");
        let parent = self.active_tab().current_dir.clone();
        // Same focus/select-on-open modal the rename surfaces use. We
        // start with an empty field (the "Untitled folder" placeholder
        // is just a hint) so an empty submit is a no-op, matching the
        // old behavior, and a typed name creates the folder.
        self.open_text_prompt(
            "New Folder",
            "Untitled folder",
            String::new(),
            move |this, name, window, cx| {
                // A typed `/` is the Finder-displayed slash → store a `:` on
                // disk (macOS). Also keeps the name a single leaf, so New
                // Folder never silently descends into an existing subdir.
                let disk = ferail_fs_native::paths::on_disk_leaf(&name).into_owned();
                let mut path = parent.clone();
                path.push(&disk);
                let cur = parent.clone();
                let op_path = path.clone();
                let undo_path = path.clone();
                this.spawn_file_op(
                    cur,
                    move || {
                        std::fs::create_dir(&op_path).map_err(|e| e.to_string())?;
                        Ok(vec![op_path])
                    },
                    "New folder",
                    None,
                    FileOpSuccessToast::None,
                    FileOpUndo::DeleteFolder(undo_path),
                    window,
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Shared single-line text-naming modal used by every naming
    /// surface (file/folder rename and favorite-shortcut rename in
    /// `shell.rs`, plus new-folder above). Pre-fills `initial`, then —
    /// once the dialog has mounted — focuses the field and selects its
    /// text so the name is ready to overtype. `on_commit` runs with the
    /// trimmed new name when the user confirms, and is skipped when the
    /// name is empty or unchanged from `initial`.
    ///
    /// One gpui modal for every naming prompt keeps the surface
    /// consistent and is cross-platform: there is no native text-prompt
    /// on Windows, so routing every prompt through here (instead of a
    /// per-platform native path) is what makes these flows work there.
    pub(crate) fn open_text_prompt(
        &mut self,
        title: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        initial: String,
        on_commit: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_named_prompt(title, placeholder, initial, true, on_commit, window, cx);
    }

    /// Backing implementation of [`Self::open_text_prompt`] with an explicit
    /// `validate_as_filename` flag. File/folder naming surfaces pass `true` so
    /// a typed name that the OS would reject or silently mangle (Windows
    /// reserved names/chars, trailing dot/space) is caught up front and the
    /// dialog stays open with an explanation. Surfaces that name something
    /// *other* than a file — the favorite-shortcut label — pass `false`.
    pub(crate) fn open_named_prompt(
        &mut self,
        title: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        initial: String,
        validate_as_filename: bool,
        on_commit: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = title.into();
        let placeholder = placeholder.into();
        let original = initial.clone();
        let input_state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input_state.update(cx, |state, cx| {
            state.set_value(initial, window, cx);
        });
        let on_commit = std::rc::Rc::new(on_commit);
        let shell = cx.entity();
        let input = input_state.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let input = input.clone();
            let shell = shell.clone();
            let original = original.clone();
            let on_commit = on_commit.clone();
            let title = title.clone();
            dialog.title(title).child(Input::new(&input).small()).on_ok(
                move |_, window, cx: &mut App| {
                    let new_name = input.read(cx).value().trim().to_string();
                    if new_name.is_empty() || new_name == original {
                        return true;
                    }
                    // Reject names the OS would refuse or silently rewrite,
                    // keeping the dialog open (return false) so the user can
                    // correct it. Validate the on-disk form (identity on
                    // Windows; the macOS `/`↔`:` swap happens in on_commit).
                    if validate_as_filename {
                        let disk = ferail_fs_native::paths::on_disk_leaf(&new_name);
                        if let Err(msg) = ferail_fs_native::paths::validate_leaf(&disk) {
                            window.push_notification(
                                gpui_component::notification::Notification::error(msg),
                                cx,
                            );
                            return false;
                        }
                    }
                    let on_commit = on_commit.clone();
                    shell.update(cx, move |this, cx| {
                        on_commit(this, new_name, window, cx);
                    });
                    true
                },
            )
        });
        // Focus the field and select its contents on the next frame,
        // once the dialog (and its input) are mounted in the tree —
        // doing it synchronously here wouldn't stick. SelectAll is the
        // input's own action, dispatched to the now-focused field.
        window.on_next_frame(move |window, cx| {
            input_state.read(cx).focus_handle(cx).focus(window, cx);
            window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        });
    }

    pub(super) fn on_rename_selected(
        &mut self,
        _: &RenameSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Rename");
        let Some(row) = self.target_row(cx) else {
            return;
        };
        let Some(entry) = self
            .active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .get(row)
            .cloned()
        else {
            return;
        };
        let Some(old_path) = self.path_for_row(row, cx) else {
            return;
        };
        let parent = self.active_tab().current_dir.clone();
        self.open_text_prompt(
            "Rename",
            "New name",
            // Pre-fill the name the user sees (display leaf, macOS `:` → `/`);
            // `on_disk_leaf` below maps any edit back to on-disk bytes, so an
            // unchanged value round-trips to a no-op.
            entry.display_name.clone(),
            move |this, new_name, window, cx| {
                // Finder parity: a typed `/` stores a `:` on disk (macOS), and
                // the rename target stays a single leaf — no accidental move
                // into a sibling directory from a `/` in the typed name.
                let disk = ferail_fs_native::paths::on_disk_leaf(&new_name).into_owned();
                let mut new_path = old_path.clone();
                new_path.set_file_name(&disk);
                let op_old_path = old_path.clone();
                let op_new_path = new_path.clone();
                this.spawn_file_op(
                    parent.clone(),
                    move || {
                        std::fs::rename(&op_old_path, &op_new_path).map_err(|e| e.to_string())?;
                        Ok(Vec::new())
                    },
                    "Rename",
                    None,
                    FileOpSuccessToast::None,
                    FileOpUndo::Rename {
                        current: new_path,
                        original: old_path.clone(),
                    },
                    window,
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Bulk rename over the resolved multi-selection
    /// (docs/features/BULK_RENAME.md). Snapshots the selection once —
    /// `(path, display name, mtime)` triples, model/cache-only — and
    /// opens the pattern-rule dialog over it. With fewer than two
    /// targets this degrades to the single-rename prompt (one) or a
    /// no-op (none), so palette/menu dispatch is always sensible.
    pub(super) fn on_bulk_rename_selected(
        &mut self,
        _: &BulkRenameSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Bulk Rename");
        // Peek the target count without consuming `context_row`: the
        // single-rename fallback below resolves its own target from it.
        let count = self.resolve_targets(self.context_row, cx).len();
        if count < 2 {
            if count == 1 {
                self.on_rename_selected(&RenameSelected, window, cx);
            }
            return;
        }
        let items: Vec<(PathBuf, String, i64)> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, entry, path)| (path, entry.display_name, entry.mtime_unix))
            .collect();
        crate::bulk_rename::open(self, items, window, cx);
    }

    pub(super) fn on_quick_look(
        &mut self,
        _: &QuickLook,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths: Vec<PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        if paths.is_empty() {
            return;
        }
        // macOS pops a Quick Look HUD with the selected file's
        // content; the closest equivalent on Windows/Linux (no Quick
        // Look API) is to make sure the in-process preview pane is
        // visible, since it already shows the same content via the
        // IPreviewHandler pipeline. Toggling rather than just
        // showing matches Spacebar's "open/dismiss" Mac feel.
        #[cfg(target_os = "macos")]
        {
            // qlmanage spawn — worker, not UI thread (Prime Directive).
            cx.background_spawn(async move {
                let refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
                let _ = crate::platform_shell::show_quick_look(&refs);
            })
            .detach();
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.on_toggle_preview(&TogglePreview, window, cx);
        }
        let _ = window;
    }

    pub(super) fn on_duplicate(
        &mut self,
        _: &Duplicate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths: Vec<PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        if paths.is_empty() {
            return;
        }
        let cur = self.active_tab().current_dir.clone();
        let count = paths.len();
        let task_label = format!(
            "Duplicating {}",
            if count == 1 {
                paths[0]
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("item")
                    .to_string()
            } else {
                format!("{count} items")
            }
        );
        let success = if count == 1 {
            "Duplicated item".to_string()
        } else {
            format!("Duplicated {count} items")
        };
        self.spawn_file_op(
            cur,
            move || {
                let mut created = Vec::new();
                for path in paths {
                    created.push(crate::platform_shell::duplicate_path(&path)?);
                }
                Ok(created)
            },
            "Duplicate",
            Some(task_label),
            FileOpSuccessToast::IfSurfaced(success),
            FileOpUndo::RemoveCreatedResult,
            window,
            cx,
        );
    }

    pub(super) fn on_make_alias(
        &mut self,
        _: &MakeAlias,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths: Vec<PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        if paths.is_empty() {
            return;
        }
        let cur = self.active_tab().current_dir.clone();
        self.spawn_file_op(
            cur,
            move || {
                let mut created = Vec::new();
                for path in paths {
                    created.push(crate::platform_shell::make_alias(&path)?);
                }
                Ok(created)
            },
            "Make alias",
            None,
            FileOpSuccessToast::None,
            FileOpUndo::None,
            window,
            cx,
        );
    }

    /// One-click Compress → a ZIP next to the selection (Finder shape).
    pub(super) fn on_compress(&mut self, _: &Compress, window: &mut Window, cx: &mut Context<Self>) {
        self.compress_selection_as(ferail_archive::Format::Zip, window, cx);
    }

    pub(super) fn on_compress_targz(
        &mut self,
        _: &CompressTarGz,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.compress_selection_as(ferail_archive::Format::TarGz, window, cx);
    }

    pub(super) fn on_compress_tarbz2(
        &mut self,
        _: &CompressTarBz2,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.compress_selection_as(ferail_archive::Format::TarBz2, window, cx);
    }

    pub(super) fn on_compress_tarxz(
        &mut self,
        _: &CompressTarXz,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.compress_selection_as(ferail_archive::Format::TarXz, window, cx);
    }

    pub(super) fn on_compress_sevenz(
        &mut self,
        _: &CompressSevenZ,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.compress_selection_as(ferail_archive::Format::SevenZ, window, cx);
    }

    pub(super) fn on_compress_tar(
        &mut self,
        _: &CompressTar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.compress_selection_as(ferail_archive::Format::Tar, window, cx);
    }

    /// Add dropped files to an existing archive, in place. Only reachable for
    /// formats the capability matrix marks editable (zip); `on_done` lets the
    /// archive workbench re-read its table of contents once the entries land.
    pub(crate) fn add_to_archive_from(
        &mut self,
        archive: PathBuf,
        sources: Vec<PathBuf>,
        password: Option<String>,
        on_done: Option<ArchiveOpDone>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if sources.is_empty() {
            return;
        }
        let count = sources.len();
        let plural = if count == 1 { "" } else { "s" };
        let name = archive
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive".to_string());
        let reload = archive
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.active_tab().current_dir.clone());
        self.spawn_archive_op(
            reload,
            move |progress, cancel| {
                let targets: Vec<&std::path::Path> =
                    sources.iter().map(|p| p.as_path()).collect();
                let opts = ferail_fs_native::CreateOptions {
                    level: Default::default(),
                    password: password.as_deref(),
                };
                let outcome =
                    ferail_fs_native::add_to_archive(&archive, &targets, opts, progress, cancel)
                        .map_err(|e| e.to_string())?;
                // Names already in the archive are skipped rather than
                // shadowed — say so instead of reporting a clean success.
                if !outcome.skipped_existing.is_empty() && outcome.added == 0 {
                    return Err(format!(
                        "Already in the archive: {}",
                        outcome.skipped_existing.join(", ")
                    ));
                }
                Ok(Vec::new())
            },
            "Add to archive",
            format!("Adding {count} item{plural} to {name}"),
            FileOpSuccessToast::IfSurfaced(format!("Added {count} item{plural} to {name}")),
            // The archive is modified in place, so there is nothing created to
            // remove; undoing an add would mean rewriting the archive without
            // those entries, which the engine can't do yet.
            FileOpUndo::None,
            on_done,
            window,
            cx,
        );
    }

    /// Open the New Archive dialog over the current selection.
    pub(super) fn on_new_archive(
        &mut self,
        _: &NewArchive,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sources: Vec<PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        crate::archive_create::open_dialog(sources, window, cx);
    }

    /// Create `output` from `sources` with the dialog's chosen options.
    // Every input is a distinct choice the dialog collected; bundling them
    // into a struct would only move the same fields one level down.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_archive_from(
        &mut self,
        sources: Vec<PathBuf>,
        output: PathBuf,
        format: ferail_archive::Format,
        level: ferail_archive::CompressionLevel,
        password: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = sources.len();
        let name = output
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive".to_string());
        let reload = output
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.active_tab().current_dir.clone());
        self.spawn_archive_op(
            reload,
            move |progress, cancel| {
                let targets: Vec<&std::path::Path> =
                    sources.iter().map(|path| path.as_path()).collect();
                let opts = ferail_fs_native::CreateOptions {
                    level,
                    password: password.as_deref(),
                };
                ferail_fs_native::create_archive(
                    format, &targets, &output, opts, progress, cancel,
                )
                .map_err(|e| e.to_string())?;
                Ok(vec![output])
            },
            "Create archive",
            format!("Creating {name}"),
            FileOpSuccessToast::IfSurfaced(format!(
                "Created {name} from {count} item{}",
                if count == 1 { "" } else { "s" }
            )),
            FileOpUndo::RemoveCreatedResult,
            None,
            window,
            cx,
        );
    }

    /// Compress the action target set into a single `format` archive next to
    /// the first target — `<name>.<ext>` for one item, `Archive.<ext>` for
    /// many, `" 2"`-deduped on collision (Finder naming). Runs on the
    /// `create_archive` engine off-thread; this replaces the macOS `ditto`
    /// shell-out so every platform shares one code path (and gains levels /
    /// password support for free later).
    pub(super) fn compress_selection_as(
        &mut self,
        format: ferail_archive::Format,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths: Vec<PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        if paths.is_empty() {
            return;
        }
        let cur = self.active_tab().current_dir.clone();
        let count = paths.len();
        let task_label = if count == 1 {
            let name = paths[0]
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("item");
            format!("Compressing {name}")
        } else {
            format!("Compressing {count} items")
        };
        let success = if count == 1 {
            "Created archive".to_string()
        } else {
            format!("Created archive from {count} items")
        };

        /// `parent/<stem>.<ext>`, deduped with a `" 2"`, `" 3"`… suffix.
        fn pick_archive_name(
            parent: &std::path::Path,
            paths: &[PathBuf],
            format: ferail_archive::Format,
        ) -> Option<PathBuf> {
            let ext = format.canonical_extension();
            let stem = if paths.len() == 1 {
                paths[0]
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Archive".to_string())
            } else {
                "Archive".to_string()
            };
            for n in 1..=9999 {
                let name = if n == 1 {
                    format!("{stem}.{ext}")
                } else {
                    format!("{stem} {n}.{ext}")
                };
                let candidate = parent.join(&name);
                if !candidate.exists() {
                    return Some(candidate);
                }
            }
            None
        }

        self.spawn_archive_op(
            cur,
            move |progress, cancel| {
                let parent = paths[0]
                    .parent()
                    .ok_or_else(|| format!("no parent directory for {}", paths[0].display()))?
                    .to_path_buf();
                let output = pick_archive_name(&parent, &paths, format)
                    .ok_or_else(|| "exhausted archive index range".to_string())?;
                let targets: Vec<&std::path::Path> =
                    paths.iter().map(|path| path.as_path()).collect();
                ferail_fs_native::create_archive(
                    format,
                    &targets,
                    &output,
                    ferail_fs_native::CreateOptions::default(),
                    progress,
                    cancel,
                )
                .map_err(|e| e.to_string())?;
                Ok(vec![output])
            },
            "Compress",
            task_label,
            FileOpSuccessToast::IfSurfaced(success),
            FileOpUndo::RemoveCreatedResult,
            None,
            window,
            cx,
        );
    }

    /// Extract Here — unpack the selected archive(s) into the current folder.
    pub(super) fn on_extract(&mut self, _: &Extract, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.gather_archive_targets(cx);
        let cur = self.active_tab().current_dir.clone();
        self.spawn_extract_into(paths, cur, None, window, cx);
    }

    /// Extract To… — pick a destination folder from a native modal, then
    /// extract there. The picker is a blocking nested run-loop that must run
    /// with no `App` borrow held, so it goes inside a spawned task (mirrors
    /// `Shell::locate_favorite`); the extraction is dispatched back on the
    /// window once a folder is chosen.
    pub(super) fn on_extract_to(
        &mut self,
        _: &ExtractTo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = self.gather_archive_targets(cx);
        if paths.is_empty() {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let Some(dest) = crate::platform_shell::pick_folder() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.spawn_extract_into(paths, dest, None, window, cx);
            });
        })
        .detach();
    }

    /// The archive subset of the action targets — lexical extension check, no
    /// I/O on the UI thread (Prime Directive).
    fn gather_archive_targets(&mut self, cx: &mut Context<Self>) -> Vec<PathBuf> {
        self.action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(ferail_archive::Format::is_archive_path)
            })
            .collect()
    }

    /// Extract each archive in `paths` into `dest_parent`, off-thread. Each
    /// archive's table of contents picks a smart destination — extract in place
    /// when it has a single root folder that isn't already taken, otherwise a
    /// `" 2"`-deduped wrapper named after the archive. Encrypted archives fail
    /// with a clear message (the password flow lives in the workbench).
    /// Run an archive operation (compress / extract) on a worker with a **live
    /// progress bar and a working cancel button**.
    ///
    /// `spawn_file_op` is the right tool for ops that finish in a blink
    /// (duplicate, alias): it registers a non-cancellable task and reports only
    /// start/end. Archive work is different — a multi-gigabyte tarball can run
    /// for minutes — so this variant mirrors `spawn_transfer_op`: the worker
    /// bumps lock-free counters on `TransferProgress` while a ~10 Hz foreground
    /// sampler reads them and drives the task row (fraction, bytes, rate, ETA,
    /// current entry). The work is never slowed by drawing its own progress,
    /// and the task panel's cancel button flips the `AtomicBool` the codec
    /// checks between entries and buffers.
    ///
    /// Cancelling leaves whatever was already written in place (extraction is
    /// not transactional); the task ends quietly rather than raising an error
    /// toast, since the user asked for it.
    #[allow(clippy::too_many_arguments)]
    fn spawn_archive_op(
        &mut self,
        reload_path: PathBuf,
        op: impl FnOnce(
                &ferail_fs_native::file_ops::TransferProgress,
                &AtomicBool,
            ) -> Result<Vec<PathBuf>, String>
            + Send
            + 'static,
        failure_label: &'static str,
        task_label: String,
        success_toast: FileOpSuccessToast,
        undo: FileOpUndo,
        // Runs on the UI thread after a successful, non-cancelled op — used
        // by surfaces that must refresh state the directory reload doesn't
        // cover (e.g. the archive workbench re-reading its table of contents
        // after entries were added).
        on_success: Option<ArchiveOpDone>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        use std::sync::atomic::Ordering as AtomicOrdering;
        use std::time::{Duration, Instant};

        let process = self.process.clone();
        let win = window.window_handle();
        let weak = cx.weak_entity();

        let cancel = Arc::new(AtomicBool::new(false));
        let prog = Arc::new(ferail_fs_native::file_ops::TransferProgress::new());
        // Set once the op returns, so the sampler stops touching a finished task.
        let done = Arc::new(AtomicBool::new(false));

        let task_id = self.process.tasks.borrow_mut().begin_with_cancel(
            crate::tasks::TaskKind::FileOp,
            task_label,
            cancel.clone(),
        );

        // Foreground sampler on its own clock (see `spawn_transfer_op` for the
        // reasoning behind the trimmed rolling-window rate).
        {
            let weak = cx.weak_entity();
            let prog = prog.clone();
            let done = done.clone();
            cx.spawn(async move |_this, cx| {
                let mut window: std::collections::VecDeque<(Instant, u64)> =
                    std::collections::VecDeque::new();
                let mut shown_rate: f64 = 0.0;
                let mut shown_eta: Option<u64> = None;
                let mut ticks: u32 = 0;
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    if done.load(AtomicOrdering::Relaxed) {
                        break;
                    }
                    let bytes_done = prog.bytes_done();
                    let bytes_total = prog.bytes_total();
                    let now = Instant::now();
                    window.push_back((now, bytes_done));
                    while window
                        .front()
                        .is_some_and(|(t, _)| now.duration_since(*t).as_secs_f64() > 6.0)
                    {
                        window.pop_front();
                    }
                    ticks = ticks.wrapping_add(1);
                    if ticks % 10 == 0 {
                        shown_rate = trimmed_window_rate(&window);
                        shown_eta = if shown_rate > 1.0 && bytes_total > bytes_done {
                            Some(round_eta(
                                ((bytes_total - bytes_done) as f64 / shown_rate) as u64,
                            ))
                        } else {
                            None
                        };
                    }
                    let stats = crate::tasks::TransferStats {
                        bytes_done,
                        bytes_total,
                        items_done: prog.items_done(),
                        items_total: prog.items_total(),
                        bytes_per_sec: shown_rate,
                        eta_secs: shown_eta,
                        current: prog.current().to_string(),
                    };
                    let Some(shell) = weak.upgrade() else { break };
                    shell.update(cx, |this, cx| {
                        let mut reg = this.process.tasks.borrow_mut();
                        // Formats without an up-front total (tar streams, 7z)
                        // leave `bytes_total` at 0 and stay indeterminate.
                        if bytes_total > 0 {
                            reg.update(task_id, bytes_done as f32 / bytes_total as f32);
                        }
                        reg.update_transfer(task_id, stats);
                        drop(reg);
                        cx.notify();
                    });
                }
            })
            .detach();
        }

        cx.spawn(async move |_this, cx| {
            let prog_for_op = prog.clone();
            let cancel_for_op = cancel.clone();
            let result = cx
                .background_executor()
                .spawn(async move { op(&prog_for_op, &cancel_for_op) })
                .await;
            done.store(true, AtomicOrdering::Relaxed);

            let cancelled = cancel.load(AtomicOrdering::Relaxed);
            let created = result.as_ref().ok().cloned().unwrap_or_default();
            let error = result.as_ref().err().cloned();

            let surfaced = if let Some(shell) = weak.upgrade() {
                let created_for_undo = created.clone();
                shell.update(cx, move |this, cx| {
                    let surfaced = {
                        let mut reg = this.process.tasks.borrow_mut();
                        match (&error, cancelled) {
                            // User-requested stop: end quietly, no error toast.
                            (_, true) => {
                                reg.end(task_id);
                                false
                            }
                            (Some(message), false) => {
                                reg.end_failed(task_id, message.clone());
                                false
                            }
                            (None, false) => reg.end_and_was_surfaced(task_id),
                        }
                    };
                    if error.is_none() && !cancelled {
                        undo.push(this, created_for_undo);
                        if let Some(done) = on_success {
                            done(this, cx);
                        }
                    }
                    cx.notify();
                    surfaced
                })
            } else {
                false
            };

            match result {
                Ok(_) => {
                    Shell::broadcast_reload_for_process(&process, vec![reload_path], cx);
                    if let FileOpSuccessToast::IfSurfaced(message) = success_toast {
                        if surfaced && !cancelled {
                            let _ = win.update(cx, |_, window, cx| {
                                use gpui_component::notification::Notification;
                                window.push_notification(Notification::success(message), cx);
                            });
                        }
                    }
                }
                Err(e) => {
                    // A cancel still reloads: partial output is on disk.
                    if cancelled {
                        Shell::broadcast_reload_for_process(&process, vec![reload_path], cx);
                        return;
                    }
                    crate::log_warn!(90, "{failure_label} failed: {e}");
                    let _ = win.update(cx, |_, window, cx| {
                        window.push_notification(file_op_error_notification(failure_label, &e), cx);
                    });
                }
            }
        })
        .detach();
    }

    pub(crate) fn spawn_extract_into(
        &mut self,
        paths: Vec<PathBuf>,
        dest_parent: PathBuf,
        password: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let count = paths.len();
        let task_label = if count == 1 {
            let name = paths[0]
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("archive");
            format!("Extracting {name}")
        } else {
            format!("Extracting {count} archives")
        };
        let success = if count == 1 {
            "Extracted archive".to_string()
        } else {
            format!("Extracted {count} archives")
        };

        fn extract_one_archive(
            archive: &std::path::Path,
            parent: &std::path::Path,
            password: Option<&str>,
            progress: &ferail_fs_native::file_ops::TransferProgress,
            cancel: &AtomicBool,
        ) -> Result<Vec<PathBuf>, String> {
            let toc = ferail_fs_native::read_archive_toc(archive, password)
                .map_err(|e| e.to_string())?;
            let opts = ferail_fs_native::ExtractOptions {
                password,
                overwrite: false,
            };
            let in_place_root = toc.single_root().and_then(|root| {
                let candidate = parent.join(root);
                (!candidate.exists()).then_some(candidate)
            });
            let (dest, created): (PathBuf, Vec<PathBuf>) = match in_place_root {
                Some(root_path) => (parent.to_path_buf(), vec![root_path]),
                None => {
                    let dest = unique_dir(parent, &archive_stem(archive));
                    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
                    (dest.clone(), vec![dest])
                }
            };
            ferail_fs_native::extract_archive(archive, &dest, opts, progress, cancel)
                .map_err(|e| e.to_string())?;
            Ok(created)
        }

        /// The archive filename with its format suffix removed
        /// (`photos.tar.gz` → `photos`), for naming a wrapper folder.
        fn archive_stem(archive: &std::path::Path) -> String {
            let leaf = archive
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(fmt) = ferail_archive::Format::from_path(&leaf) {
                let dot_ext = format!(".{}", fmt.canonical_extension());
                if let Some(stripped) = leaf.to_ascii_lowercase().strip_suffix(&dot_ext) {
                    return leaf[..stripped.len()].to_string();
                }
            }
            leaf
        }

        /// `parent/stem`, or `parent/stem 2`, `parent/stem 3`, … if taken.
        fn unique_dir(parent: &std::path::Path, stem: &str) -> PathBuf {
            let base = if stem.is_empty() { "Extracted" } else { stem };
            let mut candidate = parent.join(base);
            let mut n = 2;
            while candidate.exists() {
                candidate = parent.join(format!("{base} {n}"));
                n += 1;
            }
            candidate
        }

        self.spawn_archive_op(
            dest_parent.clone(),
            move |progress, cancel| {
                let mut created = Vec::new();
                for archive in &paths {
                    created.extend(extract_one_archive(
                        archive,
                        &dest_parent,
                        password.as_deref(),
                        progress,
                        cancel,
                    )?);
                }
                Ok(created)
            },
            "Extract",
            task_label,
            FileOpSuccessToast::IfSurfaced(success),
            FileOpUndo::RemoveCreatedResult,
            None,
            window,
            cx,
        );
    }

    /// Cherry-pick extraction (used by the archive workbench): extract the
    /// given `entries` of `archive` into a fresh `" 2"`-deduped folder named
    /// after the archive, under `dest_parent`. Off-thread through
    /// `spawn_file_op`, so it gets a task row / toast / undo like every other
    /// file op.
    pub(crate) fn extract_archive_entries_into(
        &mut self,
        archive: PathBuf,
        entries: Vec<String>,
        dest_parent: PathBuf,
        password: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if entries.is_empty() {
            return;
        }
        let count = entries.len();
        let plural = if count == 1 { "" } else { "s" };
        let task_label = format!("Extracting {count} item{plural}");
        let success = format!("Extracted {count} item{plural}");
        self.spawn_archive_op(
            dest_parent.clone(),
            move |progress, cancel| {
                let stem = archive
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Extracted".to_string());
                let mut dest = dest_parent.join(&stem);
                let mut n = 2;
                while dest.exists() {
                    dest = dest_parent.join(format!("{stem} {n}"));
                    n += 1;
                }
                std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
                let entry_refs: Vec<&str> = entries.iter().map(|s| s.as_str()).collect();
                let opts = ferail_fs_native::ExtractOptions {
                    password: password.as_deref(),
                    overwrite: false,
                };
                ferail_fs_native::extract_archive_entries(
                    &archive,
                    &dest,
                    &entry_refs,
                    opts,
                    progress,
                    cancel,
                )
                .map_err(|e| e.to_string())?;
                Ok(vec![dest])
            },
            "Extract",
            task_label,
            FileOpSuccessToast::IfSurfaced(success),
            FileOpUndo::RemoveCreatedResult,
            None,
            window,
            cx,
        );
    }

    /// Strip the Mark-of-the-Web (and the where-from provenance that
    /// rides with it) from every quarantined file in the current
    /// action target set. The xattr/ADS removal and the metadata-DB
    /// scrub run on a worker — the scrub matters because the prefetch
    /// pipeline caches quarantine state per path and would otherwise
    /// resurrect the badge from cache on the next visit. Rows update
    /// in place on completion (matched by NodeId across all tabs).
    pub(super) fn on_clear_quarantine(
        &mut self,
        _: &ClearQuarantine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets: Vec<(ferail_core::NodeId, PathBuf)> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .filter(|(_, entry, _)| entry.is_quarantined)
            .map(|(_, entry, path)| (entry.id, path))
            .collect();
        if targets.is_empty() {
            return;
        }
        let db = self.process.db_snapshot();
        cx.spawn_in(window, async move |this, cx| {
            let (cleared, failures) = cx
                .background_executor()
                .spawn(async move {
                    let mut cleared: Vec<ferail_core::NodeId> = Vec::new();
                    let mut failures: Vec<ferail_fs_native::file_ops::FileOpError> = Vec::new();
                    for (id, path) in targets {
                        match ferail_fs_native::clear_quarantine(&path) {
                            Ok(()) => {
                                if let Some(db) = db.as_ref() {
                                    if let Ok(guard) = db.lock() {
                                        let key = path.to_string_lossy().into_owned();
                                        if let Ok(Some(mut rec)) = guard.get_file(&key) {
                                            rec.quarantined = Some(false);
                                            rec.quarantine_agent = None;
                                            rec.quarantine_iso = None;
                                            rec.quarantine_where_from = None;
                                            let _ = guard.upsert_file(&rec);
                                        }
                                    }
                                }
                                cleared.push(id);
                            }
                            Err(e) => {
                                crate::log_warn!(
                                    90,
                                    "clear_quarantine failed for {}: {e}",
                                    path.display()
                                );
                                failures.push(ferail_fs_native::file_ops::FileOpError::from_io(
                                    &e, &path,
                                ));
                            }
                        }
                    }
                    (cleared, failures)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.apply_quarantine_cleared(&cleared, &failures, window, cx);
            });
        })
        .detach();
    }

    /// Foreground half of `on_clear_quarantine`: flip the cached row
    /// state for every cleared NodeId (in every tab — the same file
    /// can be visible twice) and report the outcome.
    fn apply_quarantine_cleared(
        &mut self,
        cleared: &[ferail_core::NodeId],
        failures: &[ferail_fs_native::file_ops::FileOpError],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        if !cleared.is_empty() {
            for tab in &self.tabs {
                tab.table.update(cx, |state, cx| {
                    let mut touched = false;
                    for e in state.delegate_mut().entries.iter_mut() {
                        if cleared.contains(&e.id) {
                            e.is_quarantined = false;
                            e.quarantine = None;
                            touched = true;
                        }
                    }
                    if touched {
                        state.refresh(cx);
                    }
                });
            }
            cx.notify();
            let msg = if cleared.len() == 1 {
                "Mark of the Web cleared".to_string()
            } else {
                format!("Mark of the Web cleared from {} files", cleared.len())
            };
            window.push_notification(Notification::success(msg), cx);
        }
        if !failures.is_empty() {
            // Per-item failures through the same structured report the
            // copy/move path uses (Copy button + expandable detail via
            // the shared error toast).
            window.push_notification(
                super::error_notification(crate::shell::file_op_failure_report(
                    "Clear quarantine",
                    cleared.len(),
                    0,
                    failures,
                )),
                cx,
            );
        }
    }
}

/// Windowed throughput for the transfer sampler: the mean of the
/// window's per-sample instantaneous rates with the top and bottom 20%
/// trimmed away. The trim is what keeps the displayed number level —
/// an instant-clone jump (gigabytes landing in one tick) or a stalled
/// tick (0 B/s while a drive seeks) lands in the discarded extremes
/// instead of dragging the mean around. 0.0 while there aren't enough
/// samples to say anything (the UI shows no rate while ramping).
fn trimmed_window_rate(samples: &std::collections::VecDeque<(std::time::Instant, u64)>) -> f64 {
    let mut rates: Vec<f64> = Vec::with_capacity(samples.len());
    let mut prev: Option<&(std::time::Instant, u64)> = None;
    for s in samples {
        if let Some(p) = prev {
            let dt = s.0.duration_since(p.0).as_secs_f64();
            if dt > 0.0 {
                rates.push(s.1.saturating_sub(p.1) as f64 / dt);
            }
        }
        prev = Some(s);
    }
    if rates.is_empty() {
        return 0.0;
    }
    rates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let trim = rates.len() / 5;
    let kept = &rates[trim..rates.len() - trim];
    kept.iter().sum::<f64>() / kept.len() as f64
}

/// Round an ETA up to a coarser step the larger it is, so the countdown
/// ticks calmly ("~51m" → "~50m") instead of flickering through every
/// second ("~51m 21s" → "~51m 4s" → "~52m 40s"). Rounds *up* — an ETA
/// that overshoots slightly reads better than one that hits zero while
/// the transfer is still running.
fn round_eta(secs: u64) -> u64 {
    let step = if secs >= 600 {
        60
    } else if secs >= 120 {
        10
    } else if secs >= 30 {
        5
    } else {
        1
    };
    secs.div_ceil(step) * step
}

#[cfg(test)]
mod transfer_rate_tests {
    use super::{round_eta, trimmed_window_rate};
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    /// Build a sample window from per-tick byte deltas, 100ms apart.
    fn window_of(deltas: &[u64]) -> VecDeque<(Instant, u64)> {
        let start = Instant::now() - Duration::from_secs(60);
        let mut out = VecDeque::new();
        let mut total = 0u64;
        out.push_back((start, 0));
        for (i, d) in deltas.iter().enumerate() {
            total += d;
            out.push_back((start + Duration::from_millis(100 * (i as u64 + 1)), total));
        }
        out
    }

    #[test]
    fn steady_stream_reports_its_rate() {
        // 5 MB per 100ms tick = 50 MB/s.
        let w = window_of(&[5_000_000; 20]);
        let r = trimmed_window_rate(&w);
        assert!((r - 50_000_000.0).abs() < 1_000.0, "rate was {r}");
    }

    #[test]
    fn one_tick_spike_is_trimmed_out() {
        // A steady 50 MB/s stream with one 5 GB instant-clone tick —
        // the spike must not drag the mean up.
        let mut deltas = [5_000_000u64; 20];
        deltas[10] = 5_000_000_000;
        let r = trimmed_window_rate(&window_of(&deltas));
        assert!((r - 50_000_000.0).abs() < 1_000.0, "rate was {r}");
    }

    #[test]
    fn one_tick_stall_is_trimmed_out() {
        let mut deltas = [5_000_000u64; 20];
        deltas[7] = 0;
        let r = trimmed_window_rate(&window_of(&deltas));
        assert!((r - 50_000_000.0).abs() < 1_000.0, "rate was {r}");
    }

    #[test]
    fn empty_and_single_sample_are_zero() {
        assert_eq!(trimmed_window_rate(&VecDeque::new()), 0.0);
        let one: VecDeque<_> = [(Instant::now(), 42u64)].into_iter().collect();
        assert_eq!(trimmed_window_rate(&one), 0.0);
    }

    #[test]
    fn eta_rounds_up_on_a_size_matched_step() {
        assert_eq!(round_eta(7), 7); // <30s: exact
        assert_eq!(round_eta(31), 35); // 5s steps
        assert_eq!(round_eta(121), 130); // 10s steps
        assert_eq!(round_eta(3_081), 3_120); // whole minutes
        assert_eq!(round_eta(3_120), 3_120); // already on a step
    }
}
