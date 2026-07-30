//! Process-scoped shell state — the singleton that backs every
//! `WindowShell` in the running app.
//!
//! Today there is exactly one `Shell` (renamed to `WindowShell` after
//! the multi-window work lands). The fields here are the ones the
//! `ferail-windows-instances-tabs-spec.md` calls out as "shell /
//! process-wide": NodeStore, file watcher, task registry, preview /
//! icon / metadata caches, favorites, undo stack, ant-trail counts,
//! mounted volumes. They were previously on `Shell` directly.
//!
//! Why this exists *now*, before there are multiple windows: the
//! per-window vs per-process boundary is what the rest of the spec
//! (cross-window reload fan-out, tear-off, singleton-process intents)
//! is built on. Pulling these fields into one place ahead of the
//! multi-window scaffolding means the multi-window PR is purely
//! additive — it just adds a window registry on top of an already
//! correct singleton.
//!
//! All fields are interior-mutable. `ProcessState` is shared via
//! `Rc<ProcessState>` (GPUI is single-threaded for entity access on
//! the main thread, so `Rc` is the right primitive); each window
//! borrows what it needs at the call site. Background workers grab
//! the underlying `Arc<…>` clones (`fs`, `metadata_db`) directly so
//! the `Rc<ProcessState>` itself never crosses thread boundaries.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use ferail_core::node_store::NodeStore;
use ferail_fs_native::{NativeFs, VolumeInfo, list_volumes};
use gpui::{App, Entity, WeakEntity, WindowHandle};

use crate::favorites::Favorites;
use crate::file_list::SortColumn;
use crate::fs_watcher::FsWatcher;
use crate::icons::IconCache;
use crate::preview::PreviewCache;
use crate::shell::{ClosedTab, Shell, UndoOp};
use crate::tasks::TaskRegistry;
use crate::text_preview::TextPreviewCache;

/// Soft cap on the closed-tab stack. Cmd+Shift+T undoes the last N
/// tab closes; older entries fall off the front. 16 matches Safari's
/// "Recently Closed" reach (browsers cap somewhere in 10–20); the
/// stack is in-memory only, not persisted across launches in v1.
const CLOSED_TABS_CAP: usize = 16;

/// How many recently-visited folders the Recents sidebar section
/// keeps. Finder's Recents shows a comparable handful; 12 fills the
/// section without scrolling the sidebar.
pub const RECENTS_CAP: usize = 12;

pub struct ProcessState {
    /// Shared filesystem backend. Already `Arc` because background
    /// workers hold their own clones.
    pub fs: Arc<NativeFs>,

    /// `NodeId ↔ PathBuf` identity store. Cross-window correctness
    /// hinges on this being a single instance — two windows must
    /// agree on what `NodeId(7)` means, so this can't be per-window.
    pub node_store: RefCell<NodeStore>,

    /// Persistent metadata DB (rusqlite, mutex-guarded). Populated
    /// asynchronously by `start_metadata_load`; `None` while opening
    /// and on $HOME-absence / open failure (non-fatal).
    pub metadata_db: RefCell<Option<Arc<Mutex<ferail_meta::MetadataDb>>>>,

    /// NSWorkspace-backed icon cache. Already `Rc<RefCell<>>` —
    /// preserves the existing call shape.
    pub icons: Rc<RefCell<IconCache>>,

    /// Quick Look thumbnail cache (real photo/video/PDF content),
    /// keyed by full path and shared across every tab so a file seen
    /// in one tab is warm in another. Populated viewport-only off the
    /// UI thread; read allocation-free at paint time.
    pub thumbnails: Rc<RefCell<crate::thumbnails::ThumbnailCache>>,

    /// Background task registry (enumeration, prefetch, etc.). Already
    /// `Rc<RefCell<>>` so the prefetch worker can register / retire
    /// from the foreground executor.
    pub tasks: Rc<RefCell<TaskRegistry>>,

    /// Platform file-system watcher. `None` when FSEvents init failed
    /// (rare; sandboxed CI). The poller task in `WindowShell::new`
    /// drains its channel on a foreground-executor timer.
    pub watcher: Rc<RefCell<Option<FsWatcher>>>,

    /// User-curated favorites. An Entity so subscriptions / observers
    /// across multiple windows all see the same mutations.
    pub favorites: Entity<Favorites>,

    /// Process-wide undo stack. A delete/rename in window A is
    /// reachable from a Cmd+Z in window B by design — the operation
    /// is process-owned, not window-owned (spec §1.1).
    pub undo_stack: RefCell<VecDeque<UndoOp>>,

    /// Ant-trail visit counts. `Path → hits`. Hydrated from
    /// `metadata_db` by `start_metadata_load`, bumped on every navigate.
    pub ant_visits: RefCell<HashMap<PathBuf, u32>>,

    /// Cached max for heat normalisation. `Cell` since u32 is Copy.
    pub ant_max: Cell<u32>,

    /// Recently-visited folders, most-recent-first, capped at
    /// [`RECENTS_CAP`]. A live view over the same `folder_usage` visit
    /// log the Ant Trail uses (docs/features — Recents): hydrated from
    /// the DB at startup ordered by last-access, front-inserted on
    /// every navigate. In-memory so the sidebar render never touches
    /// SQLite.
    pub recents: RefCell<Vec<PathBuf>>,

    /// Recents section disclosure state. Persisted in app_state
    /// (`recents_collapsed`), like the sidebar-collapsed flag.
    pub recents_section_collapsed: Cell<bool>,

    /// Quick Look thumbnail cache. Single instance because two windows
    /// previewing the same file should share a fetch.
    pub preview_cache: RefCell<PreviewCache>,

    /// Inline text/code preview cache (docs/features/PREVIEW.md). Text
    /// files render their content instead of a thumbnail.
    pub text_preview_cache: RefCell<TextPreviewCache>,

    /// Mounted volumes. Refreshed lazily today; `RefCell` so a future
    /// Disk Arbitration listener can refresh it from any window.
    pub volumes: RefCell<Vec<VolumeInfo>>,

    /// Well-known Location paths macOS reports as iCloud items, mapped to
    /// their `CloudState` (downloaded vs not-downloaded placeholder) — e.g.
    /// Desktop/Documents under "Desktop & Documents Folders". Computed
    /// off-thread at startup and refreshed alongside `volumes`; the sidebar
    /// reads it to draw a trailing solid/outline cloud badge without ever
    /// touching the filesystem on the render path.
    pub cloud_locations: RefCell<HashMap<PathBuf, ferail_fs_native::CloudState>>,

    /// Monotonic counter for minting process-local `TabId`s. Stable
    /// for the tab's lifetime; survives tab reorder and (Phase F)
    /// tear-off between windows.
    pub next_tab_id: Cell<u64>,

    /// Set once the first window's `start_metadata_load` has
    /// kicked off the async DB open + ant-trail / favorites hydrate.
    /// Subsequent windows short-circuit the process-wide load and
    /// only refresh their own table's heat tints against the
    /// already-populated `ant_visits`.
    pub metadata_loaded: Cell<bool>,

    /// Process-wide copy of the persisted Favorites section disclosure
    /// state. New windows opened after metadata hydration has completed
    /// can pick this up without re-opening the DB.
    pub favorites_section_collapsed: Cell<bool>,

    /// Process-wide file-list sort. This is intentionally global for now:
    /// every folder view should keep the same ordering until the user changes
    /// it. `None` means the deterministic default, Name ascending.
    pub list_sort: Rc<Cell<Option<(SortColumn, bool)>>>,

    /// Weak handles for all live Shell windows. Reload fan-out walks
    /// this list and asks every matching tab in every live window to
    /// refresh. Dead windows are pruned opportunistically.
    pub shells: RefCell<Vec<WeakEntity<Shell>>>,

    /// Process-wide closed-tab stack. Most-recently-closed at the
    /// back (push) / popped first (pop_back). Capped at
    /// `CLOSED_TABS_CAP`; older entries fall off the front. Process-
    /// scoped (spec §3.3 + NOTES Phase A+B decision): a tab closed
    /// in window A is reachable from `Cmd+Shift+T` in window B,
    /// because the closed-tab stack lives on the singleton.
    pub closed_tabs: RefCell<VecDeque<ClosedTab>>,

    /// All live viewer windows (docs/features/VIEWER.md). Each Open
    /// Viewer stacks a new window; we keep every one's handle (so it
    /// isn't dropped mid-open) and weak view for process-wide fan-out —
    /// e.g. pause-every-viewer on sleep (docs/features/POWER.md). Dead
    /// entries are pruned opportunistically, same as `shells`.
    #[allow(clippy::type_complexity)]
    pub viewers: RefCell<
        Vec<(
            WindowHandle<gpui_component::Root>,
            WeakEntity<crate::viewer::ViewerWindow>,
        )>,
    >,

    /// Paths marked by Cut (Cmd+X): the next plain Paste of exactly
    /// these items performs a Move instead of a Copy, then clears the
    /// mark. A fresh Copy/Cut overwrites it. Process-wide so a cut in
    /// one tab/window pastes-as-move in another, and shared (the same
    /// `Rc`) with each file-list delegate so cut rows render dimmed.
    /// (docs/features/FILE_OPS.md)
    pub cut_marker: Rc<RefCell<Vec<std::path::PathBuf>>>,
}

impl ProcessState {
    /// Build the singleton. Takes the `favorites` entity by value so
    /// the caller can allocate it inside their `Context` (Entity
    /// allocations need a Context; ProcessState isn't a GPUI entity).
    pub fn new(
        fs: Arc<NativeFs>,
        watcher: Rc<RefCell<Option<FsWatcher>>>,
        favorites: Entity<Favorites>,
    ) -> Rc<Self> {
        Rc::new(Self {
            fs,
            node_store: RefCell::new(NodeStore::new()),
            metadata_db: RefCell::new(None),
            icons: Rc::new(RefCell::new(IconCache::new())),
            thumbnails: Rc::new(RefCell::new(crate::thumbnails::ThumbnailCache::new())),
            tasks: Rc::new(RefCell::new(TaskRegistry::new())),
            watcher,
            favorites,
            undo_stack: RefCell::new(VecDeque::new()),
            ant_visits: RefCell::new(HashMap::new()),
            ant_max: Cell::new(0),
            recents: RefCell::new(Vec::new()),
            recents_section_collapsed: Cell::new(false),
            preview_cache: RefCell::new(PreviewCache::new()),
            text_preview_cache: RefCell::new(TextPreviewCache::new()),
            // Seeded EMPTY and filled asynchronously (start_volume_watch's
            // initial pass / fill_volumes_once): list_volumes touches every
            // drive root, and on Windows a dead mapped network drive makes
            // GetVolumeInformationW block for the SMB timeout — running it
            // here delayed the first window by up to ~45s.
            volumes: RefCell::new(Vec::new()),
            cloud_locations: RefCell::new(HashMap::new()),
            next_tab_id: Cell::new(0),
            metadata_loaded: Cell::new(false),
            favorites_section_collapsed: Cell::new(false),
            list_sort: Rc::new(Cell::new(None)),
            shells: RefCell::new(Vec::new()),
            closed_tabs: RefCell::new(VecDeque::new()),
            viewers: RefCell::new(Vec::new()),
            cut_marker: Rc::new(RefCell::new(Vec::new())),
        })
    }

    /// Mint a fresh `TabId`, monotonically increasing. Process-local.
    pub fn mint_tab_id(&self) -> crate::shell::TabId {
        let id = self.next_tab_id.get();
        self.next_tab_id.set(id.wrapping_add(1));
        crate::shell::TabId(id)
    }

    /// Snapshot the optional `MetadataDb` handle for a background
    /// worker. Returns `None` while the async open is still in flight
    /// or the DB couldn't be opened.
    pub fn db_snapshot(&self) -> Option<Arc<Mutex<ferail_meta::MetadataDb>>> {
        self.metadata_db.borrow().clone()
    }

    /// Bump a folder's visit count, growing `ant_max` if needed.
    /// Returns the new count.
    pub fn record_ant_visit(&self, path: PathBuf) -> u32 {
        let mut visits = self.ant_visits.borrow_mut();
        let entry = visits.entry(path).or_insert(0);
        *entry += 1;
        let v = *entry;
        if v > self.ant_max.get() {
            self.ant_max.set(v);
        }
        v
    }

    /// Promote `path` to the front of the Recents list (dedup, cap).
    /// Called on every navigate alongside `record_ant_visit`.
    pub fn push_recent(&self, path: PathBuf) {
        let mut recents = self.recents.borrow_mut();
        recents.retain(|p| p != &path);
        recents.insert(0, path);
        recents.truncate(RECENTS_CAP);
    }

    /// Push to the undo stack, evicting the oldest when over cap.
    pub fn push_undo(&self, op: UndoOp, cap: usize) {
        let mut stack = self.undo_stack.borrow_mut();
        if stack.len() >= cap {
            stack.pop_front();
        }
        stack.push_back(op);
    }

    /// Register a newly-created Shell window for process-wide fan-out.
    pub fn register_shell(&self, shell: WeakEntity<Shell>) {
        let mut shells = self.shells.borrow_mut();
        shells.retain(|weak| weak.upgrade().is_some());
        shells.push(shell);
    }

    /// Snapshot live Shell windows without holding the registry borrow
    /// across `Entity::update` calls.
    pub fn live_shells(&self) -> Vec<WeakEntity<Shell>> {
        let mut shells = self.shells.borrow_mut();
        shells.retain(|weak| weak.upgrade().is_some());
        shells.clone()
    }

    /// Release OS file watches for directories no live tab is showing.
    /// The watch set is otherwise add-only: every directory ever
    /// visited would stay FSEvents/inotify-watched all session — its
    /// events fanning reloads forever, and Linux eventually hitting
    /// `max_user_watches`, after which *new* watches silently fail and
    /// live-update stops. Called after navigation and tab close.
    ///
    /// The calling Shell is mid-`update`, so it cannot be `read`
    /// through `cx` — it passes its own entity id and tab directories
    /// instead, and only *other* live shells are read here.
    pub fn prune_watches(
        &self,
        own_id: gpui::EntityId,
        own_dirs: impl IntoIterator<Item = std::path::PathBuf>,
        cx: &gpui::App,
    ) {
        let mut keep: std::collections::HashSet<std::path::PathBuf> =
            own_dirs.into_iter().collect();
        for weak in self.live_shells() {
            if weak.entity_id() == own_id {
                continue;
            }
            if let Some(shell) = weak.upgrade() {
                let shell = shell.read(cx);
                for tab in &shell.tabs {
                    keep.insert(tab.current_dir.clone());
                }
            }
        }
        // Favorite parent directories are watched independently of any
        // tab so a favorited path deleted/moved while unseen still flips
        // to Missing (see `Favorites::watch_dirs`). Keep them across the
        // prune, not just the visible tab dirs.
        keep.extend(self.favorites.read(cx).watch_dirs());
        if let Some(w) = self.watcher.borrow_mut().as_mut() {
            w.retain_watched(&keep);
        }
    }

    /// Register every favorite's parent directory on the filesystem
    /// watcher (idempotent). Called whenever the favorites list changes
    /// so a newly added favorite's parent starts being watched right
    /// away — deletes/moves of the favorited path then surface as events
    /// that drive `Favorites::refresh_availability`. `prune_watches`
    /// keeps these registered across navigation.
    pub fn watch_favorite_dirs(&self, cx: &gpui::App) {
        let dirs = self.favorites.read(cx).watch_dirs();
        if dirs.is_empty() {
            return;
        }
        if let Some(w) = self.watcher.borrow_mut().as_mut() {
            for dir in dirs {
                w.watch(&dir);
            }
        }
    }

    /// Register a newly-opened viewer window for process-wide fan-out.
    /// Retains its window handle (so it isn't dropped mid-open) and
    /// prunes any viewers that have since closed.
    pub fn register_viewer(
        &self,
        handle: WindowHandle<gpui_component::Root>,
        viewer: WeakEntity<crate::viewer::ViewerWindow>,
    ) {
        let mut viewers = self.viewers.borrow_mut();
        viewers.retain(|(_, weak)| weak.upgrade().is_some());
        viewers.push((handle, viewer));
    }

    /// Snapshot live viewer windows without holding the registry borrow
    /// across `Entity::update` calls.
    pub fn live_viewers(&self) -> Vec<WeakEntity<crate::viewer::ViewerWindow>> {
        let mut viewers = self.viewers.borrow_mut();
        viewers.retain(|(_, weak)| weak.upgrade().is_some());
        viewers.iter().map(|(_, weak)| weak.clone()).collect()
    }

    /// Push the snapshot of a tab the user just closed onto the
    /// process-wide stack. Caller produces the snapshot via
    /// `Tab::snapshot_for_close()` before the tab is removed from
    /// its `Shell::tabs` vec. Trims the oldest entry when over cap.
    pub fn push_closed_tab(&self, snapshot: ClosedTab) {
        push_with_cap(
            &mut self.closed_tabs.borrow_mut(),
            snapshot,
            CLOSED_TABS_CAP,
        );
    }

    /// Pop the most recently closed tab for `Cmd+Shift+T`. Returns
    /// `None` when the stack is empty (the reopen action becomes a
    /// no-op rather than a beep).
    pub fn pop_closed_tab(&self) -> Option<ClosedTab> {
        self.closed_tabs.borrow_mut().pop_back()
    }
}

/// Bounded LIFO push: evicts the OLDEST entry (front) when at `cap`,
/// then pushes to the back. Most-recent entry is always `pop_back`.
/// Extracted from `push_closed_tab` so the eviction order is pinned
/// by unit tests without constructing a full `ProcessState`.
fn push_with_cap<T>(stack: &mut VecDeque<T>, item: T, cap: usize) {
    if stack.len() >= cap {
        stack.pop_front();
    }
    stack.push_back(item);
}

/// Newtype around `Rc<ProcessState>` so it can be stored as a GPUI
/// `Global`. App-level handlers (Cmd+N, "New Window", future Apple
/// Event delegates) all reach the singleton through `cx.global::<…>()`
/// rather than threading the Rc through every closure capture.
///
/// `Rc<ProcessState>` is `'static` (no lifetimes) and `Global` only
/// requires `'static`, so no `Send`/`Sync` gymnastics needed. The
/// global is set once in `main.rs::run_gui` before any window opens
/// and never replaced.
pub struct ProcessStateGlobal(pub Rc<ProcessState>);

impl gpui::Global for ProcessStateGlobal {}

/// Helper: read the global Rc<ProcessState>. Panics if the global
/// isn't set, which is a programmer error (the global must be set
/// before any window opens).
pub fn process_state(cx: &App) -> Rc<ProcessState> {
    cx.global::<ProcessStateGlobal>().0.clone()
}

/// Start the live volume watch. Platform mount/unmount/rename
/// notifications ([mac] NSWorkspace; win-parity stub today) feed a
/// coalescing channel; the foreground drain task re-lists volumes on
/// the background executor (O(mounted volumes) of cached NSURL keys),
/// swaps [`ProcessState::volumes`], re-probes Favorites
/// Available/Unmounted states, and notifies every live shell so the
/// sidebar repaints. Call once at startup, after the ProcessState
/// global is set.
pub fn start_volume_watch(cx: &mut App) {
    let (tx, rx) = async_channel::unbounded::<()>();
    crate::platform_shell::start_volume_observer(Box::new(move || {
        let _ = tx.try_send(());
    }));
    cx.spawn(async move |cx| {
        // First pass runs immediately: ProcessState::new seeds the
        // volume list empty (a synchronous list_volumes there hung
        // startup on dead network drives), so this is the initial fill.
        loop {
            refresh_volumes(cx).await;
            if rx.recv().await.is_err() {
                break;
            }
            // Coalesce bursts — a mount often arrives with a rename
            // right behind it; one re-list covers both.
            while rx.try_recv().is_ok() {}
        }
    })
    .detach();
}

/// One asynchronous volume/cloud-location refresh + fan-out. Used by
/// the volume watch loop and by the screenshot harness's one-shot fill.
async fn refresh_volumes(cx: &mut gpui::AsyncApp) {
    let (vols, clouds) = cx
        .background_executor()
        .spawn(async { (list_volumes(), ferail_fs_native::cloud_synced_locations()) })
        .await;
    let _ = cx.update(|cx| {
        let process = process_state(cx);
        *process.volumes.borrow_mut() = vols;
        *process.cloud_locations.borrow_mut() = clouds;
        process
            .favorites
            .update(cx, |favs, cx| favs.refresh_mount_states(cx));
        for weak in process.live_shells() {
            if let Some(shell) = weak.upgrade() {
                shell.update(cx, |this, cx| {
                    // A mount/unmount/rename may change the volume
                    // behind any tab's directory — re-query each
                    // tab's cached free-space/name off-thread.
                    this.refresh_volume_info_all_tabs(cx);
                    cx.notify();
                });
            }
        }
    });
}

/// One-shot volume fill for hosts that don't run the live watch (the
/// screenshot harness). The GUI path gets this from
/// [`start_volume_watch`]'s initial pass.
pub fn fill_volumes_once(cx: &mut App) {
    cx.spawn(async move |cx| {
        refresh_volumes(cx).await;
    })
    .detach();
}

/// Start the live power watch (docs/features/POWER.md). Platform
/// sleep/wake notifications ([mac] NSWorkspace; [win] WM_POWERBROADCAST)
/// feed a coalescing channel; the foreground drain reacts on the main
/// thread:
///
/// - **On sleep** (system or display): pause the active viewer's video
///   and slideshow, so nothing keeps decoding behind a dark screen.
/// - **On system wake**: re-list volumes (a drive may have been
///   unplugged while asleep), re-probe Favorites mount states, and
///   reload every directory tab (contents may have drifted past the
///   watcher).
///
/// Call once at startup, after the ProcessState global is set. The
/// observer callback may fire on a worker thread (win32 contract), so
/// the bridge is a thread-safe channel send; all real work happens in
/// the foreground drain.
pub fn start_power_watch(cx: &mut App) {
    use ferail_core::power::PowerEvent;
    let (tx, rx) = async_channel::unbounded::<PowerEvent>();
    crate::platform_shell::start_power_observer(Box::new(move |event| {
        let _ = tx.try_send(event);
    }));
    cx.spawn(async move |cx| {
        while let Ok(event) = rx.recv().await {
            if event.is_sleep() {
                cx.update(|cx| {
                    let process = process_state(cx);
                    // Pause every open viewer (live_viewers snapshots and
                    // prunes, so we don't hold the registry borrow across
                    // the updates).
                    for weak in process.live_viewers() {
                        if let Some(viewer) = weak.upgrade() {
                            viewer.update(cx, |vw, cx| vw.suspend_for_power(cx));
                        }
                    }
                });
            } else if event.is_system_wake() {
                let (vols, clouds) = cx
                    .background_executor()
                    .spawn(async { (list_volumes(), ferail_fs_native::cloud_synced_locations()) })
                    .await;
                cx.update(|cx| {
                    let process = process_state(cx);
                    *process.volumes.borrow_mut() = vols;
                    *process.cloud_locations.borrow_mut() = clouds;
                    process
                        .favorites
                        .update(cx, |favs, cx| favs.refresh_mount_states(cx));
                    for weak in process.live_shells() {
                        if let Some(shell) = weak.upgrade() {
                            shell.update(cx, |this, cx| this.reload_dir_tabs(cx));
                        }
                    }
                });
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod closed_tab_stack_tests {
    use super::push_with_cap;
    use std::collections::VecDeque;

    #[test]
    fn pop_back_yields_most_recently_pushed() {
        let mut stack: VecDeque<u32> = VecDeque::new();
        for n in 1..=3 {
            push_with_cap(&mut stack, n, 16);
        }
        assert_eq!(stack.pop_back(), Some(3));
        assert_eq!(stack.pop_back(), Some(2));
        assert_eq!(stack.pop_back(), Some(1));
        assert_eq!(stack.pop_back(), None);
    }

    #[test]
    fn cap_evicts_oldest_first() {
        let mut stack: VecDeque<u32> = VecDeque::new();
        // Push 20 with cap 16 → 1..=4 fall off the front; the 16
        // most recent (5..=20) survive, newest still on top.
        for n in 1..=20 {
            push_with_cap(&mut stack, n, 16);
        }
        assert_eq!(stack.len(), 16);
        assert_eq!(stack.front().copied(), Some(5));
        assert_eq!(stack.pop_back(), Some(20));
    }

    #[test]
    fn zero_cap_never_grows_unbounded() {
        // Degenerate cap: still bounded (single element), no panic.
        let mut stack: VecDeque<u32> = VecDeque::new();
        push_with_cap(&mut stack, 1, 1);
        push_with_cap(&mut stack, 2, 1);
        assert_eq!(stack.len(), 1);
        assert_eq!(stack.pop_back(), Some(2));
    }
}
