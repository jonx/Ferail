//! Runtime state for the user-curated Favorites feature
//! ([`docs/features/FAVORITES.md`]).
//!
//! This is the GPUI entity behind the sidebar Favorites section
//! ([`crate::favorites_section::FavoritesSection`]) and the
//! source-of-truth every view observes for the §5 favorited-indicator
//! badge. Persistence is delegated to [`feraille_meta::MetadataDb`];
//! this module holds only the in-memory list, the path → id index used
//! by indicators, and a thin wrapper that schedules writes off the UI
//! thread (the Prime Directive forbids SQLite queries on the paint path,
//! and we extend that to action handlers for consistency).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use feraille_core::favorites::{
    Favorite, FavoriteIcon, FavoriteId, FavoriteKind, FavoriteSort, FavoriteState, FavoriteTarget,
    fractional_between,
};
use feraille_meta::MetadataDb;
use gpui::{AppContext, Context, EventEmitter};

/// Events the entity emits when its state changes.
#[derive(Debug, Clone)]
pub enum FavoritesEvent {
    /// A new entry was appended or inserted. `index` is its position in
    /// the sorted entry list — useful for the §2.2 fade-in animation.
    Added { id: FavoriteId, index: usize },
    /// An entry was removed. The full entry payload rides along so the
    /// `UndoOp::RemoveFavorite` variant (iter 6) can capture it without
    /// a separate clone.
    Removed(Favorite),
    /// One or more entries had their `sort_index` rewritten — drag
    /// reorder, keyboard reorder, or a one-shot sort.
    Reordered,
    Renamed(FavoriteId),
    IconChanged(FavoriteId),
    Repointed(FavoriteId),
    /// Brief one-shot pulse on the existing entry when a duplicate add
    /// is attempted (§2.2 dedup rule).
    DedupPulse(FavoriteId),
}

/// Outcome of an `add_path` call. Distinguishes "actually added" from
/// "already there, dedup-pulse fired" so callers know whether to commit
/// an undo entry, scroll-to, etc.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddOutcome {
    Added(FavoriteId),
    Existing(FavoriteId),
}

pub struct Favorites {
    entries: Vec<Favorite>,
    /// Canonical-path → id index. Drives §5 indicators across file
    /// list, tree, breadcrumb, current-folder header. Maintained
    /// incrementally on every mutation so view code can query in O(1).
    /// Only populated for `FavoriteTarget::Path` entries; saved-search
    /// and tag favorites (reserved) would need a parallel index.
    index: HashMap<PathBuf, FavoriteId>,
    /// Cached runtime availability state per favorite (§8). Updated
    /// off the UI thread on hydrate / add / repoint. Render reads from
    /// here — never touches the filesystem itself (Prime Directive).
    /// The watcher extension that flips state live on rename / delete /
    /// mount events is iter 11 polish; for v1 this snapshot is taken
    /// at startup and after explicit mutations.
    state: HashMap<FavoriteId, FavoriteState>,
    /// Shared metadata DB handle for write-back. `None` when running
    /// without a writable DB (in-memory tests, screenshot harness).
    db: Option<Arc<Mutex<MetadataDb>>>,
}

impl Favorites {
    /// Construct an empty entity. Real entries arrive via [`Self::hydrate`]
    /// once `Shell::start_metadata_load` has the DB open — the constructor
    /// is cheap so it runs on the synchronous `Shell::new` path.
    pub fn new(db: Option<Arc<Mutex<MetadataDb>>>) -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
            state: HashMap::new(),
            db,
        }
    }

    /// Attach the persistent DB handle after a deferred startup load.
    pub fn attach_db(&mut self, db: Arc<Mutex<MetadataDb>>) {
        self.db = Some(db);
    }

    /// Replace in-memory state from a DB-loaded list. Emits `Reordered`
    /// so observers refresh — hydrate is a bulk update.
    pub fn hydrate(&mut self, mut entries: Vec<Favorite>, cx: &mut Context<Self>) {
        entries.sort_by(|a, b| {
            a.sort_index
                .partial_cmp(&b.sort_index)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.entries = entries;
        self.rebuild_index();
        self.refresh_state(cx);
        cx.emit(FavoritesEvent::Reordered);
        cx.notify();
    }

    /// Read the runtime availability of a favorite. `Available` when
    /// nothing has been computed yet — the watcher extension keeps
    /// this map live in iter 11 polish.
    pub fn state_for(&self, id: FavoriteId) -> FavoriteState {
        self.state
            .get(&id)
            .copied()
            .unwrap_or(FavoriteState::Available)
    }

    /// Recompute state for every favorite off the UI thread. Called
    /// after hydrate, add, repoint, and (eventually) on watcher
    /// events. Touches the filesystem (`.exists()`) so it must not
    /// run synchronously on the render path.
    fn refresh_state(&self, cx: &mut Context<Self>) {
        let probes: Vec<(FavoriteId, PathBuf, FavoriteKind)> = self
            .entries
            .iter()
            .filter_map(|f| {
                f.target
                    .as_path()
                    .map(|p| (f.id, p.to_path_buf(), f.kind))
            })
            .collect();
        if probes.is_empty() {
            return;
        }
        let task = cx.background_spawn(async move {
            let mut out = Vec::with_capacity(probes.len());
            for (id, path, kind) in probes {
                let state = classify_state(&path, kind);
                out.push((id, state));
            }
            out
        });
        cx.spawn(async move |this, cx| {
            let states = task.await;
            let _ = this.update(cx, |this, cx| {
                for (id, state) in states {
                    this.state.insert(id, state);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn rebuild_index(&mut self) {
        self.index.clear();
        for f in &self.entries {
            if let FavoriteTarget::Path(p) = &f.target {
                self.index.insert(p.clone(), f.id);
            }
        }
    }

    pub fn entries(&self) -> &[Favorite] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// O(1) §5 lookup: is the given path currently favorited?
    pub fn contains_path(&self, p: &Path) -> bool {
        self.index.contains_key(p)
    }

    /// Inverse — returns the favorite's id for a path, if any.
    pub fn id_for_path(&self, p: &Path) -> Option<FavoriteId> {
        self.index.get(p).copied()
    }

    pub fn entry_by_id(&self, id: FavoriteId) -> Option<&Favorite> {
        self.entries.iter().find(|f| f.id == id)
    }

    pub fn entry_for_path(&self, p: &Path) -> Option<&Favorite> {
        let id = self.index.get(p)?;
        self.entries.iter().find(|f| f.id == *id)
    }

    // ---- mutations (iter 4+ wires the UI entry points) ----

    /// Append a folder favorite at the end of the list. Dedup-aware:
    /// repeat-adding the same target fires `DedupPulse` and returns
    /// `Existing(id)` instead of creating a duplicate (§2.2).
    pub fn add_path(
        &mut self,
        path: PathBuf,
        kind: FavoriteKind,
        cx: &mut Context<Self>,
    ) -> AddOutcome {
        if let Some(existing) = self.index.get(&path).copied() {
            cx.emit(FavoritesEvent::DedupPulse(existing));
            return AddOutcome::Existing(existing);
        }
        let sort_index = self
            .entries
            .last()
            .map(|f| f.sort_index + 1024.0)
            .unwrap_or(0.0);
        let fav = Favorite {
            id: FavoriteId::new(),
            kind,
            target: FavoriteTarget::Path(path.clone()),
            display_name: None,
            custom_icon: None,
            sort_index,
            date_added: now_unix(),
        };
        let id = fav.id;
        let index = self.entries.len();
        self.index.insert(path, id);
        self.entries.push(fav.clone());
        self.persist_save(fav, cx);
        self.refresh_state(cx);
        cx.emit(FavoritesEvent::Added { id, index });
        cx.notify();
        AddOutcome::Added(id)
    }

    /// Insert a favorite at a specific fractional `sort_index` — used by
    /// drag-to-add (iter 8). Dedup applies as in [`Self::add_path`].
    pub fn add_path_at(
        &mut self,
        path: PathBuf,
        kind: FavoriteKind,
        sort_index: f64,
        cx: &mut Context<Self>,
    ) -> AddOutcome {
        if let Some(existing) = self.index.get(&path).copied() {
            cx.emit(FavoritesEvent::DedupPulse(existing));
            return AddOutcome::Existing(existing);
        }
        let fav = Favorite {
            id: FavoriteId::new(),
            kind,
            target: FavoriteTarget::Path(path.clone()),
            display_name: None,
            custom_icon: None,
            sort_index,
            date_added: now_unix(),
        };
        let id = fav.id;
        self.index.insert(path, id);
        self.entries.push(fav.clone());
        self.sort_entries();
        let index = self.entries.iter().position(|f| f.id == id).unwrap_or(0);
        self.persist_save(fav, cx);
        cx.emit(FavoritesEvent::Added { id, index });
        cx.notify();
        AddOutcome::Added(id)
    }

    pub fn remove(&mut self, id: FavoriteId, cx: &mut Context<Self>) -> Option<Favorite> {
        let pos = self.entries.iter().position(|f| f.id == id)?;
        let removed = self.entries.remove(pos);
        if let FavoriteTarget::Path(p) = &removed.target {
            self.index.remove(p);
        }
        self.persist_delete(id, cx);
        cx.emit(FavoritesEvent::Removed(removed.clone()));
        cx.notify();
        Some(removed)
    }

    /// Re-insert a previously-removed favorite at its prior identity +
    /// sort_index. Used by the undo path (§3.2). Preserves the original
    /// id so toggles elsewhere stay consistent.
    ///
    /// If the same target was re-added by another path between remove
    /// and undo (e.g. drag-to-add re-adds the folder, then Cmd+Z fires
    /// for the older remove), the path is already in the index — restoring
    /// would create a duplicate row. Dedup against the existing entry and
    /// emit a pulse instead.
    pub fn restore(&mut self, fav: Favorite, cx: &mut Context<Self>) {
        if let FavoriteTarget::Path(p) = &fav.target {
            if let Some(existing) = self.index.get(p).copied() {
                cx.emit(FavoritesEvent::DedupPulse(existing));
                cx.notify();
                return;
            }
            self.index.insert(p.clone(), fav.id);
        }
        let id = fav.id;
        self.persist_save(fav.clone(), cx);
        self.entries.push(fav);
        self.sort_entries();
        let index = self.entries.iter().position(|f| f.id == id).unwrap_or(0);
        cx.emit(FavoritesEvent::Added { id, index });
        cx.notify();
    }

    /// Move `id` to land between favorites at sort_indices `before` and
    /// `after`. Either bound can be `f64::NEG_INFINITY` (top) /
    /// `f64::INFINITY` (bottom).
    pub fn reorder_between(
        &mut self,
        id: FavoriteId,
        before: f64,
        after: f64,
        cx: &mut Context<Self>,
    ) {
        let Some(pos) = self.entries.iter().position(|f| f.id == id) else {
            return;
        };
        self.entries[pos].sort_index = fractional_between(before, after);
        let fav = self.entries[pos].clone();
        self.sort_entries();
        self.persist_save(fav, cx);
        cx.emit(FavoritesEvent::Reordered);
        cx.notify();
    }

    /// Shift `id` one position toward index 0 / end. Used by the
    /// `Cmd+Option+Up/Down` keyboard reorder shortcuts (§4.4).
    pub fn shift(&mut self, id: FavoriteId, by: isize, cx: &mut Context<Self>) {
        let Some(pos) = self.entries.iter().position(|f| f.id == id) else {
            return;
        };
        let target = (pos as isize + by).clamp(0, self.entries.len() as isize - 1) as usize;
        if target == pos {
            return;
        }
        let (before, after) = match by.signum() {
            -1 if target == 0 => (
                f64::NEG_INFINITY,
                self.entries[target].sort_index,
            ),
            -1 => (
                self.entries[target - 1].sort_index,
                self.entries[target].sort_index,
            ),
            1 if target == self.entries.len() - 1 => (
                self.entries[target].sort_index,
                f64::INFINITY,
            ),
            1 => (
                self.entries[target].sort_index,
                self.entries[target + 1].sort_index,
            ),
            _ => return,
        };
        self.reorder_between(id, before, after, cx);
    }

    pub fn rename(
        &mut self,
        id: FavoriteId,
        new_name: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(pos) = self.entries.iter().position(|f| f.id == id) else {
            return;
        };
        self.entries[pos].display_name = new_name.filter(|s| !s.is_empty());
        let fav = self.entries[pos].clone();
        self.persist_save(fav, cx);
        cx.emit(FavoritesEvent::Renamed(id));
        cx.notify();
    }

    pub fn set_icon(
        &mut self,
        id: FavoriteId,
        icon: Option<FavoriteIcon>,
        cx: &mut Context<Self>,
    ) {
        let Some(pos) = self.entries.iter().position(|f| f.id == id) else {
            return;
        };
        self.entries[pos].custom_icon = icon;
        let fav = self.entries[pos].clone();
        self.persist_save(fav, cx);
        cx.emit(FavoritesEvent::IconChanged(id));
        cx.notify();
    }

    /// Repoint `id` at a new target while keeping its identity. Used
    /// by the §8 Locate… flow.
    pub fn repoint(
        &mut self,
        id: FavoriteId,
        new_target: FavoriteTarget,
        cx: &mut Context<Self>,
    ) {
        let Some(pos) = self.entries.iter().position(|f| f.id == id) else {
            return;
        };
        if let FavoriteTarget::Path(p) = &self.entries[pos].target {
            self.index.remove(p);
        }
        if let FavoriteTarget::Path(p) = &new_target {
            self.index.insert(p.clone(), id);
        }
        self.entries[pos].target = new_target;
        let fav = self.entries[pos].clone();
        self.persist_save(fav, cx);
        self.refresh_state(cx);
        cx.emit(FavoritesEvent::Repointed(id));
        cx.notify();
    }

    /// One-shot sort that rewrites every `sort_index`. The manual
    /// order isn't "locked" — subsequent drags continue to work
    /// (§4.5). Persists atomically through `replace_favorites`.
    pub fn one_shot_sort(&mut self, by: FavoriteSort, cx: &mut Context<Self>) {
        match by {
            FavoriteSort::NameAsc => {
                self.entries
                    .sort_by(|a, b| a.effective_label().cmp(&b.effective_label()));
            }
            FavoriteSort::DateAddedNewest => {
                self.entries.sort_by(|a, b| b.date_added.cmp(&a.date_added));
            }
            FavoriteSort::DateAddedOldest => {
                self.entries.sort_by(|a, b| a.date_added.cmp(&b.date_added));
            }
            FavoriteSort::Kind => {
                self.entries.sort_by(|a, b| {
                    a.kind
                        .as_db_code()
                        .cmp(&b.kind.as_db_code())
                        .then_with(|| a.effective_label().cmp(&b.effective_label()))
                });
            }
        }
        // Assign indices directly over the freshly-sorted entries —
        // routing through `renormalize_sort_indices` (which sorts by
        // existing sort_index) would undo the new order.
        for (i, f) in self.entries.iter_mut().enumerate() {
            f.sort_index = (i as f64) * 1024.0;
        }
        self.persist_replace_all(cx);
        cx.emit(FavoritesEvent::Reordered);
        cx.notify();
    }

    fn sort_entries(&mut self) {
        self.entries.sort_by(|a, b| {
            a.sort_index
                .partial_cmp(&b.sort_index)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    // ---- persistence helpers ----

    fn persist_save(&self, fav: Favorite, cx: &mut Context<Self>) {
        let Some(db) = self.db.clone() else { return };
        cx.background_spawn(async move {
            if let Ok(g) = db.lock() {
                if let Err(e) = g.save_favorite(&fav) {
                    eprintln!("favorites: persist save failed: {e}");
                }
            }
        })
        .detach();
    }

    fn persist_delete(&self, id: FavoriteId, cx: &mut Context<Self>) {
        let Some(db) = self.db.clone() else { return };
        cx.background_spawn(async move {
            if let Ok(g) = db.lock() {
                if let Err(e) = g.delete_favorite(id) {
                    eprintln!("favorites: persist delete failed: {e}");
                }
            }
        })
        .detach();
    }

    fn persist_replace_all(&self, cx: &mut Context<Self>) {
        let Some(db) = self.db.clone() else { return };
        let snapshot: Vec<Favorite> = self.entries.clone();
        cx.background_spawn(async move {
            if let Ok(g) = db.lock() {
                if let Err(e) = g.replace_favorites(&snapshot) {
                    eprintln!("favorites: persist replace_all failed: {e}");
                }
            }
        })
        .detach();
    }
}

impl EventEmitter<FavoritesEvent> for Favorites {}

/// Classify the runtime state of a favorite target. Volume kind
/// distinguishes Unmounted (not in `/Volumes`) from Missing (path
/// gone for a non-volume location).
fn classify_state(path: &std::path::Path, kind: FavoriteKind) -> FavoriteState {
    if path.exists() {
        return FavoriteState::Available;
    }
    if matches!(kind, FavoriteKind::Volume) {
        FavoriteState::Unmounted
    } else {
        FavoriteState::Missing
    }
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Dev-only seed: insert favorites pointing at Home + Documents + Source
/// so iter-3 screenshots have something to render before iter 4 lands
/// the real add UI. Active only when `FERAILLE_DEV_SEED_FAVORITES=1`
/// and the list is currently empty.
pub fn maybe_seed_dev_favorites(favorites: &mut Favorites, cx: &mut Context<Favorites>) {
    if std::env::var("FERAILLE_DEV_SEED_FAVORITES").as_deref() != Ok("1") {
        return;
    }
    if !favorites.is_empty() {
        return;
    }
    let home = feraille_fs_native::home_dir();
    let candidates = [home.clone(), home.join("Documents"), home.join("Source")];
    for p in candidates.into_iter() {
        if !p.exists() {
            continue;
        }
        let _ = favorites.add_path(p, FavoriteKind::Folder, cx);
    }
}
