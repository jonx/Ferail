use super::*;

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
            .text_sm()
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
        .on_drop(cx.listener(
            move |this, payload: &TabDragPayload, _window, cx| {
                this.reorder_tab(payload.id, pos, cx);
            },
        ))
}

impl Shell {
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
        let location_paths: HashSet<PathBuf> = feraille_fs_native::paths::well_known_locations()
            .into_iter()
            .map(|loc| loc.path)
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
            is_expandable: true,
            is_expanded,
            is_active: home == current,
            capacity: None,
            icon: TreeRowIcon::Folder,
            favorited,
        }];
        if is_expanded {
            self.append_tree_descendants_filtered(
                &mut rows,
                &home,
                1,
                &current,
                Some(&location_paths),
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
        for loc in feraille_fs_native::paths::well_known_locations() {
            let path = loc.path.clone();
            let node_id = self.process.fs.id_for_path(&path);
            self.process
                .node_store
                .borrow_mut()
                .get_or_create_path_with_id(path.clone(), node_id);
            let active = path == current;
            let favorited = favs.contains_path(&path);
            let weak_for_click = weak.clone();
            let weak_for_menu = weak.clone();
            let path_for_menu = path.clone();
            let path_for_modclick = path.clone();
            let item = SidebarMenuItem::new(SharedString::from(loc.label))
                .icon(Icon::empty().path(loc.icon))
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
                        .menu(feraille_core::commands::REVEAL_LABEL, Box::new(RevealContextPath))
                        .menu("Copy Path", Box::new(CopyContextPath))
                });
            // §5: a Locations entry that's also a user Favorite gets the
            // same trailing star treatment as everywhere else.
            let item = if favorited {
                item.suffix(|_, cx| {
                    use gpui::svg;
                    svg()
                        .path("icons/nav/star.svg")
                        .w(px(11.0))
                        .h(px(11.0))
                        .text_color(cx.theme().primary)
                        .flex_shrink_0()
                        .into_any_element()
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
        let volume_paths: Vec<(PathBuf, String, Option<(u64, u64)>)> = self
            .process
            .volumes
            .borrow()
            .iter()
            .map(|v| {
                let cap = match (v.total_bytes, v.available_bytes) {
                    (Some(t), Some(a)) if t > 0 => Some((t, a)),
                    _ => None,
                };
                (v.path.clone(), v.name.clone(), cap)
            })
            .collect();
        let mut entries: Vec<(PathBuf, String, Option<(u64, u64)>, bool)> = volume_paths
            .into_iter()
            .map(|(p, n, c)| {
                let fav = favs.contains_path(&p);
                (p, n, c, fav)
            })
            .collect();
        let _ = favs;
        for (path, name, capacity, favorited) in entries.drain(..) {
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
                is_expandable: true,
                is_expanded,
                is_active: path == current,
                capacity,
                icon: TreeRowIcon::Volume,
                favorited,
            });
            if is_expanded {
                self.append_tree_descendants(&mut rows, &path, 1, &current, cx);
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
        cx: &App,
    ) {
        self.append_tree_descendants_filtered(rows, parent, depth, current, None, cx);
    }

    /// Same as [`append_tree_descendants`] but with an optional
    /// `skip_paths` filter applied to direct children only. Used by
    /// Browse to suppress depth-1 Home children that are already
    /// pinned in Locations. The filter is *not* propagated to deeper
    /// levels.
    fn append_tree_descendants_filtered(
        &self,
        rows: &mut Vec<TreeRowSpec>,
        parent: &Path,
        depth: usize,
        current: &Path,
        skip_paths: Option<&HashSet<PathBuf>>,
        cx: &App,
    ) {
        let Some(children) = self.tree_children.get(parent) else {
            return;
        };
        let favs = self.process.favorites.read(cx);
        for child in children {
            // `hidden` resolved at load time with platform semantics
            // (FileEntry::hidden contract) — pure flag read on render.
            if !self.show_hidden && child.hidden {
                continue;
            }
            if let Some(skip) = skip_paths {
                if skip.contains(&child.path) {
                    continue;
                }
            }
            let is_expanded = self.expanded.contains(&child.path);
            let favorited = favs.contains_path(&child.path);
            rows.push(TreeRowSpec {
                node_id: child.node_id,
                path: child.path.clone(),
                label: SharedString::from(child.label.clone()),
                depth,
                is_expandable: true,
                is_expanded,
                is_active: &child.path == current,
                capacity: None,
                icon: TreeRowIcon::Folder,
                favorited,
            });
            if is_expanded {
                self.append_tree_descendants(rows, &child.path, depth + 1, current, cx);
            }
        }
    }

    /// Either the file Table, or an inline error/empty state when
    /// the directory couldn't be listed (typically macOS TCC denial
    /// on ~/Documents, ~/Desktop, ~/Downloads in a sandboxed runner).
    fn file_pane_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(err) = self.active_tab().last_error.clone() {
            let (title, body) = error_copy(&err);
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .p_8()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child(title),
                )
                .child(
                    div()
                        .max_w(px(420.0))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(body),
                )
                .into_any_element();
        }
        DataTable::new(&self.active_tab().table)
            .bordered(false)
            .stripe(true)
            .small()
            .into_any_element()
    }

    /// Tabstrip above the toolbar. Each tab is a clickable pill
    /// labelled with the directory's basename; the active tab has
    /// a filled background. A trailing "+" opens a new tab; each
    /// non-active tab has a small "x" hover-affordance to close.
    fn tabstrip(&self, cx: &mut Context<Self>) -> Div {
        let active = self.active;
        let multi = self.tabs.len() > 1;
        let mut row = h_flex()
            .w_full()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary);

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
            let mut chip = h_flex()
                .id(("tab", idx))
                .items_center()
                .gap_1()
                .px_3()
                .py_1()
                .rounded(theme.radius)
                .cursor_pointer()
                .text_sm()
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
                    },
                    |payload, _offset, _window, cx| cx.new(|_| payload.clone()),
                );
            if multi {
                let close = div()
                    .id(("tab-close", idx))
                    .ml_1()
                    .px_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .hover(|this| this.text_color(theme.foreground))
                    .child("x")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        // Phase A+B+C: tabs own their own TableState,
                        // and closing the last tab closes the window
                        // (process stays resident).
                        // Phase D: snapshot before removing so
                        // Cmd+Shift+T can reopen this tab. The lookup
                        // is by TabId, not the captured `idx`, because
                        // a drag-reorder may have shifted positions
                        // since this listener was constructed.
                        let Some(target_idx) =
                            this.tabs.iter().position(|t| t.id == tab_id)
                        else {
                            return;
                        };
                        if this.tabs.len() <= 1 {
                            this.process
                                .push_closed_tab(this.tabs[target_idx].snapshot_for_close());
                            window.remove_window();
                            return;
                        }
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
        // Phase D: trailing drop gap after the last tab.
        row = row.child(tab_drop_gap(self.tabs.len(), cx));
        // Trailing "+" — new tab.
        row = row.child(
            div()
                .id("tab-new")
                .ml_1()
                .px_2()
                .py_1()
                .rounded(cx.theme().radius)
                .cursor_pointer()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .hover(|this| this.bg(cx.theme().accent.opacity(0.10)))
                .child("+")
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
        row
    }

    /// Toolbar row above the breadcrumb: Back / Forward buttons +
    /// "Show hidden" toggle. Disabled buttons grey out via Button's
    /// own disabled state — no manual styling.
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
        use gpui_component::sidebar::SidebarToggleButton;
        let can_back = self.active_tab().history_index > 0;
        let can_forward = self.active_tab().history_index + 1 < self.active_tab().history.len();
        let collapsed = self.sidebar_collapsed;
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
                        .child(
                            SidebarToggleButton::new()
                                .collapsed(collapsed)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.sidebar_collapsed = !this.sidebar_collapsed;
                                    let mut s = app_state::load();
                                    s.sidebar_collapsed = Some(this.sidebar_collapsed);
                                    app_state::save(&s);
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child("Feraille"),
                )
                .child(
                    Button::new("nav-back")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/chevron-left.svg"))
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
                        .icon(gpui_component::Icon::empty().path("icons/chevron-right.svg"))
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
                ),
        )
    }

    /// Build the preview pane on the right of the file list. Shows
    /// title / kind / size / modified / full path of the selected
    /// row. Falls back to a neutral empty state when nothing is
    /// selected. Format-specific previews (image, text, PDF) arrive
    /// in a follow-up polish iter.
    fn preview(&self, cx: &mut Context<Self>) -> Div {
        use gpui_component::{
            Sizable as _,
            button::{Button, ButtonVariants as _},
            description_list::{DescriptionItem, DescriptionList},
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

        let header = div()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.theme().muted_foreground)
            .child("Preview");

        let body: AnyElement = match selected {
            None => div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No selection")
                .into_any_element(),
            Some(entry) => {
                let mut full_path = self.active_tab().current_dir.clone();
                full_path.push(&entry.name);
                let path_str = full_path.to_string_lossy().into_owned();
                let format_label_text = {
                    let (label, _) = entry.format_label();
                    if label.is_empty() {
                        match entry.kind {
                            EntryKind::Directory => "Folder".to_string(),
                            EntryKind::File => "File".to_string(),
                            EntryKind::Symlink => "Symlink".to_string(),
                        }
                    } else {
                        label
                    }
                };
                let path_display = middle_truncate_path(&path_str, 44);

                // Quick Look thumbnail (Stage 8 native preview).
                // `preview::request` was kicked off when the row
                // was selected; this just reads whatever the cache
                // has — Loaded shows the bitmap, Pending shows a
                // muted placeholder, Failed shows nothing.
                let thumb_state = self.process.preview_cache.borrow().get(&full_path);
                let thumb_img = crate::preview::loaded_image(thumb_state.clone());

                let mut col = v_flex().gap_3();
                if let Some(img) = thumb_img {
                    col = col.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w_full()
                            .h(px(200.0))
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().secondary.opacity(0.5))
                            .child(gpui::img(img).max_w(px(248.0)).max_h(px(184.0))),
                    );
                } else if matches!(thumb_state, Some(crate::preview::PreviewState::Pending)) {
                    col = col.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w_full()
                            .h(px(200.0))
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().secondary.opacity(0.5))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Loading preview\u{2026}"),
                    );
                }

                // Filename header — truncated, with a tooltip that
                // carries the full name. The format label that used
                // to sit here as a subtitle has moved into the
                // DescriptionList below as the "Format" row, so the
                // same string isn't shown twice.
                let name_for_tooltip = entry.name.clone();
                col = col.child(
                    div()
                        .id(("preview-name", entry.id.as_raw() as usize))
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .truncate()
                        .child(SharedString::from(entry.name.clone()))
                        .tooltip(move |window, cx| {
                            Tooltip::new(SharedString::from(name_for_tooltip.clone()))
                                .build(window, cx)
                        }),
                );

                // DescriptionList: dense key/value rows. Path uses
                // a middle-truncated value + tooltip with the full
                // path. The library handles label-column sizing.
                let path_for_tooltip = path_str.clone();
                let path_value: AnyElement = div()
                    .id(("preview-path", entry.id.as_raw() as usize))
                    .truncate()
                    .child(SharedString::from(path_display))
                    .tooltip(move |window, cx| {
                        Tooltip::new(SharedString::from(path_for_tooltip.clone())).build(window, cx)
                    })
                    .into_any_element();

                // `vertical()` is a constructor — label above value
                // per row. `columns(1)` keeps it as a single column
                // in narrow preview panes where multi-column would
                // squeeze values to nothing.
                let list = DescriptionList::vertical()
                    .small()
                    .columns(1)
                    .child(
                        DescriptionItem::new("Format").value(SharedString::from(format_label_text)),
                    )
                    .child(
                        DescriptionItem::new("Size")
                            .value(SharedString::from(entry.display_size.clone())),
                    )
                    .child(
                        DescriptionItem::new("Modified")
                            .value(SharedString::from(entry.display_mtime.clone())),
                    )
                    .child(DescriptionItem::new("Where").value(path_value));
                col = col.child(list);

                // Quarantine surface — single signal via the red
                // badge. (The DescriptionList "Quarantine" row that
                // used to repeat `com.apple.quarantine` was dropped
                // — the xattr name isn't actionable user info.) The
                // rich originating-URL details from
                // LSQuarantineDataURLKey still land in feraille-meta
                // and can populate the badge tooltip in a follow-on
                // polish iter.
                if entry.is_quarantined {
                    col = col.child(
                        div()
                            .mt_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(gpui::rgb(0xFF3B30))
                            .child("Quarantined \u{00B7} Mark of the Web"),
                    );
                }

                // Action row — icon-only buttons with tooltips that
                // include the keyboard shortcut. Icon-only keeps the
                // row dense enough that all four buttons fit even at
                // the preview pane's narrow default width.
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
                    )
                    .child(
                        Button::new("preview-get-info")
                            .icon(gpui_component::Icon::empty().path("icons/info.svg"))
                            .xsmall()
                            .ghost()
                            .tooltip_with_action("Get Info", &GetInfo, Some(SHELL_CONTEXT))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.on_get_info(&GetInfo, window, cx);
                            })),
                    );
                col = col.child(actions);

                col.into_any_element()
            }
        };

        v_flex()
            .size_full()
            .min_h_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .p_4()
            .gap_3()
            .child(header)
            .child(body)
    }

    /// Build the breadcrumb row from `current_dir`. Each ancestor is
    /// clickable and navigates the pane to that level. The root `/`
    /// gets its own leading segment. When `breadcrumb_editing` is
    /// set (Cmd+L) the row swaps in an Input field instead — Enter
    /// commits the path, Blur cancels.
    fn breadcrumb(&self, cx: &mut Context<Self>) -> Div {
        if self.breadcrumb_editing {
            return h_flex()
                .w_full()
                .items_center()
                .gap_1()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .flex_1()
                        .child(Input::new(&self.breadcrumb_input).small()),
                );
        }
        let segments = path_segments(&self.active_tab().current_dir);
        let mut row = h_flex()
            .w_full()
            .items_center()
            .gap_1()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border);

        for (i, (label, path)) in segments.iter().enumerate() {
            if i > 0 {
                row = row.child(
                    div()
                        .px_1()
                        .text_xs()
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
                .text_sm()
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
                            .w(px(11.0))
                            .h(px(11.0))
                            .text_color(cx.theme().primary)
                            .flex_shrink_0(),
                    )
                })
                .tooltip({
                    let t = SharedString::from(tooltip_path);
                    move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(t.clone()).build(window, cx)
                    }
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.navigate(path_for_click.clone(), cx);
                }))
                .context_menu(move |menu, _window, cx| {
                    let favorited_now = if let Some(s) = weak_for_crumb.upgrade() {
                        let already = s
                            .read(cx)
                            .process
                            .favorites
                            .read(cx)
                            .contains_path(&path_for_menu);
                        s.update(cx, |shell, _| {
                            shell.context_target = Some(path_for_menu.clone());
                            shell.favorites_context_path = Some(path_for_menu.clone());
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
                    menu.menu("Open in New Tab", Box::new(OpenContextInNewTab))
                        .separator()
                        .menu(feraille_core::commands::REVEAL_LABEL, Box::new(RevealContextPath))
                        .menu("Copy Path", Box::new(CopyContextPath))
                        .separator()
                        .menu(favorite_label, Box::new(ToggleFavoriteForTarget))
                        .separator()
                        .menu("New Folder Here", Box::new(NewFolderHere))
                });
            row = row.child(crumb);
        }
        row
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
        }
        let weak = cx.weak_entity();
        let locations_menu = self.build_locations_menu(weak.clone(), cx);
        let favorites_section = self.build_user_favorites_section(weak.clone(), cx);
        let browse_rows = self.build_browse_rows(cx);
        let volumes_rows = self.build_volumes_rows(cx);
        let has_volumes = !self.process.volumes.borrow().is_empty();
        let breadcrumb = self.breadcrumb(cx);
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
            .child(ShellSidebarItem::group(
                SidebarGroup::new("Locations").child(locations_menu),
            ))
            .child(favorites_section)
            .child(ShellSidebarItem::tree(TreeSection::new(
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
        let total_size: u64 = entries.iter().map(|e| e.size).sum();
        // Multi-select stats: count the whole selection set and sum
        // the visible entries' sizes for the rows that are members.
        // Iterating `entries` once is O(N) and the membership check
        // is an O(1) HashSet hit per row.
        let selection = &self.active_tab().selection;
        let selected_count = selection.len();
        let selected_size: u64 = entries
            .iter()
            .filter(|e| selection.contains(&e.id))
            .map(|e| e.size)
            .sum();
        // Free-space query — sync, very cheap on macOS (statvfs).
        // Returns None on non-macOS or for paths we can't reach.
        let volume_info = feraille_fs_native::volume_info_for_path(&self.active_tab().current_dir);
        let (free_bytes, volume_name): (Option<u64>, Option<&'static str>) = match volume_info {
            Some(v) => {
                let name: Option<&'static str> = Some(Box::leak(v.name.into_boxed_str()));
                (v.available_bytes, name)
            }
            None => (None, None),
        };
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
        let toggle_task_panel: Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static> = {
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
        let toggle_hidden_cb: Rc<dyn Fn(&mut Window, &mut App) + 'static> = {
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
        let task_panel =
            crate::task_panel::render_if_open(self.task_panel_open, &self.process.tasks, cx);

        div()
            .key_context(SHELL_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_navigate_parent))
            .on_action(cx.listener(Self::on_navigate_back))
            .on_action(cx.listener(Self::on_navigate_forward))
            .on_action(cx.listener(Self::on_open_selected))
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_toggle_hidden))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_copy_path))
            .on_action(cx.listener(Self::on_open_terminal_here))
            .on_action(cx.listener(Self::on_reveal_in_finder))
            .on_action(cx.listener(Self::on_move_to_trash))
            .on_action(cx.listener(Self::on_focus_filter))
            .on_action(cx.listener(Self::on_clear_filter))
            .on_action(cx.listener(Self::on_new_folder))
            .on_action(cx.listener(Self::on_rename_selected))
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
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_zoom_reset))
            .on_action(cx.listener(Self::on_open_in_new_tab))
            .on_action(cx.listener(Self::on_duplicate))
            .on_action(cx.listener(Self::on_make_alias))
            .on_action(cx.listener(Self::on_compress))
            .on_action(cx.listener(Self::on_reveal_context_path))
            .on_action(cx.listener(Self::on_copy_context_path))
            .on_action(cx.listener(Self::on_open_terminal_at_context))
            .on_action(cx.listener(Self::on_open_context_in_new_tab))
            .on_action(cx.listener(Self::on_new_folder_here))
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
            .on_action(cx.listener(Self::on_reset_favorite_name))
            .on_action(cx.listener(Self::on_reset_favorite_icon))
            .on_action(cx.listener(Self::on_set_favorite_icon_star))
            .on_action(cx.listener(Self::on_set_favorite_icon_folder))
            .on_action(cx.listener(Self::on_set_favorite_icon_code))
            .on_action(cx.listener(Self::on_set_favorite_icon_image))
            .on_action(cx.listener(Self::on_set_favorite_icon_music))
            .on_action(cx.listener(Self::on_set_favorite_icon_archive))
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
                use gpui_component::resizable::{h_resizable, resizable_panel};
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
                let file_body_wrapped = div().flex_1().min_h_0().min_w_0().child(file_body);
                // Auto-hide the preview when the window is too narrow
                // to fit sidebar + file list + preview comfortably.
                // The user's explicit `preview_visible` flag still
                // wins when there's room — the threshold only
                // suppresses the pane, never re-enables it.
                let viewport_w = f32::from(window.viewport_size().width);
                let preview_visible =
                    self.preview_visible && viewport_w >= PREVIEW_AUTOHIDE_THRESHOLD;
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
                    px(48.0)
                } else {
                    px(self.sidebar_width)
                };
                let preview_width_px = px(self.preview_width);
                let weak = cx.weak_entity();
                let splitter = h_resizable("shell-splitter")
                    .with_state(&self.splitter_state)
                    .on_resize(move |state, _window, cx| {
                        // Callback fires per drag tick. Read sizes
                        // out of the ResizableState, write them back
                        // into Shell so the next render re-applies
                        // them, and push to disk through the
                        // throttled writer.
                        let sizes = state.read(cx).sizes().clone();
                        if let Some(s) = weak.upgrade() {
                            s.update(cx, |this, _cx| {
                                if let Some(sw) = sizes.first() {
                                    this.sidebar_width = f32::from(*sw);
                                }
                                if preview_visible && sizes.len() >= 3 {
                                    this.preview_width = f32::from(sizes[2]);
                                }
                                this.maybe_persist_splitter();
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
                                this.size_range(px(48.0)..px(48.0))
                            })
                            .when(!self.sidebar_collapsed, |this| {
                                this.size_range(px(160.0)..px(400.0))
                            })
                            .child(sidebar),
                    )
                    .child(
                        resizable_panel().child(
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
                            .size_range(px(220.0)..px(520.0))
                            .child(pane),
                    )
                } else {
                    splitter
                };
                let title_bar = self.title_bar(cx);
                let menu_bar = self.menu_bar.clone();
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
