//! File-list table delegate — Phase 4.c.
//!
//! Wraps `feraille-fs-native` enumeration in a `TableDelegate` so
//! `gpui-component`'s virtualized `Table` renders the entries
//! efficiently even for directories with thousands of files. Columns
//! are Name / Size / Kind / Modified, pre-formatted on the domain
//! side per the UI_NONBLOCKING contract.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use feraille_core::{EntryKind, FileEntry, FsBackend, NodeId};
use feraille_fs_native::NativeFs;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Div, ExternalPaths, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, Stateful, StatefulInteractiveElement as _, Styled, Window, div,
    img, px, svg,
};
use gpui_component::{
    ActiveTheme,
    menu::{PopupMenu, PopupMenuItem},
    tooltip::Tooltip,
};
use smallvec::smallvec;

use crate::icons::{IconCache, file_type_icon, tint_color};
use crate::multi_table::{Column, ColumnSort, TableDelegate, TableEvent, TableState};

/// Delegate that vends the current directory's entries to the
/// Table. Holds the live `Vec<FileEntry>`; the Shell rotates it on
/// every `navigate()`. The Vec is already filtered by both
/// `show_hidden` and `filter_text` at `load()` time — the Table
/// always sees the user-visible subset, no per-cell skipping.
pub struct FileListDelegate {
    pub entries: Vec<FileEntry>,
    pub columns: Vec<Column>,
    pub fs: Arc<NativeFs>,
    /// Snapshot of entry paths captured during enumeration/application.
    /// Rendering may read this cache, but must not call back into the
    /// filesystem resolver.
    pub paths: HashMap<NodeId, PathBuf>,
    /// Shared icon cache. Lookup-or-fetch via NSWorkspace; subsequent
    /// renders for the same kind are a HashMap hit. Wrapped in
    /// Rc<RefCell> so render_td's `&mut self` can borrow without
    /// fighting the cache.
    pub icons: Rc<RefCell<IconCache>>,
    /// Ant Trail heat per row, parallel to `entries`. Populated by
    /// `Shell::load_path` after each enumerate. 0.0 = never visited
    /// (no tint); 1.0 = the most-visited folder. Renderer maps to
    /// a low-opacity accent background.
    pub heats: Vec<f32>,
    /// Finder colour tags per row, parallel to `entries`. Populated
    /// lazily by `load()` (synchronous bulk read via
    /// `crate::platform_shell::read_canonical_tags`, capped at the first
    /// N rows so a 50k-file folder doesn't pay the per-row xattr
    /// lookup synchronously on the UI thread). Renderer pairs each
    /// row's slot with the name cell to draw small coloured dots.
    pub tags: Vec<Vec<feraille_core::commands::TagColor>>,
    /// `is_favorited[row]` is `true` when the entry's path is currently
    /// in the user-curated Favorites list — drives the §5 star indicator
    /// on the Name cell. Recomputed by `Shell::refresh_file_list_favorited`
    /// on every load and whenever the Favorites entity changes (the
    /// `cx.observe` subscription in `Shell::new`).
    pub is_favorited: Vec<bool>,
    /// NodeId-keyed selection set (spec §2.2), mirrored from the
    /// active tab on every selection mutation, every streaming
    /// batch, and `Done`. `render_tr` looks up `entries[row].id` to
    /// decide whether to paint the selection fill. NodeId-keyed
    /// (not row-indexed) so sort/filter/streaming changes can
    /// reorder rows without desyncing the visual.
    pub selected_set: HashSet<NodeId>,
    /// The keyboard-cursor / range-lead, mirrored from the active
    /// tab. At most one. Cosmetic only — the Table primitive's
    /// `selected_row` overlay is the visible focus ring.
    pub lead: Option<NodeId>,
    /// Warm cache for the right-click "Open With" submenu: the most
    /// recently fetched `(path, LaunchServices candidates)` pair.
    /// Populated off the UI thread by [`spawn_open_with_warm`] —
    /// triggered on selection-lead changes and on a cache-miss menu
    /// build. The menu builder reads ONLY this cache (prime
    /// directive: no shell queries at menu-open time); a miss shows
    /// a disabled placeholder for that one open, exactly like
    /// Finder's "Fetching…" when LaunchServices is slow.
    ///
    /// Dispatch handlers (`Shell::open_with_slot`) resolve slot
    /// indices against this same cache so the app at slot N when
    /// the menu was BUILT is the app that opens — re-fetching at
    /// dispatch could reorder candidates and launch the wrong app.
    pub open_with_warm: Option<(PathBuf, Vec<crate::platform_shell::OpenWithCandidate>)>,
    /// The user's active column sort, recorded by `perform_sort` /
    /// `apply_sort`. `None` = natural (name-ascending) order. Read
    /// by the folder-size worker so late-arriving sizes can re-apply
    /// a live Size sort instead of leaving rows in stale positions.
    pub current_sort: Option<(SortColumn, bool)>,
}

impl FileListDelegate {
    pub fn new(fs: Arc<NativeFs>, icons: Rc<RefCell<IconCache>>) -> Self {
        Self {
            entries: Vec::new(),
            // Next-level Phase 1: Magic-driven `Format` column
            // replaces the duplicate Kind + Magic columns. The Format
            // cell prefers magic-detected text, falls back to the
            // extension-derived kind, and renders a small mismatch
            // indicator when the two genuinely disagree.
            //
            // Each column is marked `.sortable()` so clicking the
            // header runs `perform_sort` below. The Table primitive
            // also handles resizing + reorder when its TableState
            // has col_resizable / col_movable enabled (both default
            // true in our pinned version).
            columns: vec![
                Column::new("name", "Name").width(360.0).sortable(),
                Column::new("size", "Size").width(100.0).sortable(),
                Column::new("format", "Format").width(220.0).sortable(),
                Column::new("modified", "Modified").width(160.0).sortable(),
                // Description column: rich ` · `-joined facts derived
                // from the magic-byte parse (bitness/arch/subsystem
                // for binaries, w×h for images, channels/kHz/duration
                // for audio, etc.). Populated by the prefetch worker —
                // empty until the worker batch lands, then never
                // touched by paint. Not sortable in v1: lex sort of
                // description strings groups MP3s near MP4s but
                // separates 32-bit from 64-bit binaries, which is
                // confusing. Revisit if users ask.
                Column::new("description", "Description").width(320.0),
            ],
            fs,
            paths: HashMap::new(),
            icons,
            heats: Vec::new(),
            tags: Vec::new(),
            is_favorited: Vec::new(),
            selected_set: HashSet::new(),
            lead: None,
            open_with_warm: None,
            current_sort: None,
        }
    }

    /// Enumerate `path` via the FS backend, apply the show-hidden +
    /// filter-text filters, and swap the entries in. Returns the
    /// error variant when the OS reports one (e.g. macOS TCC
    /// denial) so the Shell can render an empty-state.
    pub fn load(
        &mut self,
        path: PathBuf,
        show_hidden: bool,
        filter_text: &str,
    ) -> Option<feraille_core::EnumerationError> {
        let id = self.fs.id_for_path(&path);
        let handle = self.fs.enumerate(id);
        let needle = filter_text.trim().to_lowercase();
        self.entries = handle
            .initial
            .into_iter()
            // Platform hidden semantics resolved at enumerate time
            // (UF_HIDDEN / FILE_ATTRIBUTE_HIDDEN), not a name check.
            .filter(|e| show_hidden || !e.hidden)
            .filter(|e| {
                if needle.is_empty() {
                    true
                } else {
                    // Filter searches the visible Format value (the
                    // unified Magic-or-Kind label the Format column
                    // shows), not just the raw kind.
                    let (format, _) = e.format_label();
                    e.name.to_lowercase().contains(&needle)
                        || format.to_lowercase().contains(&needle)
                }
            })
            .collect();
        self.paths.clear();
        for entry in &self.entries {
            if let Some(path) = self.fs.path_for(entry.id) {
                self.paths.insert(entry.id, path);
            }
        }
        // Reset heats; Shell repopulates after load returns.
        self.heats = vec![0.0; self.entries.len()];
        // Reset favorited bits; Shell repopulates from the favorites
        // entity right after load (Shell::refresh_file_list_favorited).
        self.is_favorited = vec![false; self.entries.len()];
        // Selection is not row-indexed, so we don't clear it here.
        // Shell drives reconciliation against the new model from
        // `apply_directory_batch` / `finish_directory_load` per
        // spec §2.6.
        // Read Finder colour tags for the first `TAG_PREFETCH_CAP`
        // rows. xattr reads cost ~1ms each on macOS — fine for
        // typical folders (~50 entries), capped so a giant Downloads
        // doesn't stall the UI thread. Beyond the cap, rows render
        // tagless until either (a) we add a background prefetch
        // pipeline or (b) the user explicitly Get-Info's the row.
        const TAG_PREFETCH_CAP: usize = 200;
        self.tags = Vec::with_capacity(self.entries.len());
        for entry in self.entries.iter().take(TAG_PREFETCH_CAP) {
            let tags = self
                .path_for_entry(entry.id)
                .map(|p| crate::platform_shell::read_canonical_tags(&p))
                .unwrap_or_default();
            self.tags.push(tags);
        }
        for _ in TAG_PREFETCH_CAP..self.entries.len() {
            self.tags.push(Vec::new());
        }
        handle.error
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.paths.clear();
        self.heats.clear();
        self.tags.clear();
        self.is_favorited.clear();
        // selected_set / lead are NodeId-keyed and reconciled by
        // Shell against the new model; not cleared here.
    }

    pub fn replace_entries(
        &mut self,
        entries: Vec<FileEntry>,
        paths: HashMap<NodeId, PathBuf>,
        heats: Vec<f32>,
    ) {
        self.entries = entries;
        self.paths = paths;
        self.heats = heats;
        self.tags = vec![Vec::new(); self.entries.len()];
        self.is_favorited = vec![false; self.entries.len()];
        // selected_set / lead are NodeId-keyed; reconciliation is
        // Shell's job (see `refresh_file_list_selection`).
    }

    pub fn append_entries(
        &mut self,
        entries: Vec<FileEntry>,
        paths: HashMap<NodeId, PathBuf>,
        heats: Vec<f32>,
    ) {
        self.paths.extend(paths);
        let n = entries.len();
        self.entries.extend(entries);
        self.heats.extend(heats);
        self.tags.extend((0..n).map(|_| Vec::new()));
        self.is_favorited.extend((0..n).map(|_| false));
        // selected_set / lead untouched — NodeId-keyed, not row-keyed.
    }

    pub fn path_for_entry(&self, id: NodeId) -> Option<PathBuf> {
        self.paths.get(&id).cloned()
    }
}

impl TableDelegate for FileListDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.entries.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let _path_guard = feraille_core::path_guard::enter_render();
        // Ant Trail heat tint (Stage 9.b). Renders only on directory
        // rows — files aren't tracked in the trail. 0.0 → no tint;
        // up to ~0.30 warm-orange opacity at full heat. The warm
        // hue matches the "heat" metaphor (frequently-visited
        // folders glow brighter); accent / primary blend too far
        // into hover/selection territory.
        let heat = self.heats.get(row_ix).copied().unwrap_or(0.0);
        let kind_is_dir = self
            .entries
            .get(row_ix)
            .map(|e| matches!(e.kind, EntryKind::Directory))
            .unwrap_or(false);
        let entry_id = self.entries.get(row_ix).map(|e| e.id);
        let in_set = entry_id
            .map(|id| self.selected_set.contains(&id))
            .unwrap_or(false);
        let is_lead = entry_id == self.lead && entry_id.is_some();
        let mut row = div().id(("file-row", row_ix));
        // Folder rows are drop targets for OS file drags (dnd-spec
        // §3.5): accent ring on hover, drop surfaces as a TableEvent
        // for the shell to run the transfer into this folder. Stop
        // propagation so the pane-background target underneath
        // doesn't also fire.
        if kind_is_dir {
            row = row
                .drag_over::<ExternalPaths>(|style, _, _, cx| {
                    style
                        .border_1()
                        .border_color(cx.theme().accent)
                        .bg(cx.theme().accent.opacity(0.10))
                })
                .on_drop(cx.listener(
                    move |_state, paths: &ExternalPaths, _window, cx| {
                        cx.stop_propagation();
                        cx.emit(TableEvent::ExternalDrop {
                            row_ix,
                            paths: paths.paths().to_vec(),
                        });
                    },
                ));
        }
        if kind_is_dir && heat > 0.0 {
            // Warm orange tint, scaled by heat. Stable hue across
            // light/dark themes (a theme-color .opacity() would
            // multiply the theme's existing alpha and dim out in
            // dark mode).
            let alpha = (heat * 0.30).clamp(0.0, 1.0);
            row = row.bg(gpui::Rgba {
                r: 1.0,
                g: 0.55,
                b: 0.26,
                a: alpha,
            });
        }
        // Spec §2 multi-select fill. Painted for every set member
        // EXCEPT the lead — the Table primitive draws its own
        // `selected_row` overlay on the lead, which serves as the
        // distinct focus ring spec §2.3 calls for.
        if in_set && !is_lead {
            row = row.bg(cx.theme().table_active);
        }
        // OS drag-out: GPUI's macOS backend recognises ExternalPaths
        // and uses NSFilePromise / NSPasteboard, so dragging rows to
        // Finder / other apps drops the actual files. Spec §3.1:
        // pressing a selected row drags the full visible-order
        // selection; pressing an unselected row drags just that row.
        if let Some(entry) = self.entries.get(row_ix) {
            let row_is_selected = self.selected_set.contains(&entry.id);
            let mut drag_paths = smallvec![];
            if row_is_selected {
                for selected in &self.entries {
                    if self.selected_set.contains(&selected.id) {
                        if let Some(path) = self.path_for_entry(selected.id) {
                            drag_paths.push(path);
                        }
                    }
                }
            } else if let Some(path) = self.path_for_entry(entry.id) {
                drag_paths.push(path);
            }
            if !drag_paths.is_empty() {
                return row.on_drag(ExternalPaths(drag_paths), |paths, _, _, cx| {
                    cx.new(|_| paths.clone())
                });
            }
        }
        row
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let _path_guard = feraille_core::path_guard::enter_render();
        let Some(entry) = self.entries.get(row_ix) else {
            return div().into_any_element();
        };

        let col_key = self
            .columns
            .get(col_ix)
            .map(|col| col.key.as_ref())
            .unwrap_or("");

        match col_key {
            // Name — Lucide line-art icon tinted by category (files +
            // symlinks); macOS NSWorkspace bitmap for folders so
            // user-customised folder icons and cloud-sync overlays
            // still render. Optional quarantine badge in the top-right
            // corner. Tooltip carries the full filename so truncation
            // is recoverable. (Next-level Phase 1.)
            "name" => {
                use feraille_core::EntryKind;
                let path = self.path_for_entry(entry.id).unwrap_or_default();
                let quarantined = entry.is_quarantined;
                let icon_wrapper: gpui::AnyElement = match entry.kind {
                    EntryKind::Directory => {
                        let icon = self.icons.borrow_mut().icon_for(entry, &path);
                        div()
                            .relative()
                            .flex_shrink_0()
                            .w(px(18.0))
                            .h(px(18.0))
                            .child(img(icon).w(px(18.0)).h(px(18.0)))
                            .when(quarantined, badge_overlay)
                            .into_any_element()
                    }
                    EntryKind::File | EntryKind::Symlink => {
                        let icon = file_type_icon(entry);
                        let tint = tint_color(icon.tint, cx);
                        div()
                            .relative()
                            .flex_shrink_0()
                            .w(px(18.0))
                            .h(px(18.0))
                            .child(
                                svg()
                                    .path(icon.path)
                                    .w(px(18.0))
                                    .h(px(18.0))
                                    .text_color(tint),
                            )
                            .when(quarantined, badge_overlay)
                            .into_any_element()
                    }
                };
                let full_name = entry.name.clone();
                let tooltip_name = full_name.clone();
                // Inline tag chips — 6-DIP coloured dots after the
                // filename, one per applied Finder tag (max 7). Read
                // synchronously at load() time and stored in the
                // delegate; render only consumes the cached Vec.
                let row_tags = self.tags.get(row_ix).cloned().unwrap_or_default();
                let mut chips = gpui_component::h_flex().gap_1().flex_shrink_0();
                for color in row_tags.iter().take(7) {
                    chips = chips.child(
                        div()
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded_full()
                            .bg(tag_color_rgba(*color)),
                    );
                }
                // §5 favorited indicator: small accent star trailing
                // the name. Only painted for folder rows (files can't
                // be favorited) where the row's path is in the favorites
                // index. The parallel vec is refreshed by Shell on every
                // load + every favorites mutation.
                let is_favorited = self.is_favorited.get(row_ix).copied().unwrap_or(false);
                let star_color = cx.theme().primary;
                let star = if is_favorited && matches!(entry.kind, EntryKind::Directory) {
                    svg()
                        .path("icons/nav/star.svg")
                        .w(px(12.0))
                        .h(px(12.0))
                        .text_color(star_color)
                        .into_any_element()
                } else {
                    div().w(px(0.0)).h(px(12.0)).into_any_element()
                };
                div()
                    .id(("file-row-name", row_ix))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(icon_wrapper)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(full_name)),
                    )
                    .child(chips)
                    .child(star)
                    .tooltip(move |window, cx| Tooltip::new(tooltip_name.clone()).build(window, cx))
                    .into_any_element()
            }
            "size" => div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(entry.display_size.clone()))
                .into_any_element(),
            // Unified Format column (next-level Phase 1): replaces
            // the old Kind + Magic duplication. Mismatch indicator
            // surfaces when extension and magic disagree (renamed or
            // corrupted file).
            "format" => {
                let (label, mismatch) = entry.format_label();
                if label.is_empty() {
                    return div().into_any_element();
                }
                let tip_kind = entry.display_kind.clone();
                let tip_magic = entry.display_magic.clone();
                let mut row = div()
                    .id(("file-row-format", row_ix))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(label.clone())),
                    );
                if mismatch {
                    let alert_color = cx.theme().danger;
                    row = row
                        .child(
                            svg()
                                .path("icons/triangle-alert.svg")
                                .w(px(12.0))
                                .h(px(12.0))
                                .text_color(alert_color),
                        )
                        .tooltip(move |window, cx| {
                            Tooltip::new(SharedString::from(format!(
                                "Extension says \u{201C}{}\u{201D} but content looks like \u{201C}{}\u{201D}.",
                                tip_kind, tip_magic
                            )))
                            .build(window, cx)
                        });
                }
                row.into_any_element()
            }
            "modified" => div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(entry.display_mtime.clone()))
                .into_any_element(),
            // Description: rich facts from the magic-byte parse,
            // populated lazily by the prefetch worker. Empty string
            // renders as an empty cell — no skeleton shimmer in v1.
            "description" => div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(entry.display_description.clone()))
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        use crate::shell::{
            ClearQuarantine, Compress, CopyPath, Duplicate, GetInfo, MakeAlias, MoveToTrash,
            OpenInNewTab, OpenSelected, OpenTerminalHere, OpenWithSlot0, OpenWithSlot1,
            OpenWithSlot2, OpenWithSlot3, OpenWithSlot4, OpenWithSlot5, OpenWithSlot6,
            OpenWithSlot7, OpenWithSlot8, OpenWithSlot9, OpenWithSlot10, OpenWithSlot11,
            QuickLook, RenameSelected, RevealInFinder, ToggleFavoriteForTarget, ToggleTagBlue,
            ToggleTagGray, ToggleTagGreen, ToggleTagOrange, ToggleTagPurple, ToggleTagRed,
            ToggleTagYellow,
        };

        // Prime directive: menu building is read-only — no shell or
        // filesystem queries at menu-open time.
        //
        // Tags come from the per-row `self.tags` slots the bulk load
        // already populated; the checkmarks therefore always agree
        // with the row's visible tag dots (rows past the load cap
        // show no dots and no checkmarks — consistent).
        //
        // Open With candidates come from the `open_with_warm` cache,
        // populated off-thread on selection-lead changes (see
        // `Shell::warm_open_with_for_row`). On a cache miss — e.g.
        // a direct right-click on a row that was never selected —
        // we show a disabled placeholder this one time, kick the
        // warm fetch, and the next open has the real submenu. Same
        // UX as Finder's "Fetching…" under a slow LaunchServices.
        let target_path = self
            .entries
            .get(row_ix)
            .and_then(|entry| self.path_for_entry(entry.id));
        let warmed_candidates: Option<Vec<crate::platform_shell::OpenWithCandidate>> =
            match (&target_path, &self.open_with_warm) {
                (Some(p), Some((warm_path, cands))) if warm_path == p => Some(cands.clone()),
                _ => None,
            };
        if warmed_candidates.is_none() {
            if let Some(p) = &target_path {
                spawn_open_with_warm(cx.entity().clone(), p.clone(), cx);
            }
        }
        let applied_tags: Vec<feraille_core::commands::TagColor> =
            self.tags.get(row_ix).cloned().unwrap_or_default();

        // Tags submenu — built as a nested PopupMenu Entity via
        // PopupMenu::build. Each colour is a `menu_with_check` so
        // applied tags render with a leading checkmark. Click
        // toggles via the ToggleTagX action.
        let tag_red_on = applied_tags.contains(&feraille_core::commands::TagColor::Red);
        let tag_orange_on = applied_tags.contains(&feraille_core::commands::TagColor::Orange);
        let tag_yellow_on = applied_tags.contains(&feraille_core::commands::TagColor::Yellow);
        let tag_green_on = applied_tags.contains(&feraille_core::commands::TagColor::Green);
        let tag_blue_on = applied_tags.contains(&feraille_core::commands::TagColor::Blue);
        let tag_purple_on = applied_tags.contains(&feraille_core::commands::TagColor::Purple);
        let tag_gray_on = applied_tags.contains(&feraille_core::commands::TagColor::Gray);

        let is_folder = self
            .entries
            .get(row_ix)
            .map(|e| matches!(e.kind, EntryKind::Directory))
            .unwrap_or(false);
        let already_favorited = self.is_favorited.get(row_ix).copied().unwrap_or(false);
        let favorite_label = if already_favorited {
            "Remove from Favorites"
        } else {
            "Add to Favorites"
        };

        let mut menu = menu
            .menu("Open", Box::new(OpenSelected))
            .menu("Open in New Tab", Box::new(OpenInNewTab))
            .separator()
            .menu("Get Info", Box::new(GetInfo))
            .menu("Quick Look", Box::new(QuickLook))
            .separator()
            .menu(feraille_core::commands::REVEAL_LABEL, Box::new(RevealInFinder))
            .menu("Copy Path", Box::new(CopyPath));
        if is_folder {
            // Folder-only: open a terminal at the right-clicked directory,
            // sitting directly under Copy Path so the two path-oriented
            // actions group together.
            menu = menu.menu("Open Terminal Here", Box::new(OpenTerminalHere));
        }
        let mut menu = menu
            .separator()
            .menu("Rename\u{2026}", Box::new(RenameSelected))
            .menu("Duplicate", Box::new(Duplicate))
            .menu("Make Alias", Box::new(MakeAlias))
            .menu("Compress", Box::new(Compress));
        if self
            .entries
            .get(row_ix)
            .map(|e| e.is_quarantined)
            .unwrap_or(false)
        {
            // Quarantined rows only: strip the Mark-of-the-Web + its
            // where-from provenance. Reads the cached row flag — no
            // xattr query at menu-open time.
            menu = menu.separator().menu(
                feraille_core::commands::CLEAR_QUARANTINE_LABEL,
                Box::new(ClearQuarantine),
            );
        }
        if is_folder {
            // Toggle the row's path against the user's Favorites
            // (docs/features/FAVORITES.md §2.1). The right-click already
            // set `context_row`; `resolve_favorite_target` picks the
            // row's path from there.
            menu = menu
                .separator()
                .menu(favorite_label, Box::new(ToggleFavoriteForTarget));
        }

        // Build submenu Entities via `PopupMenu::build`, which only
        // needs `&mut App` (which we have via Context<TableState>'s
        // deref). The parent menu accepts pre-built submenu entries
        // through `PopupMenuItem::submenu(label, entity)`.
        let app_cx: &mut gpui::App = cx;

        match &warmed_candidates {
            Some(candidates) if !candidates.is_empty() => {
                let candidates_for_build = candidates.clone();
                let open_with_submenu = PopupMenu::build(window, app_cx, move |mut m, _w, _c| {
                    for (i, cand) in candidates_for_build.iter().take(12).enumerate() {
                        let label = if cand.is_default {
                            SharedString::from(format!("{} (default)", cand.name))
                        } else {
                            SharedString::from(cand.name.clone())
                        };
                        let action: Box<dyn gpui::Action> = match i {
                            0 => Box::new(OpenWithSlot0),
                            1 => Box::new(OpenWithSlot1),
                            2 => Box::new(OpenWithSlot2),
                            3 => Box::new(OpenWithSlot3),
                            4 => Box::new(OpenWithSlot4),
                            5 => Box::new(OpenWithSlot5),
                            6 => Box::new(OpenWithSlot6),
                            7 => Box::new(OpenWithSlot7),
                            8 => Box::new(OpenWithSlot8),
                            9 => Box::new(OpenWithSlot9),
                            10 => Box::new(OpenWithSlot10),
                            _ => Box::new(OpenWithSlot11),
                        };
                        m = m.menu(label, action);
                    }
                    m
                });
                menu = menu.item(PopupMenuItem::submenu("Open With", open_with_submenu));
            }
            // Cache warm but LaunchServices offered nothing: omit the
            // submenu entirely (pre-existing behavior for empty sets).
            Some(_) => {}
            // Cache miss — the warm fetch was kicked above; show a
            // disabled placeholder for this one open.
            None => {
                menu = menu.item(
                    PopupMenuItem::new("Open With (indexing\u{2026})").disabled(true),
                );
            }
        }

        let tags_submenu = PopupMenu::build(window, app_cx, move |m, _w, _c| {
            m.menu_with_check("Red", tag_red_on, Box::new(ToggleTagRed))
                .menu_with_check("Orange", tag_orange_on, Box::new(ToggleTagOrange))
                .menu_with_check("Yellow", tag_yellow_on, Box::new(ToggleTagYellow))
                .menu_with_check("Green", tag_green_on, Box::new(ToggleTagGreen))
                .menu_with_check("Blue", tag_blue_on, Box::new(ToggleTagBlue))
                .menu_with_check("Purple", tag_purple_on, Box::new(ToggleTagPurple))
                .menu_with_check("Gray", tag_gray_on, Box::new(ToggleTagGray))
        });
        menu = menu.item(PopupMenuItem::submenu("Tags", tags_submenu));

        menu.separator()
            .menu(feraille_core::commands::TRASH_LABEL, Box::new(MoveToTrash))
    }

    fn move_column(
        &mut self,
        col_ix: usize,
        to_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        if col_ix == to_ix || col_ix >= self.columns.len() || to_ix >= self.columns.len() {
            return;
        }
        let column = self.columns.remove(col_ix);
        self.columns.insert(to_ix, column);
    }

    /// Click on a header runs this — we delegate to the existing
    /// `sort_in_place` helper, mapping the column index back to a
    /// `SortColumn` via the `columns` vec's index → key lookup. The
    /// Table's column moves shift indices around, which is why we
    /// resolve via key rather than hard-coding indices.
    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        let Some(col) = self.columns.get(col_ix) else {
            return;
        };
        let Ok(sort_col) = col.key.parse::<SortColumn>() else {
            return;
        };
        match sort {
            ColumnSort::Default => {
                // "Reset to natural order" — sort by name ascending
                // (Finder convention) as a deterministic fallback,
                // since we don't retain the load-time order.
                sort_in_place(&mut self.entries, SortColumn::Name, true);
                self.current_sort = None;
            }
            ColumnSort::Ascending => {
                sort_in_place(&mut self.entries, sort_col, true);
                self.current_sort = Some((sort_col, true));
            }
            ColumnSort::Descending => {
                sort_in_place(&mut self.entries, sort_col, false);
                self.current_sort = Some((sort_col, false));
            }
        }
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        // Phase 10 polish: a centred, two-line empty state with the
        // Lucide inbox glyph above the copy reads "considered" rather
        // than "we forgot to handle this case."
        gpui_component::v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                gpui::svg()
                    .path("icons/inbox.svg")
                    .w(px(48.0))
                    .h(px(48.0))
                    .text_color(cx.theme().muted_foreground.opacity(0.5)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("This folder is empty."),
            )
    }
}

/// Fetch Open With candidates for `path` on the background executor
/// and store them in the delegate's [`FileListDelegate::open_with_warm`]
/// cache. The fetch (`open_with_candidates`, ~10–50 ms of
/// LaunchServices / IAssocHandler work) never runs on the UI thread.
/// Last-writer-wins by design: the cache holds one entry — the most
/// recently warmed path — which is always the row the user is about
/// to right-click.
pub fn spawn_open_with_warm(
    table: gpui::Entity<TableState<FileListDelegate>>,
    path: PathBuf,
    cx: &mut gpui::App,
) {
    let weak = table.downgrade();
    let fetch_path = path.clone();
    cx.spawn(async move |cx| {
        let candidates = cx
            .background_executor()
            .spawn(async move { crate::platform_shell::open_with_candidates(&fetch_path) })
            .await;
        let _ = weak.update(cx, |state, cx| {
            state.delegate_mut().open_with_warm = Some((path, candidates));
            cx.notify();
        });
    })
    .detach();
}

/// Lookup helper for double-click open / Enter key — turn a row
/// selection into the path that should be navigated to (for a folder)
/// or opened with the default app (for a file).
pub fn entry_at(delegate: &FileListDelegate, row_ix: usize) -> Option<&FileEntry> {
    delegate.entries.get(row_ix)
}

/// Resolve a file entry's NodeId to an absolute path via the FS.
pub fn path_for(fs: &NativeFs, id: NodeId) -> Option<PathBuf> {
    fs.path_for(id)
}

/// Map a Finder colour tag to its render colour. Values mirror the
/// stock macOS palette (NSColor systemRed/orange/etc. with a slight
/// saturation bump so the 6-DIP dots stay readable on tinted row
/// backgrounds).
fn tag_color_rgba(c: feraille_core::commands::TagColor) -> gpui::Rgba {
    use feraille_core::commands::TagColor;
    match c {
        TagColor::Red => gpui::Rgba {
            r: 1.0,
            g: 0.23,
            b: 0.19,
            a: 1.0,
        },
        TagColor::Orange => gpui::Rgba {
            r: 1.0,
            g: 0.58,
            b: 0.0,
            a: 1.0,
        },
        TagColor::Yellow => gpui::Rgba {
            r: 1.0,
            g: 0.80,
            b: 0.0,
            a: 1.0,
        },
        TagColor::Green => gpui::Rgba {
            r: 0.30,
            g: 0.85,
            b: 0.39,
            a: 1.0,
        },
        TagColor::Blue => gpui::Rgba {
            r: 0.0,
            g: 0.48,
            b: 1.0,
            a: 1.0,
        },
        TagColor::Purple => gpui::Rgba {
            r: 0.69,
            g: 0.32,
            b: 0.87,
            a: 1.0,
        },
        TagColor::Gray => gpui::Rgba {
            r: 0.56,
            g: 0.56,
            b: 0.58,
            a: 1.0,
        },
    }
}

/// Mark-of-the-Web quarantine badge — small red dot in the icon's
/// top-right corner. Pulled out of `render_td` so the file-icon and
/// folder-icon paths share one stylesheet.
fn badge_overlay(this: Div) -> Div {
    this.child(
        div()
            .absolute()
            .top(px(-1.0))
            .right(px(-1.0))
            .w(px(7.0))
            .h(px(7.0))
            .rounded_full()
            .bg(gpui::rgb(0xFF3B30)),
    )
}

/// Sort columns supported by `apply_sort`. Pure logic, easy to extend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Size,
    /// Unified Format column (next-level Phase 1) — sorts by the
    /// magic-detected description, falling back to the extension-
    /// derived kind. Replaces the old `Kind` + `Magic` sort options.
    Format,
    Modified,
}

impl std::str::FromStr for SortColumn {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "name" => Ok(Self::Name),
            "size" => Ok(Self::Size),
            "format" | "kind" | "magic" => Ok(Self::Format),
            "modified" | "mtime" => Ok(Self::Modified),
            _ => Err(()),
        }
    }
}

/// In-place sort with folders-first grouping (Finder convention).
/// Pure logic, easy to extend.
pub fn sort_in_place(entries: &mut [feraille_core::FileEntry], col: SortColumn, asc: bool) {
    use std::cmp::Ordering;
    entries.sort_by(|a, b| {
        // Folders always come before non-folders, regardless of
        // sort direction. The sort key only orders within each
        // group.
        let group_order = match (a.kind, b.kind) {
            (feraille_core::EntryKind::Directory, feraille_core::EntryKind::Directory) => {
                Ordering::Equal
            }
            (feraille_core::EntryKind::Directory, _) => Ordering::Less,
            (_, feraille_core::EntryKind::Directory) => Ordering::Greater,
            _ => Ordering::Equal,
        };
        if group_order != Ordering::Equal {
            return group_order;
        }
        let cmp = match col {
            SortColumn::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortColumn::Size => a.size.cmp(&b.size),
            SortColumn::Format => a
                .format_label()
                .0
                .to_lowercase()
                .cmp(&b.format_label().0.to_lowercase()),
            SortColumn::Modified => a.mtime_unix.cmp(&b.mtime_unix),
        };
        if asc { cmp } else { cmp.reverse() }
    });
}

/// Apply a column sort to the live Table by enum. The toolbar sort
/// menu calls this directly; [`apply_sort`] is the string-keyed
/// wrapper for the `--sort` CLI flag.
pub fn apply_sort_column<C: gpui::AppContext>(
    table: &gpui::Entity<TableState<FileListDelegate>>,
    col: SortColumn,
    ascending: bool,
    cx: &mut C,
) {
    table.update(cx, |state, cx| {
        sort_in_place(&mut state.delegate_mut().entries, col, ascending);
        state.delegate_mut().current_sort = Some((col, ascending));
        state.refresh(cx);
    });
}

/// Apply a column sort to the live Table. Used by the
/// `--sort <col[-desc]>` CLI flag and (eventually) by clicks on
/// the column header row.
pub fn apply_sort<C: gpui::AppContext>(
    table: &gpui::Entity<TableState<FileListDelegate>>,
    column_name: &str,
    ascending: bool,
    cx: &mut C,
) {
    let Ok(col) = column_name.parse::<SortColumn>() else {
        crate::log_warn!(90, "unknown sort column: {column_name}");
        return;
    };
    apply_sort_column(table, col, ascending, cx);
}

// Compile-time sanity check that FontWeight stays in scope — used
// transitively by render_td when we add bold-name styling for
// directories in a future polish pass.
#[allow(dead_code)]
fn _font_weight_check() -> FontWeight {
    FontWeight::MEDIUM
}
