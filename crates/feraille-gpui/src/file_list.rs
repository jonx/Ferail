//! File-list table delegate — Phase 4.c.
//!
//! Wraps `feraille-fs-native` enumeration in a `TableDelegate` so
//! `gpui-component`'s virtualized `Table` renders the entries
//! efficiently even for directories with thousands of files. Columns
//! are Name / Size / Kind / Modified, pre-formatted on the domain
//! side per the UI_NONBLOCKING contract carried over from the old app.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use feraille_core::{FileEntry, FsBackend, NodeId};
use feraille_fs_native::NativeFs;
use gpui::{
    App, AppContext as _, Context, Div, ExternalPaths, FontWeight, InteractiveElement,
    IntoElement, ParentElement, SharedString, Stateful, StatefulInteractiveElement as _, Styled,
    Window, div, img, px,
};
use gpui_component::{
    ActiveTheme,
    menu::PopupMenu,
    table::{Column, TableDelegate, TableState},
};
use smallvec::smallvec;

use crate::icons::IconCache;

/// Delegate that vends the current directory's entries to the
/// Table. Holds the live `Vec<FileEntry>`; the Shell rotates it on
/// every `navigate()`. The Vec is already filtered by both
/// `show_hidden` and `filter_text` at `load()` time — the Table
/// always sees the user-visible subset, no per-cell skipping.
pub struct FileListDelegate {
    pub entries: Vec<FileEntry>,
    pub columns: Vec<Column>,
    pub fs: Arc<NativeFs>,
    /// Shared icon cache. Lookup-or-fetch via NSWorkspace; subsequent
    /// renders for the same kind are a HashMap hit. Wrapped in
    /// Rc<RefCell> so render_td's `&mut self` can borrow without
    /// fighting the cache.
    pub icons: Rc<RefCell<IconCache>>,
}

impl FileListDelegate {
    pub fn new(fs: Arc<NativeFs>, icons: Rc<RefCell<IconCache>>) -> Self {
        Self {
            entries: Vec::new(),
            columns: vec![
                Column::new("name", "Name").width(360.0),
                Column::new("size", "Size").width(100.0),
                Column::new("kind", "Kind").width(120.0),
                Column::new("modified", "Modified").width(160.0),
            ],
            fs,
            icons,
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
                    e.name.to_lowercase().contains(&needle)
                        || e.display_kind.to_lowercase().contains(&needle)
                }
            })
            .collect();
        handle.error
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
        let row = div().id(("file-row", row_ix));
        // OS drag-out: GPUI's macOS backend recognises ExternalPaths
        // and uses NSFilePromise / NSPasteboard, so dragging a row
        // to the Finder desktop drops the actual file there. Other
        // apps (Mail, browsers) accept the same drag.
        if let Some(entry) = self.entries.get(row_ix) {
            let path = self.fs.path_for(entry.id).unwrap_or_default();
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
        let Some(entry) = self.entries.get(row_ix) else {
            return div().into_any_element();
        };

        match col_ix {
            // Name — real macOS icon (via NSWorkspace, cached by
            // file kind in self.icons) + filename. Falls back to a
            // 1×1 transparent placeholder when fetch fails (e.g.
            // outside macOS).
            0 => {
                let path = self.fs.path_for(entry.id).unwrap_or_default();
                let icon = self.icons.borrow_mut().icon_for(entry, &path);
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(
                        img(icon)
                            .w(px(18.0))
                            .h(px(18.0))
                            .flex_shrink_0(),
                    )
                    .child(
                        div()
                            .truncate()
                            .child(SharedString::from(entry.name.clone())),
                    )
                    .into_any_element()
            }
            1 => div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(entry.display_size.clone()))
                .into_any_element(),
            2 => div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(entry.display_kind.clone()))
                .into_any_element(),
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
        _row_ix: usize,
        menu: PopupMenu,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        use crate::shell::{CopyPath, MoveToTrash, OpenSelected, RevealInFinder};
        menu.menu("Open", Box::new(OpenSelected))
            .menu("Reveal in Finder", Box::new(RevealInFinder))
            .separator()
            .menu("Copy Path", Box::new(CopyPath))
            .separator()
            .menu("Move to Trash", Box::new(MoveToTrash))
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("This folder is empty.")
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

/// Sort columns supported by `apply_sort`. Matches the column ids
/// the old app used (`feraille_controls::ColumnId` in the old
/// stack); kept in this module so feraille-gpui doesn't reach for
/// the soft-renderer controls crate. Pure logic, easy to extend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Size,
    Kind,
    Magic,
    Modified,
}

impl SortColumn {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "name" => Some(Self::Name),
            "size" => Some(Self::Size),
            "kind" => Some(Self::Kind),
            "magic" => Some(Self::Magic),
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
            SortColumn::Kind => a
                .display_kind
                .to_lowercase()
                .cmp(&b.display_kind.to_lowercase()),
            SortColumn::Magic => a
                .display_magic
                .to_lowercase()
                .cmp(&b.display_magic.to_lowercase()),
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
