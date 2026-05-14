use super::*;

impl Shell {
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
        let mut cmd = std::process::Command::new("/usr/bin/open");
        cmd.arg("-R").args(&paths);
        let _ = cmd.spawn();
        let msg = if paths.len() == 1 {
            let name = paths[0]
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("item")
                .to_string();
            format!("Showing \u{201C}{}\u{201D} in Finder", name)
        } else {
            format!("Showing {} items in Finder", paths.len())
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
        let _ = std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(&path)
            .spawn();
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
        let candidates = crate::platform_shell::open_with_candidates(first);
        if let Some(c) = candidates.get(slot) {
            for path in paths {
                let _ = crate::platform_shell::open_with_app(&path, &c.path);
            }
        }
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
                .on_ok(move |_, _window, cx: &mut App| {
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
                .on_ok(move |_, _window, cx: &mut App| {
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

    pub(super) fn on_quick_look(&mut self, _: &QuickLook, _: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        if paths.is_empty() {
            return;
        }
        let refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
        let _ = crate::platform_shell::show_quick_look(&refs);
    }

    pub(super) fn on_duplicate(&mut self, _: &Duplicate, _: &mut Window, cx: &mut Context<Self>) {
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
            cx,
        );
    }

    pub(super) fn on_make_alias(&mut self, _: &MakeAlias, _: &mut Window, cx: &mut Context<Self>) {
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
            cx,
        );
    }

    pub(super) fn on_compress(&mut self, _: &Compress, _: &mut Window, cx: &mut Context<Self>) {
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
            cx,
        );
    }
}
