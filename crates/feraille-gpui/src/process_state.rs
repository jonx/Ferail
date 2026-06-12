//! Process-scoped shell state — the singleton that backs every
//! `WindowShell` in the running app.
//!
//! Today there is exactly one `Shell` (renamed to `WindowShell` after
//! the multi-window work lands). The fields here are the ones the
//! `feraille-windows-instances-tabs-spec.md` calls out as "shell /
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

use feraille_core::node_store::NodeStore;
use feraille_fs_native::{NativeFs, VolumeInfo, list_volumes};
use gpui::{App, Entity, WeakEntity};

use crate::favorites::Favorites;
use crate::fs_watcher::FsWatcher;
use crate::icons::IconCache;
use crate::preview::PreviewCache;
use crate::shell::{ClosedTab, Shell, UndoOp};
use crate::tasks::TaskRegistry;

/// Soft cap on the closed-tab stack. Cmd+Shift+T undoes the last N
/// tab closes; older entries fall off the front. 16 matches Safari's
/// "Recently Closed" reach (browsers cap somewhere in 10–20); the
/// stack is in-memory only, not persisted across launches in v1.
const CLOSED_TABS_CAP: usize = 16;

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
    pub metadata_db: RefCell<Option<Arc<Mutex<feraille_meta::MetadataDb>>>>,

    /// NSWorkspace-backed icon cache. Already `Rc<RefCell<>>` —
    /// preserves the existing call shape.
    pub icons: Rc<RefCell<IconCache>>,

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

    /// Quick Look thumbnail cache. Single instance because two windows
    /// previewing the same file should share a fetch.
    pub preview_cache: RefCell<PreviewCache>,

    /// Mounted volumes. Refreshed lazily today; `RefCell` so a future
    /// Disk Arbitration listener can refresh it from any window.
    pub volumes: RefCell<Vec<VolumeInfo>>,

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
            tasks: Rc::new(RefCell::new(TaskRegistry::new())),
            watcher,
            favorites,
            undo_stack: RefCell::new(VecDeque::new()),
            ant_visits: RefCell::new(HashMap::new()),
            ant_max: Cell::new(0),
            preview_cache: RefCell::new(PreviewCache::new()),
            volumes: RefCell::new(list_volumes()),
            next_tab_id: Cell::new(0),
            metadata_loaded: Cell::new(false),
            favorites_section_collapsed: Cell::new(false),
            shells: RefCell::new(Vec::new()),
            closed_tabs: RefCell::new(VecDeque::new()),
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
    pub fn db_snapshot(&self) -> Option<Arc<Mutex<feraille_meta::MetadataDb>>> {
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
