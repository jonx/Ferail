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
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use feraille_core::{EntryKind, EnumerationError, FileEntry, NodeId};
use feraille_fs_native::{NativeFs, home_dir, open_with_default};
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, Root, Sizable, TitleBar, WindowExt,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem},
    v_flex,
};

use crate::app_state;
use crate::file_list::FileListDelegate;
use crate::fs_watcher::{FsWatcher, POLL_INTERVAL};
use crate::multi_table::{DataTable, TableEvent, TableState};
use crate::tasks::TaskKind;
use crate::tree::{ShellSidebarItem, TreeChild, TreeRowIcon, TreeRowSpec, TreeSection};
use gpui::prelude::FluentBuilder as _;

mod actions;
mod file_ops;
mod loading;
mod path;
mod render;
mod selection;
mod tab;

pub use actions::*;
use loading::{
    LoadBatch, LoadMsg, error_copy, middle_truncate_path, run_directory_load_streaming,
    run_tree_children_load,
};
pub use path::{parse_breadcrumb_path, path_segments};
pub use tab::{ClosedTab, HistoryEntry, Tab, TabId};

/// Classification produced by `Shell::resolve_favorite_target` so
/// the toggle handler can show the appropriate toast for files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FavoriteResolved {
    Folder,
    NotAFolder,
}

/// Reversible operation pushed onto `Shell::undo_stack` after a
/// successful mutation. Filesystem variants apply synchronously via
/// [`UndoOp::apply_fs`]; favorites variants (`AddFavorite` /
/// `RemoveFavorite`) need Shell + cx and are handled inline by
/// `Shell::on_undo_last_action`.
#[derive(Clone, Debug)]
pub enum UndoOp {
    /// Rename `current` back to `original`.
    Rename { current: PathBuf, original: PathBuf },
    /// Delete the folder we just created.
    DeleteFolder(PathBuf),
    /// Undo an add: remove the favorite that was just created.
    AddFavorite(feraille_core::favorites::FavoriteId),
    /// Undo a remove: restore the captured favorite at its prior
    /// `sort_index`, with prior `display_name` / `custom_icon` /
    /// `date_added`. Identity (`FavoriteId`) is preserved so any
    /// toggle elsewhere stays consistent (§3.2).
    RemoveFavorite(feraille_core::favorites::Favorite),
}

impl UndoOp {
    /// Apply filesystem-only variants. Favorites variants return
    /// `Err` here — the caller routes them through Shell + cx.
    fn apply_fs(&self) -> Result<(), String> {
        match self {
            UndoOp::Rename { current, original } => {
                std::fs::rename(current, original).map_err(|e| e.to_string())
            }
            UndoOp::DeleteFolder(p) => std::fs::remove_dir(p).map_err(|e| e.to_string()),
            UndoOp::AddFavorite(_) | UndoOp::RemoveFavorite(_) => {
                Err("favorite undo handled by Shell".into())
            }
        }
    }

    fn label(&self) -> &'static str {
        match self {
            UndoOp::Rename { .. } => "Undid rename",
            UndoOp::DeleteFolder(_) => "Removed new folder",
            UndoOp::AddFavorite(_) => "Removed Favorite",
            UndoOp::RemoveFavorite(_) => "Restored Favorite",
        }
    }
}

const UNDO_STACK_CAP: usize = 20;

/// Key-context name for the Shell's outer container — same convention
/// gpui-component uses (e.g. `Root` / `Input`). The keymap module
/// drives every binding off `feraille_core::commands` as of Harvest
/// Stage 3; SHELL_CONTEXT gates them to the file-pane focus.
pub const SHELL_CONTEXT: &str = "Shell";

/// Phase 10: live System-Appearance follow. The macOS observer in
/// `crate::platform_shell::start_system_theme_observer` runs on the main
/// thread but has no `&mut App` — it can't call `Theme::change` itself.
/// Instead it pushes the latest dark-mode bool here; Shell::render
/// consumes the pending value (if any) and calls `Theme::change`
/// before painting. Single-digit-millisecond lag at worst.
static SYSTEM_THEME_PENDING: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(-1);

pub fn set_system_theme_pending(is_dark: bool) {
    SYSTEM_THEME_PENDING.store(
        if is_dark { 1 } else { 0 },
        std::sync::atomic::Ordering::Release,
    );
}

fn take_system_theme_pending() -> Option<bool> {
    let v = SYSTEM_THEME_PENDING.swap(-1, std::sync::atomic::Ordering::AcqRel);
    match v {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

pub fn init(cx: &mut App) {
    crate::multi_table::init(cx);
    crate::keymap::install(cx);
    crate::keymap::install_extras(cx);
}

pub struct Shell {
    /// Process-scoped state shared with every other window of this
    /// process (today there is only one window, but the singleton
    /// is what the rest of the windows-instances-tabs spec is built
    /// on; see `crates/feraille-gpui/src/process_state.rs`).
    pub process: Rc<crate::process_state::ProcessState>,
    /// Open tabs in this window. Always non-empty; closing the last
    /// tab is rejected. Each tab owns its own `Entity<TableState>`,
    /// its own enumeration generation/cancel/task, and its own
    /// last-error / pending-select state — so tab-switching is
    /// instant and an inactive tab's enumeration keeps streaming.
    pub tabs: Vec<Tab>,
    /// Index of the active tab in `tabs`.
    pub active: usize,
    /// Focus handle for the Shell's key-context. Keybindings declared
    /// against `SHELL_CONTEXT` only fire when this handle (or one of
    /// its children) holds focus.
    pub focus_handle: FocusHandle,
    /// When true, dotfiles are shown in the list. Per-window today —
    /// future preference may make it per-tab.
    pub show_hidden: bool,
    /// Row index the user most recently right-clicked. Actions
    /// dispatched from the context menu read this; keyboard actions
    /// fall back to the active tab's `selected`. Cleared after each
    /// context-menu action handler runs.
    pub context_row: Option<usize>,
    /// Path target for the sidebar / breadcrumb / empty-space
    /// context menus (Phase 6). Set by the `.context_menu(...)`
    /// closure on right-click; consumed by `RevealContextPath` /
    /// `CopyContextPath` / `OpenContextInNewTab` /
    /// `NewFolderHere` handlers. Unlike `context_row` (which targets
    /// file-list rows by index), this carries the full path because
    /// sidebar items aren't part of the file list.
    pub context_target: Option<PathBuf>,
    /// Path target for Favorites mutations
    /// (docs/features/FAVORITES.md). Set by every "Add to Favorites" /
    /// "Remove from Favorites" context-menu closure and by row-row drag
    /// handlers; consumed (`take()`-style) by
    /// `on_toggle_favorite_for_target`. Fallback chain when unset:
    /// file-list selection → active tab `current_dir`.
    pub favorites_context_path: Option<PathBuf>,
    /// Whether the background-task panel popover is open. Toggled by
    /// clicking the task region in the status bar.
    pub task_panel_open: bool,
    /// CLI-injected status-bar progress override
    /// (`--simulate-progress`). `Some(_)` keeps the strip visible
    /// at that fraction (negative = indeterminate) regardless of
    /// `tasks` state — useful for screenshots.
    pub simulated_progress: Option<f32>,
    /// Cmd+L breadcrumb edit (Stage 9.b): when true the breadcrumb
    /// renders an Input field pre-filled with the active tab's
    /// current_dir instead of the clickable segments. Enter commits
    /// (canonicalise + navigate); Blur cancels.
    pub breadcrumb_editing: bool,
    /// `InputState` for the breadcrumb edit field. Constructed once
    /// at Shell creation; visible only while
    /// `breadcrumb_editing == true`.
    pub breadcrumb_input: Entity<InputState>,
    /// Stage 9.b: keyboard-shortcuts help overlay. `Some(filter)`
    /// while visible — the string is the live filter text shown in
    /// the modal's search input.
    pub shortcuts_help_filter: Option<String>,
    /// Input state for the shortcuts-help filter. Always allocated;
    /// only rendered when the overlay is visible.
    pub shortcuts_help_input: Entity<InputState>,
    /// Whether the right-side preview pane is visible. Cmd+P toggles
    /// it; Cmd+I focuses the preview's Get Info section (today it's
    /// the only thing in the pane).
    pub preview_visible: bool,
    /// UI zoom factor (Stage 9.b.5). 1.0 = default; bumped by Cmd+=
    /// and Cmd+-. Applied to font sizes / icon sizes via Tokens-
    /// derived scaling at render time. Persisted in app_state.
    pub ui_scale: f32,
    /// Resizable-splitter state for the sidebar / center / preview
    /// columns. Persists across renders so the drag handles work as
    /// expected; sizes survive theme changes etc.
    pub splitter_state: Entity<gpui_component::resizable::ResizableState>,
    /// Current sidebar width in DIPs (next-level Phase 5). Read from
    /// `app_state::sidebar_width` at construction (or the default
    /// 220), threaded into `resizable_panel().size(...)` on every
    /// render, and updated from the splitter's `on_resize` callback
    /// when the user drags the handle.
    pub sidebar_width: f32,
    /// Current preview pane width. Same lifecycle as `sidebar_width`.
    pub preview_width: f32,
    /// Timestamp of the last persistence write for the splitter
    /// widths. The on_resize callback fires per drag tick — we
    /// debounce the file write to ~once per `SPLITTER_PERSIST_INTERVAL`
    /// so a drag doesn't hammer the config file.
    pub splitter_last_save: Option<std::time::Instant>,
    /// Sidebar collapsed to icons-only when true. Toggled by the
    /// SidebarToggleButton in the TitleBar; persisted via
    /// `app_state::sidebar_collapsed` so the choice survives
    /// restarts.
    pub sidebar_collapsed: bool,
    /// Favorites sidebar section collapsed (disclosure-triangle).
    /// Independent of the sidebar-wide icon-collapse; persisted via
    /// `MetadataDb::favorites_section_collapsed` so the choice
    /// survives restarts. Hydrated in `start_metadata_load`.
    pub favorites_section_collapsed: bool,
    /// Most-recently-focused favorite id. Set by clicks on a favorite
    /// row; consumed by the keyboard-reorder actions (§4.4) so
    /// `Cmd+Option+Up/Down` operates on the row the user just selected.
    /// `None` when no favorite has been clicked this session.
    pub focused_favorite: Option<feraille_core::favorites::FavoriteId>,
    /// Windows/Linux app menu bar (`gpui-component::AppMenuBar`).
    /// `Some(_)` only on non-macOS — those platforms have no global
    /// menu bar, so we render the menu strip in-window beneath the
    /// title bar. macOS uses `cx.set_menus()` for its NSApp menu and
    /// leaves this `None`. Reads from the same `cx.set_menus()`
    /// global state that the Mac path populates, so the menu spec
    /// has a single source of truth.
    pub menu_bar: Option<Entity<gpui_component::menu::AppMenuBar>>,
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

    #[inline]
    pub fn current_node(&self) -> NodeId {
        self.active_tab().nav.current()
    }
}

#[derive(Copy, Clone)]
enum SelectionDelta {
    Up,
    Down,
    PageUp,
    PageDown,
    First,
    Last,
}

fn now_unix_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Pull existing Ant Trail visit counts out of the metadata DB so
/// heat is reflected on the very first render. Returns
/// `(empty_map, 0)` when the DB is absent or the read fails — heat
/// tint just won't show until the user's done some navigating.
fn hydrate_ant_trail(
    db: Option<&Arc<Mutex<feraille_meta::MetadataDb>>>,
) -> (HashMap<PathBuf, u32>, u32) {
    let Some(db) = db else {
        return (HashMap::new(), 0);
    };
    let Ok(guard) = db.lock() else {
        return (HashMap::new(), 0);
    };
    let Ok(entries) = guard.load_ant_trail() else {
        return (HashMap::new(), 0);
    };
    let mut max: u32 = 0;
    let mut map: HashMap<PathBuf, u32> = HashMap::with_capacity(entries.len());
    for e in entries {
        if e.hits > max {
            max = e.hits;
        }
        map.insert(PathBuf::from(e.folder_path), e.hits);
    }
    (map, max)
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
        crate::log_warn!(90, "metadata: mkdir failed for {}: {e}", path.display());
        return None;
    }
    match feraille_meta::MetadataDb::open(&path) {
        Ok(db) => {
            crate::log_info!(90, "metadata: opened {}", path.display());
            Some(Arc::new(Mutex::new(db)))
        }
        Err(e) => {
            crate::log_warn!(90, "metadata: open failed for {}: {e}", path.display());
            None
        }
    }
}

/// One of the macOS-standard sidebar destinations shown in the
const ICON_WARM_CHUNK: usize = 16;
const ICON_WARM_INTERVAL: Duration = Duration::from_millis(16);

/// How often the splitter's drag callback is allowed to write the
/// app_state config file. 500 ms means a continuous drag samples ~2
/// times per second to disk; the final width at drag-end persists
/// because the next render re-checks the interval and flushes.
const SPLITTER_PERSIST_INTERVAL: Duration = Duration::from_millis(500);

/// Window viewport width (DIPs) below which the preview pane is
/// auto-hidden regardless of `preview_visible`. The threshold leaves
/// roughly: sidebar 220 + file list ~500 + preview 280 = 1000, so
/// dropping under ~900 makes the file list painfully narrow.
const PREVIEW_AUTOHIDE_THRESHOLD: f32 = 900.0;

impl Shell {
    /// Construct the singleton `ProcessState` for this process.
    /// Called exactly once from `main.rs` (or the screenshot harness)
    /// before any window opens; the resulting `Rc` is stashed as a
    /// GPUI `Global` and read back by every `Shell::new`.
    pub fn build_process_state(cx: &mut App) -> Rc<crate::process_state::ProcessState> {
        let fs = Arc::new(NativeFs::new());
        // Spin up the platform file-system watcher. Errors
        // (sandboxed CI without FSEvents) are non-fatal — the app
        // still runs, just without live external updates.
        let watcher_rc: Rc<RefCell<Option<FsWatcher>>> = match FsWatcher::new() {
            Ok(w) => Rc::new(RefCell::new(Some(w))),
            Err(_) => Rc::new(RefCell::new(None)),
        };
        // Favorites is process-scoped: one Entity shared across every
        // window. DB handle attached later by `start_metadata_load`.
        let favorites = cx.new(|_| crate::favorites::Favorites::new(None));
        crate::process_state::ProcessState::new(fs, watcher_rc, favorites)
    }

    pub fn new(
        process: Rc<crate::process_state::ProcessState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let persisted = app_state::load();
        let start = persisted.last_dir.clone().unwrap_or_else(home_dir);
        let start_id = process.fs.id_for_path(&start);
        // Seed the NodeStore with the start path so the very first
        // navigate doesn't re-mint a different NodeId. Idempotent —
        // the second window seeing the same path is a no-op.
        process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(start.clone(), start_id);
        // Add this window's start path to the shared watcher set.
        // Each visible tab registers its directory; watcher events are
        // fanned out to every matching tab in every live window.
        if let Some(w) = process.watcher.borrow_mut().as_mut() {
            let _ = w.watch(&start);
        }
        let show_hidden = persisted.show_hidden.unwrap_or(false);
        // FERAILLE_UI_SCALE env var (regression tool / screenshots)
        // wins over the persisted value when set. Both are clamped.
        let ui_scale = std::env::var("FERAILLE_UI_SCALE")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .or(persisted.ui_scale)
            .map(|n| n.clamp(0.6, 2.0))
            .unwrap_or(1.0);
        let focus_handle = cx.focus_handle();
        // Grab focus on first paint so the Backspace keybind works
        // immediately without the user having to click into the
        // shell.
        focus_handle.focus(window, cx);

        // Stage 9.b: shortcuts-help filter Input. Subscribed for
        // Change so typing updates `shortcuts_help_filter` live.
        let shortcuts_help_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search\u{2026}"));
        let shortcuts_help_subscription = cx.subscribe_in(&shortcuts_help_input, window, {
            let shortcuts_help_input = shortcuts_help_input.clone();
            move |this, _state, ev: &InputEvent, _window, cx| {
                if matches!(ev, InputEvent::Change) {
                    if this.shortcuts_help_filter.is_some() {
                        let v = shortcuts_help_input.read(cx).value().to_string();
                        this.shortcuts_help_filter = Some(v);
                        cx.notify();
                    }
                }
            }
        });

        // Stage 9.b: breadcrumb-edit Input. Subscribed for
        // PressEnter (commit) and Blur (cancel).
        let breadcrumb_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("/path/to/folder"));
        let breadcrumb_subscription = cx.subscribe_in(&breadcrumb_input, window, {
            let breadcrumb_input = breadcrumb_input.clone();
            move |this, _state, ev: &InputEvent, _window, cx| match ev {
                InputEvent::PressEnter { .. } => {
                    let raw = breadcrumb_input.read(cx).value().to_string();
                    let path = parse_breadcrumb_path(&raw);
                    this.breadcrumb_editing = false;
                    this.navigate(path, cx);
                }
                InputEvent::Blur => {
                    if this.breadcrumb_editing {
                        this.breadcrumb_editing = false;
                        cx.notify();
                    }
                }
                _ => {}
            }
        });

        // Foreground-executor polling task. Wakes every POLL_INTERVAL,
        // drains the channel, asks the Shell to reload if anything
        // changed. Stops when this.update returns Err — that means
        // the Shell entity has been dropped.
        let poll_watcher = process.watcher.clone();
        let poll_process = process.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let dirty_paths = poll_watcher
                    .borrow()
                    .as_ref()
                    .map(|w| w.drain_reload_relevant_paths())
                    .unwrap_or_default();
                if !dirty_paths.is_empty() {
                    if this.update(cx, |_, _| {}).is_err() {
                        break;
                    }
                    Shell::broadcast_reload_for_process(&poll_process, dirty_paths, cx);
                }
            }
        })
        .detach();

        let initial_tab = Shell::build_tab(process.clone(), start.clone(), start_id, window, cx);
        // gpui-component's AppMenuBar is the Win/Linux equivalent of
        // macOS's NSApp menu. Reads from the same `cx.set_menus()`
        // global state, so the menu spec lives once in
        // `install_app_menus`. None on macOS — the global menu bar
        // covers it natively.
        let menu_bar = if cfg!(target_os = "macos") {
            None
        } else {
            Some(gpui_component::menu::AppMenuBar::new(cx))
        };
        let mut shell = Self {
            process: process.clone(),
            tabs: vec![initial_tab],
            active: 0,
            focus_handle,
            show_hidden,
            context_row: None,
            context_target: None,
            favorites_context_path: None,
            task_panel_open: false,
            simulated_progress: None,
            breadcrumb_editing: false,
            breadcrumb_input,
            shortcuts_help_filter: None,
            shortcuts_help_input,
            // Default off: the preview pane eats ~250-300px on the
            // right and pushes file-list columns (Description in
            // particular) out of view at the default window size.
            // Cmd+P (or whatever shortcut binds the toggle in keymap)
            // brings it back. Once DB-persistence for layout state
            // wires into Shell::new the user's last choice will
            // override this default — until then this is the boot
            // state on every launch.
            preview_visible: false,
            ui_scale,
            splitter_state: cx.new(|_| gpui_component::resizable::ResizableState::default()),
            sidebar_width: persisted.sidebar_width.unwrap_or(220.0).clamp(160.0, 400.0),
            preview_width: persisted.preview_width.unwrap_or(280.0).clamp(220.0, 520.0),
            splitter_last_save: None,
            sidebar_collapsed: persisted.sidebar_collapsed.unwrap_or(false),
            favorites_section_collapsed: false,
            focused_favorite: None,
            menu_bar,
            expanded: HashSet::new(),
            tree_children: HashMap::new(),
            _subscriptions: vec![breadcrumb_subscription, shortcuts_help_subscription],
        };
        shell.process.register_shell(cx.weak_entity());
        // §5.3 live-sync: every folder-rendering view observes the
        // Favorites entity through Shell, so a single `cx.notify()`
        // here re-renders the sidebar (FavoritesSection), the
        // breadcrumb (star indicator), and the title-bar header.
        // The file list reads its own delegate's `is_favorited`
        // parallel vec, which `load_path` recomputes from the same
        // entity, so it picks up the change on the next load — for
        // truly synchronous list updates we also push a refresh here.
        let fav_subscription = cx.observe(&shell.process.favorites, |this, _favs, cx| {
            this.refresh_file_list_favorited(cx);
            cx.notify();
        });
        shell._subscriptions.push(fav_subscription);

        shell.start_metadata_load(cx);
        shell.load_path(start, cx);
        shell
    }

    /// Build a fresh `Tab` with its own `TableState` entity + table-
    /// event subscription. The subscription captures the new tab's
    /// `TabId` so events from inactive tabs (which shouldn't fire,
    /// since only the active tab is rendered, but defence in depth)
    /// are routed only when the tab is currently active.
    ///
    /// Takes `process` by value so it can be called from `Shell::new`
    /// before the `Shell` struct exists. Other callers can wrap with
    /// `self.make_tab(...)` which forwards `self.process.clone()`.
    pub fn build_tab(
        process: Rc<crate::process_state::ProcessState>,
        at: PathBuf,
        node_id: NodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Tab {
        let tab_id = process.mint_tab_id();
        let delegate = FileListDelegate::new(process.fs.clone(), process.icons.clone());
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .col_selectable(false)
                .col_movable(true)
                .col_resizable(true)
        });
        // Bridge table events on this tab to the Shell's selection
        // state. The closure captures `tab_id` so events from a
        // background tab (rare; only the active tab is hit-tested)
        // never apply gestures meant for the active tab.
        let subscription = cx.subscribe_in(
            &table,
            window,
            move |this, _table, event: &TableEvent, _window, cx| {
                if this.active_tab().id != tab_id {
                    return;
                }
                match event {
                    TableEvent::RowClicked {
                        row_ix, modifiers, ..
                    } => {
                        this.apply_row_click_gesture(*row_ix, *modifiers, cx);
                    }
                    TableEvent::LeadMoved { row_ix, modifiers } => {
                        this.apply_row_keyboard_gesture(*row_ix, *modifiers, cx);
                    }
                    TableEvent::DoubleClickedRow(row_ix) => {
                        this.activate_row(*row_ix, cx);
                    }
                    TableEvent::RightClickedRow(row_ix) => {
                        if let Some(r) = *row_ix {
                            let row_was_selected = this
                                .node_id_at_row(r, cx)
                                .map(|id| this.active_tab().selection.contains(&id))
                                .unwrap_or(false);
                            this.apply_row_right_click(r, cx);
                            // Spec §2.4: if the user right-clicks
                            // inside the current selection, the
                            // menu targets the whole set. Only
                            // stash a row-specific context target
                            // when the click replaced selection
                            // to that unselected row.
                            this.context_row = if row_was_selected { None } else { Some(r) };
                        } else {
                            this.context_row = None;
                        }
                    }
                    _ => {}
                }
            },
        );
        // Filter input — per-tab so cursor / focus / value persist
        // when the user switches tabs. The closure captures `tab_id`
        // so only this tab's enumeration is re-triggered.
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter \u{2026}"));
        let filter_subscription = cx.subscribe_in(&filter_input, window, {
            let filter_input = filter_input.clone();
            move |this, _state, ev: &InputEvent, _window, cx| {
                if matches!(ev, InputEvent::Change) {
                    let value = filter_input.read(cx).value().to_string();
                    if let Some(idx) = this.tabs.iter().position(|t| t.id == tab_id) {
                        this.tabs[idx].filter_text = value;
                        let path = this.tabs[idx].current_dir.clone();
                        this.load_path_for_tab(tab_id, path, cx);
                    }
                }
            }
        });
        Tab::new_internal(
            tab_id,
            at,
            node_id,
            table,
            subscription,
            filter_input,
            filter_subscription,
        )
    }

    /// `build_tab` wrapper for callers that already have `&mut self`.
    pub fn make_tab(
        &mut self,
        at: PathBuf,
        node_id: NodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Tab {
        Self::build_tab(self.process.clone(), at, node_id, window, cx)
    }

    /// "Which row is this action targeting?" — context_row first
    /// (right-click triggered), then selected (keyboard / single-
    /// click). Consumes context_row so the next keyboard action uses
    /// the keyboard selection.
    ///
    /// After the multi-select rework, the keyboard fallback is the
    /// **lead's row index in the current model** — the row index
    /// derived from `Tab::lead` against the delegate's `entries`.
    /// The lead is the right semantic target: a single-row action
    /// like Rename or Compress on a multi-selection should operate
    /// on the focused row, the same way Finder does.
    fn target_row(&mut self, cx: &App) -> Option<usize> {
        if let Some(r) = self.context_row.take() {
            Some(r)
        } else {
            self.active_tab()
                .lead_row(&self.active_tab().table.read(cx).delegate().entries)
        }
    }

    /// Resolve a row to an absolute path on disk. Reuses the same
    /// id_for_path fallback that activate_row uses.
    pub fn path_for_row(&self, row_ix: usize, cx: &App) -> Option<PathBuf> {
        let entry = self
            .active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .get(row_ix)?
            .clone();
        self.process
            .node_store
            .borrow_mut()
            .path_snapshot_for_job(entry.id, "Shell::path_for_row")
            .or_else(|| {
                self.active_tab()
                    .table
                    .read(cx)
                    .delegate()
                    .path_for_entry(entry.id)
            })
            .or_else(|| {
                let mut p = self.active_tab().current_dir.clone();
                p.push(&entry.name);
                Some(p)
            })
    }

    fn entry_path_for_row(&self, row_ix: usize, cx: &App) -> Option<(usize, FileEntry, PathBuf)> {
        let entry = self
            .active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .get(row_ix)?
            .clone();
        let path = self.path_for_row(row_ix, cx)?;
        Some((row_ix, entry, path))
    }

    /// Visible-order selection snapshot for bulk commands and drag
    /// payloads. This is intentionally model/cache-only: it reads the
    /// active tab's NodeId set plus the delegate's current rows and
    /// path cache, never the filesystem.
    fn selected_entries_visible_order(&self, cx: &App) -> Vec<(usize, FileEntry, PathBuf)> {
        let selection = &self.active_tab().selection;
        if selection.is_empty() {
            return Vec::new();
        }
        self.active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .iter()
            .enumerate()
            .filter_map(|(row_ix, entry)| {
                selection
                    .contains(&entry.id)
                    .then(|| self.entry_path_for_row(row_ix, cx))
                    .flatten()
            })
            .collect()
    }

    /// Resolve the target set for a command. A right-click on an
    /// unselected row consumes `context_row` and returns just that row;
    /// a right-click inside a selected set leaves `context_row` empty,
    /// so bulk-capable commands operate on the whole visible selection.
    /// Keyboard/menu invocations also use the selection when present,
    /// falling back to the lead row for legacy single-row commands.
    fn action_entries_visible_order(&mut self, cx: &App) -> Vec<(usize, FileEntry, PathBuf)> {
        if let Some(row) = self.context_row.take() {
            return self.entry_path_for_row(row, cx).into_iter().collect();
        }
        let selected = self.selected_entries_visible_order(cx);
        if !selected.is_empty() {
            return selected;
        }
        self.active_tab()
            .lead_row(&self.active_tab().table.read(cx).delegate().entries)
            .and_then(|row| self.entry_path_for_row(row, cx))
            .into_iter()
            .collect()
    }

    fn on_navigate_back(&mut self, _: &NavigateBack, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate_back(cx);
    }

    fn on_navigate_forward(&mut self, _: &NavigateForward, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate_forward(cx);
    }

    fn on_open_selected(&mut self, _: &OpenSelected, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::notification::Notification;
        let entries = self.action_entries_visible_order(cx);
        if entries.is_empty() {
            return;
        }
        if entries.len() == 1 {
            self.activate_row(entries[0].0, cx);
            return;
        }
        const OPEN_MANY_CAP: usize = 10;
        if entries.len() > OPEN_MANY_CAP {
            window.push_notification(
                Notification::info(format!(
                    "Select {OPEN_MANY_CAP} or fewer items to open them together"
                )),
                cx,
            );
            return;
        }
        for (_, entry, path) in entries {
            if matches!(entry.kind, EntryKind::Directory) {
                self.open_path_in_new_tab(path, window, cx);
            } else {
                let _ = open_with_default(&path);
            }
        }
    }

    fn on_refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
    }

    fn on_toggle_hidden(&mut self, _: &ToggleHidden, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_hidden(cx);
    }

    fn on_focus_filter(&mut self, _: &FocusFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_filter_input(window, cx);
    }

    /// Public-from-screenshot-CLI helper: focuses the filter input
    /// (same effect as Cmd+F). Stage 2's `--search` flag uses this.
    pub fn focus_filter_input(&self, window: &mut Window, cx: &mut App) {
        self.active_tab()
            .filter_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
    }

    fn on_clear_filter(&mut self, _: &ClearFilter, window: &mut Window, cx: &mut Context<Self>) {
        // Spec §2.5 escape priority: clear selection first if
        // non-empty; only fall through to clearing the filter when
        // selection is already empty. Avoids stealing Esc from a
        // user trying to drop a selection without losing their
        // filter context.
        if !self.active_tab().selection.is_empty() {
            self.clear_active_selection(cx);
            self.focus_handle.focus(window, cx);
            return;
        }
        let filter_input = self.active_tab().filter_input.clone();
        filter_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        self.active_tab_mut().filter_text.clear();
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
        self.focus_handle.focus(window, cx);
    }

    /// Cmd+Shift+H — navigate the active tab to the home directory.
    fn on_go_home(&mut self, _: &GoHome, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(home_dir(), cx);
    }

    /// Cmd+L: open breadcrumb edit mode. Pre-fills the input with
    /// the active tab's current directory, focuses it, and selects
    /// all text so the user can immediately type a replacement
    /// path. Mirrors the old app's `enter_breadcrumb_edit_mode`.
    pub fn on_edit_breadcrumb(
        &mut self,
        _: &EditBreadcrumb,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.breadcrumb_editing = true;
        let current = self.active_tab().current_dir.to_string_lossy().into_owned();
        self.breadcrumb_input.update(cx, |state, cx| {
            state.set_value(current, window, cx);
        });
        self.breadcrumb_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    /// Cmd+/ (or `--shortcuts-help[-filter]` CLI flag): open the
    /// keyboard-shortcuts help overlay. Filter starts empty
    /// (showing every command in the catalogue, grouped by
    /// category); the user can type to narrow down.
    pub fn on_shortcuts_help(
        &mut self,
        _: &ShortcutsHelp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_shortcuts_help(String::new(), window, cx);
    }

    /// Programmatic version of `on_shortcuts_help` — the CLI flag
    /// can seed the filter so the screenshot captures a focused
    /// subset of the catalogue.
    pub fn open_shortcuts_help(
        &mut self,
        initial_filter: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shortcuts_help_filter = Some(initial_filter.clone());
        self.shortcuts_help_input.update(cx, |state, cx| {
            state.set_value(initial_filter, window, cx);
        });
        self.shortcuts_help_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    /// Dismiss the shortcuts-help overlay (called when the user
    /// clicks the backdrop or presses Esc).
    pub fn close_shortcuts_help(&mut self, cx: &mut Context<Self>) {
        self.shortcuts_help_filter = None;
        cx.notify();
    }

    /// Cmd+Shift+D — open the Disk Usage window at the active tab's
    /// current directory. Spawns a new native window; if opening
    /// fails (rare — only when gpui can't allocate a window), the
    /// failure is logged-and-ignored.
    /// Throttled persistence of the splitter pane widths. Called
    /// from the `on_resize` callback (fires per drag tick at 60 Hz).
    /// Writes the config file at most once per
    /// `SPLITTER_PERSIST_INTERVAL`; the final width at drag-end
    /// always lands because subsequent renders re-check the
    /// interval and flush. Trades a few-hundred-millisecond
    /// recoverability against not hammering the file system.
    fn maybe_persist_splitter(&mut self) {
        use std::time::Instant;
        let now = Instant::now();
        let should_save = match self.splitter_last_save {
            Some(t) => now.duration_since(t) >= SPLITTER_PERSIST_INTERVAL,
            None => true,
        };
        if !should_save {
            return;
        }
        self.splitter_last_save = Some(now);
        let mut state = app_state::load();
        state.sidebar_width = Some(self.sidebar_width);
        state.preview_width = Some(self.preview_width);
        app_state::save(&state);
    }

    pub fn on_open_disk_usage(
        &mut self,
        _: &OpenDiskUsage,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.active_tab().current_dir.clone();
        let fs = self.process.fs.clone();
        let tasks = self.process.tasks.clone();
        // The DU window owns its own entity, so it can't drive our
        // notify directly. We hand it a callback closing over a weak
        // handle to this Shell; when the DU scan begin/ends a task, it
        // calls back and we re-render to refresh the status bar.
        let weak: WeakEntity<Self> = cx.weak_entity();
        let notify_owner: Rc<dyn Fn(&mut App)> = Rc::new(move |cx| {
            if let Some(s) = weak.upgrade() {
                s.update(cx, |_, cx| cx.notify());
            }
        });
        if let Err(e) = crate::disk_usage::open_window(root, fs, tasks, Some(notify_owner), cx) {
            crate::log_warn!(90, "disk-usage: open_window failed: {e:?}");
        }
    }

    /// Cmd+P — toggle preview-pane visibility. The pane defaults to
    /// shown; toggling off gives the file list the full content width.
    fn on_toggle_preview(&mut self, _: &TogglePreview, _: &mut Window, cx: &mut Context<Self>) {
        self.preview_visible = !self.preview_visible;
        cx.notify();
    }

    /// Cmd+I — focus the preview pane (which serves as Get Info
    /// today). If the pane is hidden, show it first.
    fn on_get_info(&mut self, _: &GetInfo, _: &mut Window, cx: &mut Context<Self>) {
        if !self.preview_visible {
            self.preview_visible = true;
        }
        cx.notify();
    }

    /// Cmd+= / Cmd+- / Cmd+0 — UI zoom. Bumps `ui_scale` by ±0.1
    /// (clamped to [0.6, 2.0]). The render functions multiply
    /// text sizes against this value; a full pass through every
    /// `.text_*` call site lands with the per-tokens refactor.
    fn on_zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.ui_scale = (self.ui_scale + 0.1).clamp(0.6, 2.0);
        cx.notify();
    }
    fn on_zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.ui_scale = (self.ui_scale - 0.1).clamp(0.6, 2.0);
        cx.notify();
    }
    fn on_zoom_reset(&mut self, _: &ZoomReset, _: &mut Window, cx: &mut Context<Self>) {
        self.ui_scale = 1.0;
        cx.notify();
    }

    /// Open the selected row's path in a new tab (context-menu
    /// command). Falls back to the active tab's current dir when
    /// nothing is selected.
    fn on_open_in_new_tab(
        &mut self,
        _: &OpenInNewTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = match self.target_row(cx).and_then(|r| self.path_for_row(r, cx)) {
            Some(p) => p,
            None => self.active_tab().current_dir.clone(),
        };
        self.open_path_in_new_tab(path, window, cx);
    }

    /// Push a new tab at `path` and switch to it. Shared entry point
    /// for modifier-click in the file list / sidebar / Favorites
    /// section so each surface doesn't reimplement the tab push.
    /// Inserts beside the active tab per spec §3.3.
    pub fn open_path_in_new_tab(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.process.fs.id_for_path(&path);
        self.process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(path.clone(), id);
        let tab = self.make_tab(path, id, window, cx);
        let insert_at = self.active + 1;
        self.tabs.insert(insert_at, tab);
        self.active = insert_at;
        let cur = self.active_tab().current_dir.clone();
        self.load_path(cur, cx);
    }

    fn on_open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        // Spawn a second native window hosting the SettingsView,
        // matching macOS convention where Preferences is its own
        // window not a modal sheet. Independent of the file-manager
        // shell's lifecycle — closing one doesn't close the other.
        let _ = window;
        crate::settings::open_settings_window(cx);
    }

    pub fn navigate_back(&mut self, cx: &mut Context<Self>) {
        let (path, snapshot) = {
            let tab = self.active_tab_mut();
            if tab.history_index == 0 {
                return;
            }
            // Save the current entry's selection before stepping
            // back, so a subsequent Forward restores it.
            if let Some(cur) = tab.history.get_mut(tab.history_index) {
                cur.selection = tab.selection.clone();
                cur.anchor = tab.anchor;
                cur.lead = tab.lead;
            }
            tab.history_index -= 1;
            let entry = tab.history[tab.history_index].clone();
            (entry.path.clone(), entry)
        };
        self.restore_from_history(snapshot, path, cx);
    }

    pub fn navigate_forward(&mut self, cx: &mut Context<Self>) {
        let (path, snapshot) = {
            let tab = self.active_tab_mut();
            if tab.history_index + 1 >= tab.history.len() {
                return;
            }
            if let Some(cur) = tab.history.get_mut(tab.history_index) {
                cur.selection = tab.selection.clone();
                cur.anchor = tab.anchor;
                cur.lead = tab.lead;
            }
            tab.history_index += 1;
            let entry = tab.history[tab.history_index].clone();
            (entry.path.clone(), entry)
        };
        self.restore_from_history(snapshot, path, cx);
    }

    /// Common back/forward landing: seed the tab's selection from
    /// the history entry's snapshot, then issue a reload of the
    /// destination. The reload preserves selection through
    /// `load_path` (no longer clears it), and `finish_directory_load`
    /// reconciles the snapshot against the freshly streamed model —
    /// dropping NodeIds that no longer exist.
    fn restore_from_history(
        &mut self,
        snapshot: HistoryEntry,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        {
            let tab = self.active_tab_mut();
            tab.selection = snapshot.selection;
            tab.anchor = snapshot.anchor;
            tab.lead = snapshot.lead;
            tab.filtered_out.clear();
            tab.range_live = false;
        }
        self.active_tab_mut().pending_select_row = None;
        self.active_tab_mut().pending_select_rows.clear();
        let node_id = self.process.fs.id_for_path(&path);
        self.active_tab_mut().nav.navigate_to(node_id);
        self.record_ant_visit(node_id, cx);
        self.load_path(path, cx);
    }

    /// Persist last-dir + show-hidden + UI-scale to disk off the UI
    /// thread. Even though the state file is tiny, navigation must
    /// not wait on app-support directory creation or disk writes.
    /// `theme_pref` is owned by the Settings entity — persisted there
    /// after a tile click, not from Shell.
    fn save_state_async(&self, cx: &mut Context<Self>) {
        let last_dir = self.active_tab().current_dir.clone();
        let show_hidden = self.show_hidden;
        let ui_scale = self.ui_scale;
        cx.background_executor()
            .spawn(async move {
                let mut s = app_state::load();
                s.last_dir = Some(last_dir);
                s.show_hidden = Some(show_hidden);
                s.ui_scale = Some(ui_scale);
                app_state::save(&s);
            })
            .detach();
    }

    /// Inner load: re-enumerate the directory + refresh the table +
    /// re-target the watcher. Does **not** touch history (history
    /// is only mutated by `navigate`). Loads into the currently
    /// active tab. Public so the screenshot CLI driver can call it
    /// directly after seeding tab state.
    pub fn load_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let tab_id = self.active_tab().id;
        self.load_path_for_tab(tab_id, path, cx);
    }

    /// Schedule a directory load against a specific tab. Used by
    /// `load_path` (active tab) and, in Phase A+B + beyond, any
    /// background path that wants to retarget an inactive tab —
    /// e.g. the cross-window reload fan-out in Phase E.
    pub fn load_path_for_tab(&mut self, tab_id: TabId, path: PathBuf, cx: &mut Context<Self>) {
        let Some(tab_index) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let node_id = self.process.fs.id_for_path(&path);
        self.process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(path.clone(), node_id);
        let tab = &mut self.tabs[tab_index];
        tab.nav.replace_current(node_id);
        tab.current_dir = path.clone();
        // Selection is preserved across `load_path` calls so
        // filter/refresh/show-hidden/watcher reloads can reconcile
        // against the new model (spec §2.6). `navigate`, which
        // commits a new path, clears selection itself BEFORE
        // delegating here.
        tab.last_error = None;
        tab.load_generation = tab.load_generation.wrapping_add(1);
        let generation = tab.load_generation;
        let filter = tab.filter_text.clone();
        let show_hidden = self.show_hidden;

        if let Some(cancel) = self.tabs[tab_index].load_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let task = self.process.tasks.borrow_mut().begin(
            TaskKind::Enumeration,
            format!(
                "Reading {}",
                middle_truncate_path(&path.to_string_lossy(), 40)
            ),
            true,
        );
        if let Some(previous) = self.tabs[tab_index].load_task.replace(task) {
            self.process.tasks.borrow_mut().end(previous);
        }
        self.tabs[tab_index].load_pending_first_batch = true;

        // Point the watcher at the new directory. Errors (path
        // doesn't exist, watcher saturated) are non-fatal — the
        // user still gets the listing; they just lose live updates.
        if let Some(w) = self.process.watcher.borrow_mut().as_mut() {
            let _ = w.watch(&path);
        }
        self.save_state_async(cx);

        let fs = self.process.fs.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.tabs[tab_index].load_cancel = Some(cancel.clone());
        let (tx, rx) = async_channel::unbounded();
        let worker_path = path.clone();
        cx.background_executor()
            .spawn(async move {
                run_directory_load_streaming(fs, worker_path, show_hidden, filter, cancel, tx);
            })
            .detach();

        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                let done = matches!(msg, LoadMsg::Done(_));
                let stale = this
                    .update(cx, |this, cx| {
                        // Find the loading tab by id — its index may
                        // have shifted under reorder, and it may have
                        // closed entirely.
                        let Some(idx) = this.tabs.iter().position(|t| t.id == tab_id) else {
                            return true;
                        };
                        if this.tabs[idx].load_generation != generation
                            || this.tabs[idx].current_dir != path
                        {
                            return true;
                        }
                        // Helpers below operate on `self.active_tab()`.
                        // Temporarily swap `active` to the loading tab
                        // so the helpers update its state; restore on
                        // the way out. Safe because helpers don't paint
                        // synchronously — `cx.notify()` only schedules.
                        let prev_active = this.active;
                        this.active = idx;
                        this.apply_directory_load_msg(msg, cx);
                        this.active = prev_active;
                        false
                    })
                    .unwrap_or(true);
                if stale || done {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn apply_directory_load_msg(&mut self, msg: LoadMsg, cx: &mut Context<Self>) {
        match msg {
            LoadMsg::Batch(batch) => self.apply_directory_batch(batch, cx),
            LoadMsg::Done(error) => self.finish_directory_load(error, cx),
        }
    }

    fn apply_directory_batch(&mut self, batch: LoadBatch, cx: &mut Context<Self>) {
        for (id, path) in &batch.paths {
            self.process
                .node_store
                .borrow_mut()
                .get_or_create_path_with_id(path.clone(), *id);
        }
        let heats: Vec<f32> = batch
            .entries
            .iter()
            .map(|entry| self.ant_heat(entry.id))
            .collect();
        let first_batch = self.active_tab_mut().load_pending_first_batch;
        self.active_tab_mut().load_pending_first_batch = false;
        let table = self.active_tab().table.clone();
        table.update(cx, |state, cx| {
            if first_batch {
                state.delegate_mut().clear();
            }
            state
                .delegate_mut()
                .append_entries(batch.entries, batch.paths, heats);
            state.refresh(cx);
        });
        // Repaint the §5 star badge on each row whose path is now in
        // the favorites index. Cheap (HashMap lookups across the new
        // batch); runs once per batch on the load path.
        self.refresh_file_list_favorited(cx);
        // Spec §2.6 streaming arrival passes:
        //   1. Mirror current selection state into the delegate so
        //      the parallel render view paints the rows that just
        //      arrived.
        //   2. Lift any filtered-out NodeIds back into the live
        //      selection if their rows have now streamed in.
        //   3. Recompute a still-live Shift-range so rows landing
        //      between anchor and lead join the selection.
        self.refresh_file_list_selection(cx);
        self.restore_filtered_out_against_model(cx);
        self.recompute_live_range(cx);
        // Consume any queued screenshot-driver row select now that
        // the model has data.
        self.apply_pending_select_row(cx);
        cx.notify();
    }

    /// Recompute the file list's per-row `is_favorited` parallel vec
    /// from the current Favorites entity index. Called from:
    /// - `apply_directory_batch` (after rows arrive)
    /// - The `cx.observe(&self.process.favorites, …)` subscription registered
    ///   in `Shell::new` (so add / remove / repoint repaints star
    ///   badges in the same frame, §5.3).
    pub fn refresh_file_list_favorited(&mut self, cx: &mut Context<Self>) {
        let favs = self.process.favorites.clone();
        let table = self.active_tab().table.clone();
        let favs_ref = favs.read(cx);
        // Pre-collect each row's path so the table-update closure
        // doesn't need to borrow Shell again.
        let bits: Vec<bool> = table
            .read(cx)
            .delegate()
            .entries
            .iter()
            .map(|entry| {
                table
                    .read(cx)
                    .delegate()
                    .path_for_entry(entry.id)
                    .map(|p| favs_ref.contains_path(&p))
                    .unwrap_or(false)
            })
            .collect();
        let _ = favs_ref;
        table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            // Resize defensively — the table may have been cleared
            // between the snapshot and this update.
            delegate.is_favorited.resize(delegate.entries.len(), false);
            for (i, b) in bits.into_iter().enumerate() {
                if let Some(slot) = delegate.is_favorited.get_mut(i) {
                    *slot = b;
                }
            }
            state.refresh(cx);
        });
    }

    fn finish_directory_load(&mut self, error: Option<EnumerationError>, cx: &mut Context<Self>) {
        if let Some(id) = self.active_tab_mut().load_task.take() {
            self.process.tasks.borrow_mut().end(id);
        }
        self.active_tab_mut().load_cancel = None;
        if self.active_tab_mut().load_pending_first_batch {
            self.active_tab_mut().load_pending_first_batch = false;
            let table = self.active_tab().table.clone();
            table.update(cx, |state, cx| {
                state.delegate_mut().clear();
                state.refresh(cx);
            });
        }
        let row_count = self.active_tab().table.read(cx).delegate().entries.len();
        if row_count == 0 {
            self.active_tab_mut().last_error = error;
        } else {
            if let Some(err) = error {
                crate::log_warn!(90, "directory load ended with partial rows: {err:?}");
            }
            self.active_tab_mut().last_error = None;
        }

        // Spec §2.6 `Done`: drop NodeIds no longer in the final
        // model (or hold them in `filtered_out` when a filter is
        // active), re-seat anchor / lead if they vanished. Runs
        // once per load; iter-1 navigation that cleared selection
        // upfront makes this a no-op there, but back/forward and
        // any future external-mutation reload route through here.
        self.reconcile_done(cx);

        // Stage 4: kick off magic + quarantine prefetch after the
        // foreground table state has received the final snapshot.
        let table = self.active_tab().table.clone();
        let fs = self.process.fs.clone();
        let db = self.process.db_snapshot();
        let tasks = self.process.tasks.clone();
        let weak = cx.weak_entity();
        crate::prefetch::start(table, fs, db, tasks, weak, cx);
        let icon_seeds = self.icon_seeds_from_table(cx);
        self.start_icon_warm(icon_seeds, cx);
        cx.notify();
    }

    fn icon_seeds_from_table(&self, cx: &App) -> Vec<(FileEntry, PathBuf)> {
        let table = self.active_tab().table.read(cx);
        let delegate = table.delegate();
        delegate
            .entries
            .iter()
            .filter_map(|entry| {
                delegate
                    .path_for_entry(entry.id)
                    .map(|path| (entry.clone(), path))
            })
            .collect()
    }

    fn start_icon_warm(&self, seeds: Vec<(FileEntry, PathBuf)>, cx: &mut Context<Self>) {
        if seeds.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            for chunk in seeds.chunks(ICON_WARM_CHUNK) {
                cx.background_executor().timer(ICON_WARM_INTERVAL).await;
                let rows = chunk.to_vec();
                if this
                    .update(cx, |this, cx| {
                        let mut icons = this.process.icons.borrow_mut();
                        for (entry, path) in &rows {
                            let _ = icons.icon_for(entry, path);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn refresh_active_tab_heats(&mut self, cx: &mut Context<Self>) {
        let table = self.active_tab().table.clone();
        table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            let heats: Vec<f32> = delegate
                .entries
                .iter()
                .map(|entry| self.ant_heat(entry.id))
                .collect();
            delegate.heats = heats;
            state.refresh(cx);
        });
    }

    fn start_metadata_load(&mut self, cx: &mut Context<Self>) {
        if self.process.metadata_loaded.replace(true) {
            self.favorites_section_collapsed = self.process.favorites_section_collapsed.get();
            self.refresh_active_tab_heats(cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let db = open_metadata_db();
                    let (ant_visits, ant_max) = hydrate_ant_trail(db.as_ref());
                    let favs_collapsed = db
                        .as_ref()
                        .and_then(|d| d.lock().ok().map(|g| g.favorites_section_collapsed()))
                        .unwrap_or(false);
                    let favorites = db
                        .as_ref()
                        .and_then(|d| d.lock().ok().and_then(|g| g.load_favorites().ok()))
                        .unwrap_or_default();
                    (db, ant_visits, ant_max, favs_collapsed, favorites)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let (db, ant_visits, ant_max, favs_collapsed, favorites) = loaded;
                *this.process.metadata_db.borrow_mut() = db.clone();
                *this.process.ant_visits.borrow_mut() = ant_visits;
                this.process.ant_max.set(ant_max);
                this.process.favorites_section_collapsed.set(favs_collapsed);
                this.favorites_section_collapsed = favs_collapsed;
                // Attach the writable DB to the favorites entity and
                // hydrate. The dev seed runs only when the entry list
                // is empty AND `FERAILLE_DEV_SEED_FAVORITES=1` — see
                // `crate::favorites::maybe_seed_dev_favorites`.
                let fav_entity = this.process.favorites.clone();
                fav_entity.update(cx, |f, cx| {
                    if let Some(d) = db.clone() {
                        f.attach_db(d);
                    }
                    f.hydrate(favorites, cx);
                    crate::favorites::maybe_seed_dev_favorites(f, cx);
                });
                this.refresh_active_tab_heats(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn reload_tabs_matching_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let targets: Vec<(TabId, PathBuf)> = self
            .tabs
            .iter()
            .filter(|tab| paths.iter().any(|path| path == &tab.current_dir))
            .map(|tab| (tab.id, tab.current_dir.clone()))
            .collect();
        for (tab_id, path) in targets {
            self.load_path_for_tab(tab_id, path, cx);
        }
    }

    fn broadcast_reload_for_process(
        process: &Rc<crate::process_state::ProcessState>,
        paths: Vec<PathBuf>,
        cx: &mut AsyncApp,
    ) {
        if paths.is_empty() {
            return;
        }
        for weak in process.live_shells() {
            if let Some(shell) = weak.upgrade() {
                let paths = paths.clone();
                let _ = shell.update(cx, |this, cx| {
                    this.reload_tabs_matching_paths(&paths, cx);
                });
            }
        }
    }

    fn spawn_file_op(
        &self,
        reload_path: PathBuf,
        op: impl FnOnce() -> Result<(), String> + Send + 'static,
        label: &'static str,
        cx: &mut Context<Self>,
    ) {
        let process = self.process.clone();
        cx.spawn(async move |_this, cx| {
            let result = cx.background_executor().spawn(async move { op() }).await;
            match result {
                Ok(()) => Shell::broadcast_reload_for_process(&process, vec![reload_path], cx),
                Err(e) => crate::log_warn!(90, "{label} failed: {e}"),
            }
        })
        .detach();
    }

    /// Append a reversible op to the undo stack, evicting the oldest
    /// entry when capacity is exceeded.
    fn push_undo(&mut self, op: UndoOp) {
        self.process.push_undo(op, UNDO_STACK_CAP);
    }

    pub fn on_undo_last_action(
        &mut self,
        _: &UndoLastAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        let Some(op) = self.process.undo_stack.borrow_mut().pop_back() else {
            window.push_notification(Notification::info("Nothing to undo"), cx);
            return;
        };
        let label = op.label();
        match op {
            UndoOp::AddFavorite(id) => {
                self.process.favorites.update(cx, |f, cx| {
                    f.remove(id, cx);
                });
                window.push_notification(Notification::success(label.to_string()), cx);
            }
            UndoOp::RemoveFavorite(fav) => {
                self.process.favorites.update(cx, |f, cx| {
                    f.restore(fav, cx);
                });
                window.push_notification(Notification::success(label.to_string()), cx);
            }
            fs_op => match fs_op.apply_fs() {
                Ok(()) => {
                    window.push_notification(Notification::success(label.to_string()), cx);
                    let path = self.active_tab().current_dir.clone();
                    self.load_path(path, cx);
                }
                Err(e) => {
                    window.push_notification(Notification::error(format!("Undo failed: {e}")), cx);
                }
            },
        }
    }

    pub fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
    }

    // ----- Favorites mutations (iter 4) ---------------------------

    /// Cmd+D / context-menu / menu-bar toggle. Reads the target from
    /// `favorites_context_path` (set by every "Add to Favorites" /
    /// "Remove from Favorites" closure), falling back to the file-list
    /// selection's path, then to the active tab's `current_dir`.
    /// Files are rejected with a toast (§2.3).
    pub fn on_toggle_favorite_for_target(
        &mut self,
        _: &ToggleFavoriteForTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        let target = self.resolve_favorite_target(cx);
        let Some((path, kind)) = target else {
            window.push_notification(
                Notification::info("No folder available to add to Favorites."),
                cx,
            );
            return;
        };
        match kind {
            FavoriteResolved::Folder => {
                let canonical = std::fs::canonicalize(&path).unwrap_or(path.clone());
                let already = self.process.favorites.read(cx).contains_path(&canonical);
                let favs = self.process.favorites.clone();
                if already {
                    let id = self
                        .process
                        .favorites
                        .read(cx)
                        .id_for_path(&canonical)
                        .expect("contains_path returned true");
                    let label = self
                        .process
                        .favorites
                        .read(cx)
                        .entry_by_id(id)
                        .map(|f| f.effective_label())
                        .unwrap_or_else(|| "favorite".to_string());
                    // Capture the full entry before removal so the undo
                    // restores name + icon + sort_index + date_added.
                    let removed_for_undo = self.process.favorites.read(cx).entry_by_id(id).cloned();
                    favs.update(cx, |f, cx| {
                        f.remove(id, cx);
                    });
                    if let Some(fav) = removed_for_undo {
                        self.push_undo(UndoOp::RemoveFavorite(fav));
                    }
                    window.push_notification(
                        Notification::info(format!(
                            "Removed \u{201C}{label}\u{201D} from Favorites · Cmd+Z to undo"
                        )),
                        cx,
                    );
                } else {
                    let label = canonical
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| canonical.to_string_lossy().into_owned());
                    let added_id = favs.update(cx, |f, cx| {
                        match f.add_path(
                            canonical.clone(),
                            feraille_core::favorites::FavoriteKind::Folder,
                            cx,
                        ) {
                            crate::favorites::AddOutcome::Added(id) => Some(id),
                            crate::favorites::AddOutcome::Existing(_) => None,
                        }
                    });
                    if let Some(id) = added_id {
                        self.push_undo(UndoOp::AddFavorite(id));
                    }
                    window.push_notification(
                        Notification::success(format!(
                            "Added \u{201C}{label}\u{201D} to Favorites"
                        )),
                        cx,
                    );
                }
            }
            FavoriteResolved::NotAFolder => {
                window.push_notification(
                    Notification::info("Only folders can be added to Favorites."),
                    cx,
                );
            }
        }
    }

    /// Backs `File → Add to Favorites` and the section-header `+`
    /// button. No-op if the current folder is already a favorite
    /// (dedup pulse is emitted by the entity).
    pub fn on_add_current_folder_to_favorites(
        &mut self,
        _: &AddCurrentFolderToFavorites,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self.active_tab().current_dir.clone();
        self.favorites_context_path = Some(path);
        self.on_toggle_favorite_for_target(&ToggleFavoriteForTarget, window, cx);
    }

    pub fn on_toggle_favorites_section(
        &mut self,
        _: &ToggleFavoritesSection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_favorites_section_collapsed(cx);
    }

    // ----- Favorites: one-shot sorts (§4.5) ------------------------

    pub fn on_sort_favorites_by_name(
        &mut self,
        _: &SortFavoritesByName,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.process.favorites.update(cx, |f, cx| {
            f.one_shot_sort(feraille_core::favorites::FavoriteSort::NameAsc, cx);
        });
    }

    pub fn on_sort_favorites_by_date_added_newest(
        &mut self,
        _: &SortFavoritesByDateAddedNewest,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.process.favorites.update(cx, |f, cx| {
            f.one_shot_sort(feraille_core::favorites::FavoriteSort::DateAddedNewest, cx);
        });
    }

    pub fn on_sort_favorites_by_date_added_oldest(
        &mut self,
        _: &SortFavoritesByDateAddedOldest,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.process.favorites.update(cx, |f, cx| {
            f.one_shot_sort(feraille_core::favorites::FavoriteSort::DateAddedOldest, cx);
        });
    }

    pub fn on_sort_favorites_by_kind(
        &mut self,
        _: &SortFavoritesByKind,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.process.favorites.update(cx, |f, cx| {
            f.one_shot_sort(feraille_core::favorites::FavoriteSort::Kind, cx);
        });
    }

    // ----- Favorites: keyboard reorder (§4.4) ----------------------

    pub fn on_move_favorite_up(
        &mut self,
        _: &MoveFavoriteUp,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.focused_favorite {
            self.process
                .favorites
                .update(cx, |f, cx| f.shift(id, -1, cx));
        }
    }

    pub fn on_move_favorite_down(
        &mut self,
        _: &MoveFavoriteDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.focused_favorite {
            self.process
                .favorites
                .update(cx, |f, cx| f.shift(id, 1, cx));
        }
    }

    // ----- Favorites: rename + custom icons (iter 9 / §6 / §7) -----

    /// Resolve the favorite id for the next rename/icon action. The
    /// row-level context menu sets `favorites_context_path` before
    /// dispatching, so we look the id up by canonical path.
    fn pop_favorite_id_for_action(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<feraille_core::favorites::FavoriteId> {
        let path = self.favorites_context_path.take()?;
        let canonical = std::fs::canonicalize(&path).unwrap_or(path);
        self.process.favorites.read(cx).id_for_path(&canonical)
    }

    pub fn on_rename_favorite(
        &mut self,
        _: &RenameFavorite,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        let Some(id) = self.pop_favorite_id_for_action(cx) else {
            return;
        };
        let current = self
            .process
            .favorites
            .read(cx)
            .entry_by_id(id)
            .map(|f| f.effective_label())
            .unwrap_or_default();
        // Native NSAlert prompt — keeps the rename path simple and
        // matches macOS feel. Renaming the shortcut, not the folder.
        let next = crate::platform_shell::prompt_for_text(
            "Rename Favorite",
            "Renames the shortcut\u{2019}s label only, not the folder on disk.",
            &current,
        );
        let Some(value) = next else { return };
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() {
            window.push_notification(Notification::info("Name can\u{2019}t be empty."), cx);
            return;
        }
        self.process
            .favorites
            .update(cx, |f, cx| f.rename(id, Some(trimmed), cx));
    }

    pub fn on_reset_favorite_name(
        &mut self,
        _: &ResetFavoriteName,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.pop_favorite_id_for_action(cx) else {
            return;
        };
        self.process
            .favorites
            .update(cx, |f, cx| f.rename(id, None, cx));
    }

    pub fn on_reset_favorite_icon(
        &mut self,
        _: &ResetFavoriteIcon,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.pop_favorite_id_for_action(cx) else {
            return;
        };
        self.process
            .favorites
            .update(cx, |f, cx| f.set_icon(id, None, cx));
    }

    fn set_favorite_lucide(&mut self, name: &'static str, cx: &mut Context<Self>) {
        let Some(id) = self.pop_favorite_id_for_action(cx) else {
            return;
        };
        let icon = feraille_core::favorites::FavoriteIcon::Lucide(name.into());
        self.process
            .favorites
            .update(cx, |f, cx| f.set_icon(id, Some(icon), cx));
    }

    pub fn on_set_favorite_icon_star(
        &mut self,
        _: &SetFavoriteIconStar,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_favorite_lucide("nav/star", cx);
    }
    pub fn on_set_favorite_icon_folder(
        &mut self,
        _: &SetFavoriteIconFolder,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_favorite_lucide("nav/folder", cx);
    }
    pub fn on_set_favorite_icon_code(
        &mut self,
        _: &SetFavoriteIconCode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_favorite_lucide("file/code", cx);
    }
    pub fn on_set_favorite_icon_image(
        &mut self,
        _: &SetFavoriteIconImage,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_favorite_lucide("file/image", cx);
    }
    pub fn on_set_favorite_icon_music(
        &mut self,
        _: &SetFavoriteIconMusic,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_favorite_lucide("nav/music", cx);
    }
    pub fn on_set_favorite_icon_archive(
        &mut self,
        _: &SetFavoriteIconArchive,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_favorite_lucide("file/archive", cx);
    }

    /// Pick the path the next favorites mutation should target.
    /// Order of precedence:
    ///   1. `favorites_context_path` (set by sidebar / breadcrumb /
    ///      favorite-row context menus before dispatching the action)
    ///   2. The file-list row most recently right-clicked or selected
    ///      via [`Shell::target_row`].
    ///   3. The active tab's `current_dir` (so a keyboard Cmd+D with
    ///      nothing selected toggles the current folder).
    fn resolve_favorite_target(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(PathBuf, FavoriteResolved)> {
        // A path that is already in the favorites index must be
        // classifiable as `Folder` even when its on-disk presence is
        // gone (Missing or Unmounted state) — otherwise "Remove from
        // Favorites" on a broken row routes to the NotAFolder rejection
        // toast and the user can never remove the stale shortcut.
        let already_favorite =
            |path: &Path, this: &Self| this.process.favorites.read(cx).contains_path(path);
        if let Some(p) = self.favorites_context_path.take() {
            let kind = if already_favorite(&p, self) || p.is_dir() {
                FavoriteResolved::Folder
            } else {
                FavoriteResolved::NotAFolder
            };
            return Some((p, kind));
        }
        if let Some(row) = self.target_row(cx) {
            if let Some(path) = self.path_for_row(row, cx) {
                let kind = if already_favorite(&path, self) || path.is_dir() {
                    FavoriteResolved::Folder
                } else {
                    FavoriteResolved::NotAFolder
                };
                return Some((path, kind));
            }
        }
        let current = self.active_tab().current_dir.clone();
        Some((current, FavoriteResolved::Folder))
    }

    // ----- Tab management (5.5.d) ---------------------------------

    /// Cmd+T: open a new tab beside the active one, at the active
    /// tab's current directory. Spec §4.3: "new tab default — same
    /// directory as the currently active tab (so Cmd+T is 'another
    /// view of where I am'), inserted after the active tab."
    fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.active_tab().current_dir.clone();
        let id = self.process.fs.id_for_path(&path);
        self.process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(path.clone(), id);
        let tab = self.make_tab(path.clone(), id, window, cx);
        let insert_at = self.active + 1;
        self.tabs.insert(insert_at, tab);
        self.active = insert_at;
        self.load_path(path, cx);
    }

    /// Cmd+W: close the active tab. Per spec §3.4: if it's the last
    /// tab, close the whole window. With multi-window (Phase C) the
    /// process stays resident at zero windows, so this is non-fatal.
    /// Phase D: every closed tab pushes a snapshot onto
    /// `ProcessState::closed_tabs` for `Cmd+Shift+T`. Closing the
    /// last tab via this path pushes that final tab before the
    /// window is removed.
    fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            self.process
                .push_closed_tab(self.tabs[self.active].snapshot_for_close());
            window.remove_window();
            return;
        }
        let snapshot = self.tabs[self.active].snapshot_for_close();
        self.process.push_closed_tab(snapshot);
        self.tabs.remove(self.active);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        // No re-load — the now-active tab already has its own
        // TableState populated from its earlier load (Phase A+B).
        cx.notify();
    }

    /// Cmd+Shift+W: close the entire window regardless of how many
    /// tabs it has. Per spec §3.4 the "I mean the window" verb. The
    /// process stays resident at zero windows; the user can reopen
    /// with Cmd+N. Phase D: all tabs are pushed onto the closed-tab
    /// stack in left-to-right order so the most-recent `Cmd+Shift+T`
    /// brings back the rightmost tab first (chronological reverse of
    /// individual closes).
    fn on_close_window(&mut self, _: &CloseWindow, window: &mut Window, _cx: &mut Context<Self>) {
        for tab in &self.tabs {
            self.process.push_closed_tab(tab.snapshot_for_close());
        }
        window.remove_window();
    }

    /// Cmd+Shift+T: reopen the most recently closed tab. Pops the top
    /// of `ProcessState::closed_tabs`, builds a fresh tab at the
    /// recorded directory, restores filter/history/selection, and
    /// schedules a streaming reload. Spec §3.3 "Reopen closed tab".
    fn on_reopen_closed_tab(
        &mut self,
        _: &ReopenClosedTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(closed) = self.process.pop_closed_tab() else {
            return;
        };
        let path = closed.current_dir.clone();
        // Re-register the path in NodeStore to mint (or reuse) a
        // stable NodeId. ProcessState is the singleton, so a NodeId
        // captured before the tab closed is still valid — but we
        // pass through `get_or_create_path_with_id` regardless, the
        // same way Cmd+T does, so the reopen path stays a normal
        // "new tab at this path" pipeline.
        let node_id = self.process.fs.id_for_path(&path);
        self.process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(path.clone(), node_id);

        let mut tab = self.make_tab(path.clone(), node_id, window, cx);
        // Apply the captured tab-local state onto the fresh Tab
        // before inserting it. Filter goes onto both the Tab field
        // (which the load reads) and the live `Input` entity so the
        // title-bar field renders the restored text immediately.
        tab.history = closed.history;
        tab.history_index = closed.history_index;
        tab.filter_text = closed.filter_text.clone();
        tab.selection = closed.selection;
        tab.anchor = closed.anchor;
        tab.lead = closed.lead;
        let filter_input = tab.filter_input.clone();
        filter_input.update(cx, |state, cx| {
            state.set_value(closed.filter_text, window, cx);
        });

        let insert_at = self.active + 1;
        self.tabs.insert(insert_at, tab);
        self.active = insert_at;
        // Stream the directory fresh. The captured `selection` set
        // is reconciled against the model on streaming `Done` via
        // the standard reconciliation path — best-effort per spec
        // §3.3 (rows that no longer exist drop, surviving rows
        // re-light).
        self.load_path(path, cx);
    }

    /// Ctrl+Tab: cycle to the next tab.
    fn on_next_tab(&mut self, _: &NextTab, _: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 {
            return;
        }
        self.active = (self.active + 1) % self.tabs.len();
        cx.notify();
    }

    /// Ctrl+Shift+Tab: cycle to the previous tab.
    fn on_prev_tab(&mut self, _: &PrevTab, _: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 {
            return;
        }
        self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        cx.notify();
    }

    /// Switch to the tab at `idx`. Used by tabstrip click handlers.
    /// No re-enumeration — the target tab already owns its own
    /// `TableState` with whatever its last load produced.
    pub fn select_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() || idx == self.active {
            return;
        }
        self.active = idx;
        cx.notify();
    }

    /// Phase D, spec §3.3 "Reorder tab" — move the tab identified by
    /// `from_id` into gap position `to_pos`. Gap positions number
    /// `0..=tabs.len()`: gap 0 is before the first tab, gap N is after
    /// the last. Drops at gap-of-itself or gap-just-after-itself are
    /// no-ops (dropping where you started). Active-tab tracking is by
    /// `TabId`, so the active tab follows its own move and unrelated
    /// reorders correctly shift `self.active`.
    pub fn reorder_tab(&mut self, from_id: TabId, to_pos: usize, cx: &mut Context<Self>) {
        let Some(from_idx) = self.tabs.iter().position(|t| t.id == from_id) else {
            return;
        };
        if to_pos > self.tabs.len() {
            return;
        }
        if to_pos == from_idx || to_pos == from_idx + 1 {
            return;
        }
        let active_id = self.tabs[self.active].id;
        let tab = self.tabs.remove(from_idx);
        // After removal, indices `> from_idx` shift down by one. The
        // gap at position `to_pos` in the *pre-remove* list maps to
        // `to_pos - 1` in the post-remove list iff `from_idx < to_pos`;
        // otherwise it stays at `to_pos`.
        let insert_at = if from_idx < to_pos {
            to_pos - 1
        } else {
            to_pos
        };
        self.tabs.insert(insert_at, tab);
        self.active = self
            .tabs
            .iter()
            .position(|t| t.id == active_id)
            .unwrap_or(0);
        cx.notify();
    }

    fn on_navigate_parent(&mut self, _: &NavigateParent, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate_parent(cx);
    }

    /// User activated a row (double-click or Enter). For directories
    /// we navigate into them; for files we hand off to the OS opener.
    pub fn activate_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        let path_and_kind = self
            .active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .get(row_ix)
            .map(|e| {
                (
                    self.path_for_row(row_ix, cx).unwrap_or_else(|| {
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

    /// Navigate to `path`: snapshot the current tab's selection
    /// into the history entry we're leaving, re-enumerate, refresh
    /// the Table, push to history (truncating any forward stack
    /// first), reset selection for the new path, and increment the
    /// Ant Trail visit count (Stage 9.b).
    ///
    /// Spec §2.6: navigation commits immediately and starts the
    /// new path with empty selection unless this is a back/forward
    /// (see `navigate_back` / `navigate_forward` which seed the
    /// restored selection before calling here).
    pub fn navigate(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let node_id = self.process.fs.id_for_path(&path);
        self.process
            .node_store
            .borrow_mut()
            .get_or_create_path_with_id(path.clone(), node_id);
        let tab = self.active_tab_mut();
        // Snapshot the selection we're leaving into the current
        // history entry so a Back returns to where the user was.
        if let Some(entry) = tab.history.get_mut(tab.history_index) {
            entry.selection = tab.selection.clone();
            entry.anchor = tab.anchor;
            entry.lead = tab.lead;
        }
        let same_path = tab
            .history
            .get(tab.history_index)
            .map(|e| &e.path == &path)
            .unwrap_or(false);
        if !same_path {
            tab.history.truncate(tab.history_index + 1);
            tab.history.push(HistoryEntry::new(path.clone()));
            tab.history_index = tab.history.len() - 1;
        }
        // Fresh navigation: clear selection + filter holding +
        // live-range. Back/forward override this with the restored
        // snapshot just before calling us (see `restore_from_history`).
        tab.selection.clear();
        tab.anchor = None;
        tab.lead = None;
        tab.filtered_out.clear();
        tab.range_live = false;
        tab.nav.navigate_to(node_id);
        // Any pending screenshot select belongs to the previous
        // path; drop it so a stale row index doesn't apply.
        self.active_tab_mut().pending_select_row = None;
        self.active_tab_mut().pending_select_rows.clear();
        self.record_ant_visit(node_id, cx);
        self.load_path(path, cx);
    }

    pub fn navigate_node(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
        let Some(path) = self
            .process
            .node_store
            .borrow_mut()
            .path_snapshot_for_job(node_id, "Shell::navigate_node")
        else {
            return;
        };
        self.navigate(path, cx);
    }

    /// Bump the Ant Trail visit count for `path` in the in-memory
    /// map and persist asynchronously through `metadata_db`. Cheap
    /// on the foreground executor — the DB write is a single
    /// upsert and `feraille_meta::MetadataDb` does its own
    /// connection pooling internally.
    pub fn record_ant_visit(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
        let Some(path) = self
            .process
            .node_store
            .borrow()
            .path_snapshot_for_job(node_id, "Shell::record_ant_visit")
        else {
            return;
        };
        self.process.record_ant_visit(path.clone());
        let heat = self.ant_heat(node_id);
        self.process.node_store.borrow_mut().set_heat(node_id, heat);
        if let Some(db) = self.process.db_snapshot() {
            let path_str = path.to_string_lossy().into_owned();
            let when = now_unix_secs();
            cx.background_executor()
                .spawn(async move {
                    if let Ok(guard) = db.lock() {
                        let _ = guard.record_folder_visit(&path_str, when);
                    }
                })
                .detach();
        }
    }

    /// Compute the Ant Trail heat for `path` — 0.0 (never visited)
    /// through 1.0 (most-visited folder). Log-scaled so a 10-visit
    /// folder isn't 10× brighter than a 5-visit one. Used by the
    /// file list to apply a subtle background tint per row.
    pub fn ant_heat(&self, node_id: NodeId) -> f32 {
        let cached = self.process.node_store.borrow().heat(node_id);
        if cached > 0.0 {
            return cached;
        }
        let Some(path) = self
            .process
            .node_store
            .borrow()
            .path_snapshot_for_job(node_id, "Shell::ant_heat")
        else {
            return 0.0;
        };
        let Some(&v) = self.process.ant_visits.borrow().get(&path) else {
            return 0.0;
        };
        let max = self.process.ant_max.get();
        if max <= 1 {
            return 1.0;
        }
        ((v as f32 + 1.0).log2() / (max as f32 + 1.0).log2()).clamp(0.0, 1.0)
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
            if !self.tree_children.contains_key(path) {
                self.start_tree_children_load(path.to_path_buf(), cx);
            }
        }
        cx.notify();
    }

    pub fn toggle_tree_expand_node(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
        let Some(path) = self
            .process
            .node_store
            .borrow_mut()
            .path_snapshot_for_job(node_id, "Shell::toggle_tree_expand_node")
        else {
            return;
        };
        self.toggle_tree_expand(&path, cx);
    }

    fn start_tree_children_load(&self, path: PathBuf, cx: &mut Context<Self>) {
        let fs = self.process.fs.clone();
        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            let parent = path.clone();
            let children = cx
                .background_executor()
                .spawn(async move { run_tree_children_load(fs, parent.clone()) })
                .await;
            let Some(shell) = weak.upgrade() else { return };
            let _ = shell.update(cx, |this, cx| {
                for child in &children {
                    this.process
                        .node_store
                        .borrow_mut()
                        .get_or_create_path_with_id(child.path.clone(), child.node_id);
                }
                this.tree_children.insert(path, children);
                cx.notify();
            });
        })
        .detach();
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
                let Some(name) = p.file_name().and_then(|s| s.to_str()).map(str::to_owned) else {
                    continue;
                };
                // file_type() can be cheap (no extra stat on most
                // platforms); fall back to metadata() if it errors.
                let is_dir = match dirent.file_type() {
                    Ok(ft) => {
                        ft.is_dir()
                            || (ft.is_symlink()
                                && std::fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false))
                    }
                    Err(_) => false,
                };
                if !is_dir {
                    continue;
                }
                let node_id = self.process.fs.id_for_path(&p);
                self.process
                    .node_store
                    .borrow_mut()
                    .get_or_create_path_with_id(p.clone(), node_id);
                children.push(TreeChild {
                    node_id,
                    path: p,
                    label: name,
                });
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
}
