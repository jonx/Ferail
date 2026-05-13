//! File-manager shell — main window content during Phases 4+.
//!
//! Phase 4.a: holds `current_dir`, renders a clickable breadcrumb at
//! the top of the main pane, sidebar entries are still placeholder.
//! Phase 4.b will wire the sidebar to real Locations/Volumes. Phase
//! 4.c brings the virtualized file list.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use feraille_core::{EntryKind, EnumerationError};
use feraille_fs_native::{home_dir, list_volumes, open_with_default, NativeFs, VolumeInfo};
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, Sizable, button::Button, h_flex,
    input::{Input, InputEvent, InputState},
    sidebar::{Sidebar, SidebarHeader},
    switch::Switch,
    table::{DataTable, TableEvent, TableState},
    v_flex, Root, WindowExt,
};

use crate::app_state::{self, AppState};
use crate::file_list::FileListDelegate;
use crate::fs_watcher::{FsWatcher, POLL_INTERVAL};
use crate::icons::IconCache;
use crate::tasks::TaskRegistry;
use crate::tree::{TreeChild, TreeRowSpec, TreeSection};

actions!(
    shell,
    [
        NavigateParent,
        NavigateBack,
        NavigateForward,
        OpenSelected,
        Refresh,
        ToggleHidden,
        OpenSettings,
        CopyPath,
        MoveToTrash,
        RevealInFinder,
        FocusFilter,
        ClearFilter,
        NewFolder,
        RenameSelected,
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        QuickLook,
        GoHome,
    ]
);

/// Per-tab state. Each tab has its own current directory + nav
/// history + cursor selection. Filter text, show-hidden, the
/// virtualized Table entity, and the FS watcher are shared at the
/// Shell level — Finder-style "the active tab's location is what
/// the rest of the chrome reflects."
#[derive(Clone)]
pub struct Tab {
    pub current_dir: PathBuf,
    pub history: Vec<PathBuf>,
    pub history_index: usize,
    pub selected: Option<usize>,
}

impl Tab {
    pub fn new(at: PathBuf) -> Self {
        Self {
            current_dir: at.clone(),
            history: vec![at],
            history_index: 0,
            selected: None,
        }
    }

    /// Short label for the tabstrip. Last path component, or "/" for
    /// the filesystem root.
    pub fn label(&self) -> String {
        self.current_dir
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| self.current_dir.to_string_lossy().into_owned())
    }
}

/// Key-context name for the Shell's outer container — same convention
/// gpui-component uses (e.g. `Root` / `Input`). The keymap module
/// drives every binding off `feraille_core::commands` as of Harvest
/// Stage 3; SHELL_CONTEXT gates them to the file-pane focus.
pub const SHELL_CONTEXT: &str = "Shell";

pub fn init(cx: &mut App) {
    crate::keymap::install(cx);
    crate::keymap::install_extras(cx);
}

pub struct Shell {
    /// Open tabs in this window. Always non-empty; closing the last
    /// tab is rejected. The active tab drives the breadcrumb,
    /// table, preview, watcher, etc.
    pub tabs: Vec<Tab>,
    /// Index of the active tab in `tabs`.
    pub active: usize,
    /// Volumes mounted at /Volumes. Refreshed lazily in 4.b; future
    /// iters will watch for changes via the macOS Disk Arbitration
    /// framework.
    pub volumes: Vec<VolumeInfo>,
    /// Shared FS backend. `Arc` because the file-list delegate also
    /// holds a reference (for path lookups during navigation).
    pub fs: Arc<NativeFs>,
    /// gpui-component's virtualized Table state, parameterised by
    /// our file-list delegate. Shared across tabs — switching tabs
    /// reloads it with the new tab's current_dir + the (shared)
    /// filter/show-hidden.
    pub table: Entity<TableState<FileListDelegate>>,
    /// Focus handle for the Shell's key-context. Keybindings declared
    /// against `SHELL_CONTEXT` only fire when this handle (or one of
    /// its children) holds focus.
    pub focus_handle: FocusHandle,
    /// When true, dotfiles are shown in the list. Shared across
    /// tabs — toggling it reloads the active tab.
    pub show_hidden: bool,
    /// `Some(err)` when the last `enumerate` returned an OS error
    /// (most commonly macOS TCC denial on ~/Documents etc.). Drives
    /// an in-pane empty-state instead of a silent blank list.
    pub last_error: Option<EnumerationError>,
    /// Background file-system watcher. `Rc<RefCell<>>` so the
    /// foreground-executor polling task can read it without taking
    /// a mutable borrow of the whole Shell. None if the platform
    /// watcher failed to start (rare — typically only in stripped
    /// CI environments without FSEvents).
    pub watcher: Rc<RefCell<Option<FsWatcher>>>,
    /// Row index the user most recently right-clicked. Actions
    /// dispatched from the context menu read this; keyboard actions
    /// fall back to the active tab's `selected`. Cleared after each
    /// context-menu action handler runs.
    pub context_row: Option<usize>,
    /// Persistent metadata database (rusqlite-backed) opened at
    /// startup. `None` when `$HOME` is unset or the DB couldn't be
    /// opened — in-memory state still works, just no persistence.
    /// Stage 4+ uses this for magic / quarantine / ant-trail
    /// caches; Stage 1.b just establishes the handle.
    /// `Arc<Mutex<_>>` so background workers (Stage 4 prefetch) can
    /// share it without competing borrows.
    pub metadata_db: Option<Arc<Mutex<feraille_meta::MetadataDb>>>,
    /// Shared NSWorkspace-backed icon cache. Owned at the Shell
    /// level so the file-list delegate (Phase 4.c) and the sidebar
    /// tree (Stage 9.c) share the same fetched bytes — one fetch
    /// per kind / path across the whole UI.
    pub icons: Rc<RefCell<IconCache>>,
    /// Active background tasks (Stage 5.a). Shared via
    /// `Rc<RefCell<_>>` so the prefetch worker can register +
    /// retire its job from the foreground executor without taking
    /// a mutable Shell borrow.
    pub tasks: Rc<RefCell<TaskRegistry>>,
    /// CLI-injected status-bar progress override
    /// (`--simulate-progress`). `Some(_)` keeps the strip visible
    /// at that fraction (negative = indeterminate) regardless of
    /// `tasks` state — useful for screenshots.
    pub simulated_progress: Option<f32>,
    /// Live filter text. Shared across tabs.
    pub filter_text: String,
    /// `gpui-component` Input state for the filter field in the
    /// toolbar. Owned as an Entity so InputEvent subscriptions
    /// route changes back into `filter_text`.
    pub filter_input: Entity<InputState>,
    /// Sidebar tree state (Stage 9.c): which directories are
    /// currently expanded. Updated on caret-click and by the
    /// `--expand <path>` CLI flag (which walks the path's ancestors).
    pub expanded: HashSet<PathBuf>,
    /// Cached direct-children of any path that's ever been expanded.
    /// Folders only (the tree shows hierarchy; files live in the
    /// main pane). Once cached, re-expand is instant; collapsing a
    /// folder doesn't evict its cache.
    pub tree_children: HashMap<PathBuf, Vec<TreeChild>>,
    /// Live subscription handles (Input change, future watchers).
    /// Dropping them tears down the listeners — keep alongside the
    /// Shell so they outlive any frame.
    #[allow(dead_code)]
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    /// Immutable accessor for the active tab. Panics if tabs is
    /// empty — but the constructor + close_tab() invariant keep
    /// that from happening.
    #[inline]
    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// Mutable accessor for the active tab.
    #[inline]
    pub fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }
}

/// Open the persistent metadata DB at the default location
/// (`~/Library/Application Support/Feraille/metadata.db`). Returns
/// `None` and logs a warning when `$HOME` is unset, mkdir fails, or
/// open fails — in-memory state still works in those cases, just
/// without persistence. Reuses the path resolution + parent-dir
/// helpers from `feraille_meta`.
fn open_metadata_db() -> Option<Arc<Mutex<feraille_meta::MetadataDb>>> {
    let Some(path) = feraille_meta::default_db_path() else {
        crate::log_warn!(90, "metadata: $HOME unset; persistence disabled");
        return None;
    };
    if let Err(e) = feraille_meta::ensure_parent_dir(&path) {
        crate::log_warn!(
            90,
            "metadata: mkdir failed for {}: {e}",
            path.display()
        );
        return None;
    }
    match feraille_meta::MetadataDb::open(&path) {
        Ok(db) => {
            crate::log_info!(90, "metadata: opened {}", path.display());
            Some(Arc::new(Mutex::new(db)))
        }
        Err(e) => {
            crate::log_warn!(
                90,
                "metadata: open failed for {}: {e}",
                path.display()
            );
            None
        }
    }
}

/// A named filesystem destination shown in the sidebar's Locations
/// section. The user's home directory, Applications, Documents, etc.
struct Location {
    label: &'static str,
    /// `home`-relative subpath (None ⇒ the home directory itself).
    sub: Option<&'static str>,
}

const LOCATIONS: &[Location] = &[
    Location { label: "Home", sub: None },
    Location { label: "Applications", sub: Some("Applications") },
    Location { label: "Desktop", sub: Some("Desktop") },
    Location { label: "Documents", sub: Some("Documents") },
    Location { label: "Downloads", sub: Some("Downloads") },
    Location { label: "Movies", sub: Some("Movies") },
    Location { label: "Music", sub: Some("Music") },
    Location { label: "Pictures", sub: Some("Pictures") },
];

impl Location {
    fn path(&self) -> PathBuf {
        let mut p = home_dir();
        if let Some(sub) = self.sub {
            p.push(sub);
        }
        p
    }
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let fs = Arc::new(NativeFs::new());
        let persisted = app_state::load();
        let start = persisted.last_dir.clone().unwrap_or_else(home_dir);
        let show_hidden = persisted.show_hidden.unwrap_or(false);
        let icons = Rc::new(RefCell::new(IconCache::new()));
        let mut delegate = FileListDelegate::new(fs.clone(), icons.clone());
        let last_error = delegate.load(start.clone(), show_hidden, "");
        let initial_selection = if delegate.entries.is_empty() { None } else { Some(0) };
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .col_selectable(false)
                .col_movable(false)
        });
        // Bridge Table events (selection + double-click) to the
        // Shell's own state so the preview pane sees the live row.
        cx.subscribe_in(
            &table,
            window,
            |this, _table, event: &TableEvent, _window, cx| match event {
                TableEvent::SelectRow(row_ix) => {
                    this.active_tab_mut().selected = Some(*row_ix);
                    cx.notify();
                }
                TableEvent::DoubleClickedRow(row_ix) => {
                    this.activate_row(*row_ix, cx);
                }
                TableEvent::RightClickedRow(row_ix) => {
                    this.context_row = *row_ix;
                }
                _ => {}
            },
        )
        .detach();
        let focus_handle = cx.focus_handle();
        // Grab focus on first paint so the Backspace keybind works
        // immediately without the user having to click into the
        // shell.
        focus_handle.focus(window, cx);

        let filter_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("Filter \u{2026}"));
        let filter_subscription =
            cx.subscribe_in(&filter_input, window, {
                let filter_input = filter_input.clone();
                move |this, _state, ev: &InputEvent, _window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        let value = filter_input.read(cx).value().to_string();
                        this.filter_text = value;
                        let path = this.active_tab().current_dir.clone();
                        this.load_path(path, cx);
                    }
                }
            });

        // Spin up the platform file-system watcher and start
        // watching the initial directory. If the watcher itself
        // can't be constructed (very rare; sandbox without
        // FSEvents), we just operate without it — manual Cmd+R
        // still works.
        let watcher = match FsWatcher::new() {
            Ok(mut w) => {
                let _ = w.watch(&start);
                Rc::new(RefCell::new(Some(w)))
            }
            Err(_) => Rc::new(RefCell::new(None)),
        };

        // Foreground-executor polling task. Wakes every POLL_INTERVAL,
        // drains the channel, asks the Shell to reload if anything
        // changed. Stops when this.update returns Err — that means
        // the Shell entity has been dropped.
        let poll_watcher = watcher.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let dirty = poll_watcher
                    .borrow()
                    .as_ref()
                    .map(|w| w.drain_reload_relevant())
                    .unwrap_or(false);
                if dirty {
                    if this
                        .update(cx, |this, cx| {
                            let path = this.active_tab().current_dir.clone();
                            this.load_path(path, cx);
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        })
        .detach();

        let mut initial_tab = Tab::new(start);
        initial_tab.selected = initial_selection;
        let metadata_db = open_metadata_db();
        Self {
            tabs: vec![initial_tab],
            active: 0,
            volumes: list_volumes(),
            fs,
            table,
            focus_handle,
            show_hidden,
            last_error,
            watcher,
            context_row: None,
            metadata_db,
            icons,
            tasks: Rc::new(RefCell::new(TaskRegistry::new())),
            simulated_progress: None,
            filter_text: String::new(),
            filter_input,
            expanded: HashSet::new(),
            tree_children: HashMap::new(),
            _subscriptions: vec![filter_subscription],
        }
    }

    /// "Which row is this action targeting?" — context_row first
    /// (right-click triggered), then selected (keyboard / single-
    /// click). Consumes context_row so the next keyboard action uses
    /// the keyboard selection.
    fn target_row(&mut self) -> Option<usize> {
        if let Some(r) = self.context_row.take() {
            Some(r)
        } else {
            self.active_tab().selected
        }
    }

    /// Resolve a row to an absolute path on disk. Reuses the same
    /// id_for_path fallback that activate_row uses.
    fn path_for_row(&self, row_ix: usize, cx: &App) -> Option<PathBuf> {
        let entry = self.table.read(cx).delegate().entries.get(row_ix)?.clone();
        Some(self.fs.path_for(entry.id).unwrap_or_else(|| {
            let mut p = self.active_tab().current_dir.clone();
            p.push(&entry.name);
            p
        }))
    }

    fn on_copy_path(
        &mut self,
        _: &CopyPath,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.target_row() else { return };
        let Some(path) = self.path_for_row(row, cx) else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(
            path.to_string_lossy().into_owned(),
        ));
    }

    fn on_reveal_in_finder(
        &mut self,
        _: &RevealInFinder,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.target_row() else { return };
        let Some(path) = self.path_for_row(row, cx) else { return };
        // `open -R <path>` is the macOS canonical "reveal in
        // Finder". On other platforms this no-ops.
        let _ = std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(&path)
            .spawn();
    }

    fn on_move_to_trash(
        &mut self,
        _: &MoveToTrash,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.target_row() else { return };
        let Some(path) = self.path_for_row(row, cx) else { return };
        if feraille_fs_native::move_to_trash(&path).is_ok() {
            // The fs-watcher will pick the deletion up on its next
            // poll tick, but we also reload immediately so the row
            // disappears without a noticeable lag.
            let cur = self.active_tab().current_dir.clone();
            self.load_path(cur, cx);
        }
    }

    fn on_navigate_back(
        &mut self,
        _: &NavigateBack,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_back(cx);
    }

    fn on_navigate_forward(
        &mut self,
        _: &NavigateForward,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_forward(cx);
    }

    fn on_open_selected(
        &mut self,
        _: &OpenSelected,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(idx) = self.target_row() {
            self.activate_row(idx, cx);
        }
    }

    fn on_refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
    }

    fn on_toggle_hidden(
        &mut self,
        _: &ToggleHidden,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_hidden(cx);
    }

    fn on_focus_filter(
        &mut self,
        _: &FocusFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_filter_input(window, cx);
    }

    /// Public-from-screenshot-CLI helper: focuses the filter input
    /// (same effect as Cmd+F). Stage 2's `--search` flag uses this.
    pub fn focus_filter_input(&self, window: &mut Window, cx: &mut App) {
        self.filter_input.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// Public-from-screenshot-CLI helper: opens the rename dialog
    /// for the active selection. Same effect as F2.
    pub fn trigger_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_rename_selected(&RenameSelected, window, cx);
    }

    /// Public-from-screenshot-CLI helper: opens the new-folder
    /// dialog. Same effect as Cmd+Shift+N.
    pub fn trigger_new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.on_new_folder(&NewFolder, window, cx);
    }

    /// Cmd+Shift+N: open the New Folder dialog.
    fn on_new_folder(
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
                    let _ = std::fs::create_dir(&path);
                    let cur = parent.clone();
                    shell.update(cx, move |this, cx| this.load_path(cur, cx));
                    true
                })
        });
    }

    /// F2: rename the currently-selected row.
    fn on_rename_selected(
        &mut self,
        _: &RenameSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.target_row() else { return };
        let Some(entry) = self.table.read(cx).delegate().entries.get(row).cloned() else {
            return;
        };
        let Some(old_path) = self.path_for_row(row, cx) else { return };
        let original_name = entry.name.clone();
        let input_state = cx.new(|cx| {
            let s = InputState::new(window, cx).placeholder("New name");
            // Pre-fill with the existing name (so the user is
            // editing, not typing from scratch).
            s
        });
        // Set the initial value AFTER creating the entity so the
        // window+cx are properly threaded.
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
                    let _ = std::fs::rename(&old_path, &new_path);
                    let cur = parent.clone();
                    shell.update(cx, move |this, cx| this.load_path(cur, cx));
                    true
                })
        });
    }

    fn on_clear_filter(
        &mut self,
        _: &ClearFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filter_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        self.filter_text.clear();
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
        self.focus_handle.focus(window, cx);
    }

    /// Space-bar Quick Look. Reuses the existing
    /// `feraille_shell_mac::quick_look::show` bridge — same code
    /// the old app called.
    fn on_quick_look(
        &mut self,
        _: &QuickLook,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.target_row() else { return };
        let Some(path) = self.path_for_row(row, cx) else { return };
        let _ = feraille_shell_mac::show_quick_look(&[path.as_path()]);
    }

    /// Cmd+Shift+H — navigate the active tab to the home directory.
    fn on_go_home(
        &mut self,
        _: &GoHome,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(home_dir(), cx);
    }

    fn on_open_settings(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Spawn a second native window hosting the SettingsView,
        // matching macOS convention where Preferences is its own
        // window not a modal sheet. Independent of the file-manager
        // shell's lifecycle — closing one doesn't close the other.
        let _ = window;
        crate::settings::open_settings_window(cx);
    }

    pub fn navigate_back(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        if tab.history_index > 0 {
            tab.history_index -= 1;
            let path = tab.history[tab.history_index].clone();
            self.load_path(path, cx);
        }
    }

    pub fn navigate_forward(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        if tab.history_index + 1 < tab.history.len() {
            tab.history_index += 1;
            let path = tab.history[tab.history_index].clone();
            self.load_path(path, cx);
        }
    }

    /// Persist last-dir + show-hidden to disk. Cheap (small text
    /// file in the user's app support dir); call freely.
    fn save_state(&self) {
        app_state::save(&AppState {
            last_dir: Some(self.active_tab().current_dir.clone()),
            show_hidden: Some(self.show_hidden),
        });
    }

    /// Inner load: re-enumerate the directory + refresh the table +
    /// re-target the watcher. Does **not** touch history (history
    /// is only mutated by `navigate`). Public so the screenshot
    /// CLI driver can call it directly after seeding tab state.
    pub fn load_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.active_tab_mut().current_dir = path.clone();
        let show_hidden = self.show_hidden;
        let filter = self.filter_text.clone();
        let table = self.table.clone();
        let mut err: Option<EnumerationError> = None;
        table.update(cx, |state, cx| {
            err = state.delegate_mut().load(path.clone(), show_hidden, &filter);
            state.refresh(cx);
        });
        self.last_error = err;
        self.active_tab_mut().selected = None;
        // Point the watcher at the new directory. Errors (path
        // doesn't exist, watcher saturated) are non-fatal — the
        // user still gets the listing; they just lose live updates.
        if let Some(w) = self.watcher.borrow_mut().as_mut() {
            let _ = w.watch(&path);
        }
        self.save_state();
        // Stage 4: kick off magic + quarantine prefetch for the
        // newly-loaded entries. Cheap (snapshot is collected
        // synchronously; the actual I/O runs on the background
        // executor); per-row mutations land on the foreground
        // executor when the worker completes. Stage 5.a: the worker
        // also registers / ends a Task in `tasks` so the status bar
        // surfaces the work.
        let table = self.table.clone();
        let fs = self.fs.clone();
        let db = self.metadata_db.clone();
        let tasks = self.tasks.clone();
        let weak = cx.weak_entity();
        crate::prefetch::start(table, fs, db, tasks, weak, cx);
        cx.notify();
    }

    pub fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
    }

    // ----- Tab management (5.5.d) ---------------------------------

    /// Cmd+T: open a new tab at the home directory and switch to it.
    fn on_new_tab(&mut self, _: &NewTab, _: &mut Window, cx: &mut Context<Self>) {
        self.tabs.push(Tab::new(home_dir()));
        self.active = self.tabs.len() - 1;
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
    }

    /// Cmd+W: close the active tab. Refuses to close the last one;
    /// closing a tab leaves you on the tab to its left (or 0).
    fn on_close_tab(&mut self, _: &CloseTab, _: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.tabs.remove(self.active);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
    }

    /// Ctrl+Tab: cycle to the next tab.
    fn on_next_tab(&mut self, _: &NextTab, _: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 {
            return;
        }
        self.active = (self.active + 1) % self.tabs.len();
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
    }

    /// Ctrl+Shift+Tab: cycle to the previous tab.
    fn on_prev_tab(&mut self, _: &PrevTab, _: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 {
            return;
        }
        self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
    }

    /// Switch to the tab at `idx`. Used by tabstrip click handlers.
    pub fn select_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() || idx == self.active {
            return;
        }
        self.active = idx;
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
    }

    fn on_navigate_parent(
        &mut self,
        _: &NavigateParent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_parent(cx);
    }

    /// User activated a row (double-click or Enter). For directories
    /// we navigate into them; for files we hand off to the OS opener.
    pub fn activate_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        let path_and_kind = self.table.read(cx).delegate().entries.get(row_ix).map(|e| {
            (
                self.fs.path_for(e.id).unwrap_or_else(|| {
                    let mut p = self.active_tab().current_dir.clone();
                    p.push(&e.name);
                    p
                }),
                e.kind,
            )
        });
        let Some((path, kind)) = path_and_kind else {
            return;
        };
        match kind {
            EntryKind::Directory => self.navigate(path, cx),
            EntryKind::File | EntryKind::Symlink => {
                // open_with_default routes through `open(1)` on macOS;
                // failures are logged-and-ignored — the user already
                // gets system-level feedback if the app can't open.
                let _ = open_with_default(&path);
            }
        }
    }

    /// Navigate to the parent of the current directory (Backspace
    /// keybind in 4.c.2). No-op when already at the filesystem root.
    pub fn navigate_parent(&mut self, cx: &mut Context<Self>) {
        let cur = self.active_tab().current_dir.clone();
        if let Some(parent) = cur.parent() {
            let parent = parent.to_path_buf();
            if parent != cur {
                self.navigate(parent, cx);
            }
        }
    }

    /// Navigate to `path`: re-enumerate, refresh the Table, push to
    /// the active tab's history (truncating any forward stack first),
    /// reset selection.
    pub fn navigate(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        if tab.history.get(tab.history_index) != Some(&path) {
            tab.history.truncate(tab.history_index + 1);
            tab.history.push(path.clone());
            tab.history_index = tab.history.len() - 1;
        }
        self.load_path(path, cx);
    }

    /// Toggle expansion for a directory in the sidebar tree.
    /// Collapsing also removes every descendant from `expanded` so a
    /// future re-open doesn't carry stale sub-expansions forward.
    /// Cache stays — re-expand is instantaneous.
    pub fn toggle_tree_expand(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.expanded.contains(path) {
            let prefix = path.to_path_buf();
            self.expanded.retain(|p| !p.starts_with(&prefix));
        } else {
            self.expanded.insert(path.to_path_buf());
            self.ensure_tree_children(path);
        }
        cx.notify();
    }

    /// Make sure `tree_children[path]` is populated. Cheap no-op if
    /// already cached. On first call, runs `std::fs::read_dir`
    /// synchronously (consistent with `Shell::load_path`'s sync
    /// enumeration; a unified async-streaming refactor lives in a
    /// later iter). Folder-only — files don't appear in the tree.
    ///
    /// Hidden entries are *included* in the cache; the renderer
    /// filters them out based on the live `show_hidden` flag so a
    /// toggle doesn't require cache invalidation.
    pub fn ensure_tree_children(&mut self, path: &Path) {
        if self.tree_children.contains_key(path) {
            return;
        }
        let mut children: Vec<TreeChild> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(path) {
            for dirent in rd.flatten() {
                let p = dirent.path();
                let Some(name) = p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(str::to_owned)
                else {
                    continue;
                };
                // file_type() can be cheap (no extra stat on most
                // platforms); fall back to metadata() if it errors.
                let is_dir = match dirent.file_type() {
                    Ok(ft) => {
                        ft.is_dir()
                            || (ft.is_symlink()
                                && std::fs::metadata(&p)
                                    .map(|m| m.is_dir())
                                    .unwrap_or(false))
                    }
                    Err(_) => false,
                };
                if !is_dir {
                    continue;
                }
                children.push(TreeChild { path: p, label: name });
            }
            children.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
        }
        self.tree_children.insert(path.to_path_buf(), children);
    }

    /// Expand `path` and every ancestor in the tree. Used by the
    /// `--expand <path>` CLI flag, which mirrors the old app's
    /// "reveal this path with the surrounding hierarchy unfurled"
    /// shape. Each directory's children are also enumerated into
    /// `tree_children` so the first frame already has them.
    pub fn reveal_path_in_tree(&mut self, path: &Path) {
        let mut chain: Vec<PathBuf> = vec![path.to_path_buf()];
        let mut cur = path.parent().map(|p| p.to_path_buf());
        while let Some(a) = cur {
            chain.push(a.clone());
            cur = a.parent().map(|p| p.to_path_buf());
        }
        // Walk from filesystem root toward `path` so each
        // enumeration sees its parent already populated (no
        // correctness impact, but symmetric with how the tree
        // builds top-down).
        for a in chain.into_iter().rev() {
            self.expanded.insert(a.clone());
            self.ensure_tree_children(&a);
        }
    }

    /// Build the Locations section as a flat row list with descendants
    /// of every expanded folder interleaved.
    fn build_locations_rows(&self) -> Vec<TreeRowSpec> {
        let current = self.active_tab().current_dir.clone();
        let mut rows: Vec<TreeRowSpec> = Vec::new();
        for loc in LOCATIONS {
            let path = loc.path();
            let is_expanded = self.expanded.contains(&path);
            rows.push(TreeRowSpec {
                path: path.clone(),
                label: SharedString::from(loc.label),
                depth: 0,
                is_expandable: true,
                is_expanded,
                is_active: path == current,
                capacity: None,
            });
            if is_expanded {
                self.append_tree_descendants(&mut rows, &path, 1, &current);
            }
        }
        rows
    }

    /// Build the Volumes section as a flat row list. Same recursion
    /// shape as Locations, but the depth-0 volume row carries a
    /// `(total, available)` capacity so the renderer can draw a
    /// Finder-style capacity bar.
    fn build_volumes_rows(&self) -> Vec<TreeRowSpec> {
        let current = self.active_tab().current_dir.clone();
        let mut rows: Vec<TreeRowSpec> = Vec::new();
        for v in &self.volumes {
            let path = v.path.clone();
            let is_expanded = self.expanded.contains(&path);
            let capacity = match (v.total_bytes, v.available_bytes) {
                (Some(t), Some(a)) if t > 0 => Some((t, a)),
                _ => None,
            };
            rows.push(TreeRowSpec {
                path: path.clone(),
                label: SharedString::from(v.name.clone()),
                depth: 0,
                is_expandable: true,
                is_expanded,
                is_active: path == current,
                capacity,
            });
            if is_expanded {
                self.append_tree_descendants(&mut rows, &path, 1, &current);
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
    fn append_tree_descendants(
        &self,
        rows: &mut Vec<TreeRowSpec>,
        parent: &Path,
        depth: usize,
        current: &Path,
    ) {
        let Some(children) = self.tree_children.get(parent) else {
            return;
        };
        for child in children {
            if !self.show_hidden && child.label.starts_with('.') {
                continue;
            }
            let is_expanded = self.expanded.contains(&child.path);
            rows.push(TreeRowSpec {
                path: child.path.clone(),
                label: SharedString::from(child.label.clone()),
                depth,
                is_expandable: true,
                is_expanded,
                is_active: &child.path == current,
                capacity: None,
            });
            if is_expanded {
                self.append_tree_descendants(rows, &child.path, depth + 1, current);
            }
        }
    }

    /// Either the file Table, or an inline error/empty state when
    /// the directory couldn't be listed (typically macOS TCC denial
    /// on ~/Documents, ~/Desktop, ~/Downloads in a sandboxed runner).
    fn file_pane_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(err) = self.last_error.clone() {
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
        DataTable::new(&self.table)
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
            let is_active = idx == active;
            let label = tab.label();
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
                    this.select_tab(idx, cx);
                }));
            if multi {
                let close = div()
                    .id(("tab-close", idx))
                    .ml_1()
                    .px_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .hover(|this| this.text_color(theme.foreground))
                    .child("x")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // Make sure the closed-tab index is the one we click
                        if this.active != idx {
                            this.active = idx;
                        }
                        let _ = cx; // CloseTab action would re-enter handler
                        this.tabs.remove(idx);
                        if this.active >= this.tabs.len() {
                            this.active = this.tabs.len() - 1;
                        }
                        let path = this.active_tab().current_dir.clone();
                        this.load_path(path, cx);
                    }));
                chip = chip.child(close);
            }
            row = row.child(chip);
        }
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
                .on_click(cx.listener(|this, _, _, cx| {
                    this.tabs.push(Tab::new(home_dir()));
                    this.active = this.tabs.len() - 1;
                    let path = this.active_tab().current_dir.clone();
                    this.load_path(path, cx);
                })),
        );
        row
    }

    /// Toolbar row above the breadcrumb: Back / Forward buttons +
    /// "Show hidden" toggle. Disabled buttons grey out via Button's
    /// own disabled state — no manual styling.
    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let can_back = self.active_tab().history_index > 0;
        let can_forward =
            self.active_tab().history_index + 1 < self.active_tab().history.len();
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("nav-back")
                    .small()
                    .label("\u{2190}")
                    .disabled(!can_back)
                    .on_click(cx.listener(|this, _, _, cx| this.navigate_back(cx))),
            )
            .child(
                Button::new("nav-forward")
                    .small()
                    .label("\u{2192}")
                    .disabled(!can_forward)
                    .on_click(cx.listener(|this, _, _, cx| this.navigate_forward(cx))),
            )
            .child(
                div()
                    .flex_1()
                    .max_w(px(360.0))
                    .ml_4()
                    .child(Input::new(&self.filter_input).small()),
            )
            .child(div().flex_1())
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Show hidden"),
                    )
                    .child(
                        Switch::new("hidden-toggle-toolbar")
                            .checked(self.show_hidden)
                            .on_click(cx.listener(|this, _checked: &bool, _, cx| {
                                this.toggle_hidden(cx);
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
        let selected = self
            .active_tab()
            .selected
            .and_then(|i| self.table.read(cx).delegate().entries.get(i).cloned());

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
                let kind_label = match entry.kind {
                    EntryKind::Directory => "Folder",
                    EntryKind::File => "File",
                    EntryKind::Symlink => "Symlink",
                };
                let mut full_path = self.active_tab().current_dir.clone();
                full_path.push(&entry.name);
                let path_str = full_path.to_string_lossy().into_owned();

                let mut col = v_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child(SharedString::from(entry.name.clone())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(entry.display_kind.clone())),
                    )
                    .child(preview_field("Kind", kind_label.to_string(), cx))
                    .child(preview_field("Size", entry.display_size.clone(), cx))
                    .child(preview_field("Modified", entry.display_mtime.clone(), cx))
                    .child(preview_field("Where", path_str, cx));

                // Magic byte sniff (Stage 4 prefetch populates this
                // asynchronously). Empty string while prefetch is
                // still running on a fresh directory.
                if !entry.display_magic.is_empty() {
                    col = col.child(preview_field(
                        "Magic",
                        entry.display_magic.clone(),
                        cx,
                    ));
                }
                // Quarantine details. is_quarantined flag drives the
                // section header even when individual fields are
                // unknown; the Stage 4 prefetch + feraille-meta
                // hydrate the rich fields lazily.
                if entry.is_quarantined {
                    col = col
                        .child(
                            div()
                                .mt_2()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(gpui::rgb(0xFF3B30))
                                .child("Quarantined"),
                        )
                        .child(preview_field(
                            "Mark of the Web",
                            "com.apple.quarantine".to_string(),
                            cx,
                        ));
                }
                col.into_any_element()
            }
        };

        v_flex()
            .w(px(280.0))
            .h_full()
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
    /// gets its own leading segment.
    fn breadcrumb(&self, cx: &mut Context<Self>) -> Div {
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
            let style = TextStyle {
                ..Default::default()
            };
            let _ = style; // unused-import shim if we add styled-text later
            let crumb = div()
                .id(ElementId::Name(format!("crumb-{i}").into()))
                .px_2()
                .py_1()
                .rounded(cx.theme().radius)
                .text_sm()
                .text_color(if is_last {
                    cx.theme().foreground
                } else {
                    cx.theme().muted_foreground
                })
                .when(is_last, |this| {
                    this.font_weight(FontWeight::SEMIBOLD)
                })
                .cursor_pointer()
                .hover(|this| this.bg(cx.theme().secondary))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.navigate(path.clone(), cx);
                }));
            row = row.child(crumb);
        }
        row
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.weak_entity();
        let locations_rows = self.build_locations_rows();
        let volumes_rows = self.build_volumes_rows();
        let has_volumes = !self.volumes.is_empty();
        let breadcrumb = self.breadcrumb(cx);
        let path_str = self
            .active_tab()
            .current_dir
            .to_string_lossy()
            .into_owned();

        let mut sidebar = Sidebar::new("shell-sidebar")
            .w(px(220.0))
            .header(
                SidebarHeader::new().child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child("Feraille"),
                ),
            )
            .child(TreeSection::new(
                "Locations",
                locations_rows,
                weak.clone(),
                self.icons.clone(),
            ));
        if has_volumes {
            sidebar = sidebar.child(TreeSection::new(
                "Volumes",
                volumes_rows,
                weak.clone(),
                self.icons.clone(),
            ));
        }

        let _ = path_str; // breadcrumb already shows the path

        let tabstrip = self.tabstrip(cx);
        let toolbar = self.toolbar(cx);
        let entry_count = self.table.read(cx).delegate().entries.len();
        let status_bar = crate::status_bar::render(
            entry_count,
            &self.tasks,
            self.simulated_progress,
            cx,
        );

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
            .on_action(cx.listener(Self::on_reveal_in_finder))
            .on_action(cx.listener(Self::on_move_to_trash))
            .on_action(cx.listener(Self::on_focus_filter))
            .on_action(cx.listener(Self::on_clear_filter))
            .on_action(cx.listener(Self::on_new_folder))
            .on_action(cx.listener(Self::on_rename_selected))
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_quick_look))
            .on_action(cx.listener(Self::on_go_home))
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .size_full()
                    .child(sidebar)
                    .child(
                        v_flex()
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .child(tabstrip)
                            .child(toolbar)
                            .child(breadcrumb)
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_h_0()
                                    .items_stretch()
                                    .child(
                                        div().flex_1().min_w_0().h_full().child(
                                            self.file_pane_body(cx),
                                        ),
                                    )
                                    .child(self.preview(cx)),
                            )
                            .child(status_bar),
                    ),
            )
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
    }
}

/// Map an `EnumerationError` to (title, body) copy for the in-pane
/// error state. macOS users hitting `Documents` / `Desktop` /
/// `Downloads` for the first time in a sandboxed launcher will see
/// the TCC permission case; other variants get a generic message.
fn error_copy(err: &EnumerationError) -> (&'static str, String) {
    match err {
        EnumerationError::PermissionDenied => (
            "Access required",
            "Feraille needs permission to read this folder. Grant access in \
             System Settings \u{2192} Privacy & Security \u{2192} Files and Folders."
                .to_string(),
        ),
        EnumerationError::NotFound => (
            "Folder not found",
            "This location may have been moved, renamed, or unmounted.".to_string(),
        ),
        EnumerationError::Other(msg) => (
            "Couldn't open this folder",
            msg.clone(),
        ),
    }
}

/// Two-line field for the preview pane: muted label on top, primary
/// value below. Used for Kind / Size / Modified / Where.
fn preview_field(label: &'static str, value: String, cx: &Context<Shell>) -> Div {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().foreground)
                .child(SharedString::from(value)),
        )
}

/// Split `path` into clickable breadcrumb segments. Each entry is
/// `(visible_label, path_to_navigate_to_on_click)`. The first entry
/// represents the filesystem root.
///
/// Public for the integration test in `tests/path_segments.rs` —
/// keeping it private and using an inline `#[cfg(test)] mod tests`
/// crashes the compiler (gpui's type graph plus the macro recursion
/// from `#[test]` overflows syn's parser).
pub fn path_segments(path: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut accum = PathBuf::from("/");
    out.push(("/".to_string(), accum.clone()));
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::RootDir => {}
            Component::Normal(s) => {
                accum.push(s);
                out.push((s.to_string_lossy().into_owned(), accum.clone()));
            }
            Component::CurDir => {}
            Component::ParentDir => {
                accum.pop();
            }
            Component::Prefix(_) => {}
        }
    }
    out
}

use gpui::prelude::FluentBuilder as _;
