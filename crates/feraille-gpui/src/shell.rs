//! File-manager shell — main window content during Phases 4+.
//!
//! Phase 4.a: holds `current_dir`, renders a clickable breadcrumb at
//! the top of the main pane, sidebar entries are still placeholder.
//! Phase 4.b will wire the sidebar to real Locations/Volumes. Phase
//! 4.c brings the virtualized file list.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use feraille_core::{
    EntryKind, EnumerationError, FileEntry, NodeId, navigation::NavigationState,
    node_store::NodeStore,
};
use feraille_fs_native::{
    DEFAULT_ENUMERATION_BATCH, NativeFs, VolumeInfo, home_dir, list_volumes, open_with_default,
};
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
use crate::tasks::{TaskId, TaskKind, TaskRegistry};
use crate::tree::{ShellSidebarItem, TreeChild, TreeRowIcon, TreeRowSpec, TreeSection};

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
        // Spec §2.5 — Shift-extend variants. The plain `Cursor*` /
        // `Page*` set above move the lead and collapse the selection
        // to just that row; the extend variants move the lead and
        // make the selection the inclusive span from anchor to lead.
        CursorUpExtend,
        CursorDownExtend,
        CursorFirstExtend,
        CursorLastExtend,
        PageUpExtend,
        PageDownExtend,
        /// Cmd+A — selection becomes every row currently in the
        /// (filtered) model. anchor = first visible row, lead = last
        /// visible row. Spec §2.5.
        SelectAll,
        /// Esc on a non-empty selection — clear the selection set,
        /// anchor and lead. Higher-precedence Esc behaviors
        /// (close-shortcuts-overlay, ClearFilter) are still bound
        /// against the filter input's own focus context; this fires
        /// only when the shell pane itself owns focus.
        ClearSelection,
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
        /// Cmd+Z — pop the most recent reversible action off
        /// Shell::undo_stack and replay its inverse. Currently handles
        /// Rename (rename back) and NewFolder (delete the created
        /// folder); Move-to-Trash undo is documented in the deferred
        /// list (needs NSFileManager.trashItemAt to return the trash
        /// URL so we can move the file back).
        UndoLastAction,
        // Favorites (docs/features/FAVORITES.md). The toggle action
        // is the unified Cmd+D / menu-bar / row-context-menu entry
        // point: it adds the target if absent, removes it if present,
        // pulses the row on a dedup attempt (which can't happen via
        // toggle, but matches the spec's verb shape). The target is
        // either:
        //   - `Shell::favorites_context_path` (right-clicked source row,
        //     or right-clicked favorite row), if set
        //   - the file list selection (a folder), or
        //   - the active tab's current_dir as a last fallback.
        ToggleFavoriteForTarget,
        /// Append the active tab's current directory to Favorites.
        /// Backs the section-header `+` button and the menu bar item.
        AddCurrentFolderToFavorites,
        /// Section header click handler — also reachable via menu bar
        /// "Window → Toggle Favorites Section".
        ToggleFavoritesSection,
        // One-shot sorts (§4.5). Each rewrites every `sort_index` in
        // place; subsequent drags continue to work — the order isn't
        // "locked", it's just set.
        SortFavoritesByName,
        SortFavoritesByDateAddedNewest,
        SortFavoritesByDateAddedOldest,
        SortFavoritesByKind,
        /// Cmd+Option+Up — shift the most-recently-focused favorite
        /// up one slot in the section list (§4.4).
        MoveFavoriteUp,
        /// Cmd+Option+Down — shift down one slot.
        MoveFavoriteDown,
        /// Rename the favorite under `favorites_context_path` via a
        /// native NSAlert prompt (§6).
        RenameFavorite,
        /// Clear the favorite's custom display_name so it tracks the
        /// folder's on-disk basename again (§6 "Reset to Original Name").
        ResetFavoriteName,
        /// Strip a custom icon, falling back to kind+target default (§7).
        ResetFavoriteIcon,
        // Curated icon picks (§7 "Change Icon" submenu). Each sets
        // `custom_icon = Some(Lucide(subpath))` on the contextual
        // favorite, where the subpath references an asset under
        // `crates/feraille-gpui/resources/icons/` (e.g. "nav/star",
        // "file/code"). Six pre-curated picks; a full picker UI is a
        // future polish piece.
        SetFavoriteIconStar,
        SetFavoriteIconFolder,
        SetFavoriteIconCode,
        SetFavoriteIconImage,
        SetFavoriteIconMusic,
        SetFavoriteIconArchive,
    ]
);

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
            UndoOp::DeleteFolder(p) => {
                std::fs::remove_dir(p).map_err(|e| e.to_string())
            }
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
    /// Multi-selection set keyed by `NodeId`. Per spec §2.2 this is
    /// in-memory interaction state, not persisted truth; reconciled
    /// against the model on streaming `Done` and on every model
    /// change. Empty is a valid common state.
    pub selection: HashSet<NodeId>,
    /// Anchor — fixed end of a Shift-range; set on plain click or
    /// the first Cmd-click into an empty selection. (Spec §2.3.)
    pub anchor: Option<NodeId>,
    /// Lead / cursor — moving end of a range, target of keyboard
    /// navigation, focus-ring row. Mirrored to the underlying
    /// `TableState::selected_row` so the primitive's native focus
    /// overlay marks it.
    pub lead: Option<NodeId>,
}

impl Tab {
    pub fn new(at: PathBuf, node_id: NodeId) -> Self {
        Self {
            nav: NavigationState::new(node_id),
            current_dir: at.clone(),
            history: vec![at],
            history_index: 0,
            selection: HashSet::new(),
            anchor: None,
            lead: None,
        }
    }

    /// Row index of the lead within `entries`, or `None` if no lead
    /// is set or the lead is not currently in the view's model.
    /// Used by every site that previously read `Tab::selected` as a
    /// row index — preview pane, status bar, screenshot driver,
    /// activate_row's keyboard fallback.
    pub fn lead_row(&self, entries: &[FileEntry]) -> Option<usize> {
        let lead = self.lead?;
        entries.iter().position(|e| e.id == lead)
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

/// Phase 10: live System-Appearance follow. The macOS observer in
/// `feraille_shell_mac::start_system_theme_observer` runs on the main
/// thread but has no `&mut App` — it can't call `Theme::change` itself.
/// Instead it pushes the latest dark-mode bool here; Shell::render
/// consumes the pending value (if any) and calls `Theme::change`
/// before painting. Single-digit-millisecond lag at worst.
static SYSTEM_THEME_PENDING: std::sync::atomic::AtomicI8 =
    std::sync::atomic::AtomicI8::new(-1);

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
    /// Cooperative cancel flag for the active directory enumeration.
    /// Replaced on every navigation/filter/show-hidden reload.
    pub load_cancel: Option<Arc<AtomicBool>>,
    /// Task-registry row for the active directory enumeration.
    pub load_task: Option<TaskId>,
    /// True after navigation starts and before the first batch lands.
    /// While true, the old folder rows remain visible instead of
    /// flashing an empty table.
    pub load_pending_first_batch: bool,
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
    /// Path target for Favorites mutations
    /// (docs/features/FAVORITES.md). Set by every "Add to Favorites" /
    /// "Remove from Favorites" context-menu closure and by row-row drag
    /// handlers; consumed (`take()`-style) by
    /// `on_toggle_favorite_for_target`. Fallback chain when unset:
    /// file-list selection → active tab `current_dir`.
    pub favorites_context_path: Option<PathBuf>,
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
    /// User-curated favorites entity. Source of truth for the sidebar
    /// Favorites section, the §5 favorited-indicator badge across the
    /// file list / tree / breadcrumb, and all add/remove/reorder
    /// mutations. Persistence is delegated to `metadata_db`; this
    /// entity owns the in-memory list and the path → id lookup index.
    pub favorites: Entity<crate::favorites::Favorites>,
    /// Reversible-action history. Newest at the back. Capped at
    /// UNDO_STACK_CAP so the stack doesn't grow unbounded across a
    /// long session; older ops drop off the front silently.
    pub undo_stack: VecDeque<UndoOp>,
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

struct LoadBatch {
    entries: Vec<FileEntry>,
    paths: HashMap<NodeId, PathBuf>,
}

enum LoadMsg {
    Batch(LoadBatch),
    Done(Option<EnumerationError>),
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
/// **Locations** section. Flat — no expand/collapse, no descendants
/// — so a click navigates and that's it. The user-curated **Favorites**
/// section (separate, below Locations) is a runtime data structure;
/// this type covers only the fixed OS folders. Tree-style hierarchical
/// browsing lives in the separate Browse section below.
struct Location {
    label: &'static str,
    /// `home`-relative subpath (None ⇒ the home directory itself).
    sub: Option<&'static str>,
    /// Asset path for the row's prefix icon. Resolved via
    /// `FeraAssets` so both our local bundle and the upstream
    /// gpui-component pack work.
    icon: &'static str,
}

const LOCATIONS: &[Location] = &[
    Location {
        label: "Home",
        sub: None,
        icon: "icons/nav/home.svg",
    },
    Location {
        label: "Applications",
        sub: Some("Applications"),
        icon: "icons/nav/apps.svg",
    },
    Location {
        label: "Desktop",
        sub: Some("Desktop"),
        icon: "icons/nav/desktop.svg",
    },
    Location {
        label: "Documents",
        sub: Some("Documents"),
        icon: "icons/nav/documents.svg",
    },
    Location {
        label: "Downloads",
        sub: Some("Downloads"),
        icon: "icons/nav/downloads.svg",
    },
    Location {
        label: "Trash",
        sub: Some(".Trash"),
        icon: "icons/nav/trash.svg",
    },
    Location {
        label: "Movies",
        sub: Some("Movies"),
        icon: "icons/nav/movies.svg",
    },
    Location {
        label: "Music",
        sub: Some("Music"),
        icon: "icons/nav/music.svg",
    },
    Location {
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
                .col_movable(true)
                .col_resizable(true)
        });
        // Bridge Table events (selection + double-click) to the
        // Shell's own state. SelectRow runs through the modifier-
        // aware gesture dispatch (spec §2.4) so Cmd / Shift /
        // Cmd+Shift give the right anchor/lead/set update.
        // RightClickedRow obeys spec §2.4's "preserve selection if
        // row already in set; otherwise replace to just this row"
        // rule before the menu's target reads `context_row`.
        cx.subscribe_in(
            &table,
            window,
            |this, _table, event: &TableEvent, window, cx| match event {
                TableEvent::SelectRow(row_ix) => {
                    let modifiers = window.modifiers();
                    this.apply_row_click_gesture(*row_ix, modifiers, cx);
                }
                TableEvent::DoubleClickedRow(row_ix) => {
                    this.activate_row(*row_ix, cx);
                }
                TableEvent::RightClickedRow(row_ix) => {
                    this.context_row = *row_ix;
                    if let Some(r) = *row_ix {
                        this.apply_row_right_click(r, cx);
                    }
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
            load_cancel: None,
            load_task: None,
            load_pending_first_batch: false,
            watcher,
            context_row: None,
            context_target: None,
            favorites_context_path: None,
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
            sidebar_collapsed: persisted.sidebar_collapsed.unwrap_or(false),
            favorites_section_collapsed: false,
            favorites: cx.new(|_| crate::favorites::Favorites::new(None)),
            focused_favorite: None,
            undo_stack: VecDeque::new(),
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
        // §5.3 live-sync: every folder-rendering view observes the
        // Favorites entity through Shell, so a single `cx.notify()`
        // here re-renders the sidebar (FavoritesSection), the
        // breadcrumb (star indicator), and the title-bar header.
        // The file list reads its own delegate's `is_favorited`
        // parallel vec, which `load_path` recomputes from the same
        // entity, so it picks up the change on the next load — for
        // truly synchronous list updates we also push a refresh here.
        let fav_subscription = cx.observe(&shell.favorites, |this, _favs, cx| {
            this.refresh_file_list_favorited(cx);
            cx.notify();
        });
        shell._subscriptions.push(fav_subscription);

        shell.start_metadata_load(cx);
        shell.load_path(start, cx);
        shell
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
                .lead_row(&self.table.read(cx).delegate().entries)
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

    fn on_copy_path(&mut self, _: &CopyPath, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::notification::Notification;
        let Some(row) = self.target_row(cx) else { return };
        let Some(path) = self.path_for_row(row, cx) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(
            path.to_string_lossy().into_owned(),
        ));
        // Phase 9: quiet success toast so the user sees the
        // clipboard action acknowledged. Notification::success
        // autohides after a few seconds.
        window.push_notification(
            Notification::success("Path copied to clipboard"),
            cx,
        );
    }

    fn on_reveal_in_finder(
        &mut self,
        _: &RevealInFinder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        let Some(row) = self.target_row(cx) else { return };
        let Some(path) = self.path_for_row(row, cx) else {
            return;
        };
        // `open -R <path>` is the macOS canonical "reveal in
        // Finder". On other platforms this no-ops.
        let _ = std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(&path)
            .spawn();
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("item")
            .to_string();
        window.push_notification(
            Notification::info(format!("Showing \u{201C}{}\u{201D} in Finder", name)),
            cx,
        );
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
        let Some(row) = self.target_row(cx) else { return };
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
        let Some(row) = self.target_row(cx) else { return };
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

    fn on_move_to_trash(
        &mut self,
        _: &MoveToTrash,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        let Some(row) = self.target_row(cx) else { return };
        let Some(path) = self.path_for_row(row, cx) else {
            return;
        };
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("item")
            .to_string();
        let cur = self.active_tab().current_dir.clone();
        self.spawn_file_op(
            cur,
            move || feraille_fs_native::move_to_trash(&path).map_err(|e| e.to_string()),
            "move-to-trash",
            cx,
        );
        // Quiet "in flight" toast — the trash op is near-instant
        // on macOS, so by the time the user reads this the file
        // list has already refreshed.
        window.push_notification(
            Notification::info(format!("Moved \u{201C}{}\u{201D} to Trash", name)),
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
        if let Some(idx) = self.target_row(cx) {
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
                    let undo_path = path.clone();
                    shell.update(cx, move |this, cx| {
                        this.spawn_file_op(
                            cur,
                            move || std::fs::create_dir(&op_path).map_err(|e| e.to_string()),
                            "new-folder",
                            cx,
                        );
                        // Push the undo op optimistically — the file
                        // op runs async; if it fails the undo would
                        // be a no-op (the path doesn't exist).
                        this.push_undo(UndoOp::DeleteFolder(undo_path));
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
        let Some(row) = self.target_row(cx) else { return };
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
                    let undo_current = new_path.clone();
                    let undo_original = old_path.clone();
                    shell.update(cx, move |this, cx| {
                        this.spawn_file_op(
                            cur,
                            move || {
                                std::fs::rename(&op_old_path, &op_new_path)
                                    .map_err(|e| e.to_string())
                            },
                            "rename",
                            cx,
                        );
                        this.push_undo(UndoOp::Rename {
                            current: undo_current,
                            original: undo_original,
                        });
                    });
                    true
                })
        });
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
        let Some(row) = self.target_row(cx) else { return };
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

    // -- Selection model (spec §2) ---------------------------------
    //
    // Selection state lives on `Tab` as a HashSet<NodeId> + anchor +
    // lead. Every selection mutation routes through one of the
    // helpers in this block so the parallel render vecs in the
    // delegate (`selected_in_set`, `is_lead`) and the underlying
    // `TableState::selected_row` stay in lockstep. Read paths
    // (`target_row`, status bar, preview, screenshot driver) derive
    // a row index from the lead via `Tab::lead_row`.

    /// Apply a mouse-click gesture on a row, dispatching by
    /// modifiers per spec §2.4. The Table primitive has already
    /// stamped its own `selected_row = row_ix` before this fires
    /// (via `on_row_left_click → set_selected_row`); in every
    /// branch below the lead also lands on `row_ix`, so the
    /// primitive's focus overlay tracks the lead without our help.
    fn apply_row_click_gesture(
        &mut self,
        row_ix: usize,
        modifiers: gpui::Modifiers,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.node_id_at_row(row_ix, cx) else {
            return;
        };
        let cmd = modifiers.secondary();
        let shift = modifiers.shift;
        if shift && cmd {
            // Cmd+Shift+Click: additive range — union anchor→row
            // into the existing set, lead = row, anchor unchanged.
            self.range_select(id, /* additive */ true, cx);
        } else if shift {
            // Shift+Click: replacement range from anchor to row,
            // lead = row. If no anchor, treat as plain click.
            self.range_select(id, /* additive */ false, cx);
        } else if cmd {
            // Cmd+Click: toggle membership; lead = row; anchor =
            // row when set non-empty, cleared when it just emptied.
            self.toggle_select(id, cx);
        } else {
            // Plain click: replace selection to just this row.
            self.replace_select_one(id, cx);
        }
        // Preview always follows the lead — keeps the right pane
        // and the table in lockstep regardless of which gesture
        // ran. Same cost as the old single-click behavior.
        if let Some(p) = self.path_for_row(row_ix, cx) {
            crate::preview::request(self, p, cx);
        }
        cx.notify();
    }

    /// Spec §2.4: right-click on a selected row leaves the
    /// selection alone (so "operate on all 12 selected" works);
    /// right-click on an unselected row replaces selection to
    /// that single row before the menu opens. The menu's target
    /// reads `context_row` which is set by the caller before this
    /// runs.
    fn apply_row_right_click(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        let Some(id) = self.node_id_at_row(row_ix, cx) else {
            return;
        };
        if !self.active_tab().selection.contains(&id) {
            self.replace_select_one(id, cx);
            cx.notify();
        }
    }

    /// Plain-click semantics: selection = {id}, anchor = lead = id.
    fn replace_select_one(&mut self, id: NodeId, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.selection.clear();
        tab.selection.insert(id);
        tab.anchor = Some(id);
        tab.lead = Some(id);
        self.refresh_file_list_selection(cx);
    }

    /// Cmd+Click semantics: toggle `id` in the set. lead = id.
    /// Empty after toggle → anchor cleared; otherwise anchor = id.
    fn toggle_select(&mut self, id: NodeId, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        if !tab.selection.remove(&id) {
            tab.selection.insert(id);
        }
        tab.lead = Some(id);
        tab.anchor = if tab.selection.is_empty() {
            None
        } else {
            Some(id)
        };
        self.refresh_file_list_selection(cx);
    }

    /// Shift+Click and Cmd+Shift+Click: compute the inclusive
    /// span from anchor to `id` in visible (delegate) order; if
    /// `additive` (Cmd+Shift), union it into the existing set,
    /// otherwise replace. lead = id; anchor unchanged (or seeded
    /// to id when there was none, matching spec's "treat as plain
    /// click").
    fn range_select(&mut self, id: NodeId, additive: bool, cx: &mut Context<Self>) {
        let entries: Vec<NodeId> = self
            .table
            .read(cx)
            .delegate()
            .entries
            .iter()
            .map(|e| e.id)
            .collect();
        let Some(target_idx) = entries.iter().position(|x| *x == id) else {
            return;
        };
        let anchor_id = self.active_tab().anchor;
        let anchor_idx = anchor_id.and_then(|a| entries.iter().position(|x| *x == a));
        match anchor_idx {
            None => {
                // No anchor → treat as plain click (spec §2.4).
                self.replace_select_one(id, cx);
            }
            Some(a_idx) => {
                let (lo, hi) = if a_idx <= target_idx {
                    (a_idx, target_idx)
                } else {
                    (target_idx, a_idx)
                };
                let span: HashSet<NodeId> = entries[lo..=hi].iter().copied().collect();
                let tab = self.active_tab_mut();
                if additive {
                    tab.selection.extend(span);
                } else {
                    tab.selection = span;
                }
                tab.lead = Some(id);
                // Anchor unchanged.
                self.refresh_file_list_selection(cx);
            }
        }
    }

    /// Spec §2.5: Cmd+A — select every row currently in the
    /// (filtered) model. anchor = first visible, lead = last.
    fn select_all_visible(&mut self, cx: &mut Context<Self>) {
        let (all, first, last): (HashSet<NodeId>, Option<NodeId>, Option<NodeId>) = {
            let delegate = self.table.read(cx).delegate();
            let ids: Vec<NodeId> = delegate.entries.iter().map(|e| e.id).collect();
            let first = ids.first().copied();
            let last = ids.last().copied();
            (ids.into_iter().collect(), first, last)
        };
        let tab = self.active_tab_mut();
        tab.selection = all;
        tab.anchor = first;
        tab.lead = last;
        self.refresh_file_list_selection(cx);
        cx.notify();
    }

    /// Spec §2.5 Esc: clear selection, anchor, lead.
    pub fn clear_active_selection(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.selection.clear();
        tab.anchor = None;
        tab.lead = None;
        self.refresh_file_list_selection(cx);
        cx.notify();
    }

    /// Rebuild the delegate's per-row `selected_in_set` + `is_lead`
    /// parallel vecs from the active tab's selection state, and
    /// mirror the lead's row index into the underlying
    /// `TableState::selected_row` so the primitive's focus overlay
    /// matches the keyboard cursor. Called after every selection
    /// mutation, after every streaming batch, and on `Done`.
    pub fn refresh_file_list_selection(&mut self, cx: &mut Context<Self>) {
        // Snapshot the active tab's selection state so the
        // table.update closure doesn't need to borrow Shell again.
        let selection = self.active_tab().selection.clone();
        let lead = self.active_tab().lead;
        let table = self.table.clone();
        let lead_row = table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            delegate
                .selected_in_set
                .resize(delegate.entries.len(), false);
            delegate.is_lead.resize(delegate.entries.len(), false);
            let mut lead_row: Option<usize> = None;
            for (i, entry) in delegate.entries.iter().enumerate() {
                let in_set = selection.contains(&entry.id);
                delegate.selected_in_set[i] = in_set;
                let is_lead = Some(entry.id) == lead;
                delegate.is_lead[i] = is_lead;
                if is_lead {
                    lead_row = Some(i);
                }
            }
            state.refresh(cx);
            lead_row
        });
        if let Some(row) = lead_row {
            table.update(cx, |state, cx| {
                state.set_selected_row(row, cx);
            });
        }
    }

    /// Resolve the NodeId at a given row index by reading the
    /// delegate's `entries`. Cheap (one indexed access). Returns
    /// `None` if the row index is out of bounds — possible if the
    /// model changed between an event being queued and dispatched.
    fn node_id_at_row(&self, row_ix: usize, cx: &App) -> Option<NodeId> {
        self.table
            .read(cx)
            .delegate()
            .entries
            .get(row_ix)
            .map(|e| e.id)
    }

    /// Keyboard navigation: move the lead by `delta` and, when
    /// `extend` is true (Shift-extend variants), make the
    /// selection the inclusive span from anchor to the new lead.
    /// Plain moves replace the selection with just the new lead
    /// (spec §2.5).
    fn move_selection(
        &mut self,
        delta: SelectionDelta,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        let entries: Vec<NodeId> = self
            .table
            .read(cx)
            .delegate()
            .entries
            .iter()
            .map(|e| e.id)
            .collect();
        let len = entries.len();
        if len == 0 {
            self.clear_active_selection(cx);
            return;
        }
        let page = 12i64;
        let last = len as i64 - 1;
        let cur_idx: i64 = self
            .active_tab()
            .lead
            .and_then(|id| entries.iter().position(|x| *x == id))
            .map(|i| i as i64)
            .unwrap_or(0);
        let next: i64 = match delta {
            SelectionDelta::Up => cur_idx - 1,
            SelectionDelta::Down => cur_idx + 1,
            SelectionDelta::PageUp => cur_idx - page,
            SelectionDelta::PageDown => cur_idx + page,
            SelectionDelta::First => 0,
            SelectionDelta::Last => last,
        };
        let clamped = next.clamp(0, last) as usize;
        let new_lead = entries[clamped];
        if extend {
            // Range-extend keeps anchor fixed; if there was no
            // anchor, seed it at the previous lead (or the new
            // one, which collapses to plain navigation).
            let tab = self.active_tab_mut();
            if tab.anchor.is_none() {
                tab.anchor = tab.lead.or(Some(new_lead));
            }
            let anchor_id = tab.anchor.unwrap_or(new_lead);
            let anchor_idx = entries
                .iter()
                .position(|x| *x == anchor_id)
                .unwrap_or(clamped);
            let (lo, hi) = if anchor_idx <= clamped {
                (anchor_idx, clamped)
            } else {
                (clamped, anchor_idx)
            };
            tab.selection = entries[lo..=hi].iter().copied().collect();
            tab.lead = Some(new_lead);
        } else {
            // Plain navigation collapses selection.
            self.replace_select_one(new_lead, cx);
            return;
        }
        self.refresh_file_list_selection(cx);
        // Preview pane follows the lead.
        if let Some(p) = self.path_for_row(clamped, cx) {
            crate::preview::request(self, p, cx);
        }
        cx.notify();
    }

    fn on_cursor_up(&mut self, _: &CursorUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::Up, false, cx);
    }
    fn on_cursor_down(&mut self, _: &CursorDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::Down, false, cx);
    }
    fn on_cursor_first(&mut self, _: &CursorFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::First, false, cx);
    }
    fn on_cursor_last(&mut self, _: &CursorLast, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::Last, false, cx);
    }
    fn on_page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::PageUp, false, cx);
    }
    fn on_page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::PageDown, false, cx);
    }

    fn on_cursor_up_extend(
        &mut self,
        _: &CursorUpExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::Up, true, cx);
    }
    fn on_cursor_down_extend(
        &mut self,
        _: &CursorDownExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::Down, true, cx);
    }
    fn on_cursor_first_extend(
        &mut self,
        _: &CursorFirstExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::First, true, cx);
    }
    fn on_cursor_last_extend(
        &mut self,
        _: &CursorLastExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::Last, true, cx);
    }
    fn on_page_up_extend(
        &mut self,
        _: &PageUpExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::PageUp, true, cx);
    }
    fn on_page_down_extend(
        &mut self,
        _: &PageDownExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::PageDown, true, cx);
    }

    fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_all_visible(cx);
    }

    fn on_clear_selection(
        &mut self,
        _: &ClearSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_active_selection(cx);
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
        let path = match self.target_row(cx).and_then(|r| self.path_for_row(r, cx)) {
            Some(p) => p,
            None => self.active_tab().current_dir.clone(),
        };
        self.open_path_in_new_tab(path, cx);
    }

    /// Push a new tab at `path` and switch to it. Shared entry point
    /// for modifier-click in the file list / sidebar / Favorites
    /// section so each surface doesn't reimplement the tab push.
    pub fn open_path_in_new_tab(&mut self, path: PathBuf, cx: &mut Context<Self>) {
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
        let Some(row) = self.target_row(cx) else { return };
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
        let Some(row) = self.target_row(cx) else { return };
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
        let Some(row) = self.target_row(cx) else { return };
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
        // Spec §2.6 navigation: the new path starts with empty
        // selection by default. Back/forward history will restore
        // a prior snapshot in iter 2 once tab history carries one;
        // iter 1 always starts empty.
        {
            let tab = self.active_tab_mut();
            tab.selection.clear();
            tab.anchor = None;
            tab.lead = None;
        }
        self.last_error = None;
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        let show_hidden = self.show_hidden;
        let filter = self.filter_text.clone();

        if let Some(cancel) = self.load_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let task = self.tasks.borrow_mut().begin(
            TaskKind::Enumeration,
            format!("Reading {}", middle_truncate_path(&path.to_string_lossy(), 40)),
            true,
        );
        if let Some(previous) = self.load_task.replace(task) {
            self.tasks.borrow_mut().end(previous);
        }
        self.load_pending_first_batch = true;

        // Point the watcher at the new directory. Errors (path
        // doesn't exist, watcher saturated) are non-fatal — the
        // user still gets the listing; they just lose live updates.
        if let Some(w) = self.watcher.borrow_mut().as_mut() {
            let _ = w.watch(&path);
        }
        self.save_state_async(cx);

        let fs = self.fs.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.load_cancel = Some(cancel.clone());
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
                        if this.load_generation != generation
                            || this.active_tab().current_dir != path
                        {
                            return true;
                        }
                        this.apply_directory_load_msg(msg, cx);
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
            self.node_store
                .get_or_create_path_with_id(path.clone(), *id);
        }
        let heats: Vec<f32> = batch
            .entries
            .iter()
            .map(|entry| self.ant_heat(entry.id))
            .collect();
        let first_batch = self.load_pending_first_batch;
        self.load_pending_first_batch = false;
        let table = self.table.clone();
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
        cx.notify();
    }

    /// Recompute the file list's per-row `is_favorited` parallel vec
    /// from the current Favorites entity index. Called from:
    /// - `apply_directory_batch` (after rows arrive)
    /// - The `cx.observe(&self.favorites, …)` subscription registered
    ///   in `Shell::new` (so add / remove / repoint repaints star
    ///   badges in the same frame, §5.3).
    pub fn refresh_file_list_favorited(&mut self, cx: &mut Context<Self>) {
        let favs = self.favorites.clone();
        let table = self.table.clone();
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

    fn finish_directory_load(
        &mut self,
        error: Option<EnumerationError>,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.load_task.take() {
            self.tasks.borrow_mut().end(id);
        }
        self.load_cancel = None;
        if self.load_pending_first_batch {
            self.load_pending_first_batch = false;
            let table = self.table.clone();
            table.update(cx, |state, cx| {
                state.delegate_mut().clear();
                state.refresh(cx);
            });
        }
        let row_count = self.table.read(cx).delegate().entries.len();
        if row_count == 0 {
            self.last_error = error;
        } else {
            if let Some(err) = error {
                crate::log_warn!(90, "directory load ended with partial rows: {err:?}");
            }
            self.last_error = None;
        }

        // Stage 4: kick off magic + quarantine prefetch after the
        // foreground table state has received the final snapshot.
        let table = self.table.clone();
        let fs = self.fs.clone();
        let db = self.metadata_db.clone();
        let tasks = self.tasks.clone();
        let weak = cx.weak_entity();
        crate::prefetch::start(table, fs, db, tasks, weak, cx);
        let icon_seeds = self.icon_seeds_from_table(cx);
        self.start_icon_warm(icon_seeds, cx);
        cx.notify();
    }

    fn icon_seeds_from_table(&self, cx: &App) -> Vec<(FileEntry, PathBuf)> {
        let table = self.table.read(cx);
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
                this.metadata_db = db.clone();
                this.ant_visits = ant_visits;
                this.ant_max = ant_max;
                this.favorites_section_collapsed = favs_collapsed;
                // Attach the writable DB to the favorites entity and
                // hydrate. The dev seed runs only when the entry list
                // is empty AND `FERAILLE_DEV_SEED_FAVORITES=1` — see
                // `crate::favorites::maybe_seed_dev_favorites`.
                let fav_entity = this.favorites.clone();
                fav_entity.update(cx, |f, cx| {
                    if let Some(d) = db.clone() {
                        f.attach_db(d);
                    }
                    f.hydrate(favorites, cx);
                    crate::favorites::maybe_seed_dev_favorites(f, cx);
                });
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

    /// Append a reversible op to the undo stack, evicting the oldest
    /// entry when capacity is exceeded.
    fn push_undo(&mut self, op: UndoOp) {
        if self.undo_stack.len() >= UNDO_STACK_CAP {
            self.undo_stack.pop_front();
        }
        self.undo_stack.push_back(op);
    }

    pub fn on_undo_last_action(
        &mut self,
        _: &UndoLastAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::notification::Notification;
        let Some(op) = self.undo_stack.pop_back() else {
            window.push_notification(Notification::info("Nothing to undo"), cx);
            return;
        };
        let label = op.label();
        match op {
            UndoOp::AddFavorite(id) => {
                self.favorites.update(cx, |f, cx| {
                    f.remove(id, cx);
                });
                window.push_notification(Notification::success(label.to_string()), cx);
            }
            UndoOp::RemoveFavorite(fav) => {
                self.favorites.update(cx, |f, cx| {
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
                    window.push_notification(
                        Notification::error(format!("Undo failed: {e}")),
                        cx,
                    );
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
                let already = self.favorites.read(cx).contains_path(&canonical);
                let favs = self.favorites.clone();
                if already {
                    let id = self
                        .favorites
                        .read(cx)
                        .id_for_path(&canonical)
                        .expect("contains_path returned true");
                    let label = self
                        .favorites
                        .read(cx)
                        .entry_by_id(id)
                        .map(|f| f.effective_label())
                        .unwrap_or_else(|| "favorite".to_string());
                    // Capture the full entry before removal so the undo
                    // restores name + icon + sort_index + date_added.
                    let removed_for_undo = self
                        .favorites
                        .read(cx)
                        .entry_by_id(id)
                        .cloned();
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
                        Notification::success(format!("Added \u{201C}{label}\u{201D} to Favorites")),
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
        self.favorites.update(cx, |f, cx| {
            f.one_shot_sort(feraille_core::favorites::FavoriteSort::NameAsc, cx);
        });
    }

    pub fn on_sort_favorites_by_date_added_newest(
        &mut self,
        _: &SortFavoritesByDateAddedNewest,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.favorites.update(cx, |f, cx| {
            f.one_shot_sort(
                feraille_core::favorites::FavoriteSort::DateAddedNewest,
                cx,
            );
        });
    }

    pub fn on_sort_favorites_by_date_added_oldest(
        &mut self,
        _: &SortFavoritesByDateAddedOldest,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.favorites.update(cx, |f, cx| {
            f.one_shot_sort(
                feraille_core::favorites::FavoriteSort::DateAddedOldest,
                cx,
            );
        });
    }

    pub fn on_sort_favorites_by_kind(
        &mut self,
        _: &SortFavoritesByKind,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.favorites.update(cx, |f, cx| {
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
            self.favorites.update(cx, |f, cx| f.shift(id, -1, cx));
        }
    }

    pub fn on_move_favorite_down(
        &mut self,
        _: &MoveFavoriteDown,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.focused_favorite {
            self.favorites.update(cx, |f, cx| f.shift(id, 1, cx));
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
        self.favorites.read(cx).id_for_path(&canonical)
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
            .favorites
            .read(cx)
            .entry_by_id(id)
            .map(|f| f.effective_label())
            .unwrap_or_default();
        // Native NSAlert prompt — keeps the rename path simple and
        // matches macOS feel. Renaming the shortcut, not the folder.
        let next = feraille_shell_mac::prompt_for_text(
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
        self.favorites
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
        self.favorites.update(cx, |f, cx| f.rename(id, None, cx));
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
        self.favorites.update(cx, |f, cx| f.set_icon(id, None, cx));
    }

    fn set_favorite_lucide(&mut self, name: &'static str, cx: &mut Context<Self>) {
        let Some(id) = self.pop_favorite_id_for_action(cx) else {
            return;
        };
        let icon = feraille_core::favorites::FavoriteIcon::Lucide(name.into());
        self.favorites
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
            |path: &Path, this: &Self| this.favorites.read(cx).contains_path(path);
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
    fn build_browse_rows(&mut self, cx: &App) -> Vec<TreeRowSpec> {
        let home = home_dir();
        // Hide Locations from the Browse tree so the same depth-1 entry
        // (Documents, Downloads, etc.) doesn't appear twice. User-curated
        // Favorites are *not* hidden — those are intentional shortcuts.
        let location_paths: HashSet<PathBuf> =
            LOCATIONS.iter().map(|f| f.path()).collect();
        let current = self.active_tab().current_dir.clone();
        let node_id = self.fs.id_for_path(&home);
        self.node_store
            .get_or_create_path_with_id(home.clone(), node_id);
        let is_expanded = self.expanded.contains(&home);
        let favorited = self.favorites.read(cx).contains_path(&home);
        {
            let f = self.favorites.read(cx);
            eprintln!(
                "build_browse_rows: home={:?} entries={} favorited={}",
                home,
                f.entries().len(),
                favorited
            );
            for e in f.entries() {
                if let feraille_core::favorites::FavoriteTarget::Path(p) = &e.target {
                    eprintln!("  fav target: {:?}  eq_home={}", p, p == &home);
                }
            }
        }
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
        let entries = self.favorites.read(cx).entries().to_vec();
        ShellSidebarItem::favorites(crate::favorites_section::FavoritesSection::new(
            entries,
            self.favorites_section_collapsed,
            weak,
            self.icons.clone(),
        ))
    }

    /// Flip the Favorites section's disclosure-triangle and persist
    /// the new state. Called from the section header click handler.
    pub fn toggle_favorites_section_collapsed(&mut self, cx: &mut Context<Self>) {
        self.favorites_section_collapsed = !self.favorites_section_collapsed;
        let collapsed = self.favorites_section_collapsed;
        if let Some(db) = self.metadata_db.clone() {
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
        let favs = self.favorites.read(cx);
        for loc in LOCATIONS {
            let path = loc.path();
            let node_id = self.fs.id_for_path(&path);
            self.node_store
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
                .on_click(move |event, _window, cx| {
                    if let Some(s) = weak_for_click.upgrade() {
                        let modifiers = event.modifiers();
                        let path = path_for_modclick.clone();
                        s.update(cx, |shell, cx| {
                            if modifiers.platform {
                                shell.open_path_in_new_tab(path, cx);
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
                        .menu("Reveal in Finder", Box::new(RevealContextPath))
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
        let favs = self.favorites.read(cx);
        let volume_paths: Vec<(PathBuf, String, Option<(u64, u64)>)> = self
            .volumes
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
            let node_id = self.fs.id_for_path(&path);
            self.node_store
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
        let favs = self.favorites.read(cx);
        for child in children {
            if !self.show_hidden && child.label.starts_with('.') {
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
        use gpui_component::sidebar::SidebarToggleButton;
        let can_back = self.active_tab().history_index > 0;
        let can_forward =
            self.active_tab().history_index + 1 < self.active_tab().history.len();
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
                    SidebarToggleButton::new()
                        .collapsed(collapsed)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sidebar_collapsed = !this.sidebar_collapsed;
                            let mut s = app_state::load();
                            s.sidebar_collapsed = Some(this.sidebar_collapsed);
                            app_state::save(&s);
                            cx.notify();
                        })),
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
                .child(div().flex_1())
                // Phase 7 follow-on: density buttons on the right —
                // Refresh and New Folder. Icon-only with tooltips so
                // the bar stays narrow.
                .child(
                    Button::new("toolbar-new-folder")
                        .small()
                        .ghost()
                        .icon(gpui_component::Icon::empty().path("icons/nav/folder.svg"))
                        .tooltip_with_action(
                            "New Folder",
                            &NewFolder,
                            Some(SHELL_CONTEXT),
                        )
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
            let entries = &self.table.read(cx).delegate().entries;
            self.active_tab().lead_row(entries).and_then(|i| entries.get(i).cloned())
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
            // §5 favorited indicator: trailing star on any breadcrumb
            // segment whose path is in the Favorites index. The last
            // segment is the current-folder header per §5.1, so the
            // current-folder header is covered by the same render path.
            let favorited = self.favorites.read(cx).contains_path(&path);
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
                        let already =
                            s.read(cx).favorites.read(cx).contains_path(&path_for_menu);
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
                        .menu("Reveal in Finder", Box::new(RevealContextPath))
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
        // Phase 8: status-bar density. Compute selected count / size,
        // total visible size for the active folder, and free disk on
        // the active tab's volume. Cheap O(N) sums over the already-
        // filtered entries Vec; called once per render.
        let delegate = self.table.read(cx).delegate();
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
        let volume_info = feraille_fs_native::volume_info_for_path(
            &self.active_tab().current_dir,
        );
        let (free_bytes, volume_name): (Option<u64>, Option<&'static str>) =
            match volume_info {
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
                        .favorites
                        .read(cx)
                        .entry_by_id(id)
                        .map(|f| f.effective_label())
                        .unwrap_or_else(|| "favorite".to_string());
                    let removed_for_undo = this.favorites.read(cx).entry_by_id(id).cloned();
                    this.favorites.update(cx, |f, cx| {
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

fn run_directory_load_streaming(
    fs: Arc<NativeFs>,
    path: PathBuf,
    show_hidden: bool,
    filter_text: String,
    cancel: Arc<AtomicBool>,
    tx: async_channel::Sender<LoadMsg>,
) {
    let needle = filter_text.trim().to_lowercase();
    let error = fs.enumerate_streaming(&path, DEFAULT_ENUMERATION_BATCH, &cancel, |entries| {
        let batch = filter_directory_batch(&fs, entries, show_hidden, &needle);
        if !batch.entries.is_empty() && tx.send_blocking(LoadMsg::Batch(batch)).is_err() {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    let _ = tx.send_blocking(LoadMsg::Done(error));
}

fn filter_directory_batch(
    fs: &NativeFs,
    entries: Vec<FileEntry>,
    show_hidden: bool,
    needle: &str,
) -> LoadBatch {
    let entries: Vec<FileEntry> = entries
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
                e.name.to_lowercase().contains(needle)
                    || format.to_lowercase().contains(needle)
            }
        })
        .collect();
    let mut paths = HashMap::with_capacity(entries.len());
    for entry in &entries {
        if let Some(path) = fs.path_for(entry.id) {
            paths.insert(entry.id, path);
        }
    }
    LoadBatch { entries, paths }
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
