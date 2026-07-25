use super::*;
use crate::text::IconScale as _;

/// Minimum width for rendered-markdown preview content, so its prose
/// reads as a column instead of folding to slivers in the narrow preview
/// pane. The box scrolls horizontally to reach overflow when the pane is
/// narrower than this; a wider pane lets the content grow past it.
const PREVIEW_MD_MIN_W: f32 = 520.0;

/// Code/source preview: a `whitespace_nowrap` code block clips long lines
/// but won't grow its container past the pane on its own, so the box has
/// nothing to scroll toward. We give the content a definite width sized to
/// the widest line — these tune that estimate. Width per column at the 9px
/// mono used in the code block; slightly over the real ~5.4px advance so
/// the last glyphs aren't clipped (a little slop on the right is fine,
/// lost characters are not).
const PREVIEW_CODE_CHAR_W: f32 = 5.8;
/// A horizontal tab counts as this many columns when measuring the widest
/// line (source is commonly tab-indented; 1 char would under-size it).
const PREVIEW_CODE_TAB_COLS: usize = 4;
/// Box + code-block horizontal padding added to the measured line width.
const PREVIEW_CODE_PAD: f32 = 48.0;
/// Upper bound on the sized width so a minified single-line file doesn't
/// build a multi-thousand-pixel element (it clips past this — rare).
const PREVIEW_CODE_MAX_W: f32 = 4000.0;

/// Payload carried by a tab-strip drag (Phase D, spec §3.3
/// "Reorder tab"). The same Render-as-its-own-preview shape
/// `FavoriteDragPayload` uses — a chip following the cursor with the
/// dragged tab's label. The `id` is the source tab's process-local
/// `TabId`; the drop target resolves it back to an index against the
/// current `Shell::tabs` vec so concurrent reorder operations stay
/// coherent.
///
/// Phase D drops are within-strip only. Cross-window tear-off / merge
/// (spec §3.5) needs a different payload shape and lands in Phase F.
#[derive(Clone)]
pub struct TabDragPayload {
    pub id: TabId,
    pub label: SharedString,
    /// Strip index at drag start. Only used to pick which edge of a
    /// hovered chip gets the insertion highlight — the strip can't
    /// reorder mid-drag, so the render-time index stays valid for
    /// styling. Drop handlers must still resolve by `id`.
    pub from_idx: usize,
}

impl Render for TabDragPayload {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_3()
            .py_1()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .text_scale_sm()
            .text_color(theme.foreground)
            .child(self.label.clone())
    }
}

/// One drop gap between (or at the ends of) tab chips. `pos` is the
/// gap position in `0..=tabs.len()` — gap 0 is before the first tab,
/// gap N is after the last. `drag_over` paints a 2-DIP vertical accent
/// rule so the user sees exactly where the drop will land — same idea
/// as `favorites_section::render_drop_gap` rotated 90°. On drop,
/// `Shell::reorder_tab` resolves the source `TabId` and moves the tab
/// into this position.
/// Drag payload for the resize grip under the preview thumbnail —
/// same invisible-ghost shape as `multi_table`'s `ResizeColumn`: the
/// drag machinery wants a Render entity to follow the cursor, but a
/// resize has nothing to show, so it renders `Empty`. The real state
/// (drag-start anchor) lives in `Shell::preview_thumb_drag`.
#[derive(Clone)]
pub(crate) struct ResizePreviewThumb;

impl Render for ResizePreviewThumb {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Truncated single-line URL for the preview pane's provenance rows,
/// with the full URL in a hover tooltip — same treatment as the
/// "Where" path row. Pure display; no parsing.
fn truncated_url_value(key: &'static str, url: &str, id: feraille_core::NodeId) -> AnyElement {
    let full = SharedString::from(url.to_string());
    let tip = full.clone();
    div()
        .id((key, id.as_raw() as usize))
        .truncate()
        .child(full)
        .tooltip(move |window, cx| {
            gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
        })
        .into_any_element()
}

/// One mounted volume for the sidebar Volumes section:
/// `(path, display name, Some((total, available)) capacity bytes,
/// is removable/ejectable, is network mount)`.
type VolumeRow = (PathBuf, String, Option<(u64, u64)>, bool, bool);
/// `VolumeRow` plus the "is favorited" star flag.
type VolumeRowFav = (PathBuf, String, Option<(u64, u64)>, bool, bool, bool);

fn tab_drop_gap(pos: usize, cx: &mut Context<Shell>) -> impl IntoElement {
    let theme = cx.theme();
    let accent = theme.primary;
    div()
        .id(("tab-gap", pos))
        .h(px(24.0))
        .w(px(6.0))
        .flex_shrink_0()
        .drag_over::<TabDragPayload>(move |style, _payload, _window, _cx| {
            style.border_l_2().border_color(accent)
        })
        .on_drop(
            cx.listener(move |this, payload: &TabDragPayload, _window, cx| {
                this.reorder_tab(payload.id, pos, cx);
            }),
        )
}

impl Shell {
    fn tool_result_breadcrumb_summary(&self) -> Option<String> {
        let surface = self.active_tab().tool_result.as_ref()?;
        match &surface.mode {
            // Text only — the 🔍/⧉ pictographs here rendered as tofu boxes
            // on fonts without them (AROS bundled font).
            super::tab::ToolResultMode::Search(search) => Some(format!(
                "{}  \u{00B7}  {}",
                search.needle, search.engine_label
            )),
            super::tab::ToolResultMode::Duplicates(dupe) => Some(format!(
                "{} duplicate group{} \u{00B7} {} reclaimable",
                dupe.groups,
                if dupe.groups == 1 { "" } else { "s" },
                feraille_fs_native::humanize_bytes(dupe.wasted_bytes),
            )),
            super::tab::ToolResultMode::DiskUsage(_) => Some("Disk Usage".to_string()),
            super::tab::ToolResultMode::Archive(am) => Some(
                am.archive
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Archive".to_string()),
            ),
        }
    }

    fn active_tool_result_can_pop_out(&self) -> bool {
        matches!(
            self.active_tab()
                .tool_result
                .as_ref()
                .map(|surface| &surface.mode),
            Some(super::tab::ToolResultMode::DiskUsage(_))
        )
    }

    /// Build the **Browse** section as a single-rooted, expandable
    /// tree starting at the home folder. (Phase 2: the flat
    /// shortcut list moved out into the dedicated Favorites menu
    /// above this section, which eliminates the
    /// Downloads-appears-twice IA bug — favorites don't expand.)
    ///
    /// Direct children of Home that are already pinned in Favorites
    /// are hidden from the tree so the same path can never appear
    /// twice in the sidebar, even when Home is expanded. Browse then
    /// reads as "the parts of Home that aren't already in Favorites"
    /// — Library, Public, custom subfolders, etc. — plus their
    /// hierarchy. Deeper descendants are untouched: expanding
    /// `Library/Application Support` is fine because that's already
    /// not pinned anywhere.
    fn build_browse_rows(&mut self, cx: &App) -> Vec<TreeRowSpec> {
        let home = home_dir();
        // Hide Locations from the Browse tree so the same depth-1 entry
        // (Documents, Downloads, etc.) doesn't appear twice. User-curated
        // Favorites are *not* hidden — those are intentional shortcuts.
        let location_paths: HashSet<PathBuf> = crate::special_folders::locations(cx)
            .iter()
            .map(|loc| loc.path.clone())
            .collect();
        let current = self.active_tab().current_dir.clone();
        let node_id = self.process.fs.id_for_path(&home);
        self.process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(home.clone(), node_id);
        let is_expanded = self.expanded.contains(&home);
        let favorited = self.process.favorites.read(cx).contains_path(&home);
        let mut rows: Vec<TreeRowSpec> = vec![TreeRowSpec {
            node_id,
            path: home.clone(),
            label: SharedString::from("Home"),
            depth: 0,
            guides: Vec::new(),
            is_expandable: true,
            is_expanded,
            is_active: home == current,
            capacity: None,
            icon: TreeRowIcon::Folder,
            favorited,
            ejectable: false,
        }];
        if is_expanded {
            self.append_tree_descendants_filtered(
                &mut rows,
                &home,
                1,
                &current,
                Some(&location_paths),
                &mut Vec::new(),
                cx,
            );
        }
        rows
    }

    /// Build the user-curated **Favorites** section (separate from the
    /// fixed Locations menu above). Iter 2 renders an empty section
    /// with the empty-state prompt; iter 3 wires the live entity and
    /// the §5 favorited-indicator index. The section's collapse state
    /// flows through `favorites_section_collapsed`, persisted in
    /// `MetadataDb`.
    fn build_user_favorites_section(
        &mut self,
        weak: WeakEntity<Self>,
        cx: &mut Context<Self>,
    ) -> ShellSidebarItem {
        // Snapshot the current entry list; the entity is observed at
        // construction time below so any mutation (add / remove /
        // reorder / rename) drives a Shell repaint, which re-runs
        // `build_user_favorites_section` with the fresh list.
        let entries = self.process.favorites.read(cx).entries().to_vec();
        ShellSidebarItem::favorites(crate::favorites_section::FavoritesSection::new(
            entries,
            self.favorites_section_collapsed,
            weak,
            self.process.icons.clone(),
            self.focused_favorite,
            self.favorites_focus.clone(),
            self.fav_appear.clone(),
            self.fav_pulse.clone(),
            self.fav_removing.clone(),
        ))
    }

    /// Build the **Recents** section from the in-memory recents cache
    /// (most-recent-first folders). Returns `None` when the feature is
    /// switched off or the cache is empty, so the section stays hidden
    /// for a disabled user or a brand-new profile — no clutter.
    fn build_recents_section(
        &self,
        weak: WeakEntity<Self>,
        cx: &App,
    ) -> Option<ShellSidebarItem> {
        if !crate::recents_section::recents_enabled(cx) {
            return None;
        }
        let recents = self.process.recents.borrow();
        if recents.is_empty() {
            return None;
        }
        Some(ShellSidebarItem::recents(
            crate::recents_section::RecentsSection::new(
                recents.clone(),
                self.process.recents_section_collapsed.get(),
                weak,
                self.process.icons.clone(),
            ),
        ))
    }

    /// Flip the Favorites section's disclosure-triangle and persist
    /// the new state. Called from the section header click handler.
    pub fn toggle_favorites_section_collapsed(&mut self, cx: &mut Context<Self>) {
        self.favorites_section_collapsed = !self.favorites_section_collapsed;
        let collapsed = self.favorites_section_collapsed;
        self.process.favorites_section_collapsed.set(collapsed);
        if let Some(db) = self.process.db_snapshot() {
            cx.background_spawn(async move {
                if let Ok(g) = db.lock() {
                    let _ = g.set_favorites_section_collapsed(collapsed);
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// Flip the Recents section's disclosure triangle and persist the
    /// (process-wide) collapse flag to app_state. Shared by the header
    /// click and the `ToggleRecentsSection` action.
    pub fn toggle_recents_section_collapsed(&mut self, cx: &mut Context<Self>) {
        let collapsed = !self.process.recents_section_collapsed.get();
        self.process.recents_section_collapsed.set(collapsed);
        let mut s = crate::app_state::load();
        s.recents_collapsed = Some(collapsed);
        crate::app_state::save(&s);
        cx.notify();
    }

    pub fn on_toggle_recents_section(
        &mut self,
        _: &ToggleRecentsSection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_recents_section_collapsed(cx);
    }

    /// Drop the right-clicked folder from Recents only. Recency and Ant
    /// Trail heat are separate columns of the same `folder_usage` row,
    /// so this clears the recency (DB + in-memory list) and deliberately
    /// leaves the heat (`ant_visits`) alone — taking a folder off the
    /// recent list shouldn't erase how often you go there.
    pub fn on_remove_from_recents(
        &mut self,
        _: &RemoveFromRecents,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.context_target.take() else {
            return;
        };
        self.process.recents.borrow_mut().retain(|p| p != &path);
        if let Some(db) = self.process.db_snapshot() {
            let path_str = path.to_string_lossy().into_owned();
            cx.background_spawn(async move {
                if let Ok(g) = db.lock() {
                    let _ = g.forget_recent_access(&path_str);
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// Clear Recents — confirm, then empty the recents list. Recency and
    /// Ant Trail heat are separate columns of the same `folder_usage`
    /// row, so this clears only the recency (`last_access_unix`) and
    /// keeps the heat (`hits`): the most-visited tint survives. The
    /// trailing ellipsis on every surface that fires it flags the
    /// confirmation.
    pub fn on_clear_recents(
        &mut self,
        _: &ClearRecents,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Clear Recents");
        use gpui_component::button::{Button, ButtonVariants as _};
        let win = window.window_handle();
        cx.spawn(async move |this, cx| {
            let (go_tx, go_rx) = async_channel::bounded::<bool>(1);
            let opened = win.update(cx, |_, window, cx| {
                let tx = go_tx.clone();
                window.open_dialog(cx, move |dialog, _window, _cx| {
                    let tx_go = tx.clone();
                    let tx_cancel = tx.clone();
                    dialog
                        .title("Clear Recents?")
                        .child(div().text_scale_sm().child(
                            "Forget every folder in Recents? Your Ant Trail heat is \
                             kept \u{2014} only the recent list is emptied. This can't \
                             be undone.",
                        ))
                        .child(
                            h_flex().pt_2().child(
                                Button::new("clear-recents-go")
                                    .label("Clear Recents")
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
            let _ = this.update(cx, |this, cx| {
                // Recents only: empty the in-memory list and zero the DB
                // recency column. `ant_visits` / `ant_max` (the heat
                // signal) are left untouched, so the tint stays put.
                this.process.recents.borrow_mut().clear();
                if let Some(db) = this.process.db_snapshot() {
                    cx.background_spawn(async move {
                        if let Ok(g) = db.lock() {
                            let _ = g.clear_recent_access();
                        }
                    })
                    .detach();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Build the **Locations** section: a flat `SidebarMenu` of icon-
    /// prefixed shortcuts to the macOS-standard folders. Each item
    /// navigates straight to its path; none expand, so the IA stays
    /// unambiguous next to the user-curated Favorites section below
    /// and the expandable Browse tree underneath.
    fn build_locations_menu(&mut self, weak: WeakEntity<Self>, cx: &App) -> SidebarMenu {
        use gpui_component::Icon;
        let current = self.active_tab().current_dir.clone();
        let mut menu = SidebarMenu::new();
        let favs = self.process.favorites.read(cx);
        for loc in crate::special_folders::locations(cx).iter() {
            let path = loc.path.clone();
            let node_id = self.process.fs.id_for_path(&path);
            self.process
                .node_store
                .borrow_mut()
                .get_or_create_path_with_id(path.clone(), node_id);
            let active = path == current;
            let favorited = favs.contains_path(&path);
            // In-memory lookup only — the iCloud probe ran off-thread at
            // startup / volume refresh (ProcessState::cloud_locations).
            // `None` = not an iCloud Location; `Some(Downloaded/Placeholder)`
            // drives the solid-vs-outline trailing cloud badge.
            let cloud_state = self.process.cloud_locations.borrow().get(&path).copied();
            let weak_for_click = weak.clone();
            let weak_for_menu = weak.clone();
            let path_for_menu = path.clone();
            let path_for_modclick = path.clone();
            let item = SidebarMenuItem::new(SharedString::from(loc.label))
                .icon(
                    Icon::empty()
                        .path(loc.icon)
                        // gpui-component `Size` is px-only, so pre-multiply
                        // by `ui_scale` to match the rem-scaled icons. (At
                        // scale s: rems(24/16) * (16*s) == 24*s, identical.)
                        .with_size(px(crate::tree::SIDEBAR_ICON_PX * self.ui_scale)),
                )
                .active(active)
                .on_click(move |event, window, cx| {
                    if let Some(s) = weak_for_click.upgrade() {
                        let modifiers = event.modifiers();
                        let path = path_for_modclick.clone();
                        s.update(cx, |shell, cx| {
                            if modifiers.platform {
                                shell.open_path_in_new_tab(path, window, cx);
                            } else {
                                shell.navigate_node(node_id, cx);
                            }
                        });
                    }
                })
                .context_menu(move |menu, _window, cx| {
                    // Stash the right-clicked path on Shell so
                    // the path-aware action handlers know which
                    // path the user meant.
                    if let Some(s) = weak_for_menu.upgrade() {
                        s.update(cx, |shell, _| {
                            shell.context_target = Some(path_for_menu.clone());
                        });
                    }
                    menu.menu("Open in New Tab", Box::new(OpenContextInNewTab))
                        .separator()
                        .menu(
                            feraille_core::commands::REVEAL_LABEL,
                            Box::new(RevealContextPath),
                        )
                        .menu("Copy Path", Box::new(CopyContextPath))
                });
            // Trailing badges: a cloud for iCloud Locations (Desktop /
            // Documents under "Desktop & Documents Folders") — solid when the
            // folder is downloaded locally, outline when it's a not-downloaded
            // placeholder (Finder's downloaded-vs-evicted distinction) — plus
            // the §5 accent star when the entry is also a user Favorite. Cloud
            // sits left of the star so the star stays the rightmost
            // "favorited" marker, consistent with the file list, tree, and
            // breadcrumb.
            let item = if cloud_state.is_some() || favorited {
                item.suffix(move |_, cx| {
                    use feraille_fs_native::CloudState;
                    use gpui::svg;
                    let mut badges = h_flex().items_center().gap_1();
                    if let Some(state) = cloud_state {
                        // Solid `cloud-fill` = downloaded/"enabled"; outline
                        // `cloud` = set up for cloud but not downloaded.
                        let icon = match state {
                            CloudState::Downloaded => "icons/nav/cloud-fill.svg",
                            CloudState::Placeholder => "icons/nav/cloud.svg",
                        };
                        badges = badges.child(
                            svg()
                                .path(icon)
                                .icon_px(14.0)
                                // Match the black Locations row icons rather
                                // than washed-out muted grey.
                                .text_color(cx.theme().sidebar_foreground)
                                .flex_shrink_0(),
                        );
                    }
                    if favorited {
                        badges = badges.child(
                            svg()
                                .path("icons/nav/star.svg")
                                .icon_px(11.0)
                                .text_color(cx.theme().primary)
                                .flex_shrink_0(),
                        );
                    }
                    badges.into_any_element()
                })
            } else {
                item
            };
            menu = menu.child(item);
        }
        let _ = favs;
        menu
    }

    /// Build the Volumes section as a flat row list. Same recursion
    /// shape as Locations, but the depth-0 volume row carries a
    /// `(total, available)` capacity so the renderer can draw a
    /// Finder-style capacity bar.
    fn build_volumes_rows(&mut self, cx: &App) -> Vec<TreeRowSpec> {
        let current = self.active_tab().current_dir.clone();
        let mut rows: Vec<TreeRowSpec> = Vec::new();
        // Snapshot the favorites paths once so the inner loop doesn't
        // re-read the entity per row.
        let favs = self.process.favorites.read(cx);
        let volume_paths: Vec<VolumeRow> = self
            .process
            .volumes
            .borrow()
            .iter()
            .map(|v| {
                let cap = match (v.total_bytes, v.available_bytes) {
                    (Some(t), Some(a)) if t > 0 => Some((t, a)),
                    _ => None,
                };
                (v.path.clone(), v.name.clone(), cap, v.is_removable, !v.is_local)
            })
            .collect();
        let mut entries: Vec<VolumeRowFav> = volume_paths
            .into_iter()
            .map(|(p, n, c, ejectable, is_network)| {
                let fav = favs.contains_path(&p);
                (p, n, c, fav, ejectable, is_network)
            })
            .collect();
        let _ = favs;
        for (path, name, capacity, favorited, ejectable, is_network) in entries.drain(..) {
            let node_id = self.process.fs.id_for_path(&path);
            self.process
                .node_store
                .borrow_mut()
                .get_or_create_path_with_id(path.clone(), node_id);
            let is_expanded = self.expanded.contains(&path);
            rows.push(TreeRowSpec {
                node_id,
                path: path.clone(),
                label: SharedString::from(name),
                depth: 0,
                guides: Vec::new(),
                is_expandable: true,
                is_expanded,
                is_active: path == current,
                capacity,
                icon: if is_network {
                    TreeRowIcon::Network
                } else {
                    TreeRowIcon::Volume
                },
                favorited,
                ejectable,
            });
            if is_expanded {
                self.append_tree_descendants(&mut rows, &path, 1, &current, &mut Vec::new(), cx);
            }
        }
        rows
    }

    /// Recursively append children of `parent` (and their expanded
    /// descendants) to `rows`. Reads from `tree_children` only —
    /// callers must have called `ensure_tree_children` first
    /// (`toggle_tree_expand` / `reveal_path_in_tree` do). The
    /// `show_hidden` flag is checked here, not at enumeration time,
    /// so toggling Show Hidden doesn't require cache invalidation.
    ///
    /// Thin wrapper that runs without a skip filter — used by
    /// Volumes and any deeper-than-depth-1 recursion in Browse.
    fn append_tree_descendants(
        &self,
        rows: &mut Vec<TreeRowSpec>,
        parent: &Path,
        depth: usize,
        current: &Path,
        trunk: &mut Vec<bool>,
        cx: &App,
    ) {
        self.append_tree_descendants_filtered(rows, parent, depth, current, None, trunk, cx);
    }

    /// Same as [`append_tree_descendants`] but with an optional
    /// `skip_paths` filter applied to direct children only. Used by
    /// Browse to suppress depth-1 Home children that are already
    /// pinned in Locations. The filter is *not* propagated to deeper
    /// levels.
    ///
    /// `trunk` carries one bool per ancestor level above `depth`:
    /// `true` while that ancestor still has visible siblings below
    /// it (its connector line continues through these rows). The
    /// recursion pushes/pops as it descends so each row's `guides`
    /// column list comes out precomputed — render stays a pure read.
    // 8 args, all load-bearing per recursion level; a param struct
    // would be rebuilt at every level of the walk for style points.
    #[allow(clippy::too_many_arguments)]
    fn append_tree_descendants_filtered(
        &self,
        rows: &mut Vec<TreeRowSpec>,
        parent: &Path,
        depth: usize,
        current: &Path,
        skip_paths: Option<&HashSet<PathBuf>>,
        trunk: &mut Vec<bool>,
        cx: &App,
    ) {
        let Some(children) = self.tree_children.get(parent) else {
            return;
        };
        // Resolve visibility up front — last-visible-child status
        // decides between the `├` and `└` connector, so hidden /
        // skipped children must not count.
        let visible: Vec<&TreeChild> = children
            .iter()
            .filter(|child| {
                // `hidden` resolved at load time with platform
                // semantics (FileEntry::hidden contract) — pure flag
                // read on render.
                if !self.show_hidden && child.hidden {
                    return false;
                }
                if let Some(skip) = skip_paths {
                    if skip.contains(&child.path) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let last_ix = visible.len().saturating_sub(1);
        let favs = self.process.favorites.read(cx);
        for (ix, child) in visible.into_iter().enumerate() {
            let is_last = ix == last_ix;
            let is_expanded = self.expanded.contains(&child.path);
            let favorited = favs.contains_path(&child.path);
            let mut guides: Vec<TreeGuide> = trunk
                .iter()
                .map(|&continues| {
                    if continues {
                        TreeGuide::Vertical
                    } else {
                        TreeGuide::Blank
                    }
                })
                .collect();
            guides.push(if is_last {
                TreeGuide::Corner
            } else {
                TreeGuide::Tee
            });
            rows.push(TreeRowSpec {
                node_id: child.node_id,
                path: child.path.clone(),
                label: SharedString::from(child.label.clone()),
                depth,
                guides,
                is_expandable: child.has_subdirs,
                is_expanded,
                is_active: child.path == current,
                capacity: None,
                icon: TreeRowIcon::Folder,
                favorited,
                ejectable: false,
            });
            if is_expanded {
                trunk.push(!is_last);
                self.append_tree_descendants(rows, &child.path, depth + 1, current, trunk, cx);
                trunk.pop();
            }
        }
    }

    /// Either the file Table, or an inline error/empty state when
    /// the directory couldn't be listed (typically macOS TCC denial
    /// on ~/Documents, ~/Desktop, ~/Downloads in a sandboxed runner).
    fn file_pane_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(err) = self.active_tab().last_error.clone() {
            let copy = error_copy(&err);
            let mut pane = v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .p_8()
                .child(
                    div()
                        .text_scale_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child(copy.title),
                )
                .child(
                    div()
                        .max_w(px(420.0))
                        .text_scale_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(copy.body),
                );
            // Only the link itself is clickable, rendered as a
            // separate affordance below the prose body.
            if let Some((label, url)) = copy.link {
                pane = pane.child(
                    div()
                        .id("file-pane-error-settings-link")
                        .text_scale_sm()
                        .text_color(cx.theme().primary)
                        .cursor_pointer()
                        .underline()
                        .child(label)
                        .on_click(move |_: &ClickEvent, window, cx| {
                            use gpui_component::notification::Notification;
                            // Put Feraille's own path on the clipboard so
                            // the user can paste it into the Full Disk
                            // Access "+" sheet via Go to Folder.
                            if let Some(path) = crate::platform_shell::app_bundle_path() {
                                cx.write_to_clipboard(ClipboardItem::new_string(path));
                                window.push_notification(
                                    Notification::info(
                                        "Feraille's path is copied. In the picker, click \
                                         \"+\", press \u{2318}\u{21e7}G, paste, and add it.",
                                    )
                                    .autohide(false),
                                    cx,
                                );
                            }
                            // LaunchServices resolution can stall —
                            // worker, not UI thread (Prime Directive).
                            cx.background_spawn(async move {
                                crate::platform_shell::open_url(url);
                            })
                            .detach();
                        }),
                );
            }
            return pane.into_any_element();
        }
        // A tool result may own the pane body. Search and grouped-rows
        // duplicates still use the normal table/grid path; the dedicated
        // duplicate card panel replaces it.
        if let Some(dm) = self
            .active_tab()
            .tool_result
            .as_ref()
            .and_then(|surface| surface.dupe_mode())
        {
            if dm.presentation == crate::feature_settings::DupePresentation::Panel {
                return self.dupe_panel_body(cx);
            }
        }
        if let Some(super::tab::ToolResultMode::DiskUsage(du)) = self
            .active_tab()
            .tool_result
            .as_ref()
            .map(|surface| &surface.mode)
        {
            return du.view.clone().into_any_element();
        }
        if let Some(super::tab::ToolResultMode::Archive(am)) = self
            .active_tab()
            .tool_result
            .as_ref()
            .map(|surface| &surface.mode)
        {
            return am.view.clone().into_any_element();
        }
        match self.active_tab().view_mode {
            crate::grid::ViewMode::List => DataTable::new(&self.active_tab().table)
                .bordered(false)
                .stripe(true)
                .small()
                .into_any_element(),
            crate::grid::ViewMode::Grid => self.grid_body(cx),
        }
    }

    /// Icon (grid) view of the active tab's listing. A virtualized
    /// `uniform_list` of `cols`-wide rows, reading the same delegate
    /// `entries` + selection mirror the table does and routing every
    /// gesture through the same `Shell` methods. See `crate::grid`.
    fn grid_body(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::file_list::{DragBadge, GHOST_STACK_CAP};
        use crate::thumbnails::THUMB_PX;
        use feraille_core::EntryKind;
        use gpui::ExternalPaths;
        use gpui_component::menu::ContextMenuExt as _;
        use smallvec::SmallVec;
        use std::sync::Arc;

        let icon_px = crate::grid::icon_size(cx);
        let slot = icon_px as f32;
        let bucket = crate::grid::thumb_bucket(icon_px);
        let icon_bucket = crate::grid::folder_icon_bucket(icon_px);
        let show_thumbs = crate::thumbnails::show_thumbnails(cx);
        let gap = crate::grid::cell_gap(cx);
        let cell_w = crate::grid::cell_width(icon_px, gap);
        let cell_h = crate::grid::cell_height(icon_px, gap);

        let pane_w = f32::from(self.active_tab().grid_pane_width).max(cell_w);
        let cols = crate::grid::cols_per_row(pane_w, icon_px, gap);
        let entries_len = self.active_tab().table.read(cx).delegate().entries.len();
        let row_count = entries_len.div_ceil(cols);

        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // Grid-cell hover wash — the icon grid was the one surface with no
        // hover feedback at all. Reuse the list's `table_hover` token so
        // hovering a cell reads the same as hovering a list row.
        let hover_bg = theme.table_hover;
        // Finder-style blue selection. The default theme's `accent` is a
        // near-white gray — fine for hover, invisible as a selection on a
        // busy thumbnail grid — so we key selection off the shared
        // selection accent (`theme.blue` unless the user overrode it):
        // a solid pill behind the label plus a light-blue wash and border
        // on the cell. Lead is full-strength; other members of a
        // multi-selection get a slightly lighter pill so the focused item
        // still stands out. Border width stays 1px everywhere so selection
        // never nudges cell layout by a pixel. The list pane reads the
        // same `selection_colors` helpers, so the two views match.
        let blue = crate::selection_colors::strong(cx);
        let pill_fg = crate::selection_colors::text(cx);
        let sel_bg = crate::selection_colors::fill(cx);
        let sel_border = crate::selection_colors::border(cx);
        // Ant Trail base tint + master switch, read once outside the
        // per-cell loop (the cell `.when` closures can't reach `cx`).
        // See `crate::ant_trail`.
        let ant_base = crate::ant_trail::base(cx);
        let ant_enabled = crate::ant_trail::enabled(cx);
        // Favorite-star tint (mirrors the list row's `theme.primary`)
        // and the gate for the crowding-prone adornments at small sizes.
        let star_color = theme.primary;
        let adorn_visible = icon_px >= crate::grid::ADORN_MIN_ICON;

        let weak = cx.weak_entity();
        let scroll = self.active_tab().grid_scroll.clone();
        let grid_focus = self.active_tab().grid_focus.clone();
        let tab_id = self.active_tab().id;

        let list = uniform_list("file-grid", row_count, move |row_range, _window, app| {
            // Render-only guard: nothing in here touches I/O, but it
            // keeps icon/thumbnail caches on their non-blocking path.
            let _guard = feraille_core::path_guard::enter_render();
            let Some(shell_ent) = weak.upgrade() else {
                return Vec::new();
            };
            let shell = shell_ent.read(app);
            let tab = shell.active_tab();
            let table = tab.table.read(app);
            let del = table.delegate();
            let entries = &del.entries;
            let n = entries.len();
            let thumbs = shell.process.thumbnails.clone();
            let icons = shell.process.icons.clone();

            // Selection drag payload, built ONCE per visible-range
            // render and shared by every selected cell — the previous
            // per-cell walk over ALL entries made a large selection
            // quadratic per pass. (List rows use the delegate's cached
            // DragSnapshot; this closure only holds a read borrow, so
            // it hoists instead.)
            let show_thumbs_for_drag = show_thumbs;
            let sel_drag: Option<(
                Rc<Vec<PathBuf>>,
                SmallVec<[Arc<gpui::RenderImage>; GHOST_STACK_CAP]>,
                SmallVec<[SharedString; GHOST_STACK_CAP]>,
            )> = if del.selected_set.is_empty() {
                None
            } else {
                let mut paths: Vec<PathBuf> = Vec::with_capacity(del.selected_set.len());
                let mut gicons: SmallVec<[Arc<gpui::RenderImage>; GHOST_STACK_CAP]> =
                    SmallVec::new();
                for e in entries.iter() {
                    if !del.selected_set.contains(&e.id) {
                        continue;
                    }
                    let Some(p) = del.path_for_entry(e.id) else {
                        continue;
                    };
                    if gicons.len() < GHOST_STACK_CAP {
                        let thumb = if show_thumbs_for_drag {
                            thumbs.borrow().get(&p, THUMB_PX)
                        } else {
                            None
                        };
                        match thumb {
                            Some(t) => gicons.push(t),
                            None => gicons.push(icons.borrow_mut().icon_for(e, &p)),
                        }
                    }
                    paths.push(p);
                }
                let names: SmallVec<[SharedString; GHOST_STACK_CAP]> = paths
                    .iter()
                    .take(GHOST_STACK_CAP)
                    .map(|p| {
                        p.file_name()
                            .map(|n| {
                                feraille_fs_native::paths::display_leaf(
                                    n.to_string_lossy().as_ref(),
                                )
                                .into_owned()
                            })
                            .unwrap_or_default()
                            .into()
                    })
                    .collect();
                Some((Rc::new(paths), gicons, names))
            };

            let (row_lo, row_hi) = (row_range.start, row_range.end);
            let mut out: Vec<gpui::AnyElement> = Vec::with_capacity(row_range.len());
            for grid_row in row_range {
                let mut row_el = h_flex().w_full().gap_0().px_2();
                let start = grid_row * cols;
                for c in 0..cols {
                    let i = start + c;
                    if i >= n {
                        break;
                    }
                    let entry = &entries[i];
                    let id = entry.id;
                    let path = del.path_for_entry(id).unwrap_or_default();
                    let selected = del.selected_set.contains(&id);
                    let is_lead = del.lead == Some(id);
                    let quarantined = entry.is_quarantined;
                    // Display leaf (macOS `:` → `/`) for the grid label/tooltip;
                    // deceptive names get the same highlighted treatment as the
                    // list row so switching to grid view never hides a disguise.
                    let name = entry.display_name.clone();
                    let tooltip_name: SharedString = name.clone().into();
                    let grid_label: AnyElement = if entry.name_has_hazards {
                        crate::entry_info::name_hazard_element(
                            &name,
                            SharedString::from(format!("grid-name-{i}")),
                        )
                    } else {
                        SharedString::from(name.clone()).into_any_element()
                    };

                    // Per-cell adornments, read from the same parallel
                    // delegate vecs the list row consumes (see
                    // `file_list::render_td`). All render-only lookups.
                    let cell_is_dir = matches!(entry.kind, EntryKind::Directory);
                    // Ant Trail heat tint — directories only, warm orange
                    // scaled by visit heat (matches file_list.rs heat tint).
                    let heat = del.heats.get(i).copied().unwrap_or(0.0);
                    // Cut (Cmd+X) cells dim until the move pastes.
                    let is_cut = del.cut_marker.borrow().iter().any(|c| c == &path);
                    // §5 favorite star — folder cells whose path is in the
                    // favorites index. Star is crowding-prone, so gated.
                    let show_star = adorn_visible
                        && cell_is_dir
                        && del.is_favorited.get(i).copied().unwrap_or(false);
                    // Finder colour tags → coloured dots, capped at 7.
                    let cell_tags: SmallVec<[gpui::Rgba; 7]> = if adorn_visible {
                        del.tags
                            .get(i)
                            .map(|ts| {
                                ts.iter()
                                    .take(7)
                                    .map(|c| crate::file_list::tag_color_rgba(*c))
                                    .collect()
                            })
                            .unwrap_or_default()
                    } else {
                        SmallVec::new()
                    };

                    // Thumbnail when ready + enabled, else type icon.
                    let image: gpui::AnyElement = match entry.kind {
                        EntryKind::Directory => {
                            // Crisp grid-sized NSWorkspace icon once warmed;
                            // the small list icon as an instant placeholder.
                            // (Resolve the borrow before the fallback so we
                            // don't hold `borrow()` across `borrow_mut()`.)
                            let cached = icons.borrow().get_folder_icon_sized(&path, icon_bucket);
                            let ic =
                                cached.unwrap_or_else(|| icons.borrow_mut().icon_for(entry, &path));
                            if icons.borrow().is_blank(&ic) {
                                // Platform icon bridge still a stub (Linux
                                // scaffold, AROS): Lucide folder glyph, same
                                // sizing as the file-side SVG fallback.
                                let fi = crate::icons::file_type_icon(entry);
                                let tint = crate::icons::tint_color(fi.tint, app);
                                svg()
                                    .path(fi.path)
                                    .w(px(slot * 0.72))
                                    .h(px(slot * 0.72))
                                    .text_color(tint)
                                    .into_any_element()
                            } else {
                                img(ic).max_w(px(slot)).max_h(px(slot)).into_any_element()
                            }
                        }
                        EntryKind::File | EntryKind::Symlink => {
                            let thumb = if show_thumbs && crate::thumbnails::is_thumbnailable(entry)
                            {
                                // `get_best`: show the crisp bucket once ready,
                                // else a smaller cached tier (the low-res
                                // preview, or a size warmed at another zoom) as
                                // an instant stand-in that sharpens in place.
                                thumbs.borrow().get_best(&path, bucket)
                            } else {
                                None
                            };
                            if let Some(t) = thumb {
                                img(t).max_w(px(slot)).max_h(px(slot)).into_any_element()
                            } else {
                                let fi = crate::icons::file_type_icon(entry);
                                let tint = crate::icons::tint_color(fi.tint, app);
                                svg()
                                    .path(fi.path)
                                    .w(px(slot * 0.72))
                                    .h(px(slot * 0.72))
                                    .text_color(tint)
                                    .into_any_element()
                            }
                        }
                    };

                    // OS drag-out ghost (dnd-spec §3.1, mirrors the list
                    // row): pressing a selected cell drags the whole
                    // selection (shared payload hoisted above); an
                    // unselected cell drags just itself. Ghost images
                    // come only from already-warm caches, so building
                    // them never touches the filesystem.
                    let is_dir = matches!(entry.kind, EntryKind::Directory);
                    let (drag_paths, ghost_icons, ghost_names): (
                        SmallVec<[PathBuf; 2]>,
                        SmallVec<[Arc<gpui::RenderImage>; GHOST_STACK_CAP]>,
                        SmallVec<[SharedString; GHOST_STACK_CAP]>,
                    ) = if selected {
                        match &sel_drag {
                            Some((paths, gi, gn)) => {
                                (SmallVec::from_vec((**paths).clone()), gi.clone(), gn.clone())
                            }
                            None => (SmallVec::new(), SmallVec::new(), SmallVec::new()),
                        }
                    } else {
                        let mut gi: SmallVec<[Arc<gpui::RenderImage>; GHOST_STACK_CAP]> =
                            SmallVec::new();
                        let thumb = if show_thumbs {
                            thumbs.borrow().get(&path, THUMB_PX)
                        } else {
                            None
                        };
                        match thumb {
                            Some(t) => gi.push(t),
                            None => gi.push(icons.borrow_mut().icon_for(entry, &path)),
                        }
                        let name: SharedString = path
                            .file_name()
                            .map(|n| {
                                // Drag chip shows the display leaf (macOS `:` → `/`).
                                feraille_fs_native::paths::display_leaf(
                                    n.to_string_lossy().as_ref(),
                                )
                                .into_owned()
                            })
                            .unwrap_or_default()
                            .into();
                        (
                            SmallVec::from_vec(vec![path.clone()]),
                            gi,
                            SmallVec::from_vec(vec![name]),
                        )
                    };
                    let drag_count = drag_paths.len();
                    let can_drag = !drag_paths.is_empty();
                    // Finder-style selection pill behind the label: full
                    // accent on the focused (lead) cell, slightly muted
                    // for other members of a multi-selection.
                    let label_pill = if is_lead { blue } else { blue.opacity(0.82) };

                    let weak_cell = weak.clone();
                    let weak_menu = weak.clone();
                    let weak_drop = weak.clone();
                    let weak_hover = weak.clone();
                    let inner = v_flex()
                        .id(("grid-cell-inner", i))
                        .size_full()
                        .items_center()
                        .justify_start()
                        .gap(px(1.0))
                        .p_1()
                        .rounded(px(6.0))
                        // Hover wash on unselected cells (selection bg wins
                        // when both apply) — the grid's missing hover state.
                        .when(!selected, |d| d.hover(|s| s.bg(hover_bg)))
                        // Ant Trail heat tint behind unselected directory
                        // cells (selection bg wins when both apply). Stable
                        // warm hue across themes — same recipe as the row.
                        .when(ant_enabled && !selected && cell_is_dir && heat > 0.0, |d| {
                            d.bg(crate::ant_trail::tint(ant_base, heat))
                        })
                        // Keep border width constant (border_1 everywhere) so
                        // selection never nudges cell layout by a pixel.
                        .when(selected, |d| d.bg(sel_bg))
                        .when(is_lead, |d| d.border_1().border_color(blue))
                        .when(selected && !is_lead, |d| {
                            d.border_1().border_color(sel_border)
                        })
                        .when(!selected && !is_lead, |d| {
                            d.border_1().border_color(gpui::transparent_black())
                        })
                        .child(
                            div()
                                .relative()
                                .flex_shrink_0()
                                .w(px(slot))
                                .h(px(slot))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(image)
                                .when(quarantined, crate::file_list::badge_overlay)
                                // Favorite star, top-left corner of the slot
                                // (quarantine badge owns top-right). Overlaid
                                // rather than inline so it never reflows the
                                // cell; gated to larger icons by `show_star`.
                                .when(show_star, |d| {
                                    d.child(
                                        svg()
                                            .absolute()
                                            .top(px(-1.0))
                                            .left(px(-1.0))
                                            .w(px(13.0))
                                            .h(px(13.0))
                                            .path("icons/nav/star.svg")
                                            .text_color(star_color),
                                    )
                                })
                                // Finder tag dots, centered along the slot's
                                // bottom edge — also overlaid for layout
                                // stability. Empty (and skipped) below the
                                // adornment size threshold.
                                .when(!cell_tags.is_empty(), |d| {
                                    let mut dots = h_flex()
                                        .absolute()
                                        .bottom(px(2.0))
                                        .left_0()
                                        .right_0()
                                        .justify_center()
                                        .gap_1();
                                    for color in cell_tags.iter() {
                                        dots = dots.child(
                                            div().w(px(6.0)).h(px(6.0)).rounded_full().bg(*color),
                                        );
                                    }
                                    d.child(dots)
                                }),
                        )
                        .child(
                            div()
                                .max_w_full()
                                .px(px(5.0))
                                .py(px(1.0))
                                .rounded(px(4.0))
                                .text_scale_xs()
                                .text_center()
                                .truncate()
                                .when(selected, |d| d.bg(label_pill).text_color(pill_fg))
                                .when(!selected, |d| d.text_color(muted))
                                .child(grid_label),
                        );
                    // Wrap the highlighted content in a fixed-size cell box
                    // and inset it with uniform padding, so adjacent
                    // selection fills/borders never touch. The gutter lives
                    // *inside* the cell footprint — the row still strides by
                    // `cell_width`, so column count is unchanged. Interaction
                    // (click / drag / menu / tooltip) lives on this outer box
                    // so the whole cell, gutter included, stays hittable.
                    let cell = div()
                        .id(("grid-cell", i))
                        .w(px(cell_w))
                        .h(px(cell_h))
                        .p(px(gap))
                        .flex()
                        .cursor_pointer()
                        // Cut cells dim until the move pastes (mirrors the
                        // dimmed list row).
                        .when(is_cut, |d| d.opacity(0.45))
                        .child(inner)
                        // The label is `.truncate()`d, so surface the full
                        // name on hover (mirrors the list row's tooltip).
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(tooltip_name.clone())
                                .build(window, cx)
                        })
                        .on_click(move |ev: &ClickEvent, window, app| {
                            let mods = ev.modifiers();
                            let dbl = ev.click_count() >= 2;
                            let _ = weak_cell.update(app, |this, cx| {
                                window.focus(&this.active_tab().grid_focus, cx);
                                if dbl {
                                    this.activate_row(i, cx);
                                } else {
                                    this.apply_row_click_gesture(i, mods, cx);
                                }
                            });
                        })
                        .when(can_drag, |d| {
                            d.on_drag(
                                ExternalPaths(drag_paths),
                                move |_paths, offset, _window, cx| {
                                    cx.new(|_| DragBadge {
                                        names: ghost_names.clone(),
                                        icons: ghost_icons.clone(),
                                        count: drag_count,
                                        offset,
                                    })
                                },
                            )
                        })
                        .when(is_dir, |d| {
                            // Folder cells are OS-drag drop targets (accent
                            // ring on hover) and spring-load: dwelling a drag
                            // over one drills into it. Stop propagation so the
                            // pane-background target underneath doesn't also
                            // fire. Transfer + dwell logic is shared with the
                            // list row via the Shell helpers.
                            d.drag_over::<ExternalPaths>(move |style, _payload, _window, _cx| {
                                style.border_1().border_color(blue).bg(blue.opacity(0.12))
                            })
                            .on_drop(move |paths: &ExternalPaths, window, app| {
                                app.stop_propagation();
                                let dropped = paths.paths().to_vec();
                                let _ = weak_drop.update(app, |this, cx| {
                                    this.drop_onto_folder_row(i, dropped, window, cx);
                                });
                            })
                            .on_drag_move(
                                move |e: &gpui::DragMoveEvent<ExternalPaths>, _window, app| {
                                    if e.bounds.contains(&e.event.position) {
                                        let _ = weak_hover.update(app, |this, cx| {
                                            this.spring_load_hover(i, cx);
                                        });
                                    }
                                },
                            )
                        })
                        .context_menu(move |menu, window, cx| {
                            // Same right-click menu the table uses, reached
                            // through the shared TableState delegate — so
                            // icons mode gets Rename, Open With, tags, Trash,
                            // everything the list row has, from one menu
                            // definition. Mirrors TableEvent::RightClickedRow:
                            // select the cell (unless it's already inside the
                            // selection) and stash context_row so the menu's
                            // actions (Rename included) target it.
                            use crate::multi_table::TableDelegate as _;
                            let Some(shell_ent) = weak_menu.upgrade() else {
                                return menu;
                            };
                            shell_ent.update(cx, |this, cx| {
                                let was_selected = this
                                    .node_id_at_row(i, cx)
                                    .map(|id| this.active_tab().selection.contains(&id))
                                    .unwrap_or(false);
                                this.apply_row_right_click(i, cx);
                                this.context_row = if was_selected { None } else { Some(i) };
                                // Same target-set staging as the list
                                // path so the grid menu gates bulk
                                // commands on the whole selection.
                                this.push_menu_targets(i, cx);
                                let table = this.active_tab().table.clone();
                                table.update(cx, |tbl, cx| {
                                    tbl.delegate_mut().context_menu(i, menu, window, cx)
                                })
                            })
                        });
                    row_el = row_el.child(cell);
                }
                out.push(row_el.into_any_element());
            }

            // Warm the rows uniform_list is actually rendering. This
            // closure re-runs on every scroll (grid_body does not), so
            // warming here is what keeps thumbnails loading as the user
            // scrolls. Deferred to run after this paint, on the Shell
            // entity (so completion repaints the grid).
            if show_thumbs {
                let entry_start = row_lo.saturating_mul(cols).min(n);
                let entry_end = row_hi.saturating_mul(cols).min(n);
                if entry_end > entry_start {
                    let weak_warm = weak.clone();
                    app.defer(move |app| {
                        let _ = weak_warm.update(app, |this, cx| {
                            this.warm_grid_viewport(
                                entry_start..entry_end,
                                bucket,
                                icon_bucket,
                                cx,
                            );
                        });
                    });
                }
            }
            out
        })
        .track_scroll(&scroll)
        .size_full();

        // Measure the pane width so the next frame's `cols_per_row` is
        // correct. uniform_list needs the row count before layout, so
        // we derive columns from the previous frame's measured width.
        let weak_measure = cx.weak_entity();
        let measure = canvas(
            move |bounds, _window, app| {
                let w = bounds.size.width;
                let origin = bounds.origin;
                let _ = weak_measure.update(app, |this, cx| {
                    if let Some(tab) = this.tabs.iter_mut().find(|t| t.id == tab_id) {
                        // Cache the pane origin every frame (cheap) so marquee
                        // hit-testing maps window→content coords correctly even
                        // after the window moves; width changes still notify.
                        tab.grid_pane_origin = origin;
                        if (f32::from(tab.grid_pane_width) - f32::from(w)).abs() > 0.5 {
                            tab.grid_pane_width = w;
                            cx.notify();
                        }
                    }
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        // Rubber-band rectangle, positioned relative to this `.relative()`
        // root (whose content origin is `grid_pane_origin`). Only shown
        // once the drag has moved past the click threshold.
        let marquee_rect: Option<(Pixels, Pixels, Pixels, Pixels)> = {
            let tab = self.active_tab();
            tab.marquee.as_ref().filter(|m| m.moved).map(|m| {
                let o = tab.grid_pane_origin;
                let l = f32::from(m.start.x).min(f32::from(m.current.x)) - f32::from(o.x);
                let t = f32::from(m.start.y).min(f32::from(m.current.y)) - f32::from(o.y);
                let w = (f32::from(m.start.x) - f32::from(m.current.x)).abs();
                let h = (f32::from(m.start.y) - f32::from(m.current.y)).abs();
                (px(l), px(t), px(w), px(h))
            })
        };

        div()
            .key_context(crate::grid::GRID_CONTEXT)
            .track_focus(&grid_focus)
            .relative()
            .size_full()
            .on_action(cx.listener(Self::on_grid_left))
            .on_action(cx.listener(Self::on_grid_right))
            .on_action(cx.listener(Self::on_grid_up))
            .on_action(cx.listener(Self::on_grid_down))
            .on_action(cx.listener(Self::on_grid_left_extend))
            .on_action(cx.listener(Self::on_grid_right_extend))
            .on_action(cx.listener(Self::on_grid_up_extend))
            .on_action(cx.listener(Self::on_grid_down_extend))
            // Marquee gesture: a press on empty background sweeps a
            // selection rectangle (cell presses are guarded out in the
            // handler so their click/drag still win). `up_out` ends the
            // drag when released past the pane edge.
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(Self::on_grid_marquee_down),
            )
            .on_mouse_move(cx.listener(Self::on_grid_marquee_move))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(Self::on_grid_marquee_up))
            .on_mouse_up_out(gpui::MouseButton::Left, cx.listener(Self::on_grid_marquee_up))
            .child(measure)
            .child(list)
            .when_some(marquee_rect, |d, (l, t, w, h)| {
                d.child(
                    div()
                        .absolute()
                        .left(l)
                        .top(t)
                        .w(w)
                        .h(h)
                        .bg(sel_bg)
                        .border_1()
                        .border_color(blue)
                        .rounded(px(2.0)),
                )
            })
            // Vertical scrollbar overlay, mirroring the preview pane /
            // dupe panel pattern: a 16px strip pinned to the right edge
            // as the last child of this `.relative()` container. Binds
            // to the same `grid_scroll` handle the uniform_list tracks;
            // auto-hides per the theme's `scrollbar_show` (the list
            // view gets its equivalent from the multi_table primitive).
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(16.0))
                    .child(gpui_component::scroll::Scrollbar::vertical(&scroll)),
            )
            .into_any_element()
    }

    /// Switch the active tab's view mode (toolbar switcher). Persists
    /// the choice as the new default for fresh tabs, focuses the grid
    /// and warms its first screen when switching to icons.
    pub(crate) fn set_view_mode(
        &mut self,
        mode: crate::grid::ViewMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_tab_mut().view_mode = mode;
        crate::settings::persist_view_mode(mode.as_str());
        if matches!(mode, crate::grid::ViewMode::Grid) {
            // The grid warms its own visible range on render; just give
            // it keyboard focus and trigger the re-render.
            let handle = self.active_tab().grid_focus.clone();
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// Step the grid icon size one stop along [`crate::grid::ICON_SIZES`]
    /// (toolbar −/＋). Updates the live global so the grid re-lays-out
    /// immediately, and persists the new default.
    fn step_icon_size(&self, delta: i32, cx: &mut Context<Self>) {
        let sizes = crate::grid::ICON_SIZES;
        let cur = crate::grid::icon_size(cx);
        let idx = sizes.iter().position(|&s| s == cur).unwrap_or_else(|| {
            // Not exactly on a stop — start from the nearest.
            let mut best = 0usize;
            let mut best_d = i64::MAX;
            for (i, &s) in sizes.iter().enumerate() {
                let d = (s as i64 - cur as i64).abs();
                if d < best_d {
                    best_d = d;
                    best = i;
                }
            }
            best
        });
        let new_idx = (idx as i32 + delta).clamp(0, sizes.len() as i32 - 1) as usize;
        let new = sizes[new_idx];
        if new != cur {
            cx.set_global(crate::grid::IconSize(new));
            crate::settings::persist_icon_size(new);
            cx.notify();
        }
    }

    /// Tabstrip above the toolbar. Each tab is a clickable pill
    /// labelled with the directory's basename; the active tab has
    /// a filled background. A trailing "+" opens a new tab; when more
    /// than one tab is open each carries a trailing close affordance —
    /// the shared `close.svg` glyph in a rounded hover highlight.
    fn tabstrip(&self, cx: &mut Context<Self>) -> Div {
        use gpui_component::Sizable as _;
        use gpui_component::button::{Button, ButtonVariants as _};

        // One arrow click pages the strip by ~one-and-a-half tab widths.
        const TAB_SCROLL_STEP: Pixels = px(200.0);

        let active = self.active;
        let multi = self.tabs.len() > 1;
        // The chips live in a horizontally scrollable strip so the tab
        // row can hold more tabs than fit the window. Trackpad / shift-
        // wheel scrolling rides `overflow_x_scroll` directly; the chevron
        // arrows added below page it for mouse-only users. A `flex_1`
        // viewport (assembled after the loop) clips the strip so the
        // arrows and trailing "+" stay pinned while the chips scroll
        // under them. See `Shell::scroll_tabs`.
        let mut row = h_flex()
            .id("tabstrip-scroll")
            .items_center()
            .gap_1()
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll);

        for (idx, tab) in self.tabs.iter().enumerate() {
            // Phase D: drop gap *before* this tab. Catches a drag
            // released between the previous chip and this one. The
            // first iteration places gap 0 (before the first tab).
            row = row.child(tab_drop_gap(idx, cx));

            let is_active = idx == active;
            let label = tab.label();
            let tab_id = tab.id;
            let drag_label: SharedString = label.clone().into();
            let theme = cx.theme();
            let accent = theme.primary;
            let mut chip = h_flex()
                .id(("tab", idx))
                .items_center()
                .gap_1()
                .px_3()
                .py_0p5()
                .rounded(theme.radius)
                .cursor_pointer()
                .text_scale_sm()
                .text_color(if is_active {
                    theme.foreground
                } else {
                    theme.muted_foreground
                });
            if is_active {
                chip = chip.bg(theme.background);
            } else {
                chip = chip.hover(|this| this.bg(theme.accent.opacity(0.10)));
            }
            chip = chip
                .child(div().truncate().max_w(px(160.0)).child(label))
                .on_click(cx.listener(move |this, _, _, cx| {
                    // Resolve by TabId, not the render-time `idx` —
                    // same staleness rule as the close button: a
                    // drag-reorder can shift positions between the
                    // frame this listener was built and the click.
                    if let Some(target_idx) = this.tabs.iter().position(|t| t.id == tab_id) {
                        this.select_tab(target_idx, cx);
                    }
                }))
                // Phase D drag-start: a press-then-move on a tab chip
                // initiates a tab drag (gpui's built-in threshold keeps
                // plain clicks from triggering this). The payload
                // carries the source `TabId` so the drop handler can
                // resolve the current index even if the strip changed
                // between drag-start and drop. Spec §5.4: tab drags
                // and node drags are distinguished by origin surface
                // — this is the tab-strip origin.
                .on_drag(
                    TabDragPayload {
                        id: tab_id,
                        label: drag_label,
                        from_idx: idx,
                    },
                    |payload, _offset, _window, cx| cx.new(|_| payload.clone()),
                )
                // The chip is also a drop TARGET. The between-chip gaps
                // are only 6 DIP wide — without this, the natural
                // gesture (release over another tab) lands on the chip
                // and silently does nothing. Dropping on a chip puts
                // the dragged tab in that chip's slot; the accent edge
                // shows which side the insertion happens on.
                .drag_over::<TabDragPayload>(move |style, payload, _window, _cx| {
                    if payload.from_idx == idx {
                        style
                    } else if payload.from_idx < idx {
                        style.border_r_2().border_color(accent)
                    } else {
                        style.border_l_2().border_color(accent)
                    }
                })
                .on_drop(
                    cx.listener(move |this, payload: &TabDragPayload, _window, cx| {
                        // Resolve BOTH ends by TabId at drop time —
                        // same staleness rule as click/close.
                        let (Some(from_idx), Some(chip_idx)) = (
                            this.tabs.iter().position(|t| t.id == payload.id),
                            this.tabs.iter().position(|t| t.id == tab_id),
                        ) else {
                            return;
                        };
                        let gap = tab::chip_drop_gap_index(from_idx, chip_idx);
                        this.reorder_tab(payload.id, gap, cx);
                    }),
                )
                // Files dropped on a tab chip transfer into THAT tab's
                // folder (resolved by TabId at drop), so you can move or
                // copy into a background tab without switching to it.
                .drag_over::<ExternalPaths>(move |style, _payload, _window, _cx| {
                    style.bg(accent.opacity(0.18))
                })
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                    let Some(dest) = this
                        .tabs
                        .iter()
                        .find(|t| t.id == tab_id)
                        .map(|t| t.current_dir.clone())
                    else {
                        return;
                    };
                    this.handle_external_drop(paths.paths().to_vec(), dest, window, cx);
                }));
            if multi {
                let close = div()
                    .id(("tab-close", idx))
                    .ml_0p5()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(14.0))
                    .rounded(theme.radius)
                    // Subtle close affordance: muted grey by default, darkening
                    // to foreground on hover (plus a rounded highlight) — the
                    // common tab-close convention.
                    .hover(|this| this.bg(theme.accent.opacity(0.15)))
                    // Color set ON the svg, not just the parent div: the AROS
                    // CPU renderer tints an SVG monochrome sprite from the
                    // svg element's own text color and does NOT pick up the
                    // parent's cascaded text color, so the inherited version
                    // rendered a near-black X on the dark tab (invisible).
                    .child(
                        svg()
                            .path("icons/close.svg")
                            .icon_px(11.0)
                            .text_color(theme.muted_foreground),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        // Phase A+B+C: tabs own their own TableState,
                        // and closing the last tab closes the window
                        // (process stays resident).
                        // Phase D: snapshot before removing so
                        // Cmd+Shift+T can reopen this tab. The lookup
                        // is by TabId, not the captured `idx`, because
                        // a drag-reorder may have shifted positions
                        // since this listener was constructed.
                        let Some(target_idx) = this.tabs.iter().position(|t| t.id == tab_id) else {
                            return;
                        };
                        if this.tabs.len() <= 1 {
                            this.dismiss_tab_work(&this.tabs[target_idx]);
                            this.process
                                .push_closed_tab(this.tabs[target_idx].snapshot_for_close());
                            window.remove_window();
                            return;
                        }
                        this.dismiss_tab_work(&this.tabs[target_idx]);
                        this.process
                            .push_closed_tab(this.tabs[target_idx].snapshot_for_close());
                        this.tabs.remove(target_idx);
                        // Adjust the active index relative to the closed
                        // tab's position. Closing a tab to the left of
                        // the active one shifts the active tab's index
                        // down by 1; closing the active tab leaves the
                        // index pointing at what was its right neighbor
                        // (or clamps if it was the rightmost tab);
                        // closing a tab to the right of active is a
                        // no-op for the index.
                        if target_idx < this.active {
                            this.active -= 1;
                        } else if this.active >= this.tabs.len() {
                            this.active = this.tabs.len() - 1;
                        }
                        cx.notify();
                    }));
                chip = chip.child(close);
            }
            row = row.child(chip);
        }
        // Phase D: trailing drop gap after the last tab. Stays inside
        // the scrollable strip so a drop past the last chip still lands.
        row = row.child(tab_drop_gap(self.tabs.len(), cx));

        // Scroll state measured last frame, used to drive the arrows.
        // `max_offset().x > 0` means the chips overflow the viewport, so
        // the arrows are worth showing; `offset().x` runs from 0 (start)
        // down to `-max` (end), the same convention as the preview scroll
        // (see `on_preview_text_scroll`). A small epsilon absorbs float
        // fuzz when deciding whether either end is reachable.
        let off = self.tab_scroll.offset().x;
        let max = self.tab_scroll.max_offset().x;
        let overflow = max > px(0.5);
        let can_left = off < px(-0.5);
        let can_right = off > -max + px(0.5);

        let theme = cx.theme();
        let mut outer = h_flex()
            .w_full()
            .items_center()
            .gap_1()
            .px_2()
            .py_0p5()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary);

        // Left arrow — only while the strip overflows; dimmed once
        // scrolled fully to the start.
        if overflow {
            outer = outer.child(
                Button::new("tab-scroll-left")
                    .xsmall()
                    .ghost()
                    .icon(gpui_component::Icon::empty().path("icons/nav/chevrons-left.svg"))
                    .tooltip("Scroll tabs left")
                    .disabled(!can_left)
                    .on_click(cx.listener(|this, _, _, cx| this.scroll_tabs(TAB_SCROLL_STEP, cx))),
            );
        }

        // The scrollable strip, clipped to the remaining width so the
        // arrows + "+" stay pinned to the row's right edge.
        outer = outer.child(h_flex().flex_1().overflow_x_hidden().child(row));

        // Right arrow — mirror of the left; dimmed once at the end.
        if overflow {
            outer = outer.child(
                Button::new("tab-scroll-right")
                    .xsmall()
                    .ghost()
                    .icon(gpui_component::Icon::empty().path("icons/nav/chevrons-right.svg"))
                    .tooltip("Scroll tabs right")
                    .disabled(!can_right)
                    .on_click(cx.listener(|this, _, _, cx| this.scroll_tabs(-TAB_SCROLL_STEP, cx))),
            );
        }

        // Trailing "+" — new tab. Pinned outside the scroll viewport.
        // House-style `nav/plus.svg` (Lucide outline, stroke 1.75) so the
        // affordance matches the sidebar icon family instead of a thin font
        // glyph; sized through IconScale to zoom with the UI.
        outer = outer.child(
            div()
                .id("tab-new")
                .flex()
                .items_center()
                .justify_center()
                .ml_1()
                .px_2()
                .py_0p5()
                .rounded(theme.radius)
                .cursor_pointer()
                .hover(|this| this.bg(theme.accent.opacity(0.10)))
                .child(
                    svg()
                        .path("icons/nav/plus.svg")
                        .icon_px(14.0)
                        // Solid foreground (the active-tab label colour), not
                        // muted grey, so the new-tab affordance reads clearly.
                        .text_color(theme.foreground),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    // Spec §4.3: new tab opens beside the active tab,
                    // at the active tab's directory.
                    let path = this.active_tab().current_dir.clone();
                    let id = this.process.fs.id_for_path(&path);
                    this.process
                        .node_store
                        .borrow_mut()
                        .get_or_create_path_with_id(path.clone(), id);
                    let tab = this.make_tab(path.clone(), id, window, cx);
                    let insert_at = this.active + 1;
                    this.tabs.insert(insert_at, tab);
                    this.active = insert_at;
                    this.load_path(path, cx);
                })),
        );
        outer
    }

    /// Page the tab strip horizontally by `dx` DIPs — positive reveals
    /// tabs toward the start (scroll left), negative toward the end
    /// (scroll right) — clamped to the scrollable range. Drives the
    /// tab-strip chevron arrows; trackpad / shift-wheel scrolling goes
    /// straight through `overflow_x_scroll` and never reaches here. The
    /// offset convention matches the preview scroll: `0` at the start,
    /// `-max_offset().x` at the end.
    fn scroll_tabs(&mut self, dx: Pixels, cx: &mut Context<Self>) {
        let max = self.tab_scroll.max_offset().x;
        let cur = self.tab_scroll.offset();
        let x = (cur.x + dx).clamp(-max, px(0.0));
        self.tab_scroll.set_offset(point(x, cur.y));
        cx.notify();
    }

    // Toolbar removed in Phase 7. Back / forward / filter went into
    // the TitleBar; Show Hidden moved into the status bar; nothing
    // useful was left to render between the tabstrip and the
    // breadcrumb. Future density items (Refresh, New Folder, Sort,
    // Group, Overflow) will reintroduce a toolbar when they land.

    /// TitleBar built from elements that used to live in the sidebar
    /// header + toolbar. Layout (left → right):
    ///   • "Feraille" name
    ///   • Back / forward navigation (history nav lives next to the
    ///     brand — Finder convention)
    ///   • flex spacer
    ///   • Filter `Input` (~half its previous width, centred-ish via
    ///     the trailing flex spacer)
    ///   • trailing space so the right edge isn't crowded
    ///
    /// Show-Hidden moved out of here entirely and lives in the
    /// status bar now (paired with the item count, where view-mode
    /// state belongs).
    fn title_bar(&self, cx: &mut Context<Self>) -> TitleBar {
        use crate::file_list::SortColumn;
        use gpui_component::menu::DropdownMenu;
        use gpui_component::sidebar::SidebarToggleButton;
        let can_back = self.active_tab().history_index > 0;
        let can_forward = self.active_tab().history_index + 1 < self.active_tab().history.len();
        let collapsed = self.sidebar_collapsed;
        // Active sort drives the sort button's glyph (asc/descending)
        // and the checkmark in its menu. Read once here — render-time
        // cache read, no I/O.
        let current_sort = self
            .process
            .list_sort
            .get()
            .unwrap_or((SortColumn::Name, true));
        let sort_col = current_sort.0;
        let sort_asc = current_sort.1;
        let sort_icon = if sort_asc {
            "icons/sort-ascending.svg"
        } else {
            "icons/sort-descending.svg"
        };
        let show_hidden = self.show_hidden;
        // Show Desktop is a private-symbol feature: the button only
        // exists when `feraille-shell-mac` resolved the Dock notification
        // on a supported macOS. Cached after first resolve, so this is a
        // cheap render-time read (prewarmed at startup).
        let show_desktop_available = crate::platform_shell::show_desktop_available();
        // View switcher + grid size stepper state.
        let view_mode = self.active_tab().view_mode;
        let is_grid = matches!(view_mode, crate::grid::ViewMode::Grid);
        // SHELL_CONTEXT-bearing handle for the toolbar dropdowns, so
        // their items resolve keyboard-shortcut hints against the
        // shell's stable dispatch path instead of the focus-sensitive
        // previous-frame fallback (which left the hints blank for the
        // first frame or two after the menu opened).
        let sort_menu_focus = self.focus_handle.clone();
        let overflow_menu_focus = self.focus_handle.clone();
        let dock_menu_focus = self.focus_handle.clone();
        // Active dock edge (if any), captured for the toolbar dock menu's
        // checkmarks and pressed state.
        let dock_edge = self.dock.as_ref().map(|d| d.edge);
        TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .pr_3()
                // Sidebar collapse / expand toggle. The SidebarToggle-
                // Button swaps its glyph based on the `collapsed`
                // flag (panel-left-open vs panel-left-close) so the
                // user can read what clicking will do.
                .child(
                    div()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(SidebarToggleButton::new().collapsed(collapsed).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.sidebar_collapsed = !this.sidebar_collapsed;
                                let mut s = app_state::load();
                                s.sidebar_collapsed = Some(this.sidebar_collapsed);
                                app_state::save(&s);
                                cx.notify();
                            }),
                        )),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_scale_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child("Feraille"),
                )
                .child(
                    Button::new("nav-back")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/nav/chevron-left.svg"))
                        .tooltip("Back  \u{2318}\u{5B}")
                        .disabled(!can_back)
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.navigate_back(cx))),
                )
                .child(
                    Button::new("nav-forward")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/nav/chevron-right.svg"))
                        .tooltip("Forward  \u{2318}\u{5D}")
                        .disabled(!can_forward)
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.navigate_forward(cx))),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .flex_shrink_0()
                        .w(px(220.0))
                        // Filter input — also lives inside TitleBar's
                        // drag region. Stop mouse-down propagation so
                        // Win32 doesn't capture the click as window
                        // drag (same bug as the toolbar buttons; see
                        // §Title bar drag capture above).
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(Input::new(&self.active_tab().filter_input).small()),
                )
                .child(div().flex_1())
                // Phase 7 follow-on: density buttons on the right —
                // Refresh and New Folder. Icon-only with tooltips so
                // the bar stays narrow.
                //
                // Title-bar drag capture: on Windows, gpui-component's
                // TitleBar treats its content as a draggable caption
                // region. Without `cx.stop_propagation()` on mouse-down
                // here, Win32's NCHITTEST returns HTCAPTION for the
                // button's screen rect, the OS captures the mouse for
                // window-drag, and the button's mouse-up never fires
                // — leaving the button visually pressed forever and
                // never running the click handler. Matches the pattern
                // `gpui_component::AppMenuBar` uses for its own buttons.
                // Sort dropdown — pick the column (re-pick flips
                // direction); the glyph shows the current direction and
                // the active column carries a checkmark. Wrapped in a
                // mouse-down-stopping div for the Win32 title-bar drag
                // gotcha (the DropdownMenuPopover can't take the
                // on_mouse_down handler itself).
                .child(
                    div()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(
                            Button::new("toolbar-sort")
                                .small()
                                .ghost()
                                .icon(gpui_component::Icon::empty().path(sort_icon))
                                .tooltip("Sort")
                                .dropdown_menu(move |menu, _window, _cx| {
                                    menu.action_context(sort_menu_focus.clone())
                                        .menu_with_check(
                                            "Name",
                                            sort_col == SortColumn::Name,
                                            Box::new(SortByName),
                                        )
                                        .menu_with_check(
                                            "Size",
                                            sort_col == SortColumn::Size,
                                            Box::new(SortBySize),
                                        )
                                        .menu_with_check(
                                            "Kind",
                                            sort_col == SortColumn::Format,
                                            Box::new(SortByKind),
                                        )
                                        .menu_with_check(
                                            "Date Modified",
                                            sort_col == SortColumn::Modified,
                                            Box::new(SortByModified),
                                        )
                                }),
                        ),
                )
                // Show Desktop — left of New Folder. Present only when the
                // private Dock symbol resolved on a supported OS; otherwise
                // it silently doesn't render (no crash, no empty slot).
                .children(show_desktop_available.then(|| {
                    Button::new("toolbar-show-desktop")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/nav/show-desktop.svg"))
                        .tooltip_with_action("Show Desktop", &ShowDesktop, Some(SHELL_CONTEXT))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_show_desktop(&ShowDesktop, window, cx);
                        }))
                }))
                .child(
                    Button::new("toolbar-new-folder")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/nav/folder.svg"))
                        .tooltip_with_action("New Folder", &NewFolder, Some(SHELL_CONTEXT))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_new_folder(&NewFolder, window, cx);
                        })),
                )
                .child(
                    Button::new("toolbar-refresh")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/nav/refresh.svg"))
                        .tooltip_with_action("Refresh", &Refresh, Some(SHELL_CONTEXT))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_refresh(&Refresh, window, cx);
                        })),
                )
                // Dock menu — park the whole window against a screen edge as
                // an auto-hiding drawer (docs/features/DOCK.md). Pressed look
                // while docked; the active edge carries a checkmark. Wrapped
                // in a mouse-down-stopping div for the Win32 title-bar-drag
                // gotcha, like the other dropdowns. macOS-only for now (the
                // win32/linux window primitives are stubs) — hidden elsewhere
                // so the menu isn't three silent no-ops.
                .when(cfg!(target_os = "macos"), |bar| bar.child(
                    div()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(
                            Button::new("toolbar-dock")
                                .small()
                                .ghost()
                                .selected(dock_edge.is_some())
                                .icon(gpui_component::Icon::empty().path("icons/dock.svg"))
                                .tooltip("Dock window to a screen edge")
                                .dropdown_menu(move |menu, _window, _cx| {
                                    menu.action_context(dock_menu_focus.clone())
                                        .menu_with_icon(
                                            "Dock Left",
                                            gpui_component::Icon::empty()
                                                .path("icons/dock-left.svg"),
                                            Box::new(DockLeft),
                                        )
                                        .menu_with_icon(
                                            "Dock Right",
                                            gpui_component::Icon::empty()
                                                .path("icons/dock-right.svg"),
                                            Box::new(DockRight),
                                        )
                                        .separator()
                                        .menu_with_icon(
                                            "Undock",
                                            gpui_component::Icon::empty().path("icons/undock.svg"),
                                            Box::new(Undock),
                                        )
                                }),
                        ),
                ))
                // View switcher: list ⇄ icon grid (per-tab). The active
                // mode's button is highlighted.
                .child(
                    Button::new("toolbar-view-list")
                        .small()
                        .ghost()
                        .selected(!is_grid)
                        .icon(gpui_component::Icon::empty().path("icons/view-list.svg"))
                        .tooltip("List view")
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_view_mode(crate::grid::ViewMode::List, window, cx);
                        })),
                )
                .child(
                    Button::new("toolbar-view-grid")
                        .small()
                        .ghost()
                        .selected(is_grid)
                        .icon(gpui_component::Icon::empty().path("icons/view-grid.svg"))
                        .tooltip("Icon view")
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_view_mode(crate::grid::ViewMode::Grid, window, cx);
                        })),
                )
                // Icon size stepper — only in grid mode.
                .children(is_grid.then(|| {
                    Button::new("toolbar-icon-smaller")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/minus.svg"))
                        .tooltip("Smaller icons")
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.step_icon_size(-1, cx);
                        }))
                }))
                .children(is_grid.then(|| {
                    Button::new("toolbar-icon-larger")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/plus.svg"))
                        .tooltip("Larger icons")
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.step_icon_size(1, cx);
                        }))
                }))
                // Overflow menu — the less-frequent view + action verbs
                // that don't each warrant a toolbar button. Items
                // dispatch existing actions, so they target the current
                // selection / folder exactly like their keyboard and
                // right-click counterparts.
                .child(
                    div()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(
                            Button::new("toolbar-overflow")
                                .small()
                                .ghost()
                                .icon(gpui_component::Icon::empty().path("icons/ellipsis.svg"))
                                .tooltip("More")
                                .dropdown_menu(move |menu, _window, _cx| {
                                    menu.action_context(overflow_menu_focus.clone())
                                        .menu_with_check(
                                            "Show Hidden Files",
                                            show_hidden,
                                            Box::new(ToggleHidden),
                                        )
                                        .separator()
                                        .menu("Get Info", Box::new(GetInfo))
                                        .menu("Open Viewer", Box::new(OpenViewer))
                                        .menu("Disk Usage\u{2026}", Box::new(OpenDiskUsage))
                                        .menu("Find Duplicates\u{2026}", Box::new(FindDuplicates))
                                        .separator()
                                        .menu("Copy File List", Box::new(CopyFileList))
                                        .separator()
                                        .menu("Empty Trash\u{2026}", Box::new(EmptyTrash))
                                }),
                        ),
                ),
        )
    }

    /// Render-safe path resolution for a file-list row's preview.
    ///
    /// Reads the delegate's per-entry `paths` map (a pure in-memory
    /// lookup populated at load for directory, search, AND duplicate
    /// rows) so it works for results views whose files live outside
    /// `current_dir` — without touching the guarded node store, which
    /// would panic on the paint path. Falls back to `current_dir + name`
    /// only when the map has no entry.
    fn resolve_preview_path(&self, entry: &FileEntry, cx: &App) -> PathBuf {
        self.active_tab()
            .table
            .read(cx)
            .delegate()
            .path_for_entry(entry.id)
            .unwrap_or_else(|| {
                let mut p = self.active_tab().current_dir.clone();
                p.push(&entry.name);
                p
            })
    }

    /// Wheel scroll-chaining for the inline text/code box in the preview
    /// pane. The box is a nested scroll inside `preview_scroll`, bounded to
    /// `max_h(280)` on purpose so a long file doesn't bury the Get Info
    /// details below it. Without chaining the wheel drives both scrolls at
    /// once; we want the box to consume the delta and only spill the
    /// remainder into the outer pane.
    ///
    /// `overflow_scroll`'s built-in handler runs just before this one (in the
    /// same bubble pass) and has already added the full wheel delta to
    /// `preview_text_scroll`, unclamped — so `offset()` now sits *past* the
    /// top (positive) or bottom (below `-max_offset`) by exactly the part the
    /// box couldn't use. We forward that residual to `preview_scroll` and
    /// `stop_propagation` so the outer pane's own handler — which would
    /// otherwise apply the *whole* delta and double-scroll — never fires.
    ///
    /// A short file (box not scrollable, `max_offset == 0`) spills the entire
    /// delta straight through, so its box never traps the wheel.
    fn on_preview_text_scroll(
        &mut self,
        _: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let off = self.preview_text_scroll.offset().y;
        let max = self.preview_text_scroll.max_offset().y;
        let residual = if off > px(0.0) {
            off // overshot the top
        } else if off < -max {
            off + max // overshot the bottom
        } else {
            px(0.0) // the box absorbed the whole delta
        };
        if residual != px(0.0) {
            let cur = self.preview_scroll.offset();
            let max_out = self.preview_scroll.max_offset().y;
            let y = (cur.y + residual).clamp(-max_out, px(0.0));
            self.preview_scroll.set_offset(point(cur.x, y));
            cx.notify();
        }
        cx.stop_propagation();
    }

    /// The drag grip under the preview thumbnail box. Dragging it
    /// down/up grows/shrinks the box between `PREVIEW_THUMB_MIN_H`
    /// and `PREVIEW_THUMB_MAX_H`; the height persists via the same
    /// debounced save as the splitter widths. The drag anchor (mouse
    /// y + height at drag start) is snapped in the `on_drag`
    /// constructor; `on_drag_move` then applies the absolute delta,
    /// so the box edge tracks the cursor 1:1 — no per-tick
    /// accumulation drift, no dependence on the pane's scroll offset.
    fn preview_thumb_resize_grip(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let weak = cx.weak_entity();
        div()
            .id("preview-thumb-resize")
            .group("preview-thumb-grip")
            .w_full()
            .h(px(9.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_row_resize()
            .child(
                div()
                    .w(px(48.0))
                    .h(px(3.0))
                    .rounded_full()
                    .bg(cx.theme().border)
                    .group_hover("preview-thumb-grip", |this| this.bg(cx.theme().drag_border)),
            )
            .on_drag(ResizePreviewThumb, move |drag, _offset, window, cx| {
                cx.stop_propagation();
                let y = window.mouse_position().y;
                if let Some(shell) = weak.upgrade() {
                    shell.update(cx, |this, _| {
                        this.preview_thumb_drag = Some((y, this.preview_thumb_h));
                    });
                }
                cx.new(|_| drag.clone())
            })
            .on_drag_move(cx.listener(
                |this, e: &DragMoveEvent<ResizePreviewThumb>, _window, cx| {
                    let Some((y0, h0)) = this.preview_thumb_drag else {
                        return;
                    };
                    let h = (h0 + f32::from(e.event.position.y - y0))
                        .clamp(PREVIEW_THUMB_MIN_H, PREVIEW_THUMB_MAX_H);
                    if h != this.preview_thumb_h {
                        this.preview_thumb_h = h;
                        this.schedule_splitter_save(cx);
                        cx.notify();
                    }
                },
            ))
    }

    /// Build the preview pane on the right of the file list. Shows
    /// title / kind / size / modified / full path of the selected
    /// row. Falls back to a neutral empty state when nothing is
    /// selected. Format-specific previews (image, text, PDF) arrive
    /// in a follow-up polish iter.
    fn preview(&mut self, cx: &mut Context<Self>) -> Div {
        use gpui_component::{
            Sizable as _,
            button::{Button, ButtonVariants as _},
            scroll::Scrollbar,
            tooltip::Tooltip,
        };

        // Preview always reflects the **lead** row, even with a
        // multi-selection. Matches Finder's "the focused one of
        // many" semantics.
        let selected = {
            let entries = &self.active_tab().table.read(cx).delegate().entries;
            self.active_tab()
                .lead_row(entries)
                .and_then(|i| entries.get(i).cloned())
        };

        // Resolve the row's real path from the delegate's per-entry
        // `paths` map — populated at load for directory listings AND for
        // search / duplicate results. It's a pure in-memory lookup, so
        // it's safe on the render path (unlike `path_for_row`, which
        // resolves through the guarded node store). For results views
        // the file lives outside `current_dir`, so the old
        // `current_dir + name` reconstruction keyed the preview cache
        // wrong and the thumbnail / text never appeared; the map has the
        // true path. Fall back to `current_dir + name` only if absent.
        //
        // Scroll position carries across renders (the body scrolls
        // when the window is shorter than the metadata stack), but a
        // different file starts back at the top.
        let selected_path = selected
            .as_ref()
            .map(|entry| self.resolve_preview_path(entry, cx));
        if self.preview_scroll_path != selected_path {
            self.preview_scroll_path = selected_path.clone();
            self.preview_scroll.set_offset(gpui::Point::default());
            self.preview_text_scroll.set_offset(gpui::Point::default());
        }

        let header = div()
            .text_scale_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.theme().muted_foreground)
            .child("Preview");

        let body: AnyElement = match selected {
            None => div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_scale_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No selection")
                .into_any_element(),
            Some(entry) => {
                // Same render-safe resolution as `selected_path` above.
                let full_path = selected_path
                    .clone()
                    .unwrap_or_else(|| self.resolve_preview_path(&entry, cx));

                // Keep the embedded Get Info panel pointed at the lead
                // selection (reuses the popup's view in `embedded` mode).
                let info_target = match entry.kind {
                    EntryKind::Directory => feraille_core::entry_info::InfoTarget::Folder,
                    _ => feraille_core::entry_info::InfoTarget::File,
                };
                // Hand the folder's already-computed recursive size (from the
                // Size column) to Get Info so it reuses it, not rescans.
                let known_size = if matches!(entry.kind, EntryKind::Directory) && entry.size > 0 {
                    Some(entry.size)
                } else {
                    None
                };
                let info_view = self.sync_preview_info(
                    full_path.clone(),
                    entry.name.clone(),
                    info_target,
                    known_size,
                    cx,
                );

                // Quick Look thumbnail (Stage 8 native preview).
                // `preview::request` was kicked off when the row
                // was selected; this just reads whatever the cache
                // has — Loaded shows the bitmap, Pending shows a
                // muted placeholder, Failed shows nothing.
                // Folders have no file preview — show metadata only
                // (no thumbnail/text box). Files get the media block.
                let is_dir = matches!(entry.kind, EntryKind::Directory);
                let thumb_state = if is_dir {
                    None
                } else {
                    self.process.preview_cache.borrow().get(&full_path)
                };
                let thumb_img = crate::preview::loaded_image(thumb_state.clone());
                // Text/code files render their content inline instead
                // of a thumbnail (docs/features/PREVIEW.md).
                let text_body = if is_dir {
                    None
                } else {
                    let text_state = self.process.text_preview_cache.borrow().get(&full_path);
                    crate::text_preview::loaded_text(text_state)
                };

                let mut col = v_flex().gap_3();
                if let Some(text) = text_body {
                    // Render through gpui-component's TextView:
                    // markdown files format, source files highlight
                    // (the worker already capped this to 500 lines, and
                    // TextView parses off the UI thread). The id is keyed
                    // per file (see below) so selection state can't bleed
                    // across previews.
                    //
                    // A bounded box with its own scroll on BOTH axes:
                    // vertical so a long file doesn't push the Get Info
                    // details far down the pane, horizontal so no-wrap code
                    // lines stay readable.
                    //
                    // Wheel scroll-chaining: `overflow_scroll`'s own handler
                    // applies the delta to `preview_text_scroll` first; the
                    // `on_scroll_wheel` below then forwards only what spilled
                    // past the box's top/bottom to the outer `preview_scroll`,
                    // so a long file scrolls the box, then reveals Get Info —
                    // not both at once. `track_scroll` is what makes the box's
                    // offset readable for that math.
                    let block = div()
                        .id(("preview-text", entry.id.as_raw() as usize))
                        .w_full()
                        .max_h(px(280.0))
                        .overflow_scroll()
                        .track_scroll(&self.preview_text_scroll)
                        .on_scroll_wheel(cx.listener(Self::on_preview_text_scroll))
                        .p_2()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().secondary.opacity(0.5))
                        .text_scale_xs();
                    let block = if text.is_empty() {
                        block
                            .font_family("monospace")
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from("(empty file)"))
                    } else {
                        let md = crate::text_preview::to_markdown_source(&entry.name, &text);
                        // Compact mono in code blocks, and don't wrap —
                        // long lines scroll horizontally in the block
                        // above instead of folding.
                        let style = gpui_component::text::TextViewStyle::default().code_block(
                            gpui::StyleRefinement::default()
                                .text_size(px(9.0))
                                .whitespace_nowrap(),
                        );
                        // Per-file element id (keyed on the entry id), not
                        // a constant: a TextView keeps internal selection /
                        // scroll state under its id, so a shared id let a
                        // stale text selection bleed onto the next file you
                        // previewed (it looked "already selected" on hover).
                        // A distinct id per file gives each a clean TextView
                        // at the cost of re-parsing on file switch (cheap —
                        // the worker caps content to 500 lines, off-thread).
                        let view = gpui_component::text::TextView::markdown(
                            ("preview-textview", entry.id.as_raw() as usize),
                            SharedString::from(md),
                        )
                        .style(style)
                        .selectable(true);
                        // Neither preview kind scrolls horizontally on its own
                        // in the narrow pane, so we give the content a definite
                        // width wider than the box and let the box's
                        // `overflow_scroll` reach the rest. `w_full` keeps a
                        // short file filling the pane rather than sitting in an
                        // over-wide box.
                        //
                        //  - Rendered markdown (`.md`) wraps its prose to the
                        //    container width (gpui-component forces
                        //    `whitespace_normal` on paragraphs), folding every
                        //    sentence into a sliver. A fixed reading column
                        //    (PREVIEW_MD_MIN_W) reads well and scrolls when the
                        //    pane is narrower.
                        //  - Code blocks are `whitespace_nowrap`; they clip
                        //    long lines but don't grow their container, so the
                        //    box has nothing to scroll toward. Size to the
                        //    widest line (estimated from its column count) so
                        //    the box can scroll the full line into view.
                        let is_markdown = matches!(
                            std::path::Path::new(&entry.name)
                                .extension()
                                .and_then(|e| e.to_str())
                                .map(|e| e.to_ascii_lowercase())
                                .as_deref(),
                            Some("md" | "markdown" | "mdx")
                        );
                        let min_w = if is_markdown {
                            PREVIEW_MD_MIN_W
                        } else {
                            let cols = text
                                .lines()
                                .map(|line| {
                                    line.chars()
                                        .map(|c| if c == '\t' { PREVIEW_CODE_TAB_COLS } else { 1 })
                                        .sum::<usize>()
                                })
                                .max()
                                .unwrap_or(0);
                            (cols as f32 * PREVIEW_CODE_CHAR_W + PREVIEW_CODE_PAD)
                                .min(PREVIEW_CODE_MAX_W)
                        };
                        block.child(div().w_full().min_w(px(min_w)).child(view))
                    };
                    col = col.child(block);
                } else if let Some(img) = thumb_img {
                    // Clicking the thumbnail opens the big viewer
                    // window (docs/features/VIEWER.md) on the current
                    // folder, same as Cmd+Y. A maximize glyph in the
                    // top-right corner is the discoverability affordance
                    // (only shown here, where a viewer-capable preview
                    // exists) instead of a text caption.
                    //
                    // Box height is user-adjustable via the resize grip
                    // below; the image fills whatever the box allows
                    // (aspect preserved — gpui's img derives its
                    // aspect_ratio from the bitmap's intrinsic size).
                    col = col.child(
                        div()
                            .id("preview-thumb-open")
                            .relative()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w_full()
                            .h(px(self.preview_thumb_h))
                            .p_2()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().secondary.opacity(0.5))
                            .cursor_pointer()
                            .hover(|this| this.bg(cx.theme().secondary.opacity(0.8)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.on_open_viewer(&OpenViewer, window, cx)
                            }))
                            .child(gpui::img(img).max_w_full().max_h_full())
                            .child(
                                div()
                                    .absolute()
                                    .top_2()
                                    .right_2()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(22.0))
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().background.opacity(0.75))
                                    .child(
                                        svg()
                                            .path("icons/maximize.svg")
                                            .w(px(13.0))
                                            .h(px(13.0))
                                            .text_color(cx.theme().foreground),
                                    ),
                            ),
                    );
                    col = col.child(self.preview_thumb_resize_grip(cx));
                } else if matches!(thumb_state, Some(crate::preview::PreviewState::Pending)) {
                    col = col.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w_full()
                            .h(px(self.preview_thumb_h))
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().secondary.opacity(0.5))
                            .text_scale_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Loading preview\u{2026}"),
                    );
                    col = col.child(self.preview_thumb_resize_grip(cx));
                }

                // Filename header. A clean name truncates with a full-name
                // tooltip; a name with deceptive characters (homoglyphs,
                // bidi overrides, hidden whitespace) renders each hazard
                // highlighted with its own explanatory tooltip instead.
                let name_header = div()
                    .id(("preview-name", entry.id.as_raw() as usize))
                    .text_scale_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground);
                let name_header = if entry.name_has_hazards {
                    name_header.child(crate::entry_info::name_hazard_element(
                        &entry.display_name,
                        "preview-name",
                    ))
                } else {
                    let name_for_tooltip = entry.display_name.clone();
                    name_header
                        .truncate()
                        .child(SharedString::from(entry.display_name.clone()))
                        .tooltip(move |window, cx| {
                            Tooltip::new(SharedString::from(name_for_tooltip.clone()))
                                .build(window, cx)
                        })
                };
                col = col.child(name_header);

                // The Get Info panel, embedded — the detail rows the
                // preview used to show, now editable and complete. Cmd+I
                // opens the same content as a standalone popup.
                col = col.child(info_view);

                // Quarantine surface — the red mark line, the
                // provenance the prefetch worker read off the xattr /
                // Zone.Identifier record (source URL, referrer, agent
                // + download time), and the clear action. All cached
                // on the entry; zero I/O at render time.
                if entry.is_quarantined {
                    col = col.child(
                        h_flex()
                            .mt_1()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_scale_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(gpui::rgb(0xFF3B30))
                                    .child("Quarantined \u{00B7} Mark of the Web"),
                            )
                            .child(
                                Button::new("preview-clear-quarantine")
                                    .label(feraille_core::commands::CLEAR_QUARANTINE_LABEL)
                                    .xsmall()
                                    .outline()
                                    .flex_shrink_0()
                                    .tooltip(
                                        "Remove the mark and its \
                                         downloaded-from record",
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.on_clear_quarantine(&ClearQuarantine, window, cx);
                                    })),
                            ),
                    );
                    if let Some(q) = &entry.quarantine {
                        // where_from convention (both platforms): the
                        // first URL is the download source, the second
                        // the referring page. Rendered as plain
                        // `text_xs` label/value rows so the provenance
                        // matches the Get Info rows directly above it.
                        // (The gpui-component `DescriptionList` this
                        // used before hardcodes its label at `text_sm`
                        // and lets the value inherit the ambient size;
                        // its `.small()`/`.xsmall()` knob only changes
                        // gap + padding, not font size — so it always
                        // rendered a notch larger than the rest of the
                        // pane.)
                        let muted = cx.theme().muted_foreground;
                        let prov_row = |label: &str, value: AnyElement| {
                            v_flex()
                                .gap_0p5()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_scale_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(muted)
                                        .child(label.to_string()),
                                )
                                .child(div().min_w_0().text_scale_xs().child(value))
                        };
                        let mut prov = v_flex().mt_1p5().gap_2();
                        let mut has_rows = false;
                        if let Some(src) = q.where_from.first() {
                            prov = prov.child(prov_row(
                                "Source",
                                truncated_url_value("prov-source", src, entry.id),
                            ));
                            has_rows = true;
                        }
                        if let Some(referrer) = q.where_from.get(1) {
                            prov = prov.child(prov_row(
                                "Referrer",
                                truncated_url_value("prov-referrer", referrer, entry.id),
                            ));
                            has_rows = true;
                        }
                        if q.agent.is_some() || q.downloaded_iso.is_some() {
                            let via = match (&q.agent, &q.downloaded_iso) {
                                (Some(a), Some(t)) => format!("{a} \u{00B7} {t}"),
                                (Some(a), None) => a.clone(),
                                (None, Some(t)) => t.clone(),
                                (None, None) => unreachable!(),
                            };
                            prov = prov.child(prov_row(
                                "Downloaded via",
                                div().child(SharedString::from(via)).into_any_element(),
                            ));
                            has_rows = true;
                        }
                        if has_rows {
                            col = col.child(prov);
                        }
                    }
                }

                // Action row — icon-only buttons with tooltips that
                // include the keyboard shortcut. No Get Info button here:
                // the preview pane already shows the full Get Info panel,
                // so the icon would just duplicate what's on screen (Cmd+I
                // still opens the detached Get Info window).
                // `tooltip_with_action` pulls the chord from the
                // keymap automatically so each hover reads "Open ⌘O".
                let actions = h_flex()
                    .mt_2()
                    .gap_1()
                    .child(
                        Button::new("preview-open")
                            .icon(gpui_component::Icon::empty().path("icons/external-link.svg"))
                            .xsmall()
                            .ghost()
                            .tooltip_with_action("Open", &OpenSelected, Some(SHELL_CONTEXT))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.on_open_selected(&OpenSelected, window, cx);
                            })),
                    )
                    .child(
                        Button::new("preview-reveal")
                            .icon(gpui_component::Icon::empty().path("icons/folder-open.svg"))
                            .xsmall()
                            .ghost()
                            .tooltip_with_action(
                                feraille_core::commands::REVEAL_LABEL,
                                &RevealInFinder,
                                Some(SHELL_CONTEXT),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.on_reveal_in_finder(&RevealInFinder, window, cx);
                            })),
                    )
                    .child(
                        Button::new("preview-copy-path")
                            .icon(gpui_component::Icon::empty().path("icons/copy.svg"))
                            .xsmall()
                            .ghost()
                            .tooltip_with_action("Copy Path", &CopyPath, Some(SHELL_CONTEXT))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.on_copy_path(&CopyPath, window, cx);
                            })),
                    );
                col = col.child(actions);

                col.into_any_element()
            }
        };

        // Pinned header; the body scrolls when the window is shorter
        // than the thumbnail + metadata + actions stack, with a
        // gpui-component scrollbar overlaid on the pane's right edge
        // (it only shows while the content actually overflows).
        v_flex()
            .size_full()
            .min_h_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(div().px_4().pt_4().pb_3().child(header))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("preview-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.preview_scroll)
                            .flex()
                            .flex_col()
                            .px_4()
                            .pb_4()
                            .child(body),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.0))
                            .child(Scrollbar::vertical(&self.preview_scroll)),
                    ),
            )
    }

    /// Build the breadcrumb row from `current_dir`. Each ancestor is
    /// clickable and navigates the pane to that level. The root `/`
    /// gets its own leading segment. When `breadcrumb_editing` is
    /// set (Cmd+L) the row swaps in an Input field instead — Enter
    /// commits the path, Blur cancels.
    fn breadcrumb(&self, cx: &mut Context<Self>) -> Div {
        if self.breadcrumb_editing {
            // Key routing for the autocomplete menu. Two upstream
            // quirks would otherwise leak keystrokes to the Shell
            // keymap (moving the FILE LIST cursor / opening rows
            // while the user is driving the menu):
            //
            //  1. gpui-component only registers the input's MoveUp/
            //     MoveDown handlers on MULTI-line inputs, so on this
            //     single-line field the "Input"-context binding
            //     matches but goes unhandled and the keystroke falls
            //     through to the Shell's CursorUp/CursorDown. The
            //     wrapper handlers below catch the actions and
            //     forward them to the completion menu.
            //  2. CompletionMenu::handle_action calls cx.propagate()
            //     unconditionally, which re-opens the dispatch even
            //     after the menu handled the key — Enter would both
            //     accept the completion AND fall through to the
            //     Shell's OpenSelected. Every handler here ends with
            //     stop_propagation; the Enter/Escape backstops only
            //     run at all when that leak re-propagates past the
            //     input's own handler (menu-open case), so normal
            //     PressEnter commit / Escape blur are unaffected.
            let input_for_up = self.breadcrumb_input.clone();
            let input_for_down = self.breadcrumb_input.clone();
            return h_flex()
                .w_full()
                .items_center()
                .gap_1()
                .px_4()
                .py_1()
                .border_b_1()
                .border_color(cx.theme().border)
                .on_action(move |a: &gpui_component::input::MoveUp, window, cx| {
                    input_for_up.update(cx, |state, cx| {
                        state.handle_action_for_context_menu(Box::new(a.clone()), window, cx);
                    });
                    cx.stop_propagation();
                })
                .on_action(move |a: &gpui_component::input::MoveDown, window, cx| {
                    input_for_down.update(cx, |state, cx| {
                        state.handle_action_for_context_menu(Box::new(a.clone()), window, cx);
                    });
                    cx.stop_propagation();
                })
                .on_action(move |_: &gpui_component::input::Enter, _window, cx| {
                    cx.stop_propagation();
                })
                .on_action(move |_: &gpui_component::input::Escape, _window, cx| {
                    cx.stop_propagation();
                })
                .child(
                    div()
                        .flex_1()
                        .child(Input::new(&self.breadcrumb_input).small()),
                );
        }
        let segments = path_segments(&self.active_tab().current_dir);
        // Warm each segment's child-folder list off-thread so the "Go to
        // Subfolder" submenu is populated by the time the user
        // right-clicks (Prime Directive: the enumeration runs on a
        // worker, never here). Only spawn for segments not yet cached.
        {
            let uncached: Vec<PathBuf> = segments
                .iter()
                .map(|(_, p)| p.clone())
                .filter(|p| !self.breadcrumb_children.contains_key(p))
                .collect();
            if !uncached.is_empty() {
                let weak = cx.weak_entity();
                cx.defer(move |cx| {
                    let _ = weak.update(cx, |this, cx| {
                        for p in uncached {
                            this.warm_breadcrumb_children(p, cx);
                        }
                    });
                });
            }
        }
        let mut row = h_flex()
            .w_full()
            .items_center()
            .gap_1()
            .px_4()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border);

        // Tool-result indicator: a shared pill showing the active tool's
        // summary, then "in" followed by the root breadcrumb. Makes it
        // unmistakable that the pane is a result surface, not a folder.
        if let Some(summary) = self.tool_result_breadcrumb_summary() {
            row = row
                .child(
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().accent.opacity(0.5))
                        .border_1()
                        .border_color(cx.theme().border)
                        .text_scale_xs()
                        .text_color(cx.theme().foreground)
                        .child(summary),
                )
                .child(
                    div()
                        .px_1()
                        .text_scale_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("in"),
                );
        }

        for (i, (label, path)) in segments.iter().enumerate() {
            if i > 0 {
                row = row.child(
                    div()
                        .px_1()
                        .text_scale_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("\u{203A}"), // SINGLE RIGHT-POINTING ANGLE QUOTATION MARK
                );
            }
            let is_last = i + 1 == segments.len();
            let label = label.clone();
            let path = path.clone();
            let tooltip_path = path.to_string_lossy().into_owned();
            // Phase 6 (next-level): right-click on a breadcrumb
            // segment offers "Open in New Tab" / "Reveal in Finder"
            // / "Copy Path" — same right-click surface as the
            // sidebar Favorites. context_target carries the path of
            // *this* segment, not the active tab's current_dir.
            use gpui_component::menu::ContextMenuExt as _;
            let weak_for_crumb = cx.weak_entity();
            let path_for_menu = path.clone();
            let path_for_click = path.clone();
            let path_for_drop = path.clone();
            let crumb_accent = cx.theme().primary;
            // §5 favorited indicator: trailing star on any breadcrumb
            // segment whose path is in the Favorites index. The last
            // segment is the current-folder header per §5.1, so the
            // current-folder header is covered by the same render path.
            let favorited = self.process.favorites.read(cx).contains_path(&path);
            let crumb = div()
                .id(ElementId::Name(format!("crumb-{i}").into()))
                .px_2()
                .py_1()
                .rounded(cx.theme().radius)
                .text_scale_sm()
                .flex()
                .items_center()
                .gap_1()
                .text_color(if is_last {
                    cx.theme().foreground
                } else {
                    cx.theme().muted_foreground
                })
                .when(is_last, |this| this.font_weight(FontWeight::SEMIBOLD))
                .cursor_pointer()
                .hover(|this| this.bg(cx.theme().secondary))
                .child(label)
                .when(favorited, |this| {
                    this.child(
                        svg()
                            .path("icons/nav/star.svg")
                            .icon_px(11.0)
                            .text_color(cx.theme().primary)
                            .flex_shrink_0(),
                    )
                })
                // Suppress the full-path tooltip while this crumb's
                // context menu is open so the two don't overlap
                // (docs/GPUI-UPSTREAM.md — no menu-open callback upstream).
                .when(!self.breadcrumb_menu_open, |crumb| {
                    crumb.tooltip({
                        let t = SharedString::from(tooltip_path);
                        move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(t.clone()).build(window, cx)
                        }
                    })
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.navigate(path_for_click.clone(), cx);
                }))
                // Files dropped on a breadcrumb segment transfer into
                // that ancestor folder (the last segment = current dir).
                .drag_over::<ExternalPaths>(move |style, _payload, _window, _cx| {
                    style.bg(crumb_accent.opacity(0.18))
                })
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                    this.handle_external_drop(
                        paths.paths().to_vec(),
                        path_for_drop.clone(),
                        window,
                        cx,
                    );
                }))
                .context_menu(move |menu, window, cx| {
                    let favorited_now = if let Some(s) = weak_for_crumb.upgrade() {
                        let already = s
                            .read(cx)
                            .process
                            .favorites
                            .read(cx)
                            .contains_path(&path_for_menu);
                        s.update(cx, |shell, cx| {
                            shell.context_target = Some(path_for_menu.clone());
                            shell.favorites_context_path = Some(path_for_menu.clone());
                            // Hide the crumb tooltip for as long as this
                            // menu is up; cleared on the next root click.
                            shell.breadcrumb_menu_open = true;
                            cx.notify();
                        });
                        already
                    } else {
                        false
                    };
                    let favorite_label = if favorited_now {
                        "Remove from Favorites"
                    } else {
                        "Add to Favorites"
                    };
                    // "Go to Subfolder ▸" — jump to any child folder of
                    // this segment (Finder's column-view-style lateral
                    // navigation). Children are enumerated off-thread and
                    // cached; the submenu reads that cache only (Prime
                    // Directive), showing "Loading…" on the first open.
                    let weak_sub = weak_for_crumb.clone();
                    let path_sub = path_for_menu.clone();
                    menu.menu("Open in New Tab", Box::new(OpenContextInNewTab))
                        .separator()
                        .menu(
                            feraille_core::commands::REVEAL_LABEL,
                            Box::new(RevealContextPath),
                        )
                        .menu("Copy Path", Box::new(CopyContextPath))
                        .separator()
                        .menu(favorite_label, Box::new(ToggleFavoriteForTarget))
                        .separator()
                        .menu("New Folder Here", Box::new(NewFolderHere))
                        .separator()
                        .submenu("Go to Subfolder", window, cx, move |mut sub, _w, c| {
                            use gpui_component::menu::PopupMenuItem;
                            let Some(s) = weak_sub.upgrade() else {
                                return sub;
                            };
                            let cached = s.read(c).breadcrumb_children.get(&path_sub).cloned();
                            match cached {
                                Some(Some(children)) if !children.is_empty() => {
                                    for (name, child) in children.iter() {
                                        let child = child.clone();
                                        let weak_nav = weak_sub.clone();
                                        sub = sub.item(
                                            PopupMenuItem::new(name.clone()).on_click(
                                                move |_ev, _w, cx| {
                                                    let child = child.clone();
                                                    let _ = weak_nav.update(cx, |sh, cx| {
                                                        sh.navigate(child, cx);
                                                    });
                                                },
                                            ),
                                        );
                                    }
                                    sub
                                }
                                Some(Some(_)) => sub
                                    .item(PopupMenuItem::new("No subfolders").disabled(true)),
                                _ => {
                                    // Cold or in-flight — kick a warm and show
                                    // a placeholder; a re-open shows the list.
                                    s.update(c, |sh, cx| {
                                        sh.warm_breadcrumb_children(path_sub.clone(), cx);
                                    });
                                    sub.item(
                                        PopupMenuItem::new("Loading\u{2026}").disabled(true),
                                    )
                                }
                            }
                        })
                });
            row = row.child(crumb);
        }
        if self.active_tab().tool_result.is_some() {
            if self.active_tool_result_can_pop_out() {
                row = row.child(
                    Button::new("tool-result-pop-out")
                        .small()
                        .icon(gpui_component::Icon::empty().path("icons/maximize.svg"))
                        .tooltip("Open in window")
                        .on_click(cx.listener(|_, _, window, cx| {
                            window.dispatch_action(Box::new(PopOutDiskUsage), cx);
                        })),
                );
            }
            row = row.child(
                Button::new("tool-result-close")
                    .small()
                    .icon(gpui_component::Icon::empty().path("icons/close.svg"))
                    .tooltip("Close results")
                    .on_click(cx.listener(|_, _, window, cx| {
                        window.dispatch_action(Box::new(CloseToolResult), cx);
                    })),
            );
        }
        row
    }

    /// The grab handle drawn at the window's inner edge while docked and not
    /// fully revealed (docs/features/DOCK.md). Purely a visual hint — the
    /// reveal trigger is the screen edge itself, driven by the poll loop. It
    /// anchors opposite the dock edge because the drawer hides toward its dock
    /// edge, leaving that side on-screen as the strip.
    fn dock_handle(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let dock = self.dock.as_ref()?;
        if dock.progress >= 0.999 {
            return None; // fully revealed: nothing to point at
        }
        let thickness = px(super::dock::STRIP_PX as f32);
        // Left/Right docking → always a tall, thin vertical grip pill.
        let pill = div()
            .w(px(3.))
            .h(px(40.))
            .rounded_full()
            .bg(cx.theme().muted_foreground);

        let strip = div()
            .absolute()
            .top_0()
            .bottom_0()
            .w(thickness)
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().secondary)
            .child(pill);
        // The handle sits on the window edge opposite the dock edge — the side
        // left on-screen as the strip when the drawer hides toward its edge.
        let strip = match dock.edge {
            DockEdge::Left => strip.right_0(),
            DockEdge::Right => strip.left_0(),
        };
        Some(strip)
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _path_guard = feraille_core::path_guard::enter_render();
        // Phase 10: drain any pending system-appearance change the
        // native observer pushed since the last paint, then flip the
        // gpui Theme. The observer can only set an AtomicBool; the
        // Theme::change call needs `&mut App + &mut Window` so it
        // lives here.
        if let Some(is_dark) = take_system_theme_pending() {
            let mode = if is_dark {
                gpui_component::ThemeMode::Dark
            } else {
                gpui_component::ThemeMode::Light
            };
            gpui_component::Theme::change(mode, Some(window), cx);
            // `Theme::change` re-applies the theme config, which can
            // reset the base font size — re-assert the current UI zoom
            // so an appearance flip doesn't silently reset text scaling.
            self.apply_ui_zoom(cx);
            // Keep native window chrome in step with the theme flip.
            // Deferred: render itself must not mutate AppKit state
            // (Prime Directive) — this runs on the main thread right
            // after the pass instead.
            cx.defer(move |_| {
                crate::platform_shell::set_app_appearance(is_dark);
            });
        }
        let weak = cx.weak_entity();
        let locations_menu = self.build_locations_menu(weak.clone(), cx);
        let favorites_section = self.build_user_favorites_section(weak.clone(), cx);
        let recents_section = self.build_recents_section(weak.clone(), cx);
        let browse_rows = self.build_browse_rows(cx);
        let volumes_rows = self.build_volumes_rows(cx);
        let has_volumes = !self.process.volumes.borrow().is_empty();

        // Render never fetches icons (the path guard makes cache
        // misses return the blank placeholder), so collect the
        // sidebar paths whose icon isn't cached yet and schedule a
        // background warm. `warm_folder_icon` caches even on fetch
        // failure, so this set empties out instead of respawning
        // every frame. Favorites ride along: their rows use the same
        // path-keyed cache.
        let mut icon_warm: Vec<PathBuf> = Vec::new();
        {
            let icons = self.process.icons.borrow();
            for row in browse_rows.iter().chain(volumes_rows.iter()) {
                if matches!(row.icon, TreeRowIcon::Folder) && !icons.has_folder_icon(&row.path) {
                    icon_warm.push(row.path.clone());
                }
            }
            for fav in self.process.favorites.read(cx).entries() {
                if let feraille_core::favorites::FavoriteTarget::Path(p) = &fav.target {
                    if fav.custom_icon.is_none() && !icons.has_folder_icon(p) {
                        icon_warm.push(p.clone());
                    }
                }
            }
            for p in self.process.recents.borrow().iter() {
                if !icons.has_folder_icon(p) {
                    icon_warm.push(p.clone());
                }
            }
        }
        self.start_tree_icon_warm(icon_warm, cx);
        let breadcrumb = self.breadcrumb(cx);
        // Keep the OS window caption (Windows taskbar / Alt+Tab,
        // macOS Window menu) in step with the active folder — without
        // it the window is nameless when switching tasks.
        self.sync_window_title(window);
        let path_str = self.active_tab().current_dir.to_string_lossy().into_owned();

        // `.collapsible(false)` disables gpui-component's animatable
        // wrapper (which would otherwise force a fixed expanded
        // width), letting the surrounding `resizable_panel` drive
        // the actual column width. `.w_full()` makes the Sidebar
        // fill its panel; the tree rows already use `.w_full()` on
        // each row container, so the labels grow as the user
        // drags the splitter.
        //
        // Sidebar IA: Locations = fixed OS-standard folders (flat
        // SidebarMenu); Favorites = user-curated, persisted, reorderable
        // shortcuts (docs/features/FAVORITES.md); Browse = single-rooted
        // expandable Home tree; Volumes = expandable per-volume tree.
        // Sidebar no longer carries the "Feraille" header — that moved
        // into the TitleBar at the top of the window. Icon-mode collapse
        // is enabled so the toggle button in the TitleBar can shrink the
        // sidebar to a 48-DIP icon strip.
        let mut sidebar = Sidebar::new("shell-sidebar")
            .collapsible(gpui_component::sidebar::SidebarCollapsible::Icon)
            .collapsed(self.sidebar_collapsed)
            .w_full()
            .child(ShellSidebarItem::group(LabeledMenu::new(
                "Locations",
                locations_menu,
            )))
            .child(favorites_section);
        // Recents sits below Favorites, above Browse — hidden until the
        // user has navigated somewhere (build_recents_section → None).
        if let Some(recents_section) = recents_section {
            sidebar = sidebar.child(recents_section);
        }
        sidebar = sidebar.child(ShellSidebarItem::tree(TreeSection::new(
            "Browse",
            browse_rows,
            weak.clone(),
            self.process.icons.clone(),
        )));
        if has_volumes {
            sidebar = sidebar.child(ShellSidebarItem::tree(TreeSection::new(
                "Volumes",
                volumes_rows,
                weak.clone(),
                self.process.icons.clone(),
            )));
        }

        let _ = path_str; // breadcrumb already shows the path

        let tabstrip = self.tabstrip(cx);
        // Phase 8: status-bar density. Compute selected count / size,
        // total visible size for the active folder, and free disk on
        // the active tab's volume. Cheap O(N) sums over the already-
        // filtered entries Vec; called once per render.
        let delegate = self.active_tab().table.read(cx).delegate();
        let entries = &delegate.entries;
        let entry_count = entries.len();
        // Totals come from the delegate's lazy caches — recomputed
        // once per model/selection change, not O(N) per render pass
        // (a 100k-entry folder added hundreds of µs to every repaint).
        let total_size: u64 = delegate.cached_total_size.get().unwrap_or_else(|| {
            let t = entries.iter().map(|e| e.size).sum();
            delegate.cached_total_size.set(Some(t));
            t
        });
        let selection = &self.active_tab().selection;
        let selected_count = selection.len();
        let selected_size: u64 = if selected_count == 0 {
            0
        } else {
            delegate.cached_selected_size.get().unwrap_or_else(|| {
                let s = entries
                    .iter()
                    .filter(|e| delegate.selected_set.contains(&e.id))
                    .map(|e| e.size)
                    .sum();
                delegate.cached_selected_size.set(Some(s));
                s
            })
        };
        // Free-space label — reads the per-tab cache maintained by
        // `refresh_volume_info_in_tab` (load completion + volume
        // watch). The underlying NSURL/statfs query can round-trip to
        // a network mount, so it never runs on the paint path.
        let free_bytes = self.active_tab().volume_free_bytes;
        let volume_name = self.active_tab().volume_name.clone();
        let metrics = crate::status_bar::StatusMetrics {
            entries: entry_count,
            selected_count,
            selected_size,
            total_size,
            free_bytes,
            volume_name,
        };
        let _ = delegate;
        // Clicking the task region of the status bar toggles the
        // background-task panel popover. The listener takes `&mut
        // Self` directly so we don't re-enter the entity update.
        let toggle_task_panel: crate::status_bar::ClickHandler = {
            let weak: WeakEntity<Self> = cx.weak_entity();
            Rc::new(move |_evt, _window, cx| {
                if let Some(s) = weak.upgrade() {
                    s.update(cx, |this, cx| {
                        this.task_panel_open = !this.task_panel_open;
                        cx.notify();
                    });
                }
            })
        };
        // Show-Hidden toggle moved into the status bar in Phase 7.
        // The callback wraps Shell::toggle_hidden so the switch's
        // built-in checked-state stays in sync via the next render.
        let toggle_hidden_cb: crate::status_bar::ActionHandler = {
            let weak: WeakEntity<Self> = cx.weak_entity();
            Rc::new(move |_window, cx| {
                if let Some(s) = weak.upgrade() {
                    s.update(cx, |this, cx| this.toggle_hidden(cx));
                }
            })
        };
        let status_bar = crate::status_bar::render(
            metrics,
            &self.process.tasks,
            self.simulated_progress,
            Some(toggle_task_panel),
            self.show_hidden,
            Some(toggle_hidden_cb),
            cx,
        );
        // Auto-dismiss the background-task popover when the pointer
        // leaves it. `on_hover` fires only on a hover-state change and
        // starts `false`, so opening it above the status-bar click
        // point doesn't instant-close — it shuts when the mouse, after
        // being over the popover, moves off. Click-outside dismissal
        // (the shell's `on_mouse_down`) still applies for the
        // never-hovered case.
        let task_panel =
            crate::task_panel::render_if_open(self.task_panel_open, &self.process.tasks, cx).map(
                |panel| {
                    // `.id(...)` makes the popover stateful so `on_hover` is
                    // available (it lives on StatefulInteractiveElement).
                    panel.id("task-panel-popover").on_hover(cx.listener(
                        |this, hovered: &bool, _window, cx| {
                            if !*hovered {
                                this.task_panel_open = false;
                                cx.notify();
                            }
                        },
                    ))
                },
            );

        div()
            .key_context(SHELL_CONTEXT)
            .track_focus(&self.focus_handle)
            // Type-to-select: printable keys with the list or grid
            // focused jump the selection to the first matching name.
            // Runs only for characters no keybinding claimed (gpui
            // matches actions first), so it never shadows nav keys.
            .on_key_down(cx.listener(Self::on_typeahead_key))
            // Any left-click dismisses an open breadcrumb context menu
            // (picking an item or clicking away), so it's also the
            // moment to re-enable the crumb tooltip we suppressed while
            // the menu was open (docs/GPUI-UPSTREAM.md).
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    if this.breadcrumb_menu_open {
                        this.breadcrumb_menu_open = false;
                        cx.notify();
                    }
                }),
            )
            .on_action(cx.listener(Self::on_navigate_parent))
            .on_action(cx.listener(Self::on_navigate_back))
            .on_action(cx.listener(Self::on_navigate_forward))
            .on_action(cx.listener(Self::on_open_selected))
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_show_desktop))
            .on_action(cx.listener(Self::on_dock_left))
            .on_action(cx.listener(Self::on_dock_right))
            .on_action(cx.listener(Self::on_undock))
            .on_action(cx.listener(Self::on_toggle_hidden))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_copy_path))
            .on_action(cx.listener(Self::on_copy_file_list))
            .on_action(cx.listener(Self::on_copy_files))
            .on_action(cx.listener(Self::on_cut_files))
            .on_action(cx.listener(Self::on_paste_files))
            .on_action(cx.listener(Self::on_move_paste_files))
            .on_action(cx.listener(Self::on_open_terminal_here))
            .on_action(cx.listener(Self::on_reveal_in_finder))
            .on_action(cx.listener(Self::on_move_to_trash))
            .on_action(cx.listener(Self::on_delete_immediately))
            .on_action(cx.listener(Self::on_empty_trash))
            .on_action(cx.listener(Self::on_focus_filter))
            .on_action(cx.listener(Self::on_clear_filter))
            .on_action(cx.listener(Self::on_new_folder))
            .on_action(cx.listener(Self::on_rename_selected))
            .on_action(cx.listener(Self::on_bulk_rename_selected))
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_close_window))
            .on_action(cx.listener(Self::on_reopen_closed_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_quick_look))
            .on_action(cx.listener(Self::on_go_home))
            .on_action(cx.listener(Self::on_edit_breadcrumb))
            .on_action(cx.listener(Self::on_shortcuts_help))
            .on_action(cx.listener(Self::on_open_disk_usage))
            .on_action(cx.listener(Self::on_open_archive))
            .on_action(cx.listener(Self::on_close_tool_result))
            .on_action(cx.listener(Self::on_pop_out_disk_usage))
            .on_action(cx.listener(Self::on_find_duplicates))
            .on_action(cx.listener(Self::on_open_viewer))
            .on_action(cx.listener(Self::on_slideshow_from_here))
            .on_action(cx.listener(Self::on_sort_by_name))
            .on_action(cx.listener(Self::on_sort_by_size))
            .on_action(cx.listener(Self::on_sort_by_kind))
            .on_action(cx.listener(Self::on_sort_by_modified))
            .on_action(cx.listener(Self::on_toggle_recents_section))
            .on_action(cx.listener(Self::on_remove_from_recents))
            .on_action(cx.listener(Self::on_clear_recents))
            .on_action(cx.listener(Self::on_cursor_up))
            .on_action(cx.listener(Self::on_cursor_down))
            .on_action(cx.listener(Self::on_cursor_first))
            .on_action(cx.listener(Self::on_cursor_last))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
            .on_action(cx.listener(Self::on_cursor_up_extend))
            .on_action(cx.listener(Self::on_cursor_down_extend))
            .on_action(cx.listener(Self::on_cursor_first_extend))
            .on_action(cx.listener(Self::on_cursor_last_extend))
            .on_action(cx.listener(Self::on_page_up_extend))
            .on_action(cx.listener(Self::on_page_down_extend))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_clear_selection))
            .on_action(cx.listener(Self::on_toggle_preview))
            .on_action(cx.listener(Self::on_get_info))
            .on_action(cx.listener(Self::on_clear_quarantine))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_zoom_reset))
            .on_action(cx.listener(Self::on_open_in_new_tab))
            .on_action(cx.listener(Self::on_duplicate))
            .on_action(cx.listener(Self::on_make_alias))
            .on_action(cx.listener(Self::on_compress))
            .on_action(cx.listener(Self::on_new_archive))
            .on_action(cx.listener(Self::on_compress_sevenz))
            .on_action(cx.listener(Self::on_compress_tar))
            .on_action(cx.listener(Self::on_compress_targz))
            .on_action(cx.listener(Self::on_compress_tarbz2))
            .on_action(cx.listener(Self::on_compress_tarxz))
            .on_action(cx.listener(Self::on_extract))
            .on_action(cx.listener(Self::on_extract_to))
            .on_action(cx.listener(Self::on_reveal_context_path))
            .on_action(cx.listener(Self::on_copy_context_path))
            .on_action(cx.listener(Self::on_open_terminal_at_context))
            .on_action(cx.listener(Self::on_open_context_in_new_tab))
            .on_action(cx.listener(Self::on_new_folder_here))
            .on_action(cx.listener(Self::on_eject_volume))
            .on_action(cx.listener(Self::on_toggle_tag_red))
            .on_action(cx.listener(Self::on_toggle_tag_orange))
            .on_action(cx.listener(Self::on_toggle_tag_yellow))
            .on_action(cx.listener(Self::on_toggle_tag_green))
            .on_action(cx.listener(Self::on_toggle_tag_blue))
            .on_action(cx.listener(Self::on_toggle_tag_purple))
            .on_action(cx.listener(Self::on_toggle_tag_gray))
            .on_action(cx.listener(Self::on_open_with_slot_0))
            .on_action(cx.listener(Self::on_open_with_slot_1))
            .on_action(cx.listener(Self::on_open_with_slot_2))
            .on_action(cx.listener(Self::on_open_with_slot_3))
            .on_action(cx.listener(Self::on_open_with_slot_4))
            .on_action(cx.listener(Self::on_open_with_slot_5))
            .on_action(cx.listener(Self::on_open_with_slot_6))
            .on_action(cx.listener(Self::on_open_with_slot_7))
            .on_action(cx.listener(Self::on_open_with_slot_8))
            .on_action(cx.listener(Self::on_open_with_slot_9))
            .on_action(cx.listener(Self::on_open_with_slot_10))
            .on_action(cx.listener(Self::on_open_with_slot_11))
            .on_action(cx.listener(Self::on_undo_last_action))
            .on_action(cx.listener(Self::on_toggle_favorite_for_target))
            .on_action(cx.listener(Self::on_add_current_folder_to_favorites))
            .on_action(cx.listener(Self::on_toggle_favorites_section))
            .on_action(cx.listener(Self::on_sort_favorites_by_name))
            .on_action(cx.listener(Self::on_sort_favorites_by_date_added_newest))
            .on_action(cx.listener(Self::on_sort_favorites_by_date_added_oldest))
            .on_action(cx.listener(Self::on_sort_favorites_by_kind))
            .on_action(cx.listener(Self::on_move_favorite_up))
            .on_action(cx.listener(Self::on_move_favorite_down))
            .on_action(cx.listener(Self::on_rename_favorite))
            .on_action(cx.listener(Self::on_locate_favorite))
            .on_action(cx.listener(Self::on_focus_favorite_up))
            .on_action(cx.listener(Self::on_focus_favorite_down))
            .on_action(cx.listener(Self::on_activate_favorite))
            .on_action(cx.listener(Self::on_delete_favorite))
            .on_action(cx.listener(Self::on_reset_favorite_name))
            .on_action(cx.listener(Self::on_reset_favorite_icon))
            .on_action(cx.listener(Self::on_open_favorite_icon_picker))
            // §3.1 tear-off remove. The favorites section's drop gaps
            // already intercept FavoriteDragPayload to reorder; any
            // drop that falls through to the shell's outer container
            // is by definition outside the section — treat it as a
            // remove with undo (§3.2). Same code path as the menu /
            // keyboard remove, so Cmd+Z restores at the prior index.
            .on_drop(cx.listener(
                |this, payload: &crate::favorites_section::FavoriteDragPayload, window, cx| {
                    use gpui_component::notification::Notification;
                    let id = payload.id;
                    let label = this
                        .process
                        .favorites
                        .read(cx)
                        .entry_by_id(id)
                        .map(|f| f.effective_label())
                        .unwrap_or_else(|| "favorite".to_string());
                    let removed_for_undo = this.process.favorites.read(cx).entry_by_id(id).cloned();
                    this.process.favorites.update(cx, |f, cx| {
                        f.remove(id, cx);
                    });
                    if let Some(fav) = removed_for_undo {
                        this.push_undo(UndoOp::RemoveFavorite(fav));
                    }
                    window.push_notification(
                        Notification::info(format!(
                            "Removed \u{201C}{label}\u{201D} from Favorites \u{00B7} Cmd+Z to undo"
                        )),
                        cx,
                    );
                },
            ))
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .child({
                // Three-column resizable layout: sidebar | center | preview.
                // The status bar runs full-width across the bottom so its
                // task summary + progress strip is always visible.
                use crate::splitter::{h_resizable, resizable_panel};
                let file_body = self.file_pane_body(cx);
                // Phase 6 review fix: an outer .context_menu on the
                // file body wrapper consumed the click events bound
                // for the inner DataTable row menu, causing every
                // file-row menu selection to dismiss without firing.
                // The empty-space menu (New Folder / Refresh / etc.)
                // is parked until we can split the file pane's
                // background from the rows at the event-routing
                // layer — the toolbar already exposes those actions
                // so users aren't blocked.
                // Drop target for OS file drags (Finder → Feraille,
                // and row drag-outs landing back in our own pane):
                // anywhere in the pane that a folder row didn't claim
                // first drops into the current directory
                // (docs/features/FILE_OPS.md; dnd-spec §3.5 "empty
                // space"). Folder rows stop propagation so their drop
                // wins.
                let file_body_wrapped = div()
                    .id("file-pane-drop")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .drag_over::<ExternalPaths>(|style, _, _, cx| {
                        style.bg(cx.theme().accent.opacity(0.06))
                    })
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                        let dest = this.active_tab().current_dir.clone();
                        this.handle_external_drop(paths.paths().to_vec(), dest, window, cx);
                    }))
                    .child(file_body);
                // The preview pane is hidden by default; whenever it's visible
                // the user explicitly turned it on (Cmd+P / View menu), so
                // honour that at any window width — the splitter's per-panel
                // min widths keep the layout sane on narrow windows. (A prior
                // auto-hide below 900px silently suppressed the explicit
                // toggle, so Cmd+P appeared to do nothing on smaller windows.)
                let preview_visible = self.preview_visible;
                let preview_pane = if preview_visible {
                    Some(self.preview(cx))
                } else {
                    None
                };
                // Pull the persisted widths into the panels' initial
                // `.size(...)` — they survive across launches because
                // they're written through `on_resize` to app_state
                // (debounced via SPLITTER_PERSIST_INTERVAL below).
                // Collapsed sidebar shrinks to the gpui-component
                // icon strip width (~48 DIPs). Drag handle hides
                // implicitly because we squeeze the range to a fixed
                // size in that mode.
                let sidebar_width_px = if self.sidebar_collapsed {
                    px(SIDEBAR_COLLAPSED_WIDTH)
                } else {
                    px(self.sidebar_width)
                };
                let preview_width_px = px(self.preview_width);
                let weak = cx.weak_entity();
                let sidebar_collapsed = self.sidebar_collapsed;
                let sidebar_width_before = if sidebar_collapsed {
                    SIDEBAR_COLLAPSED_WIDTH
                } else {
                    self.sidebar_width
                        .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH)
                };
                let preview_width_before = self
                    .preview_width
                    .clamp(PREVIEW_MIN_WIDTH, PREVIEW_MAX_WIDTH);
                let splitter = h_resizable("shell-splitter")
                    .with_state(&self.splitter_state)
                    .on_resize(move |state, window, cx| {
                        // Callback fires when a splitter drag ends. Read
                        // sizes out of the ResizableState, write them back
                        // into Shell so the next render re-applies them,
                        // and push to disk through the throttled writer.
                        let mut sizes = state.read(cx).sizes().clone();
                        let preview_changed = preview_visible
                            && sizes.len() >= 3
                            && (f32::from(sizes[2]) - preview_width_before).abs() > 0.5;
                        if preview_changed && !sidebar_collapsed && !sizes.is_empty() {
                            let sw = f32::from(sizes[0]);
                            if (sw - sidebar_width_before).abs() > 0.5 {
                                // Dragging the preview handle left can push the center pane
                                // into its minimum and make the resizable group borrow width
                                // from the sidebar. Restore the sidebar in the splitter state
                                // so the preview drag can't corrupt the left navigation width.
                                state.update(cx, |state, cx| {
                                    state.resize_panel(0, px(sidebar_width_before), window, cx)
                                });
                                sizes = state.read(cx).sizes().clone();
                            }
                        }
                        if let Some(s) = weak.upgrade() {
                            s.update(cx, |this, cx| {
                                if let Some(sw) = sizes.first() {
                                    let sw = f32::from(*sw);
                                    if sidebar_collapsed {
                                        this.sidebar_width = sidebar_width_before;
                                    } else if !preview_changed {
                                        this.sidebar_width =
                                            sw.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
                                    } else {
                                        this.sidebar_width = sidebar_width_before;
                                    }
                                }
                                if preview_visible && sizes.len() >= 3 {
                                    this.preview_width = f32::from(sizes[2])
                                        .clamp(PREVIEW_MIN_WIDTH, PREVIEW_MAX_WIDTH);
                                }
                                this.schedule_splitter_save(cx);
                            });
                        }
                    })
                    .child(
                        resizable_panel()
                            .size(sidebar_width_px)
                            // Collapsed: pin the panel to the icon
                            // strip width so the drag handle can't
                            // reopen it accidentally; the TitleBar
                            // toggle is the one way back to expanded.
                            .when(self.sidebar_collapsed, |this| {
                                this.size_range(
                                    px(SIDEBAR_COLLAPSED_WIDTH)..px(SIDEBAR_COLLAPSED_WIDTH),
                                )
                            })
                            .when(!self.sidebar_collapsed, |this| {
                                this.size_range(px(SIDEBAR_MIN_WIDTH)..px(SIDEBAR_MAX_WIDTH))
                            })
                            .child(sidebar),
                    )
                    .child(
                        resizable_panel()
                            .size_range(px(FILE_PANE_MIN_WIDTH)..Pixels::MAX)
                            .child(
                                v_flex()
                                    .size_full()
                                    .child(tabstrip)
                                    .child(breadcrumb)
                                    .child(file_body_wrapped),
                            ),
                    );
                let splitter = if let Some(pane) = preview_pane {
                    splitter.child(
                        resizable_panel()
                            .size(preview_width_px)
                            .size_range(px(PREVIEW_MIN_WIDTH)..px(PREVIEW_MAX_WIDTH))
                            .child(pane),
                    )
                } else {
                    splitter
                };
                let title_bar = self.title_bar(cx);
                let menu_bar = self.menu_bar.clone();
                let dock_handle = self.dock_handle(cx);
                v_flex()
                    .relative()
                    .size_full()
                    // Phase 7: TitleBar sits across the top with the
                    // app name + filter input + back/forward
                    // navigation. Replaces the sidebar-header brand
                    // mark and the toolbar's nav buttons + filter
                    // input.
                    .child(title_bar)
                    // Windows/Linux app menu strip — File/Edit/View/Go
                    // dropdowns reading from the same `cx.set_menus()`
                    // spec the macOS NSApp menu uses. `None` on macOS,
                    // so the closure runs zero-cost there.
                    .when_some(menu_bar, |this, mb| {
                        this.child(
                            div()
                                .h(px(28.))
                                .px_2()
                                .border_b_1()
                                .border_color(cx.theme().border)
                                .child(mb),
                        )
                    })
                    .child(div().flex_1().min_h_0().child(splitter))
                    .child(status_bar)
                    // Background-task panel popover sits absolute-
                    // positioned over the bottom-left corner of this
                    // column, above the status bar. Only rendered
                    // when task_panel_open == true.
                    .when_some(task_panel, |this, panel| this.child(panel))
                    // Docked-drawer grab handle (docs/features/DOCK.md) —
                    // absolute, on the inner edge, above the column content.
                    // `None` unless docked-and-not-fully-revealed.
                    .children(dock_handle)
            })
            // Dialog overlay layer — rendered last so dialogs draw
            // above the shell content. Needed for the New Folder /
            // Rename modals (5.5.c).
            .children(Root::render_dialog_layer(window, cx))
            // Notification overlay (Stage 5.c) — toasts pushed via
            // `Window::push_notification` show up in the corner the
            // active theme specifies. The outer `div().relative()`
            // gives the absolute-positioned notification list a
            // positioned ancestor to anchor against.
            .children(Root::render_notification_layer(window, cx))
            // Keyboard-shortcuts help overlay (Stage 9.b). Renders
            // only when `shortcuts_help_filter` is Some(_); the
            // module reads `self` for the filter + input state.
            .children(crate::keyboard_help::render(self, cx))
    }
}
