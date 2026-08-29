use super::*;
use crate::text::{IconScale as _, TruncateMiddle as _};
use gpui_component::ElementExt as _;
use gpui_component::scroll::ScrollableElement as _;

/// Minimum width for rendered-markdown preview content, so its prose
/// reads as a column instead of folding to slivers in the narrow preview
/// pane. The box scrolls horizontally to reach overflow when the pane is
/// narrower than this; a wider pane lets the content grow past it.
pub(crate) const PREVIEW_MD_MIN_W: f32 = 520.0;

/// Code/source preview: a `whitespace_nowrap` code block clips long lines
/// but won't grow its container past the pane on its own, so the box has
/// nothing to scroll toward. We give the content a definite width sized to
/// the widest line — these tune that estimate. Width per column at the 9px
/// mono used in the code block; slightly over the real ~5.4px advance so
/// the last glyphs aren't clipped (a little slop on the right is fine,
/// lost characters are not).
pub(crate) const PREVIEW_CODE_CHAR_W: f32 = 5.8;
/// A horizontal tab counts as this many columns when measuring the widest
/// line (source is commonly tab-indented; 1 char would under-size it).
pub(crate) const PREVIEW_CODE_TAB_COLS: usize = 4;
/// Box + code-block horizontal padding added to the measured line width.
pub(crate) const PREVIEW_CODE_PAD: f32 = 48.0;
/// Upper bound on the sized width so a minified single-line file doesn't
/// build a multi-thousand-pixel element (it clips past this — rare).
pub(crate) const PREVIEW_CODE_MAX_W: f32 = 4000.0;
/// Render-aware ceiling for preview text. The worker already caps source
/// lines; this additionally bounds heavily wrapped prose by visual lines.
pub(crate) const PREVIEW_TEXT_MAX_VISUAL_LINES: usize = 1000;

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
pub(crate) fn truncated_url_value(
    key: &'static str,
    url: &str,
    id: ferail_core::NodeId,
) -> AnyElement {
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

/// Per-cell drag payload for an unselected cell, which drags only itself.
type GridCellDrag = (
    smallvec::SmallVec<[PathBuf; 2]>,
    smallvec::SmallVec<[bool; 2]>,
    smallvec::SmallVec<[Arc<gpui::RenderImage>; crate::file_list::GHOST_STACK_CAP]>,
    smallvec::SmallVec<[SharedString; crate::file_list::GHOST_STACK_CAP]>,
    Option<Arc<AtomicBool>>,
);

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

fn icon_size_range() -> crate::scrub_slider::ScrubRange {
    crate::scrub_slider::ScrubRange::new(
        crate::grid::MIN_ICON_SIZE as f32,
        crate::grid::MAX_ICON_SIZE as f32,
        4.0,
    )
}

impl Shell {
    /// Resolve a sidebar path through the UI-side identity cache first. The
    /// first encounter still registers the backend-provided id; subsequent
    /// repaints avoid `NativeFs`'s mutex and lexical `PathBuf` normalization.
    fn sidebar_node_id(&self, path: &Path) -> NodeId {
        if let Some(id) = self.process.node_store.borrow().cached_id_for_path(path) {
            return id;
        }
        let id = self.process.fs.id_for_path(path);
        self.process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(path.to_path_buf(), id)
    }

    fn tool_result_breadcrumb_summary(&self) -> Option<String> {
        let surface = self.active_tab().tool_result.as_ref()?;
        match &surface.mode {
            // Text only — the 🔍/⧉ pictographs here rendered as tofu boxes
            // on fonts without them (AROS bundled font).
            super::tab::ToolResultMode::Search(search) => Some(format!(
                "{}  \u{00B7}  {}",
                crate::private_mode::present_label(&search.needle),
                crate::i18n::tr_static(search.engine_label)
            )),
            super::tab::ToolResultMode::Flat(flat) => Some(
                if flat.complete {
                    trn!(
                        "{n} file · all subfolders",
                        "{n} files · all subfolders",
                        flat.progress.matches as usize
                    )
                } else {
                    trn!(
                        "Scanning: {n} file in {dirs} folders",
                        "Scanning: {n} files in {dirs} folders",
                        flat.progress.matches as usize,
                        dirs = ferail_core::counts::format_count(flat.progress.dirs_scanned)
                    )
                }
                .to_string(),
            ),
            super::tab::ToolResultMode::Duplicates(dupe) => Some(
                if dupe.mode == ferail_fs_native::DupeMode::Similar {
                    trn!(
                        "{n} similar-image group · {size} reclaimable",
                        "{n} similar-image groups · {size} reclaimable",
                        dupe.groups,
                        size = ferail_fs_native::humanize_bytes(dupe.wasted_bytes)
                    )
                } else {
                    trn!(
                        "{n} duplicate group · {size} reclaimable",
                        "{n} duplicate groups · {size} reclaimable",
                        dupe.groups,
                        size = ferail_fs_native::humanize_bytes(dupe.wasted_bytes)
                    )
                }
                .to_string(),
            ),
            super::tab::ToolResultMode::DiskUsage(_) => Some(tr!("Disk Usage").to_string()),
            super::tab::ToolResultMode::Archive(am) => Some(
                am.archive
                    .file_name()
                    .map(|s| crate::private_mode::present_leaf_str(&s.to_string_lossy(), false))
                    .unwrap_or_else(|| tr!("Archive").to_string()),
            ),
            super::tab::ToolResultMode::Verify(vm) => Some(
                vm.manifest
                    .file_name()
                    .map(|name| {
                        crate::private_mode::present_leaf_str(&name.to_string_lossy(), false)
                    })
                    .unwrap_or_else(|| tr!("Checksum manifest").to_string()),
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
                | Some(super::tab::ToolResultMode::Archive(_))
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
        let node_id = self.sidebar_node_id(&home);
        let is_expanded = self.expanded.contains(&home);
        let favorited = self.process.favorites().read(cx).contains_path(&home);
        let mut rows: Vec<TreeRowSpec> = vec![TreeRowSpec {
            node_id,
            path: home.clone(),
            label: tr!("Home"),
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
    fn build_user_favorites_section(&mut self, weak: WeakEntity<Self>) -> ShellSidebarItem {
        ShellSidebarItem::favorites(crate::favorites_section::FavoritesSection::new(
            self.process.favorites(),
            self.favorites_section_collapsed,
            weak,
            self.process.icons.clone(),
            crate::tree::SIDEBAR_ICON_PX,
            self.active_tab().current_dir.clone(),
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
    fn build_recents_section(&self, weak: WeakEntity<Self>, cx: &App) -> Option<ShellSidebarItem> {
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
                crate::tree::SIDEBAR_ICON_PX,
                self.active_tab().current_dir.clone(),
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
                        .title(tr!("Clear Recents?"))
                        .child(div().text_scale_sm().child(tr!(
                            "Forget every folder in Recents? Your Ant Trail heat is \
                             kept \u{2014} only the recent list is emptied. This can't \
                             be undone."
                        )))
                        .child(
                            h_flex().pt_2().child(
                                Button::new("clear-recents-go")
                                    .label(tr!("Clear Recents"))
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

    /// Build the **Locations** section: a flat list of icon-prefixed
    /// shortcuts to the OS-standard folders. Each row navigates straight to
    /// its path; none expand, so the IA stays unambiguous next to the
    /// user-curated Favorites section below and the Browse tree underneath.
    ///
    /// These render through [`crate::locations_section`] rather than
    /// gpui-component's `SidebarMenu`, because that widget exposes no drop
    /// hooks — Locations silently rejected every drag until this moved to
    /// rows we own. Building stays here, where `&Shell` state lives.
    fn build_locations_rows(&mut self, cx: &App) -> Vec<crate::locations_section::LocationRow> {
        let current = self.active_tab().current_dir.clone();
        let favs = self.process.favorites().read(cx);
        let mut rows = Vec::new();
        for loc in crate::special_folders::locations(cx).iter() {
            let path = loc.path.clone();
            let node_id = self.sidebar_node_id(&path);
            rows.push(crate::locations_section::LocationRow {
                node_id,
                is_active: path == current,
                favorited: favs.contains_path(&path),
                // In-memory lookup only — the iCloud probe ran off-thread at
                // startup / volume refresh (ProcessState::cloud_locations).
                // `None` = not an iCloud Location; `Some(..)` drives the
                // solid-vs-outline trailing cloud badge.
                cloud: self.process.cloud_locations.borrow().get(&path).copied(),
                label: crate::i18n::tr_static(loc.label),
                icon: loc.icon,
                path,
            });
        }
        rows
    }

    /// Cached dynamic roots (currently Windows WSL distributions). This is
    /// O(number of distributions) and does not touch the registry, launch a
    /// process or probe a UNC path while rendering.
    fn build_platform_location_rows(&self) -> Vec<crate::locations_section::PlatformLocationRow> {
        let current = &self.active_tab().current_dir;
        self.process
            .platform_locations
            .borrow()
            .roots()
            .iter()
            .map(|root| crate::locations_section::PlatformLocationRow {
                id: root.id.clone(),
                label: root.label.to_string().into(),
                state: root.state.clone(),
                version: root.version,
                is_default: root.is_default,
                is_active: matches!(
                    &root.state,
                    ferail_core::platform_locations::PathBackedRootState::Ready(path)
                        if path == current
                ),
            })
            .collect()
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
        let favs = self.process.favorites().read(cx);
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
                (
                    v.path.clone(),
                    v.name.clone(),
                    cap,
                    v.is_removable,
                    !v.is_local,
                )
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
            let node_id = self.sidebar_node_id(&path);
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
        let favs = self.process.favorites().read(cx);
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
    fn platform_namespace_body(&self, cx: &mut Context<Self>) -> AnyElement {
        use ferail_core::platform_namespace::{
            PlatformItemFlags, PlatformItemKind, PlatformSurfacePhase,
        };
        use gpui_component::menu::ContextMenuExt as _;

        let Some(session) = self.active_tab().platform_namespace.as_ref() else {
            return div().into_any_element();
        };
        let centered = |message: SharedString| {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    svg()
                        .path("icons/inbox.svg")
                        .icon_px(48.0)
                        .text_color(cx.theme().muted_foreground.opacity(0.5)),
                )
                .child(
                    div()
                        .text_scale_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(message),
                )
                .into_any_element()
        };
        match session.phase() {
            PlatformSurfacePhase::Idle | PlatformSurfacePhase::Loading
                if session.store().items().is_empty() =>
            {
                return centered(tr!("Loading…"));
            }
            PlatformSurfacePhase::Unavailable(_) => {
                return centered(tr!("Unavailable"));
            }
            PlatformSurfacePhase::Ready if session.store().items().is_empty() => {
                return centered(tr!("This folder is empty."));
            }
            _ => {}
        }

        let row_count = session.store().items().len();
        let tab_id = self.active_tab().id;
        let weak = cx.weak_entity();
        let selected_bg = cx.theme().accent.opacity(0.20);
        let hover_bg = cx.theme().accent.opacity(0.10);
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let scroll = session.scroll().clone();
        let list = uniform_list(
            "platform-namespace-list",
            row_count,
            move |visible, _window, app| {
                let Some(shell_entity) = weak.upgrade() else {
                    return Vec::new();
                };
                let shell = shell_entity.read(app);
                let Some(session) = shell
                    .tabs
                    .iter()
                    .find(|tab| tab.id == tab_id)
                    .and_then(|tab| tab.platform_namespace.as_ref())
                else {
                    return Vec::new();
                };
                visible
                    .filter_map(|index| {
                        let item = session.store().items().get(index)?;
                        let item_id = item.id;
                        let selected = session.selection().is_selected(&item_id);
                        let hidden = item.flags.contains(PlatformItemFlags::HIDDEN);
                        let native_menu = item.capabilities.contains(
                            ferail_core::platform_namespace::PlatformCapabilities::NATIVE_MENU,
                        );
                        let icon = match item.kind {
                            PlatformItemKind::Container => "icons/folder.svg",
                            PlatformItemKind::Link => "icons/file/symlink.svg",
                            PlatformItemKind::File => "icons/file/generic.svg",
                        };
                        let label: SharedString = item.label.to_string().into();
                        let tooltip = label.clone();
                        let row_shell = weak.clone();
                        let menu_shell = weak.clone();
                        Some(
                            h_flex()
                                .id(("platform-namespace-row", index))
                                .w_full()
                                .h(px(32.0))
                                .px_3()
                                .gap_2()
                                .cursor_pointer()
                                .text_color(if hidden { muted } else { foreground })
                                .when(hidden, |row| row.opacity(0.65))
                                .when(selected, |row| row.bg(selected_bg))
                                .when(!selected, |row| row.hover(|hover| hover.bg(hover_bg)))
                                .child(svg().path(icon).icon_px(18.0).flex_shrink_0())
                                .child(div().min_w_0().truncate().child(label))
                                .tooltip(move |window, cx| {
                                    gpui_component::tooltip::Tooltip::new(tooltip.clone())
                                        .build(window, cx)
                                })
                                .on_click(move |event: &ClickEvent, _window, app| {
                                    let toggle = event.modifiers().platform;
                                    let activate = event.click_count() >= 2;
                                    let _ = row_shell.update(app, |shell, cx| {
                                        shell.click_platform_item(
                                            tab_id, item_id, toggle, activate, cx,
                                        );
                                    });
                                })
                                .context_menu(move |menu, window, app| {
                                    if !native_menu {
                                        return menu;
                                    }
                                    let extended = window.modifiers().shift;
                                    if extended {
                                        let win = window.window_handle();
                                        let _ = menu_shell.update(app, |shell, cx| {
                                            shell.show_platform_native_menu(
                                                tab_id, item_id, true, win, cx,
                                            );
                                        });
                                        return menu;
                                    }
                                    let action_shell = menu_shell.clone();
                                    menu.item(
                                        gpui_component::menu::PopupMenuItem::new(tr!("More…"))
                                            .on_click(move |_event, window, app| {
                                                let win = window.window_handle();
                                                let _ = action_shell.update(app, |shell, cx| {
                                                    shell.show_platform_native_menu(
                                                        tab_id, item_id, false, win, cx,
                                                    );
                                                });
                                            }),
                                    )
                                })
                                .into_any_element(),
                        )
                    })
                    .collect()
            },
        )
        .track_scroll(&scroll)
        .size_full();
        div()
            .relative()
            .size_full()
            .child(list)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(16.0))
                    .child(gpui_component::scroll::Scrollbar::vertical(&scroll)),
            )
            .size_full()
            .into_any_element()
    }

    fn file_pane_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.active_tab().platform_namespace.is_some() {
            return self.platform_namespace_body(cx);
        }
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
                            // Put Ferail's own path on the clipboard so
                            // the user can paste it into the Full Disk
                            // Access "+" sheet via Go to Folder.
                            if let Some(path) = crate::platform_shell::app_bundle_path() {
                                cx.write_to_clipboard(ClipboardItem::new_string(path));
                                window.push_notification(
                                    Notification::info(tr!(
                                        "Ferail's path is copied. In the picker, click \
                                         \"+\", press \u{2318}\u{21e7}G, paste, and add it."
                                    ))
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
        if let Some(super::tab::ToolResultMode::Verify(vm)) = self
            .active_tab()
            .tool_result
            .as_ref()
            .map(|surface| &surface.mode)
        {
            return vm.view.clone().into_any_element();
        }
        match self.active_tab().view_mode {
            crate::grid::ViewMode::List => {
                let table = self.active_tab().table.clone();
                let marquee_rect = self
                    .active_tab()
                    .marquee
                    .as_ref()
                    .filter(|m| m.moved && m.surface == super::tab::MarqueeSurface::List)
                    .map(|m| {
                        let origin = table.read(cx).table_bounds().origin;
                        let left = m.start.x.min(m.current.x) - origin.x;
                        let top = m.start.y.min(m.current.y) - origin.y;
                        let width = (m.start.x - m.current.x).abs();
                        let height = (m.start.y - m.current.y).abs();
                        (left, top, width, height)
                    });
                let fill = crate::selection_colors::fill(cx);
                let border = crate::selection_colors::strong(cx);
                div()
                    .relative()
                    .size_full()
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(Self::on_list_marquee_down),
                    )
                    .on_mouse_move(cx.listener(Self::on_list_marquee_move))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(Self::on_list_marquee_up),
                    )
                    .on_mouse_up_out(
                        gpui::MouseButton::Left,
                        cx.listener(Self::on_list_marquee_up),
                    )
                    .child(DataTable::new(&table).bordered(false).stripe(true).small())
                    .when_some(marquee_rect, |container, (left, top, width, height)| {
                        container.child(
                            div()
                                .absolute()
                                .left(left)
                                .top(top)
                                .w(width)
                                .h(height)
                                .bg(fill)
                                .border_1()
                                .border_color(border)
                                .rounded(px(2.0)),
                        )
                    })
                    .into_any_element()
            }
            crate::grid::ViewMode::Grid => self.grid_body(cx),
        }
    }

    /// Icon (grid) view of the active tab's listing. A virtualized
    /// `uniform_list` of `cols`-wide rows, reading the same delegate
    /// `entries` + selection mirror the table does and routing every
    /// gesture through the same `Shell` methods. See `crate::grid`.
    fn grid_body(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::file_list::{DragBadge, GHOST_STACK_CAP};
        use crate::multi_table::PlatformContextMenuExt as _;
        use crate::thumbnails::THUMB_PX;
        use ferail_core::EntryKind;
        use gpui::ExternalPaths;
        use smallvec::SmallVec;
        use std::sync::Arc;

        let icon_px = crate::grid::icon_size(cx);
        let slot = icon_px as f32;
        // How thumbnails fill the square slot (Settings → Layout → Icon
        // fit). It also picks the fetch bucket: every mode but Best fit
        // magnifies the image past shrink-to-fit, so it needs more source
        // pixels to stay crisp.
        let fit = crate::grid::thumb_fit(cx);
        let bucket = crate::grid::thumb_bucket(icon_px, fit);
        let icon_bucket = crate::grid::folder_icon_bucket(icon_px);
        let show_thumbs = crate::thumbnails::show_thumbnails(cx);
        let gap = crate::grid::cell_gap(cx);
        let cell_w = crate::grid::cell_width(icon_px, gap);
        let cell_h = crate::grid::cell_height(icon_px, gap);
        let grid_name_budget = ((cell_w - gap * 2.0 - 10.0) / 8.0).floor().max(10.0) as usize;

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
            let _guard = ferail_core::path_guard::enter_render();
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
            let can_begin_drag = !app.has_active_drag();

            // The list and grid now share one lazily-built selection snapshot,
            // invalidated only when the model or selection changes. Before,
            // every drag repaint made the grid walk all entries, resolve every
            // selected path, and probe the image caches again.
            let sel_drag = can_begin_drag.then(|| del.drag_snapshot(app)).flatten();

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
                    // The tab owns selection state. Marquee drag updates this
                    // incrementally and mirrors the list delegate at mouse-up.
                    let selected = tab.is_selected(id);
                    let is_lead = del.lead == Some(id);
                    let quarantined = entry.is_quarantined;
                    // Display leaf (macOS `:` → `/`) for the grid label/tooltip;
                    // deceptive names get the same highlighted treatment as the
                    // list row so switching to grid view never hides a disguise.
                    let name: SharedString = crate::private_mode::present_leaf_str(
                        &entry.display_name,
                        matches!(entry.kind, EntryKind::Directory),
                    )
                    .into();
                    let tooltip_name: SharedString = name.clone();
                    let inline_session = shell.inline_name_edit.snapshot().filter(|session| {
                        session.target
                            == crate::inline_edit::FileNameEditTarget {
                                tab_id: tab.id.0,
                                node_id: id,
                            }
                    });
                    let is_editing = inline_session.is_some();
                    let grid_label: AnyElement = if let Some(session) = inline_session {
                        crate::inline_edit::InlineEditor::new(
                            ("inline-grid-name", id.as_raw()),
                            crate::inline_edit::InlineEditInput::Text(
                                shell.inline_name_input.clone(),
                            ),
                            crate::inline_edit::InlineEditLayout::Grid,
                            &session,
                            tr!("File name"),
                        )
                        .into_any_element()
                    } else if entry.name_has_hazards && !crate::private_mode::enabled() {
                        crate::entry_info::name_hazard_element_elided(
                            &name,
                            SharedString::from(format!("grid-name-{i}")),
                            grid_name_budget,
                        )
                    } else {
                        name.clone().into_any_element()
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
                    let show_star = !is_editing
                        && adorn_visible
                        && cell_is_dir
                        && del.is_favorited.get(i).copied().unwrap_or(false);
                    // Finder colour tags → coloured dots, capped at 7.
                    let cell_tags: SmallVec<[gpui::Rgba; 7]> = if adorn_visible && !is_editing {
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
                            let thumb = if show_thumbs
                                && !crate::private_mode::enabled()
                                && crate::thumbnails::is_thumbnailable(entry)
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
                                // Always paint a full slot-sized element and
                                // let gpui's object-fit letterbox or crop
                                // inside it. Sizing the element to the scaled
                                // image instead would have Fill frame lay out
                                // an enormous element (a panorama covering a
                                // 512-px slot is ~100,000 px wide) purely to
                                // have it clipped back to the slot.
                                let dims = t.size(0);
                                let object_fit =
                                    fit.object_fit(u32::from(dims.width), u32::from(dims.height));
                                img(t)
                                    .w(px(slot))
                                    .h(px(slot))
                                    .object_fit(object_fit)
                                    .into_any_element()
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
                    // Archive cells take dropped files the same way archive
                    // rows in the list do (name-based, no probing here).
                    let archive_add_target =
                        crate::file_list::archive_drop_target(entry.name.as_ref(), entry.kind);
                    let (drag_paths, drag_dirs, ghost_icons, ghost_names, native_owned):
                        GridCellDrag =
                        if !can_begin_drag {
                            (
                                SmallVec::new(),
                                SmallVec::new(),
                                SmallVec::new(),
                                SmallVec::new(),
                                None,
                            )
                        } else if selected {
                            match &sel_drag {
                                Some(snapshot) => (
                                    SmallVec::from_vec(snapshot.paths.as_ref().clone()),
                                    SmallVec::from_vec(snapshot.dirs.as_ref().clone()),
                                    snapshot.icons.clone(),
                                    snapshot.names.clone(),
                                    Some(snapshot.native_owned.clone()),
                                ),
                                None => (
                                    SmallVec::new(),
                                    SmallVec::new(),
                                    SmallVec::new(),
                                    SmallVec::new(),
                                    None,
                                ),
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
                                    ferail_fs_native::paths::display_leaf(
                                        n.to_string_lossy().as_ref(),
                                    )
                                    .into_owned()
                                })
                                .unwrap_or_default()
                                .into();
                            (
                                SmallVec::from_vec(vec![path.clone()]),
                                SmallVec::from_vec(vec![is_dir]),
                                gi,
                                SmallVec::from_vec(vec![name]),
                                Some(Arc::new(AtomicBool::new(false))),
                            )
                        };
                    let drag_count = drag_paths.len();
                    let can_drag = !drag_paths.is_empty();
                    // Finder-style selection pill behind the label: full
                    // accent on the focused (lead) cell, slightly muted
                    // for other members of a multi-selection.
                    let label_pill = if is_lead { blue } else { blue.opacity(0.82) };

                    let weak_cell = weak.clone();
                    let weak_label_rename = weak.clone();
                    let weak_menu = weak.clone();
                    let weak_native_menu = weak.clone();
                    let weak_drop = weak.clone();
                    let weak_hover = weak.clone();
                    let weak_archive_drop = weak.clone();
                    let weak_archive_hover = weak.clone();
                    let weak_native_archive_drop = weak.clone();
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
                                .id(("grid-label", i))
                                .max_w_full()
                                .when(is_editing, |d| d.w_full())
                                .px(px(5.0))
                                .py(px(1.0))
                                .rounded(px(4.0))
                                .text_scale_xs()
                                .text_center()
                                .when(!is_editing, |d| d.truncate())
                                .when(selected && !is_editing, |d| {
                                    d.bg(label_pill).text_color(pill_fg)
                                })
                                .when(!selected && !is_editing, |d| d.text_color(muted))
                                .on_click(move |event: &ClickEvent, window, app| {
                                    let modifiers = event.modifiers();
                                    let modified = modifiers.platform
                                        || modifiers.control
                                        || modifiers.alt
                                        || modifiers.shift;
                                    if crate::inline_edit::should_begin_click_rename(
                                        selected,
                                        is_editing,
                                        event.click_count(),
                                        modified,
                                    ) {
                                        app.stop_propagation();
                                        let _ = weak_label_rename.update(app, |this, cx| {
                                            this.begin_inline_name_edit_at_row(i, window, cx);
                                        });
                                    }
                                })
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
                        // Cut cells dim until the move pastes; hidden
                        // entries dim more gently (mirrors the list
                        // rows — cut wins over hidden).
                        .when(is_cut, |d| d.opacity(0.45))
                        .when(!is_cut && entry.hidden, |d| d.opacity(0.6))
                        .child(inner)
                        // The label is `.truncate()`d, so surface the full
                        // name on hover (mirrors the list row's tooltip).
                        .when(!is_editing, |this| {
                            this.tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(tooltip_name.clone())
                                    .build(window, cx)
                            })
                        })
                        .on_click(move |ev: &ClickEvent, window, app| {
                            let mods = ev.modifiers();
                            let dbl = ev.click_count() >= 2;
                            let _ = weak_cell.update(app, |this, cx| {
                                window.focus(&this.active_tab().grid_focus, cx);
                                if dbl {
                                    this.activate_row(i, Some(window.window_handle()), cx);
                                } else {
                                    this.apply_row_click_gesture(i, mods, cx);
                                }
                            });
                        })
                        .on_mouse_down(
                            gpui::MouseButton::Right,
                            move |event: &gpui::MouseDownEvent, window, app| {
                                #[cfg(windows)]
                                if event.modifiers.shift {
                                    app.stop_propagation();
                                    let _ = weak_native_menu.update(app, |this, cx| {
                                        let was_selected = this
                                            .node_id_at_row(i, cx)
                                            .map(|id| this.active_tab().is_selected(id))
                                            .unwrap_or(false);
                                        this.apply_row_right_click(i, cx);
                                        this.context_row =
                                            if was_selected { None } else { Some(i) };
                                        this.on_show_windows_context_menu(
                                            &crate::shell::ShowWindowsContextMenu,
                                            window,
                                            cx,
                                        );
                                    });
                                }
                                #[cfg(not(windows))]
                                let _ = (event, window, app, &weak_native_menu);
                            },
                        )
                        .when(can_drag, |d| {
                            let native_owned =
                                native_owned.expect("draggable cell has handoff flag");
                            let native_owned_for_badge = native_owned.clone();
                            let native_owned_for_payload = native_owned.clone();
                            d.on_drag(
                                ExternalPaths(drag_paths),
                                move |_paths, offset, _window, cx| {
                                    native_owned_for_badge.store(false, Ordering::Release);
                                    cx.new(|_| DragBadge {
                                        names: ghost_names.clone(),
                                        icons: ghost_icons.clone(),
                                        count: drag_count,
                                        offset,
                                        native_owned: native_owned_for_badge.clone(),
                                    })
                                },
                            )
                            // Promote to a native drag session when the
                            // pointer leaves the window; dir-ness comes
                            // from the cached EntryKind, never a stat.
                            .external_drag_payload::<ExternalPaths>(move |paths, _window, _cx| {
                                native_owned_for_payload.store(true, Ordering::Release);
                                Some(gpui::ExternalDragPayload::Files(gpui::FileDragPaths::new(
                                    paths.paths().iter().cloned().zip(drag_dirs.iter().copied()),
                                )))
                            })
                        })
                        .when(
                            archive_add_target != crate::file_list::ArchiveDropTarget::No,
                            |d| {
                                let accepts = archive_add_target
                                    == crate::file_list::ArchiveDropTarget::Accepts;
                                let weak_archive_drop = weak.clone();
                                d.drag_over::<ExternalPaths>(move |style, _payload, _window, cx| {
                                    if accepts {
                                        style
                                            .cursor_copy()
                                            .border_1()
                                            .border_color(cx.theme().accent)
                                            .bg(cx.theme().accent.opacity(0.12))
                                    } else {
                                        style
                                            .cursor_not_allowed()
                                            .border_1()
                                            .border_color(cx.theme().danger)
                                            .bg(cx.theme().danger.opacity(0.08))
                                    }
                                })
                                .on_drop(move |paths: &ExternalPaths, window, app| {
                                    // Consume either way, so a refused archive
                                    // never falls through to the pane
                                    // background and lands in the folder.
                                    app.stop_propagation();
                                    if !accepts {
                                        return;
                                    }
                                    let dropped = paths.paths().to_vec();
                                    let _ = weak_archive_drop.update(app, |this, cx| {
                                        this.drop_onto_archive_row(i, dropped, window, cx);
                                    });
                                })
                                // Archive members dropped on an archive cell
                                // are added to it, same as on a list row.
                                .drag_over::<crate::file_list::ArchiveEntryDrag>(
                                    move |style, _payload, _window, cx| {
                                        if accepts {
                                            style
                                                .cursor_copy()
                                                .border_1()
                                                .border_color(cx.theme().accent)
                                                .bg(cx.theme().accent.opacity(0.12))
                                        } else {
                                            style
                                                .cursor_not_allowed()
                                                .border_1()
                                                .border_color(cx.theme().danger)
                                                .bg(cx.theme().danger.opacity(0.08))
                                        }
                                    },
                                )
                                .on_drop({
                                    let weak_member_drop = weak.clone();
                                    move |drag: &crate::file_list::ArchiveEntryDrag, window, app| {
                                        app.stop_propagation();
                                        if !accepts {
                                            return;
                                        }
                                        let archive = drag.archive.clone();
                                        let entries = drag.entries.clone();
                                        let password = drag.password.clone();
                                        let _ = weak_member_drop.update(app, |this, cx| {
                                            let Some(target) = this.path_for_row(i, cx) else {
                                                return;
                                            };
                                            this.add_archive_entries_to_archive(
                                                archive, entries, password, target, window, cx,
                                            );
                                        });
                                    }
                                })
                                // Cross-window promise sessions arrive as
                                // plain mouse events (GPUI-UPSTREAM #11).
                                .on_mouse_move(|_event, window, _app| {
                                    if crate::file_list::native_archive_drag_active() {
                                        window.refresh();
                                    }
                                })
                                .on_mouse_up(
                                    gpui::MouseButton::Left,
                                    {
                                        let weak_native_drop = weak.clone();
                                        move |_event, window, app| {
                                            if app.has_active_drag() {
                                                return;
                                            }
                                            let Some(drag) =
                                                crate::file_list::take_native_archive_drag()
                                            else {
                                                return;
                                            };
                                            app.stop_propagation();
                                            if !accepts {
                                                return;
                                            }
                                            let _ = weak_native_drop.update(app, |this, cx| {
                                                let Some(target) = this.path_for_row(i, cx) else {
                                                    return;
                                                };
                                                this.add_archive_entries_to_archive(
                                                    drag.archive,
                                                    drag.entries,
                                                    drag.password,
                                                    target,
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }
                                    },
                                )
                            },
                        )
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
                            .drag_over::<crate::file_list::ArchiveEntryDrag>(
                                move |style, _payload, _window, _cx| {
                                    style.border_1().border_color(blue).bg(blue.opacity(0.12))
                                },
                            )
                            .on_drop(
                                move |drag: &crate::file_list::ArchiveEntryDrag, window, app| {
                                    app.stop_propagation();
                                    let archive = drag.archive.clone();
                                    let entries = drag.entries.clone();
                                    let password = drag.password.clone();
                                    let _ = weak_archive_drop.update(app, |this, cx| {
                                        if let Some(dest) = this.path_for_row(i, cx) {
                                            this.extract_archive_entries_into(
                                                archive, entries, dest, password, window, cx,
                                            );
                                        }
                                    });
                                },
                            )
                            .on_drag_move(
                                move |e: &gpui::DragMoveEvent<
                                    crate::file_list::ArchiveEntryDrag,
                                >,
                                      _window,
                                      app| {
                                    if e.bounds.contains(&e.event.position) {
                                        let _ = weak_archive_hover.update(app, |this, cx| {
                                            this.spring_load_hover(i, cx);
                                        });
                                    }
                                },
                            )
                            .on_mouse_move(|_event, window, _app| {
                                if crate::file_list::native_archive_drag_active() {
                                    window.refresh();
                                }
                            })
                            .on_mouse_up(
                                gpui::MouseButton::Left,
                                move |_event, window, app| {
                                    if app.has_active_drag() {
                                        return;
                                    }
                                    let Some(drag) = crate::file_list::take_native_archive_drag()
                                    else {
                                        return;
                                    };
                                    app.stop_propagation();
                                    let _ = weak_native_archive_drop.update(app, |this, cx| {
                                        if let Some(dest) = this.path_for_row(i, cx) {
                                            this.extract_archive_entries_into(
                                                drag.archive,
                                                drag.entries,
                                                dest,
                                                drag.password,
                                                window,
                                                cx,
                                            );
                                        }
                                    });
                                },
                            )
                        })
                        .platform_context_menu(move |menu, window, cx| {
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
                                    .map(|id| this.active_tab().is_selected(id))
                                    .unwrap_or(false);
                                this.apply_row_right_click(i, cx);
                                this.context_row = if was_selected { None } else { Some(i) };
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
                    let load_generation = tab.load_generation;
                    let weak_warm = weak.clone();
                    app.defer(move |app| {
                        let _ = weak_warm.update(app, |this, cx| {
                            if this.active_tab().id != tab_id {
                                return;
                            }
                            let key =
                                (load_generation, entry_start, entry_end, bucket, icon_bucket);
                            if this.active_tab().grid_warm_key == Some(key) {
                                return;
                            }
                            this.active_tab_mut().grid_warm_key = Some(key);
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
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(Self::on_grid_marquee_up),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(Self::on_grid_marquee_up),
            )
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

    /// Set the live grid icon size without touching the settings file.
    ///
    /// The grid reads `grid::icon_size` during render, so writing the global
    /// is what makes cells resize under the cursor. Scrubbing the track goes
    /// through here on every mouse-move; only the release persists.
    fn set_icon_size_live(&self, px: u32, cx: &mut Context<Self>) {
        let px = crate::grid::clamp_icon_size(px);
        if crate::grid::icon_size(cx) != px {
            cx.set_global(crate::grid::IconSize(px));
            cx.notify();
        }
    }

    /// Set the grid icon size and persist it — the discrete controls
    /// (−/＋ and reset), where one click is one deliberate size.
    fn apply_icon_size(&self, px: u32, cx: &mut Context<Self>) {
        let px = crate::grid::clamp_icon_size(px);
        if crate::grid::icon_size(cx) == px {
            return;
        }
        self.set_icon_size_live(px, cx);
        crate::settings::persist_icon_size(px);
    }

    /// Step the grid icon size one stop along [`crate::grid::ICON_SIZES`]
    /// (toolbar −/＋). Only the sign of `delta` is read.
    ///
    /// Now that the track exists the current size is usually *not* on a
    /// stop, so this takes the next stop strictly past it rather than the
    /// nearest one: from 100 px, ＋ has to reach 128, and picking the
    /// nearest would pick 96 and read as the button moving the wrong way.
    fn step_icon_size(&self, delta: i32, cx: &mut Context<Self>) {
        let sizes = crate::grid::ICON_SIZES;
        let cur = crate::grid::icon_size(cx);
        let next = if delta >= 0 {
            sizes.iter().copied().find(|&s| s > cur)
        } else {
            sizes.iter().copied().rev().find(|&s| s < cur)
        };
        // Already past the last stop in that direction: nothing to do.
        if let Some(next) = next {
            self.apply_icon_size(next, cx);
        }
    }

    /// Reset the grid icon size to [`crate::grid::DEFAULT_ICON_SIZE`]
    /// (the toolbar ⟲ beside the stepper). One click back to sane after
    /// the track has been dragged somewhere extreme.
    fn reset_icon_size(&self, cx: &mut Context<Self>) {
        self.apply_icon_size(crate::grid::DEFAULT_ICON_SIZE, cx);
    }

    /// The size a cursor x maps to on the icon-size track. `None` before
    /// the track has painted (so there are no bounds to map against).
    fn icon_size_at(&self, x: Pixels) -> Option<u32> {
        icon_size_range()
            .value_at(self.icon_size_track, x)
            .map(|value| crate::grid::clamp_icon_size(value as u32))
    }

    /// Scrub in progress. Lives on the shell root so the drag keeps
    /// following the cursor once it leaves the 96-px bar — dragging past
    /// either end should peg to that end, not stop responding.
    pub(super) fn on_icon_size_drag(
        &mut self,
        e: &gpui::MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.icon_size_dragging {
            return;
        }
        // A release we never saw (outside the window, say) ends the scrub.
        if e.pressed_button != Some(gpui::MouseButton::Left) {
            self.end_icon_size_drag(cx);
            return;
        }
        if let Some(px) = self.icon_size_at(e.position.x) {
            self.set_icon_size_live(px, cx);
        }
    }

    /// End a scrub and write the size the user landed on. This is the only
    /// place a drag reaches the settings file: persisting per mouse-move
    /// would enqueue a settings write on every frame of one gesture.
    pub(super) fn end_icon_size_drag(&mut self, cx: &mut Context<Self>) {
        if !self.icon_size_dragging {
            return;
        }
        self.icon_size_dragging = false;
        crate::settings::persist_icon_size(crate::grid::icon_size(cx));
        cx.notify();
    }

    /// Grid icon-size instance of the shared two-tone scrub track.
    fn icon_size_track(&self, icon_px: u32, cx: &mut Context<Self>) -> impl IntoElement {
        const TRACK_W: f32 = 96.0;
        let entity = cx.entity();

        crate::scrub_slider::track(
            "toolbar-icon-size-track",
            icon_size_range().fraction(icon_px as f32),
            false,
            cx,
        )
        .w(px(TRACK_W))
        // Capture the painted bounds so a cursor x maps back to a size.
        .child(
            canvas(
                move |bounds, _, cx| entity.update(cx, |this, _| this.icon_size_track = bounds),
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        // Press anywhere on the bar jumps there and starts scrubbing.
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, e: &gpui::MouseDownEvent, _window, cx| {
                cx.stop_propagation();
                this.icon_size_dragging = true;
                if let Some(size) = this.icon_size_at(e.position.x) {
                    this.set_icon_size_live(size, cx);
                }
                cx.notify();
            }),
        )
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
            let label = crate::private_mode::present_label(&tab.label());
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
                    .tooltip(tr!("Scroll tabs left"))
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
                    .tooltip(tr!("Scroll tabs right"))
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
    ///   • "Ferail" name
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
    fn title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> TitleBar {
        use crate::file_list::SortColumn;
        use gpui_component::menu::DropdownMenu;
        use gpui_component::menu::PopupMenuItem;
        use gpui_component::sidebar::SidebarToggleButton;
        let (can_back, can_forward) = self
            .active_tab()
            .platform_namespace
            .as_ref()
            .map(|session| (session.can_go_back(), session.can_go_forward()))
            .unwrap_or_else(|| {
                (
                    self.active_tab().history_index > 0,
                    self.active_tab().history_index + 1 < self.active_tab().history.len(),
                )
            });
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
        let filter_input = self.active_tab().filter_input.clone();
        let filter_has_value = !filter_input.read(cx).value().is_empty();
        let filter_tab_id = self.active_tab().id;
        let filter_suggestions = self.active_tab().filter_suggestions.clone();
        let weak_filter_escape = cx.weak_entity();
        let filter_completion_menu = if filter_suggestions.is_open() {
            let mut menu = v_flex()
                .w_full()
                .max_h(px(240.0))
                .overflow_y_scrollbar()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .shadow_md()
                .p_1();
            for (index, suggestion) in filter_suggestions.items().iter().take(10).enumerate() {
                let weak = cx.weak_entity();
                let selected = index == filter_suggestions.selected_index();
                let label = suggestion.label.clone();
                let detail = suggestion.detail.clone();
                menu = menu.child(
                    h_flex()
                        .id(("filter-completion", index))
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded(cx.theme().radius)
                        .text_scale_sm()
                        .when(selected, |this| this.bg(cx.theme().accent.opacity(0.18)))
                        .hover(|this| this.bg(cx.theme().accent.opacity(0.12)))
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .when_some(detail, |this, detail| {
                            this.child(
                                div()
                                    .flex_shrink_0()
                                    .text_scale_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(detail),
                            )
                        })
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            let _ = weak.update(cx, |this, cx| {
                                this.accept_filter_completion(
                                    filter_tab_id,
                                    Some(index),
                                    window,
                                    cx,
                                );
                            });
                        }),
                );
            }
            Some(
                deferred(
                    div()
                        .absolute()
                        .top(px(30.0))
                        .left_0()
                        .w_full()
                        .occlude()
                        .child(menu),
                )
                .with_priority(10)
                .into_any_element(),
            )
        } else {
            None
        };
        // Show Desktop is a private-symbol feature: the button only
        // exists when `ferail-shell-mac` resolved the Dock notification
        // on a supported macOS. Cached after first resolve, so this is a
        // cheap render-time read (prewarmed at startup).
        let show_desktop_available = crate::platform_shell::show_desktop_available();
        // View switcher + grid size control state.
        let view_mode = self.active_tab().view_mode;
        let is_grid = matches!(view_mode, crate::grid::ViewMode::Grid);
        let is_flat = self
            .active_tab()
            .tool_result
            .as_ref()
            .is_some_and(|surface| surface.flat_mode().is_some());
        // Live grid icon size — drives the slider's readout and greys the
        // reset button out when there is nothing to reset. Render-time
        // global read, no I/O.
        let icon_px = crate::grid::icon_size(cx);
        let readout_color = cx.theme().muted_foreground;
        // ---- Width-tiered overflow ----------------------------------
        // Same shape as the viewer's toolbar (`viewer/window.rs`): the bar
        // keeps its height, so a narrow window folds whole clusters into the
        // trailing "…" menu instead of wrapping or running off the edge.
        // Unlike the viewer's, nothing up here is a flex_1 that can absorb a
        // bad estimate — the two spacers collapse to zero and then the
        // overflow button itself is pushed out of the window — so these lean
        // deliberately generous and fold a touch early.
        //
        // Logical px at `ui_scale == 1`, each including the 8-px flex gap
        // that follows it; scaled by the window rem size so UI zoom counts.
        // Measured, not guessed: at 990 px the "…" lost its last dot off the
        // right edge and at 1000 px it cleared, so the full grid-mode bar
        // needs ~1005. This term appears in every tier, so correcting it here
        // shifts all the fold points together. The version suffix beside the
        // wordmark then widened the brand by a measured 32 logical px (ink
        // plus its gap, read off a column profile at ui_scale 1); rounded to
        // 34 to keep this estimate on the generous side it wants to be on.
        const W_BASE: f32 = 562.0; // sidebar + brand + version + back/fwd + filter + (?) + "…" + padding
        const W_VIEW: f32 = 102.0; // list / grid / flat switcher — never folds
        const W_SORT: f32 = 34.0;
        const W_DESKTOP: f32 = 34.0;
        const W_NEW_REFRESH: f32 = 68.0;
        const W_DOCK: f32 = 34.0;
        const W_SIZE_STEPS: f32 = 102.0; // − ＋ ⟲
        const W_SIZE_BAR: f32 = 138.0; // track + px readout

        let ui_scale = window.rem_size().as_f32() / crate::text::BASE_REM_PX;
        let avail = window.viewport_size().width.as_f32();
        let has_dock = cfg!(target_os = "macos");

        // First-to-collapse first. The ordering is the judgement here: the
        // size *bar* goes before the size *buttons* (which still express the
        // same thing, just coarsely); window/system placement verbs go before
        // anything that acts on the current folder; and New Folder / Refresh
        // hold out longest because they are the two things people came to the
        // toolbar for. The view switcher is not a tier at all — it is how you
        // get back out of icon view, and it is the most-used pair up here.
        let tiers = [
            if is_grid { W_SIZE_BAR } else { 0.0 },
            if has_dock { W_DOCK } else { 0.0 },
            if show_desktop_available {
                W_DESKTOP
            } else {
                0.0
            },
            if is_grid { W_SIZE_STEPS } else { 0.0 },
            W_SORT,
            W_NEW_REFRESH,
        ];
        let mut need = W_BASE + W_VIEW + tiers.iter().sum::<f32>();
        let mut hide = [false; 6];
        // No `W_MENU` term as the viewer has: this toolbar's "…" is always
        // present, so folding never has to make room for it.
        for (i, w) in tiers.iter().enumerate() {
            if need * ui_scale <= avail {
                break;
            }
            hide[i] = true;
            need -= w;
        }
        let [
            hide_size_bar,
            hide_dock,
            hide_desktop,
            hide_size_steps,
            hide_sort,
            hide_new_refresh,
        ] = hide;

        // Below the last tier the remainder is still ~600 px, and a bar whose
        // children are all `flex_shrink_0` just pushes its own tail — the view
        // switcher and the "…" itself — off the edge. Flexbox cannot rescue
        // that from in here: gpui-component's `#bar` takes its automatic
        // minimum size from our total content width, so it overflows its own
        // parent and no shrink pressure ever reaches our children. Size the
        // one elastic element ourselves instead, from the width the tiers
        // already measured.
        const FILTER_W: f32 = 220.0;
        const FILTER_MIN_W: f32 = 110.0;
        // Without the margin the arithmetic lands the last element flush on
        // the window edge, which costs the "…" its final dot.
        const EDGE_MARGIN: f32 = 16.0;
        let deficit = (need + EDGE_MARGIN - avail / ui_scale).max(0.0);
        let filter_w = (FILTER_W - deficit).clamp(FILTER_MIN_W, FILTER_W);
        let show_size_slider = is_grid && !hide_size_bar;
        let show_size_steps = is_grid && !hide_size_steps;
        // SHELL_CONTEXT-bearing handle for the toolbar dropdowns, so
        // their items resolve keyboard-shortcut hints against the
        // shell's stable dispatch path instead of the focus-sensitive
        // previous-frame fallback (which left the hints blank for the
        // first frame or two after the menu opened).
        let sort_menu_focus = self.focus_handle.clone();
        let sort_submenu_focus = self.focus_handle.clone();
        let overflow_menu_focus = self.focus_handle.clone();
        // The folded icon-size items are click-backed, not action-backed, so
        // the overflow menu needs a handle on the shell to call them.
        let overflow_entity = cx.entity();
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
                        .id("sidebar-density-toggle")
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new(tr!("Toggle Sidebar"))
                                .action(&CycleSidebarSize, Some(SHELL_CONTEXT))
                                .build(window, cx)
                        })
                        .child(SidebarToggleButton::new().collapsed(collapsed).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.cycle_sidebar_size(cx);
                            }),
                        )),
                )
                // Wordmark + build version. The version rides here and not in
                // the OS caption because the custom TitleBar replaces the
                // native one (see `Shell::sync_window_title`) — that caption
                // only reaches Alt+Tab and the macOS Window menu, so it never
                // appears in a screenshot. This wordmark does, which is the
                // whole point: a screenshot sent in a bug report says which
                // build it came from. Muted and a tier smaller so it reads as
                // metadata beside the brand, not as part of it.
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .text_scale_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .child("Ferail"),
                        )
                        .child(
                            div()
                                .text_scale_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(env!("CARGO_PKG_VERSION")),
                        ),
                )
                // The sole visible Private Mode indicator and in-app exit.
                // Its painted bounds are remembered so the process-wide
                // interaction shield can allow exactly this one click.
                .child(
                    div()
                        .flex_shrink_0()
                        .on_prepaint({
                            let shell = cx.entity();
                            move |bounds, _, cx| {
                                shell.update(cx, |this, _| this.private_toggle_bounds = bounds)
                            }
                        })
                        .child(
                            Button::new("toolbar-private-mode")
                                .small()
                                .ghost()
                                .selected(crate::private_mode::enabled())
                                .when(crate::private_mode::enabled(), |this| {
                                    this.text_color(cx.theme().primary)
                                })
                                .icon(gpui_component::Icon::empty().path("icons/privacy.svg"))
                                .tooltip_with_action(
                                    tr!("Private Mode"),
                                    &crate::private_mode::TogglePrivateMode,
                                    None,
                                )
                                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(|_, _window, cx| crate::private_mode::toggle(cx)),
                        ),
                )
                .child(
                    Button::new("nav-back")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/nav/chevron-left.svg"))
                        .tooltip(format!("{}  \u{2318}\u{5B}", tr!("Back")))
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
                        .tooltip(format!("{}  \u{2318}\u{5D}", tr!("Forward")))
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
                        // Computed above: full width until every tier has
                        // folded, then it gives way so the tail of the bar
                        // stays on screen.
                        .w(px(filter_w))
                        // Center the field in the title-bar row. Without
                        // this the wrapper is a block div whose height is
                        // whatever the input computes, so the field sat
                        // slightly high against its `.small()` neighbours
                        // (the private-mode twin below always had it).
                        .flex()
                        .items_center()
                        // Filter input — also lives inside TitleBar's
                        // drag region. Stop mouse-down propagation so
                        // Win32 doesn't capture the click as window
                        // drag (same bug as the toolbar buttons; see
                        // §Title bar drag capture above).
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        // Keep the input's inline clear affordance.
                        // `clean` emits Change like any edit, so the existing
                        // subscription drops the filter and reloads through
                        // the normal path, with no second clearing seam.
                        .child(if crate::private_mode::enabled() {
                            div()
                                .h(px(28.0))
                                .w_full()
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().border)
                                .text_scale_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr!("Private"))
                                .into_any_element()
                        } else {
                            div()
                                .relative()
                                .w_full()
                                .on_action({
                                    let weak = cx.weak_entity();
                                    move |_: &gpui_component::input::MoveUp, _window, cx| {
                                        let handled = weak
                                            .update(cx, |this, cx| {
                                                this.move_filter_completion(-1, cx)
                                            })
                                            .unwrap_or(false);
                                        if handled {
                                            cx.stop_propagation();
                                        }
                                    }
                                })
                                .on_action({
                                    let weak = cx.weak_entity();
                                    move |_: &gpui_component::input::MoveDown, _window, cx| {
                                        let handled = weak
                                            .update(cx, |this, cx| {
                                                this.move_filter_completion(1, cx)
                                            })
                                            .unwrap_or(false);
                                        if handled {
                                            cx.stop_propagation();
                                        }
                                    }
                                })
                                .on_action(move |_: &gpui_component::input::Escape, window, cx| {
                                    let _ = weak_filter_escape.update(cx, |this, cx| {
                                        this.on_clear_filter(
                                            &crate::shell::ClearFilter,
                                            window,
                                            cx,
                                        );
                                    });
                                    cx.stop_propagation();
                                })
                                .child(
                                    Input::new(&filter_input)
                                        .xsmall()
                                        .h(px(28.0))
                                        .when(filter_has_value, |this| this.pr_7()),
                                )
                                .when(filter_has_value, |this| {
                                    let filter_input = filter_input.clone();
                                    this.child(
                                        div()
                                            .absolute()
                                            .right_0()
                                            // Stretch to the field instead of
                                            // repeating its height: the X stays
                                            // centered at any ui_scale.
                                            .top_0()
                                            .bottom_0()
                                            .flex()
                                            .items_center()
                                            .child(
                                                Button::new("filter-clear")
                                                    .xsmall()
                                                    .ghost()
                                                    .icon(
                                                        gpui_component::Icon::empty()
                                                            .path("icons/close.svg"),
                                                    )
                                                    .on_click(move |_, window, cx| {
                                                        filter_input.update(cx, |state, cx| {
                                                            state.clean(window, cx);
                                                        });
                                                    }),
                                        ),
                                    )
                                })
                                .when_some(filter_completion_menu, |this, menu| this.child(menu))
                                .into_any_element()
                        }),
                )
                // (?) — filter-syntax cheat sheet (filter_help.rs). A
                // stopgap until the filter grows chips; same mouse-down
                // stop as its neighbours for the Win32 title-bar drag.
                .child(
                    div()
                        .flex_shrink_0()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(
                            Button::new("filter-help")
                                .small()
                                .ghost()
                                .icon(gpui_component::Icon::empty().path("icons/circle-help.svg"))
                                .tooltip(tr!("Filter syntax"))
                                .on_click(|_, window, cx| {
                                    crate::filter_help::open_filter_help_dialog(window, cx);
                                }),
                        ),
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
                .children((!hide_sort).then(|| {
                    div()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(
                            Button::new("toolbar-sort")
                                .small()
                                .ghost()
                                .icon(gpui_component::Icon::empty().path(sort_icon))
                                .tooltip(tr!("Sort"))
                                .dropdown_menu(move |menu, _window, _cx| {
                                    let menu = menu
                                        .action_context(sort_menu_focus.clone())
                                        .menu_with_check(
                                            tr!("Name"),
                                            sort_col == SortColumn::Name,
                                            Box::new(SortByName),
                                        )
                                        .menu_with_check(
                                            tr!("Size"),
                                            sort_col == SortColumn::Size,
                                            Box::new(SortBySize),
                                        )
                                        .menu_with_check(
                                            tr!("Kind"),
                                            sort_col == SortColumn::Format,
                                            Box::new(SortByKind),
                                        )
                                        .menu_with_check(
                                            tr!("Date Modified"),
                                            sort_col == SortColumn::Modified,
                                            Box::new(SortByModified),
                                        );
                                    // The one ordering the column headers
                                    // can't give you: rank folders by how
                                    // often you open them. Flat View rows
                                    // carry no heat, so it isn't offered
                                    // there.
                                    if is_flat {
                                        menu
                                    } else {
                                        menu.menu_with_check(
                                            tr!("Ant Trail"),
                                            sort_col == SortColumn::AntTrail,
                                            Box::new(SortByAntTrail),
                                        )
                                    }
                                }),
                        )
                }))
                // Show Desktop — left of New Folder. Present only when the
                // private Dock symbol resolved on a supported OS; otherwise
                // it silently doesn't render (no crash, no empty slot).
                .children((show_desktop_available && !hide_desktop).then(|| {
                    Button::new("toolbar-show-desktop")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/nav/show-desktop.svg"))
                        .tooltip_with_action(tr!("Show Desktop"), &ShowDesktop, Some(SHELL_CONTEXT))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_show_desktop(&ShowDesktop, window, cx);
                        }))
                }))
                .children((!hide_new_refresh).then(|| {
                    Button::new("toolbar-new-folder")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/nav/folder.svg"))
                        .tooltip_with_action(tr!("New Folder"), &NewFolder, Some(SHELL_CONTEXT))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_new_folder(&NewFolder, window, cx);
                        }))
                }))
                .children((!hide_new_refresh).then(|| {
                    Button::new("toolbar-refresh")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/nav/refresh.svg"))
                        .tooltip_with_action(tr!("Refresh"), &Refresh, Some(SHELL_CONTEXT))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_refresh(&Refresh, window, cx);
                        }))
                }))
                // Dock menu — park the whole window against a screen edge as
                // an auto-hiding drawer (docs/features/DOCK.md). Pressed look
                // while docked; the active edge carries a checkmark. Wrapped
                // in a mouse-down-stopping div for the Win32 title-bar-drag
                // gotcha, like the other dropdowns. macOS-only for now (the
                // win32/linux window primitives are stubs) — hidden elsewhere
                // so the menu isn't three silent no-ops.
                .when(has_dock && !hide_dock, |bar| {
                    bar.child(
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
                                    .tooltip(tr!("Dock window to a screen edge"))
                                    .dropdown_menu(move |menu, _window, _cx| {
                                        menu.action_context(dock_menu_focus.clone())
                                            .menu_with_icon(
                                                tr!("Dock Left"),
                                                gpui_component::Icon::empty()
                                                    .path("icons/dock-left.svg"),
                                                Box::new(DockLeft),
                                            )
                                            .menu_with_icon(
                                                tr!("Dock Right"),
                                                gpui_component::Icon::empty()
                                                    .path("icons/dock-right.svg"),
                                                Box::new(DockRight),
                                            )
                                            .separator()
                                            .menu_with_icon(
                                                tr!("Undock"),
                                                gpui_component::Icon::empty()
                                                    .path("icons/undock.svg"),
                                                Box::new(Undock),
                                            )
                                    }),
                            ),
                    )
                })
                // View switcher: list ⇄ icon grid (per-tab). The active
                // mode's button is highlighted.
                .child(
                    Button::new("toolbar-view-list")
                        .small()
                        .ghost()
                        .selected(!is_grid && !is_flat)
                        .icon(gpui_component::Icon::empty().path("icons/view-list.svg"))
                        .tooltip(tr!("List view"))
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
                        .disabled(is_flat)
                        .icon(gpui_component::Icon::empty().path("icons/view-grid.svg"))
                        .tooltip(tr!("Icon view"))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_view_mode(crate::grid::ViewMode::Grid, window, cx);
                        })),
                )
                .child(
                    Button::new("toolbar-view-flat")
                        .small()
                        .ghost()
                        .selected(is_flat)
                        .icon(gpui_component::Icon::empty().path("icons/folder-tree.svg"))
                        .tooltip(tr!("Include Subfolders"))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.on_toggle_flat_view(&ToggleFlatView, window, cx);
                        })),
                )
                // Icon size stepper — only in grid mode.
                .children(show_size_steps.then(|| {
                    Button::new("toolbar-icon-smaller")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/minus.svg"))
                        .tooltip(tr!("Smaller icons"))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.step_icon_size(-1, cx);
                        }))
                }))
                .children(show_size_steps.then(|| {
                    Button::new("toolbar-icon-larger")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/plus.svg"))
                        .tooltip(tr!("Larger icons"))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.step_icon_size(1, cx);
                        }))
                }))
                // Reset the icon size to the default, beside the stepper it
                // undoes. Disabled when already there, so the button states
                // what it would do.
                .children(show_size_steps.then(|| {
                    Button::new("toolbar-icon-reset")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/undo-2.svg"))
                        .tooltip(tr!("Reset icon size"))
                        .disabled(icon_px == crate::grid::DEFAULT_ICON_SIZE)
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.reset_icon_size(cx);
                        }))
                }))
                // Continuous icon-size slider — the exact size the −/＋
                // stops cannot express — plus a live px readout.
                //
                // The mouse-down-stopping wrapper is the same title-bar
                // drag guard the buttons use, but it matters more here:
                // TitleBar starts a window move on the first mouse-move
                // after a mouse-down it saw, and a slider *is* a drag, so
                // without this every size drag would drag the window
                // instead.
                .children(show_size_slider.then(|| {
                    h_flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap_2()
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(self.icon_size_track(icon_px, cx))
                        .child(
                            div()
                                .flex_shrink_0()
                                .min_w(rems(1.6))
                                .text_scale_xs()
                                .text_color(readout_color)
                                .child(format!("{icon_px}")),
                        )
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
                                .tooltip(tr!("More"))
                                .dropdown_menu(move |mut menu, window, cx| {
                                    menu = menu.action_context(overflow_menu_focus.clone());
                                    // Clusters the width tiers folded away,
                                    // in the order they sit in the bar, so
                                    // the menu reads as the bar's tail rather
                                    // than a second unrelated list. Each item
                                    // dispatches the same action (or calls the
                                    // same method) its button does.
                                    let mut folded = false;
                                    if hide_sort {
                                        // Nested rather than inlined: "Size"
                                        // means the sort column here and the
                                        // icon size three items below, and
                                        // flat neighbours would be a coin flip.
                                        let focus = sort_submenu_focus.clone();
                                        menu = menu.submenu(
                                            tr!("Sort"),
                                            window,
                                            cx,
                                            move |sub, _w, _c| {
                                                let sub = sub
                                                    .action_context(focus.clone())
                                                    .menu_with_check(
                                                        tr!("Name"),
                                                        sort_col == SortColumn::Name,
                                                        Box::new(SortByName),
                                                    )
                                                    .menu_with_check(
                                                        tr!("Size"),
                                                        sort_col == SortColumn::Size,
                                                        Box::new(SortBySize),
                                                    )
                                                    .menu_with_check(
                                                        tr!("Kind"),
                                                        sort_col == SortColumn::Format,
                                                        Box::new(SortByKind),
                                                    )
                                                    .menu_with_check(
                                                        tr!("Date Modified"),
                                                        sort_col == SortColumn::Modified,
                                                        Box::new(SortByModified),
                                                    );
                                                if is_flat {
                                                    sub
                                                } else {
                                                    sub.menu_with_check(
                                                        tr!("Ant Trail"),
                                                        sort_col == SortColumn::AntTrail,
                                                        Box::new(SortByAntTrail),
                                                    )
                                                }
                                            },
                                        );
                                        folded = true;
                                    }
                                    if hide_new_refresh {
                                        menu = menu
                                            .menu(tr!("New Folder"), Box::new(NewFolder))
                                            .menu(tr!("Refresh"), Box::new(Refresh));
                                        folded = true;
                                    }
                                    if hide_desktop && show_desktop_available {
                                        menu =
                                            menu.menu(tr!("Show Desktop"), Box::new(ShowDesktop));
                                        folded = true;
                                    }
                                    if hide_dock && has_dock {
                                        menu = menu
                                            .menu_with_check(
                                                tr!("Dock Left"),
                                                dock_edge
                                                    == Some(crate::shell::dock::DockEdge::Left),
                                                Box::new(DockLeft),
                                            )
                                            .menu_with_check(
                                                tr!("Dock Right"),
                                                dock_edge
                                                    == Some(crate::shell::dock::DockEdge::Right),
                                                Box::new(DockRight),
                                            )
                                            .menu(tr!("Undock"), Box::new(Undock));
                                        folded = true;
                                    }
                                    if hide_size_steps && is_grid {
                                        // No actions back these — the toolbar
                                        // buttons call the methods directly —
                                        // so they go through the shell entity.
                                        // Labels are the buttons' own tooltip
                                        // strings rather than Title Case
                                        // variants, to avoid shipping
                                        // near-duplicate msgids.
                                        let smaller = overflow_entity.clone();
                                        let larger = overflow_entity.clone();
                                        let reset = overflow_entity.clone();
                                        menu = menu
                                            .item(
                                                PopupMenuItem::new(tr!("Smaller icons")).on_click(
                                                    move |_, _, cx| {
                                                        smaller.update(cx, |this, cx| {
                                                            this.step_icon_size(-1, cx)
                                                        });
                                                    },
                                                ),
                                            )
                                            .item(PopupMenuItem::new(tr!("Larger icons")).on_click(
                                                move |_, _, cx| {
                                                    larger.update(cx, |this, cx| {
                                                        this.step_icon_size(1, cx)
                                                    });
                                                },
                                            ))
                                            .item(
                                                PopupMenuItem::new(tr!("Reset icon size"))
                                                    .on_click(move |_, _, cx| {
                                                        reset.update(cx, |this, cx| {
                                                            this.reset_icon_size(cx)
                                                        });
                                                    }),
                                            );
                                        folded = true;
                                    }
                                    if folded {
                                        menu = menu.separator();
                                    }
                                    menu.menu_with_check(
                                        tr!("Show Hidden Files"),
                                        show_hidden,
                                        Box::new(ToggleHidden),
                                    )
                                    .separator()
                                    .menu(tr!("Get Info"), Box::new(GetInfo))
                                    .menu(tr!("Open Viewer"), Box::new(OpenViewer))
                                    .menu(tr!("Disk Usage\u{2026}"), Box::new(OpenDiskUsage))
                                    .menu(tr!("Find Duplicates\u{2026}"), Box::new(FindDuplicates))
                                    .menu(
                                        tr!("Find Similar Images\u{2026}"),
                                        Box::new(FindSimilarImages),
                                    )
                                    .separator()
                                    // Shift-click widens the copy to subfolder
                                    // contents (see `on_copy_file_list`); the
                                    // tooltip is the only place that modifier
                                    // is discoverable, hence the custom item.
                                    .menu_element(Box::new(CopyFileList), |_, _| {
                                        div()
                                            .id("copy-file-list-tip")
                                            .w_full()
                                            .tooltip(|window, cx| {
                                                gpui_component::tooltip::Tooltip::new(tr!(
                                                    "Shift-click to also include the contents of every subfolder"
                                                ))
                                                .build(window, cx)
                                            })
                                            .child(tr!("Copy File List"))
                                    })
                                    .separator()
                                    .menu(tr!("Empty Trash\u{2026}"), Box::new(EmptyTrash))
                                }),
                        ),
                ),
        )
    }

    // Render-safe path resolution for a file-list row's preview.
    //
    // Reads the delegate's per-entry `paths` map (a pure in-memory
    // lookup populated at load for directory, search, AND duplicate
    // rows) so it works for results views whose files live outside
    // `current_dir` — without touching the guarded node store, which
    // would panic on the paint path. Falls back to `current_dir + name`
    // only when the map has no entry.

    /// Build the breadcrumb row from `current_dir`. Each ancestor is
    /// clickable and navigates the pane to that level. The root `/`
    /// gets its own leading segment. When a breadcrumb inline-edit session
    /// is active (Cmd+L) the row swaps in an Input field instead — Enter
    /// commits the path, Blur cancels.
    /// Host the preview panel, pointing it at whatever this tab has selected.
    fn preview_pane(&mut self, cx: &mut Context<Self>) -> Div {
        use crate::preview_panel::PreviewTarget;

        // Preview always reflects the **lead** row, even with a
        // multi-selection — Finder's "the focused one of many" semantics.
        let selected = {
            let entries = &self.active_tab().table.read(cx).delegate().entries;
            self.active_tab()
                .lead_row(entries)
                .and_then(|i| entries.get(i).cloned())
        };
        // A docked archive workbench overrides the tab's selection.
        if let Some(target) = self.preview_override.clone() {
            let panel = self.ensure_preview_panel(cx);
            panel.update(cx, |panel, cx| panel.set_target(target, cx));
            return div().size_full().child(panel);
        }
        let target = match selected {
            Some(entry) => {
                // The delegate's per-entry map has the true path — search and
                // duplicate results live outside `current_dir`, so rebuilding
                // it from the name would key the preview cache wrong.
                let path = self
                    .active_tab()
                    .table
                    .read(cx)
                    .delegate()
                    .path_for_entry(entry.id)
                    .unwrap_or_else(|| {
                        let mut p = self.active_tab().current_dir.clone();
                        p.push(entry.name.as_ref());
                        p
                    });
                PreviewTarget::File {
                    path,
                    entry: Box::new(entry),
                }
            }
            // Nothing selected but parked at a volume's mount root: preview the
            // volume, which is where a sidebar volume click lands.
            None => {
                let dir = self.active_tab().current_dir.clone();
                match self.mounted_volume_name(&dir) {
                    Some(name) => PreviewTarget::Volume { path: dir, name },
                    None => PreviewTarget::None,
                }
            }
        };

        let panel = self.ensure_preview_panel(cx);
        panel.update(cx, |panel, cx| panel.set_target(target, cx));
        div().size_full().child(panel)
    }

    /// The Shell's preview panel, created on first use.
    fn ensure_preview_panel(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<crate::preview_panel::PreviewPanel> {
        use crate::preview_panel::{PreviewCloseRequested, PreviewPanel};
        if let Some(panel) = &self.preview_panel {
            return panel.clone();
        }
        let process = self.process.clone();
        let weak = cx.weak_entity();
        let thumb_h = self.preview_thumb_h;
        let panel = cx.new(|_| PreviewPanel::new(process, weak, thumb_h));
        cx.subscribe(&panel, |this, _panel, _: &PreviewCloseRequested, cx| {
            this.preview_visible = false;
            this.preview_override = None;
            if let Some(view) = this.active_archive_view() {
                view.update(cx, |v, cx| v.set_preview_enabled(false, cx));
            }
            cx.notify();
        })
        .detach();
        self.preview_panel = Some(panel.clone());
        panel
    }

    fn breadcrumb(&self, cx: &mut Context<Self>) -> Div {
        if let Some(session) = self.active_tab().platform_namespace.as_ref() {
            let mut row = h_flex()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .items_center()
                .gap_1()
                .px_4()
                .py_1()
                .border_b_1()
                .border_color(cx.theme().border);
            let breadcrumbs = session.store().breadcrumbs().to_vec();
            let tab_id = self.active_tab().id;
            for (index, breadcrumb) in breadcrumbs.into_iter().enumerate() {
                if index > 0 {
                    row = row.child(
                        div()
                            .px_1()
                            .text_scale_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("›"),
                    );
                }
                let is_last = index + 1 == session.store().breadcrumbs().len();
                let weak = cx.weak_entity();
                let location = breadcrumb.location;
                row = row.child(
                    div()
                        .id(("platform-breadcrumb", index))
                        .px_2()
                        .py_1()
                        .rounded(cx.theme().radius)
                        .text_scale_sm()
                        .text_color(if is_last {
                            cx.theme().foreground
                        } else {
                            cx.theme().muted_foreground
                        })
                        .when(is_last, |crumb| crumb.font_weight(FontWeight::SEMIBOLD))
                        .cursor_pointer()
                        .hover(|crumb| crumb.bg(cx.theme().secondary))
                        .child(SharedString::from(breadcrumb.label.to_string()))
                        .on_click(move |_: &ClickEvent, _window, app| {
                            let _ = weak.update(app, |shell, cx| {
                                shell.navigate_platform_location(tab_id, location.clone(), cx);
                            });
                        }),
                );
            }
            return row;
        }
        let breadcrumb_session = self
            .breadcrumb_edit
            .snapshot()
            .filter(|session| session.target == self.active_tab().id);
        if let Some(breadcrumb_session) = breadcrumb_session {
            if crate::private_mode::enabled() {
                return h_flex()
                    .w_full()
                    .items_center()
                    .gap_1()
                    .px_4()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().truncate().text_scale_sm().child(
                        crate::private_mode::present_path(&self.active_tab().current_dir),
                    ));
            }
            let path_suggestions = self.breadcrumb_suggestions.clone();
            let completion_menu = if path_suggestions.is_open() {
                let mut menu = v_flex()
                    .w_full()
                    .max_h(px(260.0))
                    .overflow_y_scrollbar()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().popover)
                    .shadow_md()
                    .p_1();
                for (index, suggestion) in path_suggestions.items().iter().take(12).enumerate() {
                    let weak = cx.weak_entity();
                    let selected = index == path_suggestions.selected_index();
                    menu = menu.child(
                        h_flex()
                            .id(("path-completion", index))
                            .w_full()
                            .px_2()
                            .py_1()
                            .rounded(cx.theme().radius)
                            .text_scale_sm()
                            .when(selected, |this| this.bg(cx.theme().accent.opacity(0.18)))
                            .hover(|this| this.bg(cx.theme().accent.opacity(0.12)))
                            .child(suggestion.label.clone())
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                let _ = weak.update(cx, |this, cx| {
                                    this.accept_breadcrumb_completion(Some(index), window, cx);
                                });
                            }),
                    );
                }
                Some(
                    deferred(
                        div()
                            .absolute()
                            .left(px(16.0))
                            .right(px(16.0))
                            .top(px(36.0))
                            .occlude()
                            .child(menu),
                    )
                    .with_priority(10)
                    .into_any_element(),
                )
            } else {
                None
            };
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
            let weak_escape = cx.weak_entity();
            return h_flex()
                .relative()
                .w_full()
                .items_center()
                .gap_1()
                .px_4()
                .py_1()
                .border_b_1()
                .border_color(cx.theme().border)
                .on_action({
                    let weak = cx.weak_entity();
                    move |_: &gpui_component::input::MoveUp, _window, cx| {
                        let handled = weak
                            .update(cx, |this, cx| this.move_breadcrumb_completion(-1, cx))
                            .unwrap_or(false);
                        if handled {
                            cx.stop_propagation();
                        }
                    }
                })
                .on_action({
                    let weak = cx.weak_entity();
                    move |_: &gpui_component::input::MoveDown, _window, cx| {
                        let handled = weak
                            .update(cx, |this, cx| this.move_breadcrumb_completion(1, cx))
                            .unwrap_or(false);
                        if handled {
                            cx.stop_propagation();
                        }
                    }
                })
                .on_action(move |_: &gpui_component::input::Enter, _window, cx| {
                    cx.stop_propagation();
                })
                .on_action(move |_: &gpui_component::input::Escape, window, cx| {
                    let _ = weak_escape.update(cx, |this, cx| {
                        if this.breadcrumb_edit.clear() {
                            this.breadcrumb_suggestions.clear();
                            this.focus_handle.focus(window, cx);
                            cx.notify();
                        }
                    });
                    cx.stop_propagation();
                })
                .child(div().flex_1().child(crate::inline_edit::InlineEditor::new(
                    "inline-path-editor",
                    crate::inline_edit::InlineEditInput::Text(self.breadcrumb_input.clone()),
                    crate::inline_edit::InlineEditLayout::AddressBar,
                    &breadcrumb_session,
                    tr!("Path"),
                )))
                .when_some(completion_menu, |this, menu| this.child(menu));
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
            .min_w_0()
            .overflow_hidden()
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
                        .child(tr!("in")),
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
            let tooltip_path = crate::private_mode::present_path(&path);
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
            let path_for_archive_drop = path.clone();
            let path_for_native_drop = path.clone();
            let crumb_accent = cx.theme().primary;
            // §5 favorited indicator: trailing star on any breadcrumb
            // segment whose path is in the Favorites index. The last
            // segment is the current-folder header per §5.1, so the
            // current-folder header is covered by the same render path.
            let favorited = self.process.favorites().read(cx).contains_path(&path);
            let crumb = div()
                .id(ElementId::Name(format!("crumb-{i}").into()))
                .min_w_0()
                .overflow_hidden()
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
                // gpui allows one `.hover()` per element, so the ordinary
                // hover wash and the drop-target ring share it. The ring is
                // the only feedback a crumb can give during a native archive
                // promise session — gpui holds no typed drag then, so
                // `drag_over` never fires.
                .hover({
                    let secondary = cx.theme().secondary;
                    let accent = cx.theme().accent;
                    let native_dragging = crate::file_list::native_archive_drag_active();
                    move |this| {
                        if native_dragging {
                            this.border_1()
                                .border_color(accent)
                                .bg(accent.opacity(0.18))
                        } else {
                            this.bg(secondary)
                        }
                    }
                })
                .child(div().min_w_0().truncate_middle().child(label))
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
                // The ring matches every other drop target so an acceptable
                // destination reads the same wherever the pointer is.
                .drag_over::<ExternalPaths>(move |style, _payload, _window, _cx| {
                    style
                        .border_1()
                        .border_color(crumb_accent)
                        .bg(crumb_accent.opacity(0.18))
                })
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                    this.handle_external_drop(
                        paths.paths().to_vec(),
                        path_for_drop.clone(),
                        window,
                        cx,
                    );
                }))
                // Archive members dropped on a crumb extract into that
                // ancestor folder — the same destination a file drop uses.
                .drag_over::<crate::file_list::ArchiveEntryDrag>(
                    move |style, _payload, _window, cx| {
                        style
                            .border_1()
                            .border_color(cx.theme().accent)
                            .bg(cx.theme().accent.opacity(0.18))
                    },
                )
                .on_drop(cx.listener({
                    let dest = path_for_archive_drop.clone();
                    move |this, drag: &crate::file_list::ArchiveEntryDrag, window, cx| {
                        cx.stop_propagation();
                        this.extract_archive_entries_into(
                            drag.archive.clone(),
                            drag.entries.clone(),
                            dest.clone(),
                            drag.password.clone(),
                            window,
                            cx,
                        );
                    }
                }))
                // Cross-window promise sessions arrive as plain mouse events
                // (see docs/GPUI-UPSTREAM.md #11): repaint so the ring above
                // tracks the crumb under the pointer, and treat the release
                // as the drop.
                .on_mouse_move(cx.listener(|_this, _event, _window, cx| {
                    if crate::file_list::native_archive_drag_active() {
                        cx.notify();
                    }
                }))
                .on_mouse_up(
                    gpui::MouseButton::Left,
                    cx.listener({
                        let dest = path_for_native_drop.clone();
                        move |this, _event, window, cx| {
                            if cx.has_active_drag() {
                                return;
                            }
                            let Some(drag) = crate::file_list::take_native_archive_drag() else {
                                return;
                            };
                            cx.stop_propagation();
                            crate::log_info!(
                                100,
                                "archive-drag: accepted by breadcrumb -> {}",
                                dest.display()
                            );
                            this.extract_archive_entries_into(
                                drag.archive,
                                drag.entries,
                                dest.clone(),
                                drag.password,
                                window,
                                cx,
                            );
                        }
                    }),
                )
                .context_menu(move |menu, window, cx| {
                    let favorited_now = if let Some(s) = weak_for_crumb.upgrade() {
                        let already = s
                            .read(cx)
                            .process
                            .favorites()
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
                        tr!("Remove from Favorites")
                    } else {
                        tr!("Add to Favorites")
                    };
                    // "Go to Subfolder ▸" — jump to any child folder of
                    // this segment (Finder's column-view-style lateral
                    // navigation). Children are enumerated off-thread and
                    // cached; the submenu reads that cache only (Prime
                    // Directive), showing "Loading…" on the first open.
                    let weak_sub = weak_for_crumb.clone();
                    let path_sub = path_for_menu.clone();
                    menu.menu(tr!("Open in New Tab"), Box::new(OpenContextInNewTab))
                        .separator()
                        .menu(
                            crate::i18n::tr_static(ferail_core::commands::REVEAL_LABEL),
                            Box::new(RevealContextPath),
                        )
                        .menu(tr!("Copy Path"), Box::new(CopyContextPath))
                        .separator()
                        .menu(favorite_label, Box::new(ToggleFavoriteForTarget))
                        .separator()
                        .menu(tr!("New Folder Here"), Box::new(NewFolderHere))
                        .separator()
                        .submenu(tr!("Go to Subfolder"), window, cx, move |mut sub, _w, c| {
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
                                        sub = sub.item(PopupMenuItem::new(name.clone()).on_click(
                                            move |_ev, _w, cx| {
                                                let child = child.clone();
                                                let _ = weak_nav.update(cx, |sh, cx| {
                                                    sh.navigate(child, cx);
                                                });
                                            },
                                        ));
                                    }
                                    sub
                                }
                                Some(Some(_)) => sub
                                    .item(PopupMenuItem::new(tr!("No subfolders")).disabled(true)),
                                _ => {
                                    // Cold or in-flight — kick a warm and show
                                    // a placeholder; a re-open shows the list.
                                    s.update(c, |sh, cx| {
                                        sh.warm_breadcrumb_children(path_sub.clone(), cx);
                                    });
                                    sub.item(
                                        PopupMenuItem::new(tr!("Loading\u{2026}")).disabled(true),
                                    )
                                }
                            }
                        })
                });
            row = row.child(crumb);
        }
        // Explorer-style discovery affordance: segments keep their ordinary
        // navigation click, while the otherwise-empty tail enters path edit
        // mode. The text cursor makes the area discoverable; the compact icon
        // teaches the existing Cmd/Ctrl+L command when no empty tail remains.
        row = row
            .child(
                div()
                    .id("breadcrumb-edit-space")
                    .flex_1()
                    .h_full()
                    .min_w(px(8.0))
                    .cursor_text()
                    .tooltip(|window, cx| {
                        gpui_component::tooltip::Tooltip::new(tr!("Edit Path")).build(window, cx)
                    })
                    .on_click(cx.listener(|_, _, window, cx| {
                        window.dispatch_action(Box::new(EditBreadcrumb), cx);
                    })),
            )
            .child(
                Button::new("breadcrumb-edit")
                    .small()
                    .ghost()
                    .icon(gpui_component::Icon::empty().path("icons/path-edit.svg"))
                    .tooltip_with_action(tr!("Edit Path"), &EditBreadcrumb, Some(SHELL_CONTEXT))
                    .on_click(cx.listener(|_, _, window, cx| {
                        window.dispatch_action(Box::new(EditBreadcrumb), cx);
                    })),
            );
        if self.active_tab().tool_result.is_some() {
            if self.active_tool_result_can_pop_out() {
                // One button, two surfaces — dispatch the action that matches
                // whichever tool currently owns the pane.
                let is_archive = matches!(
                    self.active_tab().tool_result.as_ref().map(|s| &s.mode),
                    Some(super::tab::ToolResultMode::Archive(_))
                );
                row = row.child(
                    Button::new("tool-result-pop-out")
                        .small()
                        .icon(gpui_component::Icon::empty().path("icons/maximize.svg"))
                        .tooltip(tr!("Open in window"))
                        .on_click(cx.listener(move |_, _, window, cx| {
                            if is_archive {
                                window.dispatch_action(Box::new(PopOutArchive), cx);
                            } else {
                                window.dispatch_action(Box::new(PopOutDiskUsage), cx);
                            }
                        })),
                );
            }
            row = row.child(
                Button::new("tool-result-close")
                    .small()
                    .icon(gpui_component::Icon::empty().path("icons/close.svg"))
                    .tooltip(tr!("Close results"))
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
        let _path_guard = ferail_core::path_guard::enter_render();
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
        let locations_rows = self.build_locations_rows(cx);
        let platform_location_rows = self.build_platform_location_rows();
        let favorites_section = self.build_user_favorites_section(weak.clone());
        let recents_section = self.build_recents_section(weak.clone(), cx);
        let browse_rows = self.build_browse_rows(cx);
        let volumes_rows = self.build_volumes_rows(cx);
        let has_volumes = !self.process.volumes.borrow().is_empty();

        // Render never fetches icons (the path guard makes cache
        // misses return the blank placeholder), so collect the
        // sidebar paths whose icon isn't cached yet and schedule a
        // background warm. A failed fetch caches the placeholder and
        // in-flight keys are skipped, so this set empties out instead
        // of respawning every frame. Favorites ride along: their rows use the same
        // path-keyed cache.
        let mut icon_warm: Vec<PathBuf> = Vec::new();
        {
            let icons = self.process.icons.borrow();
            for row in browse_rows.iter().chain(volumes_rows.iter()) {
                if matches!(row.icon, TreeRowIcon::Folder) && icons.needs_path_icon(&row.path, None)
                {
                    icon_warm.push(row.path.clone());
                }
            }
            for fav in self.process.favorites().read(cx).entries() {
                if let ferail_core::favorites::FavoriteTarget::Path(p) = &fav.target {
                    if fav.custom_icon.is_none() && icons.needs_path_icon(p, None) {
                        icon_warm.push(p.clone());
                    }
                }
            }
            for p in self.process.recents.borrow().iter() {
                if icons.needs_path_icon(p, None) {
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
        // Sidebar no longer carries the "Ferail" header — that moved
        // into the TitleBar at the top of the window. Icon-mode collapse
        // is enabled so the toggle button in the TitleBar can shrink the
        // sidebar to a 48-DIP icon strip.
        use crate::sidebar_layout::SidebarSection;
        let collapsed = |section| self.sidebar_layout.is_collapsed(section);
        let mut sections: Vec<(SidebarSection, ShellSidebarItem)> = vec![
            (
                SidebarSection::Locations,
                ShellSidebarItem::locations(crate::locations_section::LocationsSection::new(
                    tr!("Locations"),
                    locations_rows,
                    weak.clone(),
                    crate::tree::SIDEBAR_ICON_PX,
                    collapsed(SidebarSection::Locations),
                )),
            ),
            (SidebarSection::Favorites, favorites_section),
            (
                SidebarSection::Browse,
                ShellSidebarItem::tree(TreeSection::new(
                    tr!("Browse"),
                    browse_rows,
                    weak.clone(),
                    self.process.icons.clone(),
                    crate::tree::SIDEBAR_ICON_PX,
                    SidebarSection::Browse,
                    collapsed(SidebarSection::Browse),
                )),
            ),
        ];
        #[cfg(windows)]
        sections.push((
            SidebarSection::Windows,
            ShellSidebarItem::windows_namespace(
                crate::locations_section::WindowsNamespaceSection::new(
                    weak.clone(),
                    crate::tree::SIDEBAR_ICON_PX,
                    collapsed(SidebarSection::Windows),
                ),
            ),
        ));
        if !platform_location_rows.is_empty() {
            sections.push((
                SidebarSection::Linux,
                ShellSidebarItem::platform_locations(
                    crate::locations_section::PlatformLocationsSection::new(
                        tr!("Linux"),
                        platform_location_rows,
                        weak.clone(),
                        crate::tree::SIDEBAR_ICON_PX,
                        collapsed(SidebarSection::Linux),
                    ),
                ),
            ));
        }
        if let Some(recents_section) = recents_section {
            sections.push((SidebarSection::Recents, recents_section));
        }
        if has_volumes {
            sections.push((
                SidebarSection::Volumes,
                ShellSidebarItem::tree(TreeSection::new(
                    tr!("Volumes"),
                    volumes_rows,
                    weak.clone(),
                    self.process.icons.clone(),
                    crate::tree::SIDEBAR_ICON_PX,
                    SidebarSection::Volumes,
                    collapsed(SidebarSection::Volumes),
                )),
            ));
        }
        sections.sort_by_key(|(section, _)| {
            self.sidebar_layout
                .order
                .iter()
                .position(|candidate| candidate == section)
                .unwrap_or(usize::MAX)
        });

        // gpui-component's icon-collapse has a fixed 48-DIP width and adds
        // padding around every custom section. That cannot represent Ferail's
        // zoom-scaled 24-DIP icons without either clipping or dead space. Keep
        // the official Sidebar for the full resizable mode; the icon-only
        // strip renders the same SidebarItem implementations directly with a
        // geometry derived from the effective icon size.
        let collapsed_geometry = crate::sidebar_layout::collapsed_sidebar_geometry(
            crate::tree::SIDEBAR_ICON_PX,
            self.ui_scale,
        );
        let collapsed_outer_margin = collapsed_geometry.outer_margin;
        let collapsed_sidebar_width = collapsed_geometry.width;
        let sidebar = if self.sidebar_collapsed {
            let section_rows = sections
                .into_iter()
                .enumerate()
                .map(|(index, (_, section))| {
                    let section = gpui_component::Collapsible::collapsed(section, true);
                    div()
                        .w_full()
                        .when(index > 0, |this| this.mt(px(4.0 * self.ui_scale)))
                        .child(
                            gpui_component::sidebar::SidebarItem::render(
                                section, index, window, cx,
                            )
                            .into_any_element(),
                        )
                })
                .collect::<Vec<_>>();
            v_flex()
                .id("shell-sidebar-icons")
                .size_full()
                .overflow_y_scroll()
                .px(px(collapsed_outer_margin))
                .pt(px(8.0 * self.ui_scale))
                .children(section_rows)
                .into_any_element()
        } else {
            let mut sidebar = Sidebar::new("shell-sidebar").w_full();
            for (section_id, section) in sections {
                sidebar = sidebar
                    .child(ShellSidebarItem::section_gap(
                        crate::tree::SidebarSectionGap::new(Some(section_id), weak.clone()),
                    ))
                    .child(section);
            }
            sidebar
                .child(ShellSidebarItem::section_gap(
                    crate::tree::SidebarSectionGap::new(None, weak.clone()),
                ))
                .into_any_element()
        };

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
        let selected_count = self.active_tab().selection_count(entry_count);
        let selected_size: u64 = if selected_count == 0 {
            0
        } else if selected_count == entry_count {
            total_size
        } else {
            delegate.cached_selected_size.get().unwrap_or_else(|| {
                let s = entries
                    .iter()
                    .filter(|e| delegate.is_selected(e.id))
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
        let volume_read_only = self.active_tab().volume_read_only;
        // Hidden aggregate cached by the last completed load. An archive
        // listing has no hidden-file concept, so archive mode zeroes it
        // below along with the other overrides.
        let hidden_summary = self.active_tab().hidden_summary;
        // Same lifecycle: what the filter field excluded from the last
        // completed load, so the count/size beside it can't pass a
        // filtered view off as the whole folder.
        let filter_summary = self.active_tab().filter_summary;
        // A docked archive workbench owns its own table, so the tab's delegate
        // is (correctly) empty — read the counts from the archive's table
        // instead, or the status bar would report "Empty folder" while the
        // pane is showing an archive's contents.
        let archive_mode = self
            .active_tab()
            .tool_result
            .as_ref()
            .and_then(|s| s.archive_mode())
            .is_some();
        let (entry_count, total_size, selected_count, selected_size) = self
            .active_tab()
            .tool_result
            .as_ref()
            .and_then(|s| s.archive_mode())
            .map(|am| {
                let table = am.view.read(cx).table();
                let del = table.read(cx).delegate();
                let sel: Vec<&ferail_core::FileEntry> = del
                    .entries
                    .iter()
                    .filter(|e| del.is_selected(e.id))
                    .collect();
                (
                    del.entries.len(),
                    del.entries.iter().map(|e| e.size).sum::<u64>(),
                    sel.len(),
                    sel.iter().map(|e| e.size).sum::<u64>(),
                )
            })
            .unwrap_or((entry_count, total_size, selected_count, selected_size));
        let platform_mode = self.active_tab().platform_namespace.is_some();
        let (
            entry_count,
            total_size,
            selected_count,
            selected_size,
            free_bytes,
            volume_name,
            volume_read_only,
        ) = self
            .active_tab()
            .platform_namespace
            .as_ref()
            .map(|session| {
                let count = session.store().items().len();
                (
                    count,
                    0,
                    session.selection().selected_count(count),
                    0,
                    None,
                    None,
                    false,
                )
            })
            .unwrap_or((
                entry_count,
                total_size,
                selected_count,
                selected_size,
                free_bytes,
                volume_name,
                volume_read_only,
            ));
        let metrics = crate::status_bar::StatusMetrics {
            entries: entry_count,
            selected_count,
            selected_size,
            total_size,
            sizes_unavailable: platform_mode,
            free_bytes,
            volume_name,
            volume_read_only,
            hidden_count: if archive_mode || platform_mode {
                0
            } else {
                hidden_summary.count
            },
            hidden_bytes: if archive_mode || platform_mode {
                0
            } else {
                hidden_summary.bytes
            },
            filtered_count: if archive_mode || platform_mode {
                0
            } else {
                filter_summary.count
            },
            filtered_bytes: if archive_mode || platform_mode {
                0
            } else {
                filter_summary.bytes
            },
            // Filled in just before the status_bar::render call, where
            // the window id is in hand.
            stats: None,
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
        // App-footprint stats segment. The real path reads the cached
        // snapshot the off-thread sampler last published (never
        // samples here — Prime Directive); `--simulate-stats` pins the
        // fixed reference values instead.
        let metrics = crate::status_bar::StatusMetrics {
            stats: if self.simulated_stats {
                Some(crate::system_stats::SegmentParts::simulated())
            } else {
                self.process
                    .system_stats
                    .borrow()
                    .as_ref()
                    .map(|s| s.segment_parts(window.window_handle().window_id()))
            },
            ..metrics
        };
        let status_bar = crate::status_bar::render(
            metrics,
            &self.process.tasks,
            self.simulated_progress,
            Some(toggle_task_panel),
            self.show_hidden,
            Some(toggle_hidden_cb),
            window,
            cx,
        );
        // Auto-dismiss the background-task popover when the pointer
        // leaves it. `on_hover` fires only on a hover-state change and
        // starts `false`, so opening it above the status-bar click
        // point doesn't instant-close — it shuts when the mouse, after
        // being over the popover, moves off. Click-outside dismissal
        // (the shell's `on_mouse_down`) still applies for the
        // never-hovered case.
        let task_panel = crate::task_panel::render_if_open(
            self.task_panel_open && !crate::private_mode::enabled(),
            &self.process.tasks,
            cx,
        )
        .map(|panel| {
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
        });

        let content = div()
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
            .on_action(cx.listener(Self::on_toggle_flat_view))
            .on_action(cx.listener(Self::on_toggle_performance_hud))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_copy_path))
            .on_action(cx.listener(Self::on_show_windows_context_menu))
            .on_action(cx.listener(Self::on_generate_sha256))
            .on_action(cx.listener(Self::on_verify_checksums))
            .on_action(cx.listener(Self::on_create_checksum_file))
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
            .on_action(cx.listener(Self::on_go_to_folder))
            .on_action(cx.listener(Self::on_edit_breadcrumb))
            .on_action(cx.listener(Self::on_shortcuts_help))
            .on_action(cx.listener(Self::on_open_disk_usage))
            .on_action(cx.listener(Self::on_open_archive))
            .on_action(cx.listener(Self::on_convert_archive))
            .on_action(cx.listener(Self::on_pop_out_archive))
            .on_action(cx.listener(Self::on_close_tool_result))
            .on_action(cx.listener(Self::on_pop_out_disk_usage))
            .on_action(cx.listener(Self::on_find_duplicates))
            .on_action(cx.listener(Self::on_find_similar_images))
            .on_action(cx.listener(Self::on_open_viewer))
            .on_action(cx.listener(Self::on_slideshow_from_here))
            .on_action(cx.listener(Self::on_sort_by_name))
            .on_action(cx.listener(Self::on_sort_by_size))
            .on_action(cx.listener(Self::on_sort_by_kind))
            .on_action(cx.listener(Self::on_sort_by_modified))
            .on_action(cx.listener(Self::on_sort_by_ant_trail))
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
            .on_action(cx.listener(Self::on_cycle_sidebar_size))
            .on_action(cx.listener(Self::on_reset_sidebar_order))
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
            .on_action(cx.listener(Self::on_show_lock_holders))
            .on_action(cx.listener(Self::on_show_lock_holders_at_context))
            .on_action(cx.listener(Self::on_get_info_at_context))
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
            .on_action(cx.listener(Self::on_edit_file))
            .on_action(cx.listener(Self::on_edit_image))
            .on_action(cx.listener(Self::on_edit_text_file))
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
                        .favorites()
                        .read(cx)
                        .entry_by_id(id)
                        .map(|f| f.effective_label())
                        .unwrap_or_else(|| tr!("favorite").to_string());
                    let removed_for_undo =
                        this.process.favorites().read(cx).entry_by_id(id).cloned();
                    this.process.favorites().update(cx, |f, cx| {
                        f.remove(id, cx);
                    });
                    if let Some(fav) = removed_for_undo {
                        this.push_undo(UndoOp::RemoveFavorite(fav));
                    }
                    window.push_notification(
                        Notification::info(tr!(
                            "Removed \u{201C}{label}\u{201D} from Favorites \u{00B7} Cmd+Z to undo",
                            label = label
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
                let open_archive = self
                    .active_archive_view()
                    .map(|view| view.read(cx).archive_path().to_path_buf());
                let native_archive_dragging = crate::file_list::native_archive_drag_active();
                let native_drop_color = cx.theme().accent;
                let native_reject_color = cx.theme().danger;
                let open_archive_for_native = open_archive.clone();
                // Phase 6 review fix: an outer .context_menu on the
                // file body wrapper consumed the click events bound
                // for the inner DataTable row menu, causing every
                // file-row menu selection to dismiss without firing.
                // Don't reintroduce one here. The empty-space menu
                // (New Folder / Paste / …) now lives INSIDE the
                // table's own platform context-menu wrapper instead — see
                // `FileListDelegate::background_context_menu` and the
                // capture-phase region pick in `TableState::render` —
                // so it can't fight the row menus for events.
                // Drop target for OS file drags (Finder → Ferail,
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
                    .drag_over::<ExternalPaths>(move |style, _, _, cx| {
                        if platform_mode {
                            style.cursor_not_allowed()
                        } else {
                            style.bg(cx.theme().accent.opacity(0.06))
                        }
                    })
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                        if this.reject_platform_filesystem_command(window, cx) {
                            return;
                        }
                        let dest = this.active_tab().current_dir.clone();
                        this.handle_external_drop(paths.paths().to_vec(), dest, window, cx);
                    }))
                    // Entries dragged out of an archive workbench land in this
                    // tab's folder.
                    .drag_over::<crate::file_list::ArchiveEntryDrag>(move |style, drag, _, cx| {
                        if platform_mode || open_archive.as_ref() == Some(&drag.archive) {
                            style.cursor_not_allowed()
                        } else {
                            style
                                .cursor_copy()
                                .border_1()
                                .border_color(cx.theme().accent)
                                .bg(cx.theme().accent.opacity(0.06))
                        }
                    })
                    .on_drop(cx.listener(
                        |this, drag: &crate::file_list::ArchiveEntryDrag, window, cx| {
                            if this.reject_platform_filesystem_command(window, cx) {
                                return;
                            }
                            if this.active_archive_view().is_some_and(|view| {
                                view.read(cx).archive_path() == drag.archive.as_path()
                            }) {
                                // Releasing over the archive itself cancels
                                // the drag. Never let it fall through to this
                                // folder pane's current-directory semantics.
                                cx.stop_propagation();
                                return;
                            }
                            let dest = this.active_tab().current_dir.clone();
                            this.extract_archive_entries_into(
                                drag.archive.clone(),
                                drag.entries.clone(),
                                dest,
                                drag.password.clone(),
                                window,
                                cx,
                            );
                        },
                    ))
                    .on_mouse_move(cx.listener(|_this, _event, _window, cx| {
                        if crate::file_list::native_archive_drag_active() {
                            // The first native update entered with no GPUI
                            // typed drag; repaint so the hover cursor/ring
                            // below reflects the coordinator payload.
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _event, window, cx| {
                            // A retained GPUI drag is handled by on_drop.
                            // This is solely the cross-window promised-file
                            // fallback, where GPUI has already discarded it.
                            if cx.has_active_drag() {
                                return;
                            }
                            if this.reject_platform_filesystem_command(window, cx) {
                                return;
                            }
                            let Some(drag) = crate::file_list::take_native_archive_drag() else {
                                return;
                            };
                            cx.stop_propagation();
                            if this.active_archive_view().is_some() {
                                crate::log_info!(100, "archive-drag: rejected by archive pane");
                                return;
                            }
                            let dest = this.active_tab().current_dir.clone();
                            crate::log_info!(
                                100,
                                "archive-drag: accepted by file pane -> {}",
                                dest.display()
                            );
                            this.extract_archive_entries_into(
                                drag.archive,
                                drag.entries,
                                dest,
                                drag.password,
                                window,
                                cx,
                            );
                        }),
                    )
                    .when(native_archive_dragging, move |style| {
                        let reject = platform_mode || open_archive_for_native.is_some();
                        style.hover(move |style| {
                            if reject {
                                style
                                    .cursor_not_allowed()
                                    .border_2()
                                    .border_color(native_reject_color)
                            } else {
                                style
                                    .cursor_copy()
                                    .border_2()
                                    .border_color(native_drop_color)
                            }
                        })
                    })
                    .child(file_body);
                // The preview pane is hidden by default; whenever it's visible
                // the user explicitly turned it on (Cmd+P / View menu), so
                // honour that at any window width — the splitter's per-panel
                // min widths keep the layout sane on narrow windows. (A prior
                // auto-hide below 900px silently suppressed the explicit
                // toggle, so Cmd+P appeared to do nothing on smaller windows.)
                let preview_visible = self.preview_visible;
                let preview_pane = if preview_visible {
                    Some(self.preview_pane(cx))
                } else {
                    None
                };
                // Pull the persisted widths into the panels' initial
                // `.size(...)` — they survive across launches because
                // they're written through `on_resize` to app_state
                // (debounced via SPLITTER_PERSIST_INTERVAL below).
                let sidebar_width_px = if self.sidebar_collapsed {
                    px(collapsed_sidebar_width)
                } else {
                    px(self.sidebar_width)
                };
                let preview_width_px = px(self.preview_width);
                let weak = cx.weak_entity();
                let sidebar_collapsed = self.sidebar_collapsed;
                let sidebar_fixed = self.sidebar_collapsed;
                let sidebar_width_before = if sidebar_collapsed {
                    collapsed_sidebar_width
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
                        if preview_changed && !sidebar_fixed && !sizes.is_empty() {
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
                                    if sidebar_fixed {
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
                            .when(self.sidebar_collapsed, |this| {
                                this.overflow_hidden()
                                    .bg(cx.theme().sidebar)
                                    .border_r_1()
                                    .border_color(cx.theme().sidebar_border)
                            })
                            // Collapsed: pin the panel to the icon
                            // strip width so the drag handle can't
                            // reopen it accidentally; the TitleBar
                            // toggle is the one way back to expanded.
                            .when(self.sidebar_collapsed, |this| {
                                this.size_range(
                                    px(collapsed_sidebar_width)..px(collapsed_sidebar_width),
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
                            // A narrow center pane must clip its own breadcrumb
                            // and tool controls at the splitter boundary.  A
                            // child painting outside here used to cover the
                            // archive preview pane to its right.
                            .overflow_hidden()
                            .child(
                                v_flex()
                                    .size_full()
                                    .min_w_0()
                                    .overflow_hidden()
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
                let title_bar = self.title_bar(window, cx);
                let menu_bar = self.menu_bar.clone();
                let dock_handle = self.dock_handle(cx);
                v_flex()
                    .relative()
                    .size_full()
                    // Icon-size scrub, tracked at the window root so the drag
                    // keeps following the cursor once it leaves the 96-px bar
                    // (and so a release anywhere still commits the size).
                    // Both handlers return immediately unless a scrub is
                    // actually in flight.
                    .on_mouse_move(cx.listener(|this, event, window, cx| {
                        this.on_icon_size_drag(event, window, cx);
                        this.on_similar_criteria_drag(event, cx);
                    }))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.end_icon_size_drag(cx);
                            this.end_similar_criteria_drag();
                        }),
                    )
                    .on_mouse_up_out(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.end_icon_size_drag(cx);
                            this.end_similar_criteria_drag();
                        }),
                    )
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
            .when(!crate::private_mode::enabled(), |this| {
                this.children(Root::render_dialog_layer(window, cx))
            })
            // Notification overlay (Stage 5.c) — toasts pushed via
            // `Window::push_notification` show up in the corner the
            // active theme specifies. The outer `div().relative()`
            // gives the absolute-positioned notification list a
            // positioned ancestor to anchor against.
            .when(!crate::private_mode::enabled(), |this| {
                this.children(Root::render_notification_layer(window, cx))
            })
            // Explicit diagnostics only. The monitor is configured with
            // continuous(false), so it observes Ferail's real redraw workload
            // rather than keeping the window artificially busy.
            .when_some(self.performance_monitor.clone(), |this, monitor| {
                this.child(gpui_fps::FpsOverlay::new(&monitor))
            })
            // Keyboard-shortcuts help overlay (Stage 9.b). Renders
            // only when `shortcuts_help_filter` is Some(_); the
            // module reads `self` for the filter + input state.
            .children(crate::keyboard_help::render(self, cx))
            .into_any_element();
        crate::private_mode::protect_with_toggle(content, cx, Some(self.private_toggle_bounds))
    }
}
