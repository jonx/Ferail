//! File-list table delegate — Phase 4.c.
//!
//! Wraps `feraille-fs-native` enumeration in a `TableDelegate` so
//! `gpui-component`'s virtualized `Table` renders the entries
//! efficiently even for directories with thousands of files. Columns
//! are Name / Size / Kind / Modified, pre-formatted on the domain
//! side per the UI_NONBLOCKING contract carried over from the old app.

use std::cell::RefCell;
use std::collections::HashMap;
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
    table::{Column, TableDelegate, TableState},
    tooltip::Tooltip,
};
use smallvec::smallvec;

use crate::icons::{IconCache, file_type_icon, tint_color};

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
            columns: vec![
                Column::new("name", "Name").width(360.0),
                Column::new("size", "Size").width(100.0),
                Column::new("format", "Format").width(220.0),
                Column::new("modified", "Modified").width(160.0),
            ],
            fs,
            paths: HashMap::new(),
            icons,
            heats: Vec::new(),
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
            .filter(|e| show_hidden || !e.name.starts_with('.'))
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
        handle.error
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.paths.clear();
        self.heats.clear();
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
        _cx: &mut Context<TableState<Self>>,
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
        let mut row = div().id(("file-row", row_ix));
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
        // OS drag-out: GPUI's macOS backend recognises ExternalPaths
        // and uses NSFilePromise / NSPasteboard, so dragging a row
        // to the Finder desktop drops the actual file there. Other
        // apps (Mail, browsers) accept the same drag.
        if let Some(entry) = self.entries.get(row_ix) {
            let path = self.path_for_entry(entry.id).unwrap_or_default();
            if !path.as_os_str().is_empty() {
                let paths = ExternalPaths(smallvec![path]);
                return row.on_drag(paths, |paths, _, _, cx| cx.new(|_| paths.clone()));
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

        match col_ix {
            // Name — Lucide line-art icon tinted by category (files +
            // symlinks); macOS NSWorkspace bitmap for folders so
            // user-customised folder icons and cloud-sync overlays
            // still render. Optional quarantine badge in the top-right
            // corner. Tooltip carries the full filename so truncation
            // is recoverable. (Next-level Phase 1.)
            0 => {
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
                    .tooltip(move |window, cx| {
                        Tooltip::new(tooltip_name.clone()).build(window, cx)
                    })
                    .into_any_element()
            }
            1 => div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(entry.display_size.clone()))
                .into_any_element(),
            // Unified Format column (next-level Phase 1): replaces
            // the old Kind + Magic duplication. Mismatch indicator
            // surfaces when extension and magic disagree (renamed or
            // corrupted file).
            2 => {
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
            3 => div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(entry.display_mtime.clone()))
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
            Compress, CopyPath, Duplicate, GetInfo, MakeAlias, MoveToTrash, OpenInNewTab,
            OpenSelected, OpenWithSlot0, OpenWithSlot1, OpenWithSlot2, OpenWithSlot3,
            OpenWithSlot4, OpenWithSlot5, OpenWithSlot6, OpenWithSlot7, OpenWithSlot8,
            OpenWithSlot9, OpenWithSlot10, OpenWithSlot11, QuickLook, RenameSelected,
            RevealInFinder, ToggleTagBlue, ToggleTagGray, ToggleTagGreen, ToggleTagOrange,
            ToggleTagPurple, ToggleTagRed, ToggleTagYellow,
        };

        // Phase 6 follow-on: snapshot Open-With candidates and the
        // currently-applied tags for this row, both via synchronous
        // shell-mac calls (~10–50 ms each on macOS). The handlers
        // re-fetch on dispatch — duplicate work is acceptable at
        // human-scale right-click frequency, and avoids plumbing
        // per-right-click state up to Shell.
        let target_path = self
            .entries
            .get(row_ix)
            .and_then(|entry| self.path_for_entry(entry.id));
        let open_with_candidates: Vec<feraille_shell_mac::OpenWithCandidate> = target_path
            .as_ref()
            .map(|p| feraille_shell_mac::open_with_candidates(p))
            .unwrap_or_default();
        let applied_tags: Vec<feraille_core::commands::TagColor> = target_path
            .as_ref()
            .map(|p| feraille_shell_mac::read_canonical_tags(p))
            .unwrap_or_default();

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

        let mut menu = menu
            .menu("Open", Box::new(OpenSelected))
            .menu("Open in New Tab", Box::new(OpenInNewTab))
            .separator()
            .menu("Get Info", Box::new(GetInfo))
            .menu("Quick Look", Box::new(QuickLook))
            .separator()
            .menu("Reveal in Finder", Box::new(RevealInFinder))
            .menu("Copy Path", Box::new(CopyPath))
            .separator()
            .menu("Rename\u{2026}", Box::new(RenameSelected))
            .menu("Duplicate", Box::new(Duplicate))
            .menu("Make Alias", Box::new(MakeAlias))
            .menu("Compress", Box::new(Compress));

        // Build submenu Entities via `PopupMenu::build`, which only
        // needs `&mut App` (which we have via Context<TableState>'s
        // deref). The parent menu accepts pre-built submenu entries
        // through `PopupMenuItem::submenu(label, entity)`.
        let app_cx: &mut gpui::App = cx;

        if !open_with_candidates.is_empty() {
            let candidates_for_build = open_with_candidates.clone();
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

        menu.separator().menu("Move to Trash", Box::new(MoveToTrash))
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

/// Lookup helper for double-click open / Enter key — turn a row
/// selection into the path that should be navigated to (for a folder)
/// or opened with the default app (for a file).
pub fn entry_at<'a>(delegate: &'a FileListDelegate, row_ix: usize) -> Option<&'a FileEntry> {
    delegate.entries.get(row_ix)
}

/// Resolve a file entry's NodeId to an absolute path via the FS.
pub fn path_for(fs: &NativeFs, id: NodeId) -> Option<PathBuf> {
    fs.path_for(id)
}

/// Mark-of-the-Web quarantine badge — small red dot in the icon's
/// top-right corner. Same convention as the old soft-renderer app.
/// Pulled out of `render_td` so the file-icon and folder-icon paths
/// share one stylesheet.
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

/// Sort columns supported by `apply_sort`. Matches the column ids
/// the old app used (`feraille_controls::ColumnId` in the old
/// stack); kept in this module so feraille-gpui doesn't reach for
/// the soft-renderer controls crate. Pure logic, easy to extend.
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

impl SortColumn {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "name" => Some(Self::Name),
            "size" => Some(Self::Size),
            "format" | "kind" | "magic" => Some(Self::Format),
            "modified" | "mtime" => Some(Self::Modified),
            _ => None,
        }
    }
}

/// In-place sort with folders-first grouping (Finder convention).
/// Pure-logic port of `feraille_controls::sort_entries` — same
/// shape, just inlined here so we don't need to link the old UI
/// crate. Comments carried over from the original.
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

/// Apply a column sort to the live Table. Used by the
/// `--sort <col[-desc]>` CLI flag and (eventually) by clicks on
/// the column header row.
pub fn apply_sort<C: gpui::AppContext>(
    table: &gpui::Entity<TableState<FileListDelegate>>,
    column_name: &str,
    ascending: bool,
    cx: &mut C,
) {
    let Some(col) = SortColumn::from_str(column_name) else {
        crate::log_warn!(90, "unknown sort column: {column_name}");
        return;
    };
    table.update(cx, |state, cx| {
        sort_in_place(&mut state.delegate_mut().entries, col, ascending);
        state.refresh(cx);
    });
}

// Compile-time sanity check that FontWeight stays in scope — used
// transitively by render_td when we add bold-name styling for
// directories in a future polish pass.
#[allow(dead_code)]
fn _font_weight_check() -> FontWeight {
    FontWeight::MEDIUM
}
