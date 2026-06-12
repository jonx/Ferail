use super::*;

/// Copy vs move for [`Shell::spawn_transfer_op`]. Future drop-target
/// work (drag-into-app, drop-onto-favorite) feeds the same worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransferMode {
    Copy,
    Move,
}

impl Shell {
    /// Cmd+C — write the selection's file URLs to the pasteboard.
    pub(super) fn on_copy_files(
        &mut self,
        _: &CopyFiles,
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
        let refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
        crate::platform_shell::clipboard_copy_file_urls(&refs);
        let msg = match paths.as_slice() {
            [single] => format!(
                "Copied \u{201c}{}\u{201d}",
                single.file_name().unwrap_or_default().to_string_lossy()
            ),
            many => format!("Copied {} items", many.len()),
        };
        window.push_notification(Notification::success(msg), cx);
    }

    /// Cmd+V — paste (copy) the pasteboard's files into this folder.
    pub(super) fn on_paste_files(
        &mut self,
        _: &PasteFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.paste_from_clipboard(TransferMode::Copy, window, cx);
    }

    /// Cmd+Option+V — Finder's "Move Items Here".
    pub(super) fn on_move_paste_files(
        &mut self,
        _: &MovePasteFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        use feraille_fs_native::file_ops::{self as engine, CollisionPolicy};
        use gpui_component::button::ButtonVariants as _;
        use gpui_component::notification::Notification;

        let cancel = Arc::new(AtomicBool::new(false));
        let verb = match mode {
            TransferMode::Copy => "Copying",
            TransferMode::Move => "Moving",
        };
        let noun = match sources.as_slice() {
            [single] => format!(
                "\u{201c}{}\u{201d}",
                single.file_name().unwrap_or_default().to_string_lossy()
            ),
            many => format!("{} items", many.len()),
        };
        let task_id = self.process.tasks.borrow_mut().begin_with_cancel(
            crate::tasks::TaskKind::FileOp,
            format!("{verb} {noun}\u{2026}"),
            cancel.clone(),
        );

        // Engine progress ticks land on this channel from the worker
        // thread; the drain task coalesces them into ~10 Hz registry
        // updates so the UI never repaints per chunk.
        let (progress_tx, progress_rx) = async_channel::unbounded::<(u64, u64)>();
        {
            let weak = cx.weak_entity();
            cx.spawn(async move |_this, cx| {
                while let Ok(mut tick) = progress_rx.recv().await {
                    while let Ok(t) = progress_rx.try_recv() {
                        tick = t;
                    }
                    let frac = if tick.1 == 0 {
                        1.0
                    } else {
                        tick.0 as f32 / tick.1 as f32
                    };
                    let Some(shell) = weak.upgrade() else { break };
                    shell.update(cx, |this, cx| {
                        this.process.tasks.borrow_mut().update(task_id, frac);
                        cx.notify();
                    });
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(100))
                        .await;
                }
            })
            .detach();
        }

        let win = window.window_handle();
        let process = self.process.clone();
        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            let end_task = |cx: &mut AsyncApp| {
                if let Some(shell) = weak.upgrade() {
                    shell.update(cx, |this, cx| {
                        this.process.tasks.borrow_mut().end(task_id);
                        cx.notify();
                    });
                }
            };

            // 1. Plan (walk + conflict scan) off the UI thread.
            let plan = {
                let (s, d, c) = (sources.clone(), dest.clone(), cancel.clone());
                cx.background_executor()
                    .spawn(async move { engine::plan_transfer(&s, &d, &c) })
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

            // 2. Collision policy. Pasting next to the originals
            // obviously means "make me a copy" — no dialog.
            let same_dir = sources.iter().all(|s| s.parent() == Some(dest.as_path()));
            let policy = if plan.conflicts.is_empty() || same_dir {
                CollisionPolicy::KeepBoth
            } else {
                let (choice_tx, choice_rx) =
                    async_channel::bounded::<Option<CollisionPolicy>>(1);
                let conflict_count = plan.conflicts.len();
                let dest_label = dest
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| dest.display().to_string());
                let opened = win.update(cx, |_, window, cx| {
                    let tx = choice_tx.clone();
                    window.open_dialog(cx, move |dialog, _window, _cx| {
                        let tx_ok = tx.clone();
                        let tx_cancel = tx.clone();
                        let tx_replace = tx.clone();
                        let tx_skip = tx.clone();
                        let body = if conflict_count == 1 {
                            format!(
                                "An item with the same name already exists in \u{201c}{dest_label}\u{201d}."
                            )
                        } else {
                            format!(
                                "{conflict_count} items with the same names already exist in \u{201c}{dest_label}\u{201d}."
                            )
                        };
                        // All three policies as explicit buttons (the
                        // plain Dialog's ok/cancel footer doesn't
                        // render alongside custom children in the
                        // pinned gpui-component rev); ✕ / Esc cancel
                        // the whole operation via the dropped-sender
                        // path below.
                        dialog
                            .title("Items already exist")
                            .child(div().text_sm().child(body))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .pt_2()
                                    .child(
                                        Button::new("collision-keep-both")
                                            .label("Keep Both")
                                            .primary()
                                            .small()
                                            .on_click(move |_, window, cx| {
                                                let _ = tx_ok
                                                    .try_send(Some(CollisionPolicy::KeepBoth));
                                                window.close_dialog(cx);
                                            }),
                                    )
                                    .child(
                                        Button::new("collision-replace")
                                            .label("Replace")
                                            .small()
                                            .on_click(move |_, window, cx| {
                                                let _ = tx_replace
                                                    .try_send(Some(CollisionPolicy::Replace));
                                                window.close_dialog(cx);
                                            }),
                                    )
                                    .child(
                                        Button::new("collision-skip")
                                            .label("Skip Existing")
                                            .small()
                                            .on_click(move |_, window, cx| {
                                                let _ = tx_skip
                                                    .try_send(Some(CollisionPolicy::Skip));
                                                window.close_dialog(cx);
                                            }),
                                    ),
                            )
                            .on_cancel(move |_, _, _| {
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
                    Ok(Some(p)) => p,
                    _ => {
                        end_task(cx);
                        return;
                    }
                }
            };

            // 3. Run the engine on the background executor. The
            // same-volume answer rides along for move-undo
            // eligibility (stat — not allowed on the UI thread).
            let result = {
                let c = cancel.clone();
                cx.background_executor()
                    .spawn(async move {
                        let all_same_volume = plan
                            .sources
                            .iter()
                            .all(|s| engine::same_volume(s, &plan.dest_dir));
                        let mut progress =
                            |d: u64, t: u64| {
                                let _ = progress_tx.try_send((d, t));
                            };
                        let outcome = match mode {
                            TransferMode::Copy => {
                                engine::run_copy(&plan, policy, &mut progress, &c)
                            }
                            TransferMode::Move => {
                                engine::run_move(&plan, policy, &mut progress, &c)
                            }
                        };
                        outcome.map(|o| (o, all_same_volume))
                    })
                    .await
            };

            // 4. Finish: end task, register undo, reload, notify.
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| {
                    this.process.tasks.borrow_mut().end(task_id);
                    if let Ok((outcome, all_same_volume)) = &result {
                        if !outcome.created.is_empty() {
                            match mode {
                                TransferMode::Move if *all_same_volume => {
                                    this.push_undo(UndoOp::MoveBack(outcome.created.clone()));
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
            if mode == TransferMode::Move {
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
                let done_verb = match mode {
                    TransferMode::Copy => "Copied",
                    TransferMode::Move => "Moved",
                };
                match &result {
                    Ok((outcome, _)) => {
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

    pub(super) fn on_reveal_in_finder(
        &mut self,
        _: &RevealInFinder,
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
        for path in &paths {
            crate::platform_shell::reveal_in_finder(path);
        }
        let reveal_target = if cfg!(windows) { "Explorer" } else { "Finder" };
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
        window.push_notification(Notification::info(msg), cx);
    }

    pub(super) fn on_reveal_context_path(
        &mut self,
        _: &RevealContextPath,
        _: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let Some(path) = self.context_target.take() else {
            return;
        };
        crate::platform_shell::reveal_in_finder(&path);
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
        crate::platform_shell::open_terminal(&path);
    }

    /// Sidebar/tree/breadcrumb "Open Terminal Here". Resolves the
    /// right-clicked folder from `context_target` (mirrors
    /// `on_copy_context_path`).
    pub(super) fn on_open_terminal_at_context(
        &mut self,
        _: &OpenTerminalAtContext,
        _: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let Some(path) = self.context_target.take() else {
            return;
        };
        crate::platform_shell::open_terminal(&path);
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
        color: feraille_core::commands::TagColor,
        cx: &mut Context<Self>,
    ) {
        for (_, _, path) in self.action_entries_visible_order(cx) {
            let _ = crate::platform_shell::toggle_tag(&path, color);
        }
    }

    pub(super) fn on_toggle_tag_red(
        &mut self,
        _: &ToggleTagRed,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Red, cx);
    }

    pub(super) fn on_toggle_tag_orange(
        &mut self,
        _: &ToggleTagOrange,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Orange, cx);
    }

    pub(super) fn on_toggle_tag_yellow(
        &mut self,
        _: &ToggleTagYellow,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Yellow, cx);
    }

    pub(super) fn on_toggle_tag_green(
        &mut self,
        _: &ToggleTagGreen,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Green, cx);
    }

    pub(super) fn on_toggle_tag_blue(
        &mut self,
        _: &ToggleTagBlue,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Blue, cx);
    }

    pub(super) fn on_toggle_tag_purple(
        &mut self,
        _: &ToggleTagPurple,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Purple, cx);
    }

    pub(super) fn on_toggle_tag_gray(
        &mut self,
        _: &ToggleTagGray,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Gray, cx);
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
            for path in paths {
                let _ = crate::platform_shell::open_with_app(&path, &app);
            }
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
        self.spawn_file_op(
            cur,
            move || {
                for path in paths {
                    feraille_fs_native::move_to_trash(&path).map_err(|e| e.to_string())?;
                }
                Ok(())
            },
            "move-to-trash",
            window,
            cx,
        );
        window.push_notification(
            Notification::info(format!("Moved \u{201C}{}\u{201D} to Trash", name)),
            cx,
        );
    }

    pub fn trigger_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_rename_selected(&RenameSelected, window, cx);
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
        let parent = self.active_tab().current_dir.clone();
        let input_state = cx.new(|cx| InputState::new(window, cx).placeholder("Untitled folder"));
        let input_for_ok = input_state.clone();
        let shell = cx.entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let input = input_state.clone();
            let input_for_ok = input_for_ok.clone();
            let shell = shell.clone();
            let parent = parent.clone();
            dialog
                .title("New Folder")
                .child(Input::new(&input).small())
                .on_ok(move |_, window, cx: &mut App| {
                    let name = input_for_ok.read(cx).value().trim().to_string();
                    if name.is_empty() {
                        return true;
                    }
                    let mut path = parent.clone();
                    path.push(&name);
                    let cur = parent.clone();
                    let op_path = path.clone();
                    let undo_path = path.clone();
                    shell.update(cx, move |this, cx| {
                        this.spawn_file_op(
                            cur,
                            move || std::fs::create_dir(&op_path).map_err(|e| e.to_string()),
                            "new-folder",
                            window,
                            cx,
                        );
                        this.push_undo(UndoOp::DeleteFolder(undo_path));
                    });
                    true
                })
        });
    }

    pub(super) fn on_rename_selected(
        &mut self,
        _: &RenameSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        let original_name = entry.name.clone();
        let input_state = cx.new(|cx| InputState::new(window, cx).placeholder("New name"));
        input_state.update(cx, |state, cx| {
            state.set_value(original_name.clone(), window, cx);
        });
        let input_for_ok = input_state.clone();
        let shell = cx.entity();
        let parent = self.active_tab().current_dir.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let input = input_state.clone();
            let input_for_ok = input_for_ok.clone();
            let shell = shell.clone();
            let old_path = old_path.clone();
            let original_name = original_name.clone();
            let parent = parent.clone();
            dialog
                .title("Rename")
                .child(Input::new(&input).small())
                .on_ok(move |_, window, cx: &mut App| {
                    let new_name = input_for_ok.read(cx).value().trim().to_string();
                    if new_name.is_empty() || new_name == original_name {
                        return true;
                    }
                    let mut new_path = old_path.clone();
                    new_path.set_file_name(&new_name);
                    let cur = parent.clone();
                    let op_old_path = old_path.clone();
                    let op_new_path = new_path.clone();
                    let undo_current = new_path.clone();
                    let undo_original = old_path.clone();
                    shell.update(cx, move |this, cx| {
                        this.spawn_file_op(
                            cur,
                            move || {
                                std::fs::rename(&op_old_path, &op_new_path)
                                    .map_err(|e| e.to_string())
                            },
                            "rename",
                            window,
                            cx,
                        );
                        this.push_undo(UndoOp::Rename {
                            current: undo_current,
                            original: undo_original,
                        });
                    });
                    true
                })
        });
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
            let refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
            let _ = crate::platform_shell::show_quick_look(&refs);
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.on_toggle_preview(&TogglePreview, window, cx);
        }
        let _ = window;
    }

    pub(super) fn on_duplicate(&mut self, _: &Duplicate, window: &mut Window, cx: &mut Context<Self>) {
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
                for path in paths {
                    crate::platform_shell::duplicate_path(&path).map(|_| ())?;
                }
                Ok(())
            },
            "duplicate",
            window,
            cx,
        );
    }

    pub(super) fn on_make_alias(&mut self, _: &MakeAlias, window: &mut Window, cx: &mut Context<Self>) {
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
                for path in paths {
                    crate::platform_shell::make_alias(&path).map(|_| ())?;
                }
                Ok(())
            },
            "make-alias",
            window,
            cx,
        );
    }

    pub(super) fn on_compress(&mut self, _: &Compress, window: &mut Window, cx: &mut Context<Self>) {
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
                let targets: Vec<&std::path::Path> =
                    paths.iter().map(|path| path.as_path()).collect();
                crate::platform_shell::compress_paths(&targets).map(|_| ())
            },
            "compress",
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
        let targets: Vec<(feraille_core::NodeId, PathBuf)> = self
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
            let (cleared, failed) = cx
                .background_executor()
                .spawn(async move {
                    let mut cleared: Vec<feraille_core::NodeId> = Vec::new();
                    let mut failed = 0usize;
                    for (id, path) in targets {
                        match feraille_fs_native::clear_quarantine(&path) {
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
                                failed += 1;
                            }
                        }
                    }
                    (cleared, failed)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.apply_quarantine_cleared(&cleared, failed, window, cx);
            });
        })
        .detach();
    }

    /// Foreground half of `on_clear_quarantine`: flip the cached row
    /// state for every cleared NodeId (in every tab — the same file
    /// can be visible twice) and report the outcome.
    fn apply_quarantine_cleared(
        &mut self,
        cleared: &[feraille_core::NodeId],
        failed: usize,
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
        if failed > 0 {
            window.push_notification(
                Notification::warning(format!(
                    "Couldn't clear the mark on {failed} file(s) — see log"
                )),
                cx,
            );
        }
    }
}
