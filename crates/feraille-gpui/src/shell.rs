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
use std::time::Duration;

use feraille_core::{
    EntryKind, EnumerationError, FileEntry, FsBackend, NodeId, navigation::NavigationState,
    node_store::NodeStore,
};
use feraille_fs_native::{NativeFs, VolumeInfo, home_dir, list_volumes, open_with_default};
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, Root, Sizable, TitleBar, WindowExt,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem},
    table::{DataTable, TableEvent, TableState},
    v_flex,
};

use crate::app_state;
use crate::file_list::FileListDelegate;
use crate::fs_watcher::{FsWatcher, POLL_INTERVAL};
use crate::icons::IconCache;
use crate::tasks::TaskRegistry;
use crate::tree::{ShellSidebarItem, TreeChild, TreeRowSpec, TreeSection};

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
        EditBreadcrumb,
        ShortcutsHelp,
        OpenDiskUsage,
        CursorUp,
        CursorDown,
        CursorFirst,
        CursorLast,
        PageUp,
        PageDown,
        TogglePreview,
        GetInfo,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        OpenInNewTab,
        Duplicate,
        MakeAlias,
        Compress,
        // Phase 6 (next-level): right-click context menus on the
        // sidebar / breadcrumb / file-pane background. These four
        // actions all operate on `Shell::context_target` instead
        // of the file-list selection. The right-click event hands
        // the closure in `context_menu(...)` a PathBuf; the
        // closure stashes it on Shell, then dispatches one of the
        // actions below. Handlers `take()` the target so the next
        // keyboard-driven action falls back to the regular row
        // selection.
        RevealContextPath,
        CopyContextPath,
        OpenContextInNewTab,
        NewFolderHere,
        // Phase 6 follow-on: Tags + Open-With submenus. Seven tag
        // colours match Finder's canonical Red/Orange/Yellow/Green/
        // Blue/Purple/Gray set; toggle behaviour mirrors
        // `feraille_shell_mac::toggle_tag`.
        ToggleTagRed,
        ToggleTagOrange,
        ToggleTagYellow,
        ToggleTagGreen,
        ToggleTagBlue,
        ToggleTagPurple,
        ToggleTagGray,
        // Twelve indexed slots for the Open-With submenu. The menu
        // builder lays out at most this many candidate apps; each
        // handler re-resolves the candidates for the target row at
        // dispatch time and opens slot N. Twelve covers every file
        // kind we've seen in practice (~5 is typical).
        OpenWithSlot0,
        OpenWithSlot1,
        OpenWithSlot2,
        OpenWithSlot3,
        OpenWithSlot4,
        OpenWithSlot5,
        OpenWithSlot6,
        OpenWithSlot7,
        OpenWithSlot8,
        OpenWithSlot9,
        OpenWithSlot10,
        OpenWithSlot11,
    ]
);

/// Per-tab state. Each tab has its own current directory + nav
/// history + cursor selection. Filter text, show-hidden, the
/// virtualized Table entity, and the FS watcher are shared at the
/// Shell level — Finder-style "the active tab's location is what
/// the rest of the chrome reflects."
#[derive(Clone)]
pub struct Tab {
    /// Authoritative location identity. `current_dir` remains as a
    /// display/job snapshot during the migration, but navigation logic
    /// moves through this NodeId state first.
    pub nav: NavigationState,
    pub current_dir: PathBuf,
    pub history: Vec<PathBuf>,
    pub history_index: usize,
    pub selected: Option<usize>,
}

impl Tab {
    pub fn new(at: PathBuf, node_id: NodeId) -> Self {
        Self {
            nav: NavigationState::new(node_id),
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
    /// Shared node identity store. This is the GPUI bridge toward the
    /// Windows architecture: views carry NodeId, paths are resolved only
    /// for jobs/actions.
    pub node_store: NodeStore,
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
    /// Monotonic generation for directory loads. Background enumeration
    /// results apply only if their generation still matches.
    pub load_generation: u64,
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
    /// Path target for the sidebar / breadcrumb / empty-space
    /// context menus (Phase 6). Set by the `.context_menu(...)`
    /// closure on right-click; consumed by `RevealContextPath` /
    /// `CopyContextPath` / `OpenContextInNewTab` /
    /// `NewFolderHere` handlers. Unlike `context_row` (which targets
    /// file-list rows by index), this carries the full path because
    /// sidebar items aren't part of the file list.
    pub context_target: Option<PathBuf>,
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
    /// Whether the background-task panel popover is open. Toggled by
    /// clicking the task region in the status bar.
    pub task_panel_open: bool,
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
    /// Per-path Quick Look thumbnail cache. Populated lazily by
    /// `preview::request` on selection change.
    pub preview_cache: crate::preview::PreviewCache,
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
    /// Sidebar tree state (Stage 9.c): which directories are
    /// currently expanded. Updated on caret-click and by the
    /// `--expand <path>` CLI flag (which walks the path's ancestors).
    pub expanded: HashSet<PathBuf>,
    /// Cached direct-children of any path that's ever been expanded.
    /// Folders only (the tree shows hierarchy; files live in the
    /// main pane). Once cached, re-expand is instant; collapsing a
    /// folder doesn't evict its cache.
    pub tree_children: HashMap<PathBuf, Vec<TreeChild>>,
    /// Ant Trail visit counts (Stage 9.b). Path → hits. Hydrated
    /// from `metadata_db` on startup, incremented on every
    /// `navigate`, persisted through `record_folder_visit`.
    pub ant_visits: HashMap<PathBuf, u32>,
    /// Cached max visit count for heat normalisation. Updated
    /// whenever a row's count crosses the existing max.
    pub ant_max: u32,
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

struct LoadResult {
    entries: Vec<FileEntry>,
    paths: HashMap<NodeId, PathBuf>,
    error: Option<EnumerationError>,
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

/// A user-pinned filesystem shortcut shown in the sidebar's
/// **Favorites** section. Flat — no expand/collapse, no descendants
/// — so a click navigates and that's it. Tree-style hierarchical
/// browsing lives in the separate Browse section below the favorites.
struct Favorite {
    label: &'static str,
    /// `home`-relative subpath (None ⇒ the home directory itself).
    sub: Option<&'static str>,
    /// Asset path for the row's prefix icon. Resolved via
    /// `FeraAssets` (Phase 1 composite source) so both our local
    /// bundle and the upstream gpui-component pack work.
    icon: &'static str,
}

const FAVORITES: &[Favorite] = &[
    Favorite {
        label: "Home",
        sub: None,
        icon: "icons/nav/home.svg",
    },
    Favorite {
        label: "Applications",
        sub: Some("Applications"),
        icon: "icons/nav/apps.svg",
    },
    Favorite {
        label: "Desktop",
        sub: Some("Desktop"),
        icon: "icons/nav/desktop.svg",
    },
    Favorite {
        label: "Documents",
        sub: Some("Documents"),
        icon: "icons/nav/documents.svg",
    },
    Favorite {
        label: "Downloads",
        sub: Some("Downloads"),
        icon: "icons/nav/downloads.svg",
    },
    Favorite {
        label: "Movies",
        sub: Some("Movies"),
        icon: "icons/nav/movies.svg",
    },
    Favorite {
        label: "Music",
        sub: Some("Music"),
        icon: "icons/nav/music.svg",
    },
    Favorite {
        label: "Pictures",
        sub: Some("Pictures"),
        icon: "icons/nav/pictures.svg",
    },
];

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

impl Favorite {
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
        let start_id = fs.id_for_path(&start);
        let mut node_store = NodeStore::new();
        node_store.get_or_create_path_with_id(start.clone(), start_id);
        let show_hidden = persisted.show_hidden.unwrap_or(false);
        // FERAILLE_UI_SCALE env var (regression tool / screenshots)
        // wins over the persisted value when set. Both are clamped.
        let ui_scale = std::env::var("FERAILLE_UI_SCALE")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .or(persisted.ui_scale)
            .map(|n| n.clamp(0.6, 2.0))
            .unwrap_or(1.0);
        let icons = Rc::new(RefCell::new(IconCache::new()));
        let delegate = FileListDelegate::new(fs.clone(), icons.clone());
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
                    // Kick off a Quick Look thumbnail fetch for the
                    // newly-selected row. Cheap (cache hit on
                    // re-select); cold paths run on the background
                    // executor and the worker writes back into
                    // `preview_cache` via cx.spawn.
                    if let Some(p) = this.path_for_row(*row_ix, cx) {
                        crate::preview::request(this, p, cx);
                    }
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

        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter \u{2026}"));
        let filter_subscription = cx.subscribe_in(&filter_input, window, {
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

        let initial_tab = Tab::new(start.clone(), start_id);
        let mut shell = Self {
            tabs: vec![initial_tab],
            active: 0,
            volumes: list_volumes(),
            fs,
            node_store,
            table,
            focus_handle,
            show_hidden,
            last_error: None,
            load_generation: 0,
            watcher,
            context_row: None,
            context_target: None,
            metadata_db: None,
            icons,
            tasks: Rc::new(RefCell::new(TaskRegistry::new())),
            task_panel_open: false,
            simulated_progress: None,
            filter_text: String::new(),
            filter_input,
            breadcrumb_editing: false,
            breadcrumb_input,
            shortcuts_help_filter: None,
            shortcuts_help_input,
            preview_visible: true,
            ui_scale,
            preview_cache: crate::preview::PreviewCache::new(),
            splitter_state: cx.new(|_| gpui_component::resizable::ResizableState::default()),
            sidebar_width: persisted.sidebar_width.unwrap_or(220.0).clamp(160.0, 400.0),
            preview_width: persisted.preview_width.unwrap_or(280.0).clamp(220.0, 520.0),
            splitter_last_save: None,
            expanded: HashSet::new(),
            tree_children: HashMap::new(),
            ant_visits: HashMap::new(),
            ant_max: 0,
            _subscriptions: vec![
                filter_subscription,
                breadcrumb_subscription,
                shortcuts_help_subscription,
            ],
        };
        shell.start_metadata_load(cx);
        shell.load_path(start, cx);
        shell
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
    pub fn path_for_row(&self, row_ix: usize, cx: &App) -> Option<PathBuf> {
        let entry = self.table.read(cx).delegate().entries.get(row_ix)?.clone();
        self.node_store
            .path_snapshot_for_job(entry.id, "Shell::path_for_row")
            .or_else(|| self.table.read(cx).delegate().path_for_entry(entry.id))
            .or_else(|| {
                let mut p = self.active_tab().current_dir.clone();
                p.push(&entry.name);
                Some(p)
            })
    }

    fn on_copy_path(&mut self, _: &CopyPath, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.target_row() else { return };
        let Some(path) = self.path_for_row(row, cx) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(
            path.to_string_lossy().into_owned(),
        ));
    }

    fn on_reveal_in_finder(&mut self, _: &RevealInFinder, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.target_row() else { return };
        let Some(path) = self.path_for_row(row, cx) else {
            return;
        };
        // `open -R <path>` is the macOS canonical "reveal in
        // Finder". On other platforms this no-ops.
        let _ = std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(&path)
            .spawn();
    }

    // -- Phase 6 (next-level) ----------------------------------------
    // Path-aware context-menu handlers. Each consumes
    // `context_target` (set by the right-click closure) so the next
    // keyboard action falls back to the file-row selection.

    fn on_reveal_context_path(
        &mut self,
        _: &RevealContextPath,
        _: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let Some(path) = self.context_target.take() else { return };
        let _ = std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(&path)
            .spawn();
    }

    fn on_copy_context_path(
        &mut self,
        _: &CopyContextPath,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.context_target.take() else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(
            path.to_string_lossy().into_owned(),
        ));
    }

    fn on_open_context_in_new_tab(
        &mut self,
        _: &OpenContextInNewTab,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.context_target.take() else { return };
        let id = self.fs.id_for_path(&path);
        self.node_store
            .get_or_create_path_with_id(path.clone(), id);
        let tab = Tab::new(path, id);
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        let active_id = self.fs.id_for_path(&self.active_tab().current_dir);
        self.navigate_node(active_id, cx);
    }

    fn on_new_folder_here(
        &mut self,
        _: &NewFolderHere,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Mirrors the existing NewFolder action but pins the parent
        // to `context_target` rather than the current tab's dir.
        // When no target is set (e.g. dispatched via menu without
        // right-click priming), fall through to the regular handler.
        if let Some(target) = self.context_target.take() {
            let saved = self.active_tab().current_dir.clone();
            self.active_tab_mut().current_dir = target;
            self.on_new_folder(&NewFolder, window, cx);
            self.active_tab_mut().current_dir = saved;
        } else {
            self.on_new_folder(&NewFolder, window, cx);
        }
    }

    // -- Phase 6 follow-on: Tags submenu --------------------------

    fn toggle_tag_on_target(
        &mut self,
        color: feraille_core::commands::TagColor,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.target_row() else { return };
        let Some(path) = self.path_for_row(row, cx) else { return };
        let _ = feraille_shell_mac::toggle_tag(&path, color);
    }

    fn on_toggle_tag_red(&mut self, _: &ToggleTagRed, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Red, cx);
    }
    fn on_toggle_tag_orange(
        &mut self,
        _: &ToggleTagOrange,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Orange, cx);
    }
    fn on_toggle_tag_yellow(
        &mut self,
        _: &ToggleTagYellow,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Yellow, cx);
    }
    fn on_toggle_tag_green(
        &mut self,
        _: &ToggleTagGreen,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Green, cx);
    }
    fn on_toggle_tag_blue(&mut self, _: &ToggleTagBlue, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Blue, cx);
    }
    fn on_toggle_tag_purple(
        &mut self,
        _: &ToggleTagPurple,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Purple, cx);
    }
    fn on_toggle_tag_gray(&mut self, _: &ToggleTagGray, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_tag_on_target(feraille_core::commands::TagColor::Gray, cx);
    }

    // -- Phase 6 follow-on: Open With submenu ---------------------

    fn open_with_slot(&mut self, slot: usize, cx: &mut Context<Self>) {
        let Some(row) = self.target_row() else { return };
        let Some(path) = self.path_for_row(row, cx) else { return };
        let candidates = feraille_shell_mac::open_with_candidates(&path);
        if let Some(c) = candidates.get(slot) {
            let _ = feraille_shell_mac::open_with_app(&path, &c.path);
        }
    }

    fn on_open_with_slot_0(
        &mut self,
        _: &OpenWithSlot0,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(0, cx);
    }
    fn on_open_with_slot_1(
        &mut self,
        _: &OpenWithSlot1,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(1, cx);
    }
    fn on_open_with_slot_2(
        &mut self,
        _: &OpenWithSlot2,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(2, cx);
    }
    fn on_open_with_slot_3(
        &mut self,
        _: &OpenWithSlot3,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(3, cx);
    }
    fn on_open_with_slot_4(
        &mut self,
        _: &OpenWithSlot4,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(4, cx);
    }
    fn on_open_with_slot_5(
        &mut self,
        _: &OpenWithSlot5,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(5, cx);
    }
    fn on_open_with_slot_6(
        &mut self,
        _: &OpenWithSlot6,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(6, cx);
    }
    fn on_open_with_slot_7(
        &mut self,
        _: &OpenWithSlot7,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(7, cx);
    }
    fn on_open_with_slot_8(
        &mut self,
        _: &OpenWithSlot8,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(8, cx);
    }
    fn on_open_with_slot_9(
        &mut self,
        _: &OpenWithSlot9,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(9, cx);
    }
    fn on_open_with_slot_10(
        &mut self,
        _: &OpenWithSlot10,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(10, cx);
    }
    fn on_open_with_slot_11(
        &mut self,
        _: &OpenWithSlot11,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_with_slot(11, cx);
    }

    fn on_move_to_trash(&mut self, _: &MoveToTrash, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.target_row() else { return };
        let Some(path) = self.path_for_row(row, cx) else {
            return;
        };
        let cur = self.active_tab().current_dir.clone();
        self.spawn_file_op(
            cur,
            move || feraille_fs_native::move_to_trash(&path).map_err(|e| e.to_string()),
            "move-to-trash",
            cx,
        );
    }

    fn on_navigate_back(&mut self, _: &NavigateBack, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate_back(cx);
    }

    fn on_navigate_forward(&mut self, _: &NavigateForward, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate_forward(cx);
    }

    fn on_open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(idx) = self.target_row() {
            self.activate_row(idx, cx);
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
        self.filter_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
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
    fn on_new_folder(&mut self, _: &NewFolder, window: &mut Window, cx: &mut Context<Self>) {
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
                    let cur = parent.clone();
                    let op_path = path.clone();
                    shell.update(cx, move |this, cx| {
                        this.spawn_file_op(
                            cur,
                            move || std::fs::create_dir(&op_path).map_err(|e| e.to_string()),
                            "new-folder",
                            cx,
                        )
                    });
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
        let Some(old_path) = self.path_for_row(row, cx) else {
            return;
        };
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
                    let cur = parent.clone();
                    let op_old_path = old_path.clone();
                    let op_new_path = new_path.clone();
                    shell.update(cx, move |this, cx| {
                        this.spawn_file_op(
                            cur,
                            move || {
                                std::fs::rename(&op_old_path, &op_new_path)
                                    .map_err(|e| e.to_string())
                            },
                            "rename",
                            cx,
                        )
                    });
                    true
                })
        });
    }

    fn on_clear_filter(&mut self, _: &ClearFilter, window: &mut Window, cx: &mut Context<Self>) {
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
    fn on_quick_look(&mut self, _: &QuickLook, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.target_row() else { return };
        let Some(path) = self.path_for_row(row, cx) else {
            return;
        };
        let _ = feraille_shell_mac::show_quick_look(&[path.as_path()]);
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
        let fs = self.fs.clone();
        let tasks = self.tasks.clone();
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

    /// File-list keyboard navigation — up/down/home/end/pgup/pgdn.
    /// Bounds-clamped against `entries.len()`; no-op when the list
    /// is empty.
    fn move_selection(&mut self, delta: SelectionDelta, cx: &mut Context<Self>) {
        let len = self.table.read(cx).delegate().entries.len();
        if len == 0 {
            self.active_tab_mut().selected = None;
            return;
        }
        let page = 12usize;
        let cur = self.active_tab().selected.unwrap_or(0) as i64;
        let last = len as i64 - 1;
        let next: i64 = match delta {
            SelectionDelta::Up => cur - 1,
            SelectionDelta::Down => cur + 1,
            SelectionDelta::PageUp => cur - page as i64,
            SelectionDelta::PageDown => cur + page as i64,
            SelectionDelta::First => 0,
            SelectionDelta::Last => last,
        };
        let clamped = next.clamp(0, last) as usize;
        self.active_tab_mut().selected = Some(clamped);
        cx.notify();
    }

    fn on_cursor_up(&mut self, _: &CursorUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::Up, cx);
    }
    fn on_cursor_down(&mut self, _: &CursorDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::Down, cx);
    }
    fn on_cursor_first(&mut self, _: &CursorFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::First, cx);
    }
    fn on_cursor_last(&mut self, _: &CursorLast, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::Last, cx);
    }
    fn on_page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::PageUp, cx);
    }
    fn on_page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::PageDown, cx);
    }

    /// Cmd+P — toggle preview-pane visibility. The pane defaults to
    /// shown; toggling off gives the file list the full content
    /// width.
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
    fn on_open_in_new_tab(&mut self, _: &OpenInNewTab, _: &mut Window, cx: &mut Context<Self>) {
        let path = match self.target_row().and_then(|r| self.path_for_row(r, cx)) {
            Some(p) => p,
            None => self.active_tab().current_dir.clone(),
        };
        let id = self.fs.id_for_path(&path);
        self.node_store.get_or_create_path_with_id(path.clone(), id);
        self.tabs.push(Tab::new(path, id));
        self.active = self.tabs.len() - 1;
        let cur = self.active_tab().current_dir.clone();
        self.load_path(cur, cx);
    }

    /// Right-click → Duplicate. Calls
    /// `feraille_shell_mac::duplicate_path` (NSWorkspace duplicate on
    /// macOS, std::fs::copy fallback elsewhere). The watcher picks
    /// up the new file; we also force-reload for snappiness.
    fn on_duplicate(&mut self, _: &Duplicate, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.target_row() else { return };
        let Some(path) = self.path_for_row(row, cx) else {
            return;
        };
        let cur = self.active_tab().current_dir.clone();
        self.spawn_file_op(
            cur,
            move || feraille_shell_mac::duplicate_path(&path).map(|_| ()),
            "duplicate",
            cx,
        );
    }

    /// Right-click → Make Alias. Creates a Finder alias next to the
    /// source.
    fn on_make_alias(&mut self, _: &MakeAlias, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.target_row() else { return };
        let Some(path) = self.path_for_row(row, cx) else {
            return;
        };
        let cur = self.active_tab().current_dir.clone();
        self.spawn_file_op(
            cur,
            move || feraille_shell_mac::make_alias(&path).map(|_| ()),
            "make-alias",
            cx,
        );
    }

    /// Right-click → Compress. Zips the selected path (or a list,
    /// when multi-select lands). The archive lands next to the
    /// source with a `.zip` suffix.
    fn on_compress(&mut self, _: &Compress, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.target_row() else { return };
        let Some(path) = self.path_for_row(row, cx) else {
            return;
        };
        let cur = self.active_tab().current_dir.clone();
        self.spawn_file_op(
            cur,
            move || {
                let targets: Vec<&std::path::Path> = vec![path.as_path()];
                feraille_shell_mac::compress_paths(&targets).map(|_| ())
            },
            "compress",
            cx,
        );
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
    /// is only mutated by `navigate`). Public so the screenshot
    /// CLI driver can call it directly after seeding tab state.
    pub fn load_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let node_id = self.fs.id_for_path(&path);
        self.node_store
            .get_or_create_path_with_id(path.clone(), node_id);
        self.active_tab_mut().nav.replace_current(node_id);
        self.active_tab_mut().current_dir = path.clone();
        self.active_tab_mut().selected = None;
        self.last_error = None;
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        let show_hidden = self.show_hidden;
        let filter = self.filter_text.clone();
        let table = self.table.clone();

        table.update(cx, |state, cx| {
            state.delegate_mut().clear();
            state.refresh(cx);
        });

        // Point the watcher at the new directory. Errors (path
        // doesn't exist, watcher saturated) are non-fatal — the
        // user still gets the listing; they just lose live updates.
        if let Some(w) = self.watcher.borrow_mut().as_mut() {
            let _ = w.watch(&path);
        }
        self.save_state_async(cx);

        let fs = self.fs.clone();
        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            let path_for_worker = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { run_directory_load(fs, path_for_worker, show_hidden, filter) })
                .await;
            let Some(shell) = weak.upgrade() else { return };
            let _ = shell.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                this.apply_directory_load(result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_directory_load(&mut self, result: LoadResult, cx: &mut Context<Self>) {
        for (id, path) in &result.paths {
            self.node_store
                .get_or_create_path_with_id(path.clone(), *id);
        }
        let icon_seeds: Vec<(FileEntry, PathBuf)> = result
            .entries
            .iter()
            .filter_map(|entry| {
                result
                    .paths
                    .get(&entry.id)
                    .cloned()
                    .map(|path| (entry.clone(), path))
            })
            .collect();
        let heats: Vec<f32> = result
            .entries
            .iter()
            .map(|entry| self.ant_heat(entry.id))
            .collect();
        let table = self.table.clone();
        table.update(cx, |state, cx| {
            state
                .delegate_mut()
                .replace_entries(result.entries, result.paths, heats);
            state.refresh(cx);
        });
        self.last_error = result.error;

        // Stage 4: kick off magic + quarantine prefetch after the
        // foreground table state has received the new snapshot.
        let table = self.table.clone();
        let fs = self.fs.clone();
        let db = self.metadata_db.clone();
        let tasks = self.tasks.clone();
        let weak = cx.weak_entity();
        crate::prefetch::start(table, fs, db, tasks, weak, cx);
        self.start_icon_warm(icon_seeds, cx);
        cx.notify();
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
                        let mut icons = this.icons.borrow_mut();
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

    fn start_metadata_load(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let db = open_metadata_db();
                    let (ant_visits, ant_max) = hydrate_ant_trail(db.as_ref());
                    (db, ant_visits, ant_max)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let (db, ant_visits, ant_max) = loaded;
                this.metadata_db = db;
                this.ant_visits = ant_visits;
                this.ant_max = ant_max;
                let table = this.table.clone();
                table.update(cx, |state, cx| {
                    let delegate = state.delegate_mut();
                    let heats: Vec<f32> = delegate
                        .entries
                        .iter()
                        .map(|entry| this.ant_heat(entry.id))
                        .collect();
                    delegate.heats = heats;
                    state.refresh(cx);
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn spawn_file_op(
        &self,
        reload_path: PathBuf,
        op: impl FnOnce() -> Result<(), String> + Send + 'static,
        label: &'static str,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.weak_entity();
        cx.spawn(async move |_this, cx| {
            let result = cx.background_executor().spawn(async move { op() }).await;
            let Some(shell) = weak.upgrade() else { return };
            let _ = shell.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.load_path(reload_path, cx),
                    Err(e) => crate::log_warn!(90, "{label} failed: {e}"),
                }
            });
        })
        .detach();
    }

    pub fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        let path = self.active_tab().current_dir.clone();
        self.load_path(path, cx);
    }

    // ----- Tab management (5.5.d) ---------------------------------

    /// Cmd+T: open a new tab at the home directory and switch to it.
    fn on_new_tab(&mut self, _: &NewTab, _: &mut Window, cx: &mut Context<Self>) {
        let path = home_dir();
        let id = self.fs.id_for_path(&path);
        self.node_store.get_or_create_path_with_id(path.clone(), id);
        self.tabs.push(Tab::new(path, id));
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

    fn on_navigate_parent(&mut self, _: &NavigateParent, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate_parent(cx);
    }

    /// User activated a row (double-click or Enter). For directories
    /// we navigate into them; for files we hand off to the OS opener.
    pub fn activate_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        let path_and_kind = self.table.read(cx).delegate().entries.get(row_ix).map(|e| {
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

    /// Navigate to `path`: re-enumerate, refresh the Table, push to
    /// the active tab's history (truncating any forward stack first),
    /// reset selection. Also increments the Ant Trail visit count
    /// for `path` (Stage 9.b) and persists through metadata_db.
    pub fn navigate(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let node_id = self.fs.id_for_path(&path);
        self.node_store
            .get_or_create_path_with_id(path.clone(), node_id);
        let tab = self.active_tab_mut();
        if tab.history.get(tab.history_index) != Some(&path) {
            tab.history.truncate(tab.history_index + 1);
            tab.history.push(path.clone());
            tab.history_index = tab.history.len() - 1;
        }
        tab.nav.navigate_to(node_id);
        self.record_ant_visit(node_id, cx);
        self.load_path(path, cx);
    }

    pub fn navigate_node(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
        let Some(path) = self
            .node_store
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
            .node_store
            .path_snapshot_for_job(node_id, "Shell::record_ant_visit")
        else {
            return;
        };
        let entry = self.ant_visits.entry(path.clone()).or_insert(0);
        *entry += 1;
        if *entry > self.ant_max {
            self.ant_max = *entry;
        }
        self.node_store.set_heat(node_id, self.ant_heat(node_id));
        if let Some(db) = self.metadata_db.clone() {
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
        let cached = self.node_store.heat(node_id);
        if cached > 0.0 {
            return cached;
        }
        let Some(path) = self.node_store.path_for_action(node_id, "Shell::ant_heat") else {
            return 0.0;
        };
        let Some(&v) = self.ant_visits.get(path) else {
            return 0.0;
        };
        if self.ant_max <= 1 {
            return 1.0;
        }
        ((v as f32 + 1.0).log2() / (self.ant_max as f32 + 1.0).log2()).clamp(0.0, 1.0)
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
            .node_store
            .path_snapshot_for_job(node_id, "Shell::toggle_tree_expand_node")
        else {
            return;
        };
        self.toggle_tree_expand(&path, cx);
    }

    fn start_tree_children_load(&self, path: PathBuf, cx: &mut Context<Self>) {
        let fs = self.fs.clone();
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
                    this.node_store
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
                let node_id = self.fs.id_for_path(&p);
                self.node_store
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
    fn build_browse_rows(&mut self) -> Vec<TreeRowSpec> {
        let home = home_dir();
        let favorite_paths: HashSet<PathBuf> =
            FAVORITES.iter().map(|f| f.path()).collect();
        let current = self.active_tab().current_dir.clone();
        let node_id = self.fs.id_for_path(&home);
        self.node_store
            .get_or_create_path_with_id(home.clone(), node_id);
        let is_expanded = self.expanded.contains(&home);
        let mut rows: Vec<TreeRowSpec> = vec![TreeRowSpec {
            node_id,
            path: home.clone(),
            label: SharedString::from("Home"),
            depth: 0,
            is_expandable: true,
            is_expanded,
            is_active: home == current,
            capacity: None,
        }];
        if is_expanded {
            self.append_tree_descendants_filtered(
                &mut rows,
                &home,
                1,
                &current,
                Some(&favorite_paths),
            );
        }
        rows
    }

    /// Build the Favorites section: a flat `SidebarMenu` of icon-
    /// prefixed shortcuts. Each item navigates straight to its
    /// path; none expand, so the IA stays unambiguous next to the
    /// expandable Browse tree below.
    fn build_favorites_menu(&mut self, weak: WeakEntity<Self>) -> SidebarMenu {
        use gpui_component::Icon;
        let current = self.active_tab().current_dir.clone();
        let mut menu = SidebarMenu::new();
        for fav in FAVORITES {
            let path = fav.path();
            let node_id = self.fs.id_for_path(&path);
            self.node_store
                .get_or_create_path_with_id(path.clone(), node_id);
            let active = path == current;
            let weak_for_click = weak.clone();
            let weak_for_menu = weak.clone();
            let path_for_menu = path.clone();
            menu = menu.child(
                SidebarMenuItem::new(SharedString::from(fav.label))
                    .icon(Icon::empty().path(fav.icon))
                    .active(active)
                    .on_click(move |_window, _evt, cx| {
                        if let Some(s) = weak_for_click.upgrade() {
                            s.update(cx, |shell, cx| {
                                shell.navigate_node(node_id, cx);
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
                        menu.menu(
                            "Open in New Tab",
                            Box::new(OpenContextInNewTab),
                        )
                        .separator()
                        .menu(
                            "Reveal in Finder",
                            Box::new(RevealContextPath),
                        )
                        .menu("Copy Path", Box::new(CopyContextPath))
                    }),
            );
        }
        menu
    }

    /// Build the Volumes section as a flat row list. Same recursion
    /// shape as Locations, but the depth-0 volume row carries a
    /// `(total, available)` capacity so the renderer can draw a
    /// Finder-style capacity bar.
    fn build_volumes_rows(&mut self) -> Vec<TreeRowSpec> {
        let current = self.active_tab().current_dir.clone();
        let mut rows: Vec<TreeRowSpec> = Vec::new();
        for v in &self.volumes {
            let path = v.path.clone();
            let node_id = self.fs.id_for_path(&path);
            self.node_store
                .get_or_create_path_with_id(path.clone(), node_id);
            let is_expanded = self.expanded.contains(&path);
            let capacity = match (v.total_bytes, v.available_bytes) {
                (Some(t), Some(a)) if t > 0 => Some((t, a)),
                _ => None,
            };
            rows.push(TreeRowSpec {
                node_id,
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
    ///
    /// Thin wrapper that runs without a skip filter — used by
    /// Volumes and any deeper-than-depth-1 recursion in Browse.
    fn append_tree_descendants(
        &self,
        rows: &mut Vec<TreeRowSpec>,
        parent: &Path,
        depth: usize,
        current: &Path,
    ) {
        self.append_tree_descendants_filtered(rows, parent, depth, current, None);
    }

    /// Same as [`append_tree_descendants`] but with an optional
    /// `skip_paths` filter applied to direct children only. Used by
    /// Browse to suppress depth-1 Home children that are already
    /// pinned in Favorites. The filter is *not* propagated to deeper
    /// levels: once we're inside `Library/`, recursion ignores it so
    /// `Library/Application Support` still lists.
    fn append_tree_descendants_filtered(
        &self,
        rows: &mut Vec<TreeRowSpec>,
        parent: &Path,
        depth: usize,
        current: &Path,
        skip_paths: Option<&HashSet<PathBuf>>,
    ) {
        let Some(children) = self.tree_children.get(parent) else {
            return;
        };
        for child in children {
            if !self.show_hidden && child.label.starts_with('.') {
                continue;
            }
            // Filter is applied at this depth only — passes `None`
            // to the recursive call, so grandchildren are never
            // filtered. Keeps Browse complete past the depth-1
            // Favorites overlap.
            if let Some(skip) = skip_paths {
                if skip.contains(&child.path) {
                    continue;
                }
            }
            let is_expanded = self.expanded.contains(&child.path);
            rows.push(TreeRowSpec {
                node_id: child.node_id,
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
    fn file_pane_body(&self, cx: &mut Context<Self>) -> AnyElement {
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
                    let path = home_dir();
                    let id = this.fs.id_for_path(&path);
                    this.node_store.get_or_create_path_with_id(path.clone(), id);
                    this.tabs.push(Tab::new(path, id));
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
        let can_back = self.active_tab().history_index > 0;
        let can_forward =
            self.active_tab().history_index + 1 < self.active_tab().history.len();
        TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .pr_3()
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
                        .on_click(cx.listener(|this, _, _, cx| this.navigate_back(cx))),
                )
                .child(
                    Button::new("nav-forward")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/chevron-right.svg"))
                        .tooltip("Forward  \u{2318}\u{5D}")
                        .disabled(!can_forward)
                        .on_click(cx.listener(|this, _, _, cx| this.navigate_forward(cx))),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .flex_shrink_0()
                        .w(px(220.0))
                        .child(Input::new(&self.filter_input).small()),
                )
                .child(div().flex_1()),
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
                let thumb_state = self.preview_cache.get(&full_path);
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
                        Tooltip::new(SharedString::from(path_for_tooltip.clone()))
                            .build(window, cx)
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
                        DescriptionItem::new("Format")
                            .value(SharedString::from(format_label_text)),
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
                                "Reveal in Finder",
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
                .when(is_last, |this| this.font_weight(FontWeight::SEMIBOLD))
                .cursor_pointer()
                .hover(|this| this.bg(cx.theme().secondary))
                .child(label)
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
                    if let Some(s) = weak_for_crumb.upgrade() {
                        s.update(cx, |shell, _| {
                            shell.context_target = Some(path_for_menu.clone());
                        });
                    }
                    menu.menu("Open in New Tab", Box::new(OpenContextInNewTab))
                        .separator()
                        .menu("Reveal in Finder", Box::new(RevealContextPath))
                        .menu("Copy Path", Box::new(CopyContextPath))
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
        let weak = cx.weak_entity();
        let favorites_menu = self.build_favorites_menu(weak.clone());
        let browse_rows = self.build_browse_rows();
        let volumes_rows = self.build_volumes_rows();
        let has_volumes = !self.volumes.is_empty();
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
        // Phase 2 IA: Favorites = flat icon-prefixed shortcuts
        // (SidebarMenu primitive); Browse = single-rooted expandable
        // tree at Home; Volumes = expandable per-volume tree. Each
        // section is visually distinct (header style differs between
        // SidebarGroup labels and TreeSection headers) and Downloads
        // can no longer appear twice because Favorites items don't
        // expand.
        // Sidebar no longer carries the "Feraille" header — that moved
        // into the TitleBar at the top of the window (Phase 7).
        let mut sidebar = Sidebar::new("shell-sidebar")
            .collapsible(false)
            .w_full()
            .child(ShellSidebarItem::group(
                SidebarGroup::new("Favorites").child(favorites_menu),
            ))
            .child(ShellSidebarItem::tree(TreeSection::new(
                "Browse",
                browse_rows,
                weak.clone(),
                self.icons.clone(),
            )));
        if has_volumes {
            sidebar = sidebar.child(ShellSidebarItem::tree(TreeSection::new(
                "Volumes",
                volumes_rows,
                weak.clone(),
                self.icons.clone(),
            )));
        }

        let _ = path_str; // breadcrumb already shows the path

        let tabstrip = self.tabstrip(cx);
        let entry_count = self.table.read(cx).delegate().entries.len();
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
            entry_count,
            &self.tasks,
            self.simulated_progress,
            Some(toggle_task_panel),
            self.show_hidden,
            Some(toggle_hidden_cb),
            cx,
        );
        let task_panel = crate::task_panel::render_if_open(self.task_panel_open, &self.tasks, cx);

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
            .on_action(cx.listener(Self::on_edit_breadcrumb))
            .on_action(cx.listener(Self::on_shortcuts_help))
            .on_action(cx.listener(Self::on_open_disk_usage))
            .on_action(cx.listener(Self::on_cursor_up))
            .on_action(cx.listener(Self::on_cursor_down))
            .on_action(cx.listener(Self::on_cursor_first))
            .on_action(cx.listener(Self::on_cursor_last))
            .on_action(cx.listener(Self::on_page_up))
            .on_action(cx.listener(Self::on_page_down))
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
                let file_body_wrapped =
                    div().flex_1().min_h_0().min_w_0().child(file_body);
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
                let sidebar_width_px = px(self.sidebar_width);
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
                            .size_range(px(160.0)..px(400.0))
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
                v_flex()
                    .relative()
                    .size_full()
                    // Phase 7: TitleBar sits across the top with the
                    // app name + filter input + back/forward
                    // navigation. Replaces the sidebar-header brand
                    // mark and the toolbar's nav buttons + filter
                    // input.
                    .child(title_bar)
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

fn run_directory_load(
    fs: Arc<NativeFs>,
    path: PathBuf,
    show_hidden: bool,
    filter_text: String,
) -> LoadResult {
    let id = fs.id_for_path(&path);
    let handle = fs.enumerate(id);
    let needle = filter_text.trim().to_lowercase();
    let entries: Vec<FileEntry> = handle
        .initial
        .into_iter()
        .filter(|e| show_hidden || !e.name.starts_with('.'))
        .filter(|e| {
            if needle.is_empty() {
                true
            } else {
                // Filter searches the visible Format value too —
                // otherwise typing "pdf document" or "zip archive"
                // misses rows where the magic-detected text is the
                // only place those phrases appear.
                let (format, _) = e.format_label();
                e.name.to_lowercase().contains(&needle)
                    || format.to_lowercase().contains(&needle)
            }
        })
        .collect();
    let mut paths = HashMap::with_capacity(entries.len());
    for entry in &entries {
        if let Some(path) = fs.path_for(entry.id) {
            paths.insert(entry.id, path);
        }
    }
    LoadResult {
        entries,
        paths,
        error: handle.error,
    }
}

fn run_tree_children_load(fs: Arc<NativeFs>, path: PathBuf) -> Vec<TreeChild> {
    let mut children: Vec<TreeChild> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(path) {
        for dirent in rd.flatten() {
            let p = dirent.path();
            let Some(name) = p.file_name().and_then(|s| s.to_str()).map(str::to_owned) else {
                continue;
            };
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
            let node_id = fs.id_for_path(&p);
            children.push(TreeChild {
                node_id,
                path: p,
                label: name,
            });
        }
        children.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    }
    children
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
        EnumerationError::Other(msg) => ("Couldn't open this folder", msg.clone()),
    }
}

/// Middle-truncate a path so the basename stays visible but the
/// middle is collapsed to an ellipsis. Useful in the preview pane
/// where the full path would otherwise blow out the column width.
/// Falls back to a tail-truncation when the basename alone exceeds
/// `max`. Char-based length counting (handles non-ASCII path
/// components); byte indexing only ever lands on `/` which is ASCII.
fn middle_truncate_path(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let basename_start = s.rfind('/').map(|i| i + 1).unwrap_or(0);
    let basename: Vec<char> = s[basename_start..].chars().collect();
    if basename.len() + 3 >= max {
        let take = max.saturating_sub(1);
        let start = basename.len().saturating_sub(take);
        let tail: String = basename[start..].iter().collect();
        return format!("\u{2026}{}", tail);
    }
    let prefix_budget = max - basename.len() - 2;
    let prefix: String = chars[..prefix_budget].iter().collect();
    let bn: String = basename.iter().collect();
    format!("{}\u{2026}/{}", prefix, bn)
}

#[cfg(test)]
mod middle_truncate_tests {
    use super::middle_truncate_path;

    #[test]
    fn short_path_unchanged() {
        assert_eq!(middle_truncate_path("/Users/x/file.txt", 40), "/Users/x/file.txt");
    }

    #[test]
    fn long_path_keeps_basename() {
        let out = middle_truncate_path(
            "/Users/jkn/Library/Application Support/Feraille/file.txt",
            30,
        );
        assert!(out.ends_with("/file.txt"), "basename preserved: {out}");
        assert!(out.contains('\u{2026}'), "ellipsis inserted: {out}");
    }

    #[test]
    fn very_long_basename_tail_truncates() {
        let s = "/x/this-is-an-absurdly-long-filename-that-blows-past-the-limit.txt";
        let out = middle_truncate_path(s, 20);
        assert!(out.starts_with('\u{2026}'), "leading ellipsis: {out}");
        assert!(out.len() <= 25, "approx max width respected: {out}");
    }
}

/// Parse a user-typed breadcrumb-input string into a real path:
/// expands a leading `~` to `$HOME`. It deliberately does not
/// canonicalise or stat the path on the UI thread; navigation's
/// background enumeration reports errors.
pub fn parse_breadcrumb_path(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    let expanded = if let Some(rest) = trimmed.strip_prefix('~') {
        let mut h = home_dir();
        let suffix = rest.trim_start_matches('/');
        if !suffix.is_empty() {
            h.push(suffix);
        }
        h
    } else {
        PathBuf::from(trimmed)
    };
    expanded
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
