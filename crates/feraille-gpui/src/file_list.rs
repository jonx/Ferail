//! File-list table delegate — Phase 4.c.
//!
//! Wraps `feraille-fs-native` enumeration in a `TableDelegate` so
//! `gpui-component`'s virtualized `Table` renders the entries
//! efficiently even for directories with thousands of files. Columns
//! are Name / Size / Kind / Modified, pre-formatted on the domain
//! side per the UI_NONBLOCKING contract carried over from the old app.

use std::path::PathBuf;
use std::sync::Arc;

use feraille_core::{EntryKind, FileEntry, FsBackend, NodeId};
use feraille_fs_native::NativeFs;
use gpui::{
    App, AppContext as _, Context, Div, ExternalPaths, FontWeight, InteractiveElement,
    IntoElement, ParentElement, SharedString, Stateful, StatefulInteractiveElement as _, Styled,
    Window, div,
};
use gpui_component::{
    ActiveTheme,
    menu::PopupMenu,
    table::{Column, TableDelegate, TableState},
};
use smallvec::smallvec;

/// Delegate that vends the current directory's entries to the
/// Table. Holds the live `Vec<FileEntry>`; the Shell rotates it on
/// every `navigate()`.
pub struct FileListDelegate {
    pub entries: Vec<FileEntry>,
    pub columns: Vec<Column>,
    pub fs: Arc<NativeFs>,
}

impl FileListDelegate {
    pub fn new(fs: Arc<NativeFs>) -> Self {
        Self {
            entries: Vec::new(),
            columns: vec![
                Column::new("name", "Name").width(360.0),
                Column::new("size", "Size").width(100.0),
                Column::new("kind", "Kind").width(120.0),
                Column::new("modified", "Modified").width(160.0),
            ],
            fs,
        }
    }

    /// Enumerate `path` via the FS backend and swap the entries in.
    /// Returns the error variant when the OS reports one (e.g. macOS
    /// TCC denial) so the Shell can render an empty-state.
    pub fn load(&mut self, path: PathBuf, show_hidden: bool) -> Option<feraille_core::EnumerationError> {
        let id = self.fs.id_for_path(&path);
        let handle = self.fs.enumerate(id);
        self.entries = if show_hidden {
            handle.initial
        } else {
            handle
                .initial
                .into_iter()
                .filter(|e| !e.name.starts_with('.'))
                .collect()
        };
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
            // Name — icon glyph (just text for now; real icons come
            // in a future iter that re-uses feraille_shell_mac's
            // NSWorkspace fetcher) + filename.
            0 => {
                let glyph = match entry.kind {
                    EntryKind::Directory => "\u{1F4C1}", // FILE FOLDER
                    EntryKind::File => "\u{1F4C4}",      // PAGE FACING UP
                    EntryKind::Symlink => "\u{2934}",    // ARROW POINTING RIGHTWARDS THEN CURVING UPWARDS
                };
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(
                        div()
                            .w_4()
                            .flex_shrink_0()
                            .text_color(cx.theme().muted_foreground)
                            .child(glyph),
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

// Compile-time sanity check that FontWeight stays in scope — used
// transitively by render_td when we add bold-name styling for
// directories in a future polish pass.
#[allow(dead_code)]
fn _font_weight_check() -> FontWeight {
    FontWeight::MEDIUM
}
