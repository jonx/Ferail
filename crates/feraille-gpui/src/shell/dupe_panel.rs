//! Dedicated grouped duplicate panel (docs/features/DUPLICATES.md).
//!
//! [`crate::feature_settings::DupePresentation::Panel`] renders confirmed
//! duplicate groups as collapsible cards instead of adjacent table rows,
//! and adds the group-level cleanup actions the grouped-rows view can't
//! express: keep-newest, keep-this (select all but one), and trash the
//! marked set. The backing model is `Tab::dupe_groups` ([`DupeGroupView`])
//! and selection rides the tab's existing `selection` set, so the trash
//! flow and node store are shared rather than duplicated.
//!
//! Prime directive: the render path reads the retained model only — no
//! I/O, no settings reads (presentation is cached on `DupeViewMode` at
//! scan launch). The destructive action runs the same off-thread trash
//! worker as `on_move_to_trash`, then prunes the model and rebuilds the
//! card list from what survived.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
};

use feraille_core::NodeId;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::Scrollbar;

use super::dupes::storage_note;
use super::tab::DupeGroupView;
use super::*;

impl Shell {
    /// Card-based duplicate panel for the active tab. Caller guarantees
    /// `dupe_mode` is `Some` with `presentation == Panel`.
    pub(super) fn dupe_panel_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let tab = self.active_tab();
        let Some(dm) = tab.dupe_mode.as_ref() else {
            return div().into_any_element();
        };
        let selection = &tab.selection;
        let selected = selection.len();
        let scanning = tab.load_cancel.is_some();

        // Toolbar: a running summary plus the global actions. "Reclaimable"
        // is the whole-scan figure; the selected count tells the user how
        // much the Trash button will act on.
        let summary = format!(
            "{} group{} \u{00B7} {} reclaimable",
            dm.groups,
            if dm.groups == 1 { "" } else { "s" },
            feraille_fs_native::humanize_bytes(dm.wasted_bytes),
        );
        let toolbar = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(summary),
            )
            .when(scanning, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("scanning\u{2026}"),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("dupe-keep-newest-all")
                    .small()
                    .ghost()
                    .label("Keep newest everywhere")
                    .tooltip("Mark every copy except the most recent in each group")
                    .on_click(cx.listener(|this, _, _, cx| this.dupe_stage_keep_newest_all(cx))),
            )
            .child(
                Button::new("dupe-clear")
                    .small()
                    .ghost()
                    .label("Clear")
                    .disabled(selected == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.dupe_clear_marks(cx))),
            )
            .child(
                Button::new("dupe-trash-selected")
                    .small()
                    .danger()
                    .label(if selected == 0 {
                        "Trash marked".to_string()
                    } else {
                        format!("Trash {selected} marked")
                    })
                    .disabled(selected == 0)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.dupe_trash_marked(window, cx)),
                    ),
            );

        let list = if tab.dupe_groups.is_empty() {
            div()
                .id("dupe-panel-empty")
                .flex_1()
                .child(
                    div()
                        .p_8()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(if scanning {
                            "Scanning for duplicates\u{2026}"
                        } else {
                            "No duplicates found."
                        }),
                )
                .into_any_element()
        } else {
            let view = cx.entity().clone();
            let scroll = tab.dupe_panel_scroll.clone();
            let item_sizes: Rc<Vec<Size<Pixels>>> = Rc::new(
                tab.dupe_groups
                    .iter()
                    .map(dupe_group_card_estimated_size)
                    .collect(),
            );
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    crate::multi_table::v_virtual_list(
                        view,
                        "dupe-panel-scroll",
                        item_sizes,
                        move |this, visible_range: Range<usize>, _window, cx| {
                            let tab = this.active_tab();
                            let root = tab
                                .dupe_mode
                                .as_ref()
                                .map(|dm| dm.root.clone())
                                .unwrap_or_default();
                            let selection = tab.selection.clone();
                            let groups: Vec<DupeGroupView> = visible_range
                                .filter_map(|ix| tab.dupe_groups.get(ix).cloned())
                                .collect();

                            groups
                                .into_iter()
                                .map(|group| this.dupe_group_card(&group, &root, &selection, cx))
                                .collect::<Vec<_>>()
                        },
                    )
                    .track_scroll(&scroll)
                    .flex_1()
                    .size_full()
                    .p_2()
                    .gap_2()
                    .with_sizing_behavior(ListSizingBehavior::Auto),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(16.0))
                        .child(Scrollbar::vertical(&scroll)),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .child(toolbar)
            .child(list)
            .into_any_element()
    }

    /// One collapsible group card.
    fn dupe_group_card(
        &self,
        group: &DupeGroupView,
        root: &Path,
        selection: &std::collections::HashSet<NodeId>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let group_no = group.group_no;
        let copies = group.members.len();
        let reclaimable = group.reclaimable_bytes();

        // macOS/APFS zero-copy remediation: replace the redundant copies
        // with clones of the keeper — frees the bytes without deleting any
        // file. Hidden off macOS; surfaces a toast if the volume isn't
        // APFS (clonefile errors there).
        let dedup_btn = if cfg!(target_os = "macos") && group.distinct_occupants() > 1 {
            Some(
                Button::new(ElementId::Name(format!("dupe-clone-{group_no}").into()))
                    .xsmall()
                    .ghost()
                    .label("Dedup \u{2192} clones")
                    .tooltip("Replace extra copies with APFS clones (keeps every file)")
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.dupe_dedup_group(group_no, window, cx)
                    })),
            )
        } else {
            None
        };

        let header =
            h_flex()
                .id(ElementId::Name(format!("dupe-card-{group_no}").into()))
                .w_full()
                .items_center()
                .gap_2()
                .px_2()
                .py_1p5()
                .cursor_pointer()
                .hover(|this| this.bg(theme.secondary))
                .on_click(cx.listener(move |this, _, _, cx| this.dupe_toggle_group(group_no, cx)))
                .child(div().w(px(14.0)).text_color(theme.muted_foreground).child(
                    if group.expanded {
                        "\u{25BE}"
                    } else {
                        "\u{25B8}"
                    },
                ))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(format!("#{group_no}")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{copies} copies \u{00B7} {} each \u{00B7} {} reclaimable",
                            feraille_fs_native::humanize_bytes(group.bytes_each),
                            feraille_fs_native::humanize_bytes(reclaimable),
                        )),
                )
                .child(div().flex_1())
                .child(
                    Button::new(ElementId::Name(format!("dupe-newest-{group_no}").into()))
                        .xsmall()
                        .ghost()
                        .label("Keep newest")
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dupe_stage_keep_newest(group_no, cx)
                        })),
                )
                .child(
                    Button::new(ElementId::Name(format!("dupe-allbutone-{group_no}").into()))
                        .xsmall()
                        .ghost()
                        .label("All but one")
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dupe_stage_all_but_one(group_no, cx)
                        })),
                )
                .children(dedup_btn);

        let mut card = v_flex()
            .w_full()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(header);

        if group.expanded {
            let mut body = v_flex().w_full().px_2().pb_1();
            for member in &group.members {
                let node = member.node;
                let marked = selection.contains(&node);
                let is_keeper = group.keeper == Some(node);
                let name = member
                    .path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let location = member_location(&member.path, root);
                let note = storage_note(member.is_hardlink, member.is_clone);

                // Marked-for-trash checkbox.
                let check = div()
                    .id(ElementId::Name(
                        format!("dupe-mark-{}", node.as_raw()).into(),
                    ))
                    .flex_shrink_0()
                    .w(px(15.0))
                    .h(px(15.0))
                    .rounded(px(2.0))
                    .border_1()
                    .border_color(if marked { theme.danger } else { theme.border })
                    .when(marked, |this| this.bg(theme.danger))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .when(marked, |this| {
                        this.child(div().text_xs().text_color(gpui::white()).child("\u{2713}"))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.dupe_toggle_mark(node, cx)));

                // Keep-this radio.
                let radio =
                    div()
                        .id(ElementId::Name(
                            format!("dupe-keep-{}", node.as_raw()).into(),
                        ))
                        .flex_shrink_0()
                        .w(px(15.0))
                        .h(px(15.0))
                        .rounded_full()
                        .border_1()
                        .border_color(if is_keeper {
                            theme.primary
                        } else {
                            theme.border
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .when(is_keeper, |this| {
                            this.child(div().w(px(7.0)).h(px(7.0)).rounded_full().bg(theme.primary))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dupe_pick_keeper(group_no, node, cx)
                        }));

                let shares = member.shares_storage();
                let row = h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .py_0p5()
                    .child(check)
                    .child(radio)
                    // Name takes the bulk and ellipsizes when too long
                    // (build-artifact names can be enormous); the location
                    // gets a bounded tail. `min_w_0` lets the flex child
                    // shrink below its content so truncation can kick in.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(if shares {
                                theme.muted_foreground
                            } else {
                                theme.foreground
                            })
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .max_w(px(280.0))
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{location}{note}")),
                    );
                body = body.child(row);
            }
            card = card.child(body);
        }

        card.into_any_element()
    }

    // ===== Group actions =====

    fn dupe_group_mut<'a>(
        groups: &'a mut [DupeGroupView],
        group_no: usize,
    ) -> Option<&'a mut DupeGroupView> {
        groups.iter_mut().find(|g| g.group_no == group_no)
    }

    /// Expand / collapse a single card.
    fn dupe_toggle_group(&mut self, group_no: usize, cx: &mut Context<Self>) {
        if let Some(g) = Self::dupe_group_mut(&mut self.active_tab_mut().dupe_groups, group_no) {
            g.expanded = !g.expanded;
            cx.notify();
        }
    }

    /// Pick a keeper (the "keep this" radio) and mark every other member
    /// of that group for trashing.
    fn dupe_pick_keeper(&mut self, group_no: usize, keeper: NodeId, cx: &mut Context<Self>) {
        let victims = {
            let groups = &mut self.active_tab_mut().dupe_groups;
            let Some(g) = Self::dupe_group_mut(groups, group_no) else {
                return;
            };
            g.keeper = Some(keeper);
            g.victims_for_keeper(keeper)
        };
        self.dupe_mark(&victims, cx);
    }

    /// Toggle one member in/out of the marked-for-trash set.
    fn dupe_toggle_mark(&mut self, node: NodeId, cx: &mut Context<Self>) {
        let sel = &mut self.active_tab_mut().selection;
        if !sel.remove(&node) {
            sel.insert(node);
        }
        cx.notify();
    }

    /// Mark this group's all-but-newest for trashing.
    fn dupe_stage_keep_newest(&mut self, group_no: usize, cx: &mut Context<Self>) {
        let victims = Self::dupe_group_mut(&mut self.active_tab_mut().dupe_groups, group_no)
            .map(|g| g.victims_keep_newest())
            .unwrap_or_default();
        self.dupe_mark(&victims, cx);
    }

    /// Mark this group's all-but-keeper (defaults to the first member).
    fn dupe_stage_all_but_one(&mut self, group_no: usize, cx: &mut Context<Self>) {
        let victims = Self::dupe_group_mut(&mut self.active_tab_mut().dupe_groups, group_no)
            .map(|g| g.victims_all_but_one())
            .unwrap_or_default();
        self.dupe_mark(&victims, cx);
    }

    /// Global "keep newest everywhere": union of every group's
    /// all-but-newest.
    fn dupe_stage_keep_newest_all(&mut self, cx: &mut Context<Self>) {
        let victims: Vec<NodeId> = self
            .active_tab()
            .dupe_groups
            .iter()
            .flat_map(|g| g.victims_keep_newest())
            .collect();
        self.dupe_mark(&victims, cx);
    }

    /// Add nodes to the marked-for-trash set.
    fn dupe_mark(&mut self, nodes: &[NodeId], cx: &mut Context<Self>) {
        let sel = &mut self.active_tab_mut().selection;
        for n in nodes {
            sel.insert(*n);
        }
        cx.notify();
    }

    /// Clear all marks.
    fn dupe_clear_marks(&mut self, cx: &mut Context<Self>) {
        self.active_tab_mut().selection.clear();
        cx.notify();
    }

    /// Trash every marked member, then prune the retained model and
    /// rebuild the card list / table from what survived. Mirrors
    /// `on_move_to_trash`'s off-thread worker + undo, but owns the prune
    /// because a dupe tab's watcher reload is suppressed.
    fn dupe_trash_marked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::notification::Notification;

        let tab_id = self.active_tab().id;
        let marked = self.active_tab().selection.clone();
        if marked.is_empty() {
            return;
        }
        // Resolve marked nodes to paths via the retained model (no I/O).
        let paths: Vec<PathBuf> = self
            .active_tab()
            .dupe_groups
            .iter()
            .flat_map(|g| g.members.iter())
            .filter(|m| marked.contains(&m.node))
            .map(|m| m.path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        let count = paths.len();
        let name = format!("{count} duplicate{}", if count == 1 { "" } else { "s" });

        let process = self.process.clone();
        let task_id = self.process.tasks.borrow_mut().begin(
            crate::tasks::TaskKind::FileOp,
            format!("Trashing {name}"),
            false,
        );
        let weak = cx.weak_entity();
        let win = window.window_handle();
        cx.spawn(async move |_this, cx| {
            let (pairs, error) = cx
                .background_executor()
                .spawn(async move {
                    let mut pairs: Vec<(PathBuf, PathBuf)> = Vec::new();
                    for path in &paths {
                        match feraille_fs_native::move_to_trash(path) {
                            Ok(Some(trashed)) => pairs.push((path.clone(), trashed)),
                            Ok(None) => {}
                            Err(e) => return (pairs, Some(e.to_string())),
                        }
                    }
                    (pairs, None)
                })
                .await;
            match &error {
                Some(e) => process.tasks.borrow_mut().end_failed(task_id, e.clone()),
                None => process.tasks.borrow_mut().end(task_id),
            }
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| {
                    if !pairs.is_empty() {
                        let trashed_nodes: Vec<PathBuf> =
                            pairs.iter().map(|(orig, _)| orig.clone()).collect();
                        this.prune_dupe_model_by_path(tab_id, &trashed_nodes, cx);
                        this.push_undo(UndoOp::TrashRestore(pairs.clone()));
                    }
                    cx.notify();
                });
            }
            let _ = win.update(cx, |_, window, cx| match &error {
                None => window
                    .push_notification(Notification::info(format!("Moved {name} to Trash")), cx),
                Some(e) => {
                    window.push_notification(Notification::error(format!("Trash failed: {e}")), cx)
                }
            });
        })
        .detach();
    }

    /// macOS/APFS zero-copy dedup: replace a group's redundant,
    /// storage-owning copies with `clonefile` clones of the keeper, after
    /// an explicit confirm. Keeps every file; frees the duplicated bytes.
    #[cfg(target_os = "macos")]
    fn dupe_dedup_group(&mut self, group_no: usize, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::notification::Notification;

        let tab_id = self.active_tab().id;
        let (keeper_path, victims): (PathBuf, Vec<(NodeId, PathBuf)>) = {
            let Some(g) = self
                .active_tab()
                .dupe_groups
                .iter()
                .find(|g| g.group_no == group_no)
            else {
                return;
            };
            let Some(keeper) = g.keeper.or_else(|| g.newest()) else {
                return;
            };
            let Some(keeper_path) = g
                .members
                .iter()
                .find(|m| m.node == keeper)
                .map(|m| m.path.clone())
            else {
                return;
            };
            let victims = g
                .members
                .iter()
                .filter(|m| m.node != keeper && !m.shares_storage())
                .map(|m| (m.node, m.path.clone()))
                .collect();
            (keeper_path, victims)
        };
        if victims.is_empty() {
            return;
        }
        let count = victims.len();
        let keeper_name = keeper_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        let process = self.process.clone();
        let weak = cx.weak_entity();
        let win = window.window_handle();
        cx.spawn(async move |_this, cx| {
            let (go_tx, go_rx) = async_channel::bounded::<bool>(1);
            let opened = win.update(cx, |_, window, cx| {
                let tx = go_tx.clone();
                window.open_dialog(cx, move |dialog, _window, _cx| {
                    let tx_go = tx.clone();
                    let tx_cancel = tx.clone();
                    let body = format!(
                        "Replace {count} extra cop{} with APFS clones of \u{201C}{keeper_name}\u{201D}? \
                         Every file stays; the duplicated bytes are freed.",
                        if count == 1 { "y" } else { "ies" },
                    );
                    dialog
                        .title("Dedup with clones?")
                        .child(div().text_sm().child(body))
                        .child(
                            h_flex().pt_2().child(
                                Button::new("dupe-clone-go")
                                    .label("Dedup")
                                    .primary()
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
                format!("Cloning {count} duplicate{}", if count == 1 { "" } else { "s" }),
                false,
            );
            let victim_paths: Vec<PathBuf> = victims.iter().map(|(_, p)| p.clone()).collect();
            let keeper_for_bg = keeper_path.clone();
            let (done_ix, error) = cx
                .background_executor()
                .spawn(async move {
                    let mut done: Vec<usize> = Vec::new();
                    let mut err: Option<String> = None;
                    for (i, vp) in victim_paths.iter().enumerate() {
                        match feraille_fs_native::clone_dedup(&keeper_for_bg, vp) {
                            Ok(()) => done.push(i),
                            Err(e) => {
                                err = Some(e);
                                break;
                            }
                        }
                    }
                    (done, err)
                })
                .await;
            match &error {
                Some(e) => process.tasks.borrow_mut().end_failed(task_id, e.clone()),
                None => process.tasks.borrow_mut().end(task_id),
            }
            let cloned: Vec<NodeId> = done_ix
                .into_iter()
                .filter_map(|i| victims.get(i).map(|(n, _)| *n))
                .collect();
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| {
                    this.mark_dupe_members_cloned(tab_id, group_no, &cloned, cx);
                });
            }
            let _ = win.update(cx, |_, window, cx| match &error {
                None => window.push_notification(
                    Notification::info(format!(
                        "Replaced {count} cop{} with clones",
                        if count == 1 { "y" } else { "ies" }
                    )),
                    cx,
                ),
                Some(e) => {
                    window.push_notification(Notification::error(format!("Dedup failed: {e}")), cx)
                }
            });
        })
        .detach();
    }

    #[cfg(not(target_os = "macos"))]
    fn dupe_dedup_group(
        &mut self,
        _group_no: usize,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    /// Flag freshly-created clones in the retained model and recompute the
    /// reclaim summary. The panel re-renders straight from the model — no
    /// table rebuild, no I/O. macOS-only (the dedup path that calls it is
    /// gated).
    #[cfg(target_os = "macos")]
    fn mark_dupe_members_cloned(
        &mut self,
        tab_id: TabId,
        group_no: usize,
        nodes: &[NodeId],
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let tab = &mut self.tabs[idx];
        if let Some(g) = tab.dupe_groups.iter_mut().find(|g| g.group_no == group_no) {
            for m in g.members.iter_mut() {
                if nodes.contains(&m.node) {
                    m.is_clone = true;
                }
            }
        }
        if let Some(dm) = tab.dupe_mode.as_mut() {
            dm.wasted_bytes = tab.dupe_groups.iter().map(|g| g.reclaimable_bytes()).sum();
        }
        cx.notify();
    }

    /// Drop trashed members (by path) from the retained groups, drop any
    /// group left with fewer than two members, renumber, recompute the
    /// reclaim summary, and clear the marks. The panel renders from this
    /// model directly, so there is nothing to rebuild and no I/O.
    fn prune_dupe_model_by_path(
        &mut self,
        tab_id: TabId,
        trashed: &[PathBuf],
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let trashed: std::collections::HashSet<&PathBuf> = trashed.iter().collect();
        let tab = &mut self.tabs[idx];
        for g in tab.dupe_groups.iter_mut() {
            g.members.retain(|m| !trashed.contains(&m.path));
        }
        tab.dupe_groups.retain(|g| g.members.len() >= 2);
        // Renumber 1..N so the cards stay gap-free after a cleanup.
        for (i, g) in tab.dupe_groups.iter_mut().enumerate() {
            g.group_no = i + 1;
        }
        tab.selection.clear();
        if let Some(dm) = tab.dupe_mode.as_mut() {
            dm.groups = tab.dupe_groups.len();
            dm.wasted_bytes = tab.dupe_groups.iter().map(|g| g.reclaimable_bytes()).sum();
        }
        cx.notify();
    }
}

fn dupe_group_card_estimated_size(group: &DupeGroupView) -> Size<Pixels> {
    const HEADER_H: f32 = 36.0;
    const BODY_PAD_H: f32 = 8.0;
    const MEMBER_ROW_H: f32 = 25.0;

    let body_h = if group.expanded {
        BODY_PAD_H + MEMBER_ROW_H * group.members.len() as f32
    } else {
        0.0
    };
    Size {
        width: px(0.0),
        height: px(HEADER_H + body_h),
    }
}

/// Member's parent directory relative to the scan root, matching the
/// grouped-rows location string.
fn member_location(path: &Path, root: &Path) -> String {
    path.parent()
        .map(|parent| match parent.strip_prefix(root) {
            Ok(rel) if rel.as_os_str().is_empty() => "\u{00B7}".to_string(),
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => parent.to_string_lossy().into_owned(),
        })
        .unwrap_or_default()
}
