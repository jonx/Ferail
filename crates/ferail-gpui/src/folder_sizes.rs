//! Background folder-size worker: recursive sizes for the Size
//! column's directory rows.
//!
//! Mirrors the `prefetch` worker shape (seed snapshot on the
//! foreground, I/O on the background executor, apply through the
//! Shell's weak handle) with one difference: results stream back
//! folder-by-folder over an `async_channel` instead of one
//! end-of-pass batch, because a single deep folder can take seconds
//! to walk and the cheap ones shouldn't wait for it.
//!
//! Cache contract (`folder_sizes` table in the metadata DB): a row
//! is valid while the folder's live mtime equals the row's
//! `mtime_unix` **and** the row is younger than [`FOLDER_SIZE_TTL_SECS`].
//! Cache hits are sent back immediately in one batch before any
//! walking starts, so revisiting a folder fills every known size in
//! a single frame.
//!
//! The mtime fast-path has a macOS/POSIX blind spot: a directory's
//! mtime only changes when its *direct* children change, so edits
//! deep inside a subtree leave a parent's cached size looking valid.
//! Two mechanisms close that gap (see docs/features/FRESHNESS.md):
//!   - **In-app work** invalidates exactly: every mutation deletes
//!     the cached size for the mutated path and all its ancestors
//!     (`Shell::invalidate_folder_size_ancestors`), so the next visit
//!     recomputes.
//!   - **External (3rd-party) work** is caught lazily: the TTL
//!     bounds how long a deep external change can hide, and an
//!     explicit Refresh (`start(.., force = true)`) recomputes visible
//!     rows on demand.
//!
//! Nothing else bypasses the cache. Navigation, revisiting a folder you
//! just left, and the window coming forward all answer from it: the
//! Size column is meant to settle, not to re-measure under the user.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferail_core::{EntryKind, NodeId};
use ferail_fs_native::{
    NativeFs, SubtreeTotals, folder_contents_summary, humanize_bytes, recursive_totals,
};
use ferail_meta::{FolderSizeRecord, MetadataDb};
use gpui::Entity;

use crate::file_list::{FileListDelegate, SortColumn};
use crate::multi_table::TableState;
use crate::shell::{Shell, TabId};
use crate::tasks::{TaskKind, TaskRegistry};

/// AROS soak switch for the gate in [`start`]: `SetEnv FERAIL_FOLDER_SIZES 1`
/// (any non-empty value except `0`) before launch enables the walker.
/// Read once: the env can't change mid-run, and `start` runs on the UI
/// thread where we keep even cheap syscalls out of the per-navigation path.
fn aros_walker_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("FERAIL_FOLDER_SIZES").is_ok_and(|v| !v.is_empty() && v != "0")
    })
}

/// How long a cached folder size is trusted before a revisit
/// recomputes it. This is the lazy safety net for deep *external*
/// changes the folder's own mtime can't reveal (an in-app mutation
/// invalidates the exact ancestor chain immediately and doesn't wait
/// for this). A recompute only happens when the folder is actually
/// loaded again, off the UI thread, so a longer TTL trades a little
/// staleness for fewer re-walks of big trees. Tune here, one place.
const FOLDER_SIZE_TTL_SECS: i64 = 10 * 60;
const SIZE_CHANNEL_BATCHES: usize = 8;
const SIZE_ROWS_PER_BATCH: usize = 64;
const SIZE_BATCH_MAX_LATENCY: Duration = Duration::from_millis(100);
/// Bound one foreground update even when a warm cache can feed the worker much
/// faster than GPUI can repaint. Eight full channel batches are coalesced into
/// one sort/refresh, then the receiver yields for a frame.
const SIZE_UI_ROWS_PER_TICK: usize = SIZE_CHANNEL_BATCHES * SIZE_ROWS_PER_BATCH;
const SIZE_UI_TICK: Duration = Duration::from_millis(16);

/// Process-wide master switch for the background folder-size walker
/// (Settings → Performance). Recursively summing a directory tree is
/// the heaviest routine the app runs on a slow disk, so this lets the
/// user trade the Size column's folder totals for less background I/O.
/// Seeded from persisted settings at startup; the callers that spawn a
/// pass check it, and a Shell observer restarts/stops passes when it
/// flips. Default on.
#[derive(Clone, Copy)]
pub struct FolderSizingEnabled(pub bool);

impl gpui::Global for FolderSizingEnabled {}

/// Whether the background folder-size walker is allowed to run.
pub fn folder_sizing_enabled(cx: &gpui::App) -> bool {
    cx.try_global::<FolderSizingEnabled>()
        .map(|g| g.0)
        .unwrap_or(true)
}

/// Snapshot used to seed the worker: `Send` copies of the bits it
/// needs, like `PrefetchSeed`.
struct SizeSeed {
    row_ix: usize,
    node: NodeId,
    path: PathBuf,
    /// The folder's own mtime at enumerate time; stored alongside
    /// the computed size so the next visit can validate the row.
    mtime_unix: i64,
}

/// One resolved folder size. Keyed by `NodeId`, not row index:
/// rows can re-sort while the walk is in flight. Carries the recursive
/// item counts from the same walk so the apply can fill the folder's
/// Description column ("N files · M folders") alongside the Size.
struct SizeRow {
    row_ix: usize,
    node: NodeId,
    size: u64,
    file_count: u64,
    dir_count: u64,
}

/// Spawn a folder-size pass over the current entries of `table`.
/// Called from `Shell::finish_directory_load_in_tab` after the
/// final enumeration snapshot lands. Cheap: returns immediately.
///
/// `cancel` is owned by the tab (`Tab::folder_size_cancel`) and
/// flipped on the next navigation/reload, which both stops the
/// in-flight `recursive_size` walk at its next dirent and marks
/// any partial sum as not-cacheable.
/// `force` bypasses the cache fast-path entirely: every folder is
/// re-walked and written through. Armed only by Refresh
/// (`Tab::force_folder_sizes`), which is the user asking for exactly
/// that; every other load path passes `false`.
#[allow(clippy::too_many_arguments)]
pub fn start(
    table: Entity<TableState<FileListDelegate>>,
    fs: Arc<NativeFs>,
    db: Option<Arc<Mutex<MetadataDb>>>,
    tasks: Rc<RefCell<TaskRegistry>>,
    tab_id: TabId,
    generation: u64,
    cancel: Arc<AtomicBool>,
    force: bool,
    cx: &mut gpui::Context<Shell>,
) {
    // AROS: RE-GATED 2026-07-17, runtime-unlockable since 2026-07-21. The
    // recursive du walk bus-faulted the Ferail task under hosted AROS
    // (`Error 0x80000002`, contained Guru), and because it starts on *every*
    // directory listing it made ordinary navigation unusable. posixc's
    // unlocked fd table (T-FDLOCK, aros-upstream d702d708) was a real bug
    // but NOT the whole story: that un-gate was premature. The 2026-07-18
    // preemption + diag-scanner fixes are the likelier real story, so the
    // gate is now a runtime switch: `SetEnv FERAIL_FOLDER_SIZES 1` before
    // launch enables the walker on AROS for soak testing without a rebuild.
    // Default stays OFF until it survives a human soak; a missing nicety
    // beats an unusable file list. See docs/features/aros-port.md.
    if cfg!(target_os = "aros") && !aros_walker_enabled() {
        return;
    }
    // Snapshot the directory rows on the foreground executor. The
    // worker gets `Send` data only. Symlinks-to-directories are
    // `EntryKind::Symlink` and stay excluded: we never follow them.
    let seeds: Vec<SizeSeed> = table
        .read(cx)
        .delegate()
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e.kind, EntryKind::Directory))
        .filter_map(|(row_ix, e)| {
            let path = fs.path_for(e.id)?;
            Some(SizeSeed {
                row_ix,
                node: e.id,
                path,
                mtime_unix: e.mtime_unix,
            })
        })
        .collect();

    if seeds.is_empty() {
        return;
    }
    let seed_count = seeds.len();
    crate::log_info!(90, "folder-sizes: starting for {seed_count} folders");

    let task_id = tasks.borrow_mut().begin(
        TaskKind::FolderSize,
        trn!(
            "Sizing {n} folder\u{2026}",
            "Sizing {n} folders\u{2026}",
            seed_count
        ),
        false,
    );

    let (tx, rx) = async_channel::bounded::<Vec<SizeRow>>(SIZE_CHANNEL_BATCHES);
    let worker_cancel = cancel.clone();
    cx.background_executor()
        .spawn(async move {
            run_worker(seeds, db, force, worker_cancel, tx);
        })
        .detach();

    let table_for_apply = table.clone();
    let tasks_for_end = tasks.clone();
    cx.spawn(async move |this, cx| {
        while let Ok(mut batch) = rx.recv().await {
            while batch.len() < SIZE_UI_ROWS_PER_TICK {
                match rx.try_recv() {
                    Ok(next) => batch.extend(next),
                    Err(async_channel::TryRecvError::Empty) => break,
                    Err(async_channel::TryRecvError::Closed) => break,
                }
            }
            let stale = this
                .update(cx, |shell, cx| {
                    // Same staleness rule as the directory-load
                    // pipeline: the tab may have closed or moved on.
                    let Some(idx) = shell.tabs.iter().position(|t| t.id == tab_id) else {
                        return true;
                    };
                    if shell.tabs[idx].load_generation != generation {
                        return true;
                    }
                    let visible = shell.active_tab().id == tab_id;
                    table_for_apply.update(cx, |state, cx| {
                        let delegate = state.delegate_mut();
                        apply_batch(delegate, batch);
                        delegate.sync_sort_from_process();
                        // Folders sort as 0-byte rows until their
                        // sizes land; if the user is sorted by Size,
                        // keep the order truthful as values arrive
                        // (Finder does the same).
                        if let Some((SortColumn::Size, asc)) = delegate.current_sort {
                            delegate.apply_sort(SortColumn::Size, asc);
                        }
                        if visible {
                            state.refresh(cx);
                        }
                    });
                    if visible {
                        cx.notify();
                    }
                    false
                })
                .unwrap_or(true);
            if stale {
                break;
            }
            cx.background_executor().timer(SIZE_UI_TICK).await;
        }
        // Channel closed (worker done) or stale-exit: either way the
        // registry row comes down. The worker itself stops at the
        // cancel flag the navigation flipped.
        tasks_for_end.borrow_mut().end(task_id);
        crate::log_info!(90, "folder-sizes: pass ended");
    })
    .detach();
}

/// Body of the background pass. Two phases: a cache sweep that
/// answers mtime-valid folders in bounded batches, then a
/// compute sweep that walks the misses one folder at a time,
/// streaming each result as it lands and writing through to the DB.
fn run_worker(
    seeds: Vec<SizeSeed>,
    db: Option<Arc<Mutex<MetadataDb>>>,
    force: bool,
    cancel: Arc<AtomicBool>,
    tx: async_channel::Sender<Vec<SizeRow>>,
) {
    let now = now_unix();
    let mut hit_count = 0usize;
    let mut hit_batch = Vec::with_capacity(SIZE_ROWS_PER_BATCH);
    let mut misses: Vec<SizeSeed> = Vec::new();
    for seed in seeds {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        // `force` skips the cache so a recent deep external change is
        // re-walked; otherwise a row counts as a hit only while its
        // mtime still matches and it's younger than the TTL.
        let cached = if force {
            None
        } else {
            let path_str = seed.path.to_string_lossy().into_owned();
            db.as_ref().and_then(|db| {
                let guard = db.lock().ok()?;
                guard.get_folder_size(&path_str).ok().flatten()
            })
        };
        match cached {
            Some(rec)
                if rec.mtime_unix == seed.mtime_unix
                    && now.saturating_sub(rec.computed_at_unix) < FOLDER_SIZE_TTL_SECS =>
            {
                hit_count += 1;
                hit_batch.push(SizeRow {
                    row_ix: seed.row_ix,
                    node: seed.node,
                    size: rec.size,
                    file_count: rec.file_count,
                    dir_count: rec.dir_count,
                });
                if hit_batch.len() >= SIZE_ROWS_PER_BATCH {
                    if tx.send_blocking(std::mem::take(&mut hit_batch)).is_err() {
                        return;
                    }
                    hit_batch.reserve(SIZE_ROWS_PER_BATCH);
                }
            }
            _ => misses.push(seed),
        }
    }
    // The one line that tells you whether the cache is doing its job:
    // a revisit to an unchanged folder should report all hits and walk
    // nothing. Persistent misses on a folder you keep returning to mean
    // either its mtime is moving under you or the pass never survives
    // long enough to write its rows.
    crate::log_info!(
        90,
        "folder-sizes: {} cache hit(s), {} to walk{}",
        hit_count,
        misses.len(),
        if force { " (forced)" } else { "" }
    );
    if !hit_batch.is_empty() && tx.send_blocking(hit_batch).is_err() {
        return;
    }

    let mut ready = Vec::with_capacity(SIZE_ROWS_PER_BATCH);
    let mut last_flush = Instant::now();
    for seed in misses {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let SubtreeTotals {
            apparent: size,
            files,
            dirs,
            ..
        } = recursive_totals(&seed.path, &cancel);
        // A cancelled walk returns a partial sum, neither cacheable
        // nor showable.
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if let Some(db) = db.as_ref() {
            if let Ok(guard) = db.lock() {
                let _ = guard.upsert_folder_size(&FolderSizeRecord {
                    path: seed.path.to_string_lossy().into_owned(),
                    mtime_unix: seed.mtime_unix,
                    size,
                    computed_at_unix: now_unix(),
                    file_count: files,
                    dir_count: dirs,
                });
            }
        }
        ready.push(SizeRow {
            row_ix: seed.row_ix,
            node: seed.node,
            size,
            file_count: files,
            dir_count: dirs,
        });
        if ready.len() >= SIZE_ROWS_PER_BATCH || last_flush.elapsed() >= SIZE_BATCH_MAX_LATENCY {
            if tx.send_blocking(std::mem::take(&mut ready)).is_err() {
                return;
            }
            ready.reserve(SIZE_ROWS_PER_BATCH);
            last_flush = Instant::now();
        }
    }
    if !ready.is_empty() {
        let _ = tx.send_blocking(ready);
    }
}

/// Apply resolved sizes to the live `FileEntry` rows, matched by
/// `NodeId`. Rows that vanished (re-enumeration, filter) skip
/// silently. Sets `size` (so Size-sorting orders folders correctly),
/// the pre-formatted `display_size`, and the folder's `display_description`
/// with the recursive item counts (per the no-alloc-on-paint contract,
/// both strings are formatted here, not during render). Only folders
/// carry a count description; the prefetch worker leaves directory
/// descriptions empty (dirs have no magic facts), so the two never
/// fight over the same field.
fn apply_batch(delegate: &mut FileListDelegate, batch: Vec<SizeRow>) {
    // Sizes change → the status bar's cached totals are stale.
    delegate.invalidate_drag_snapshot();
    let needs_index = batch.iter().any(|row| {
        delegate
            .entries
            .get(row.row_ix)
            .is_none_or(|entry| entry.id != row.node)
    });
    let node_rows = needs_index.then(|| {
        delegate
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id, index))
            .collect::<std::collections::HashMap<_, _>>()
    });
    for row in batch {
        let row_ix = if needs_index {
            node_rows
                .as_ref()
                .and_then(|rows| rows.get(&row.node).copied())
        } else {
            Some(row.row_ix)
        };
        if let Some(e) = row_ix.and_then(|index| delegate.entries.get_mut(index)) {
            e.size = row.size;
            // "0 B" on an empty folder reads like a measurement
            // error; Finder-style "--" reads as "nothing in here".
            e.display_size = if row.size == 0 {
                "--".into()
            } else {
                humanize_bytes(row.size).into()
            };
            e.display_description = folder_contents_summary(row.file_count, row.dir_count).into();
        }
    }
}

/// Resolve one folder's recursive size through the shared
/// `folder_sizes` cache: the single-path analogue of the worker's
/// batch sweep, for Get Info's "Calculate" button. Honors the same
/// contract: a cached row counts only while its mtime matches the
/// folder's live mtime and it's younger than [`FOLDER_SIZE_TTL_SECS`];
/// otherwise the folder is walked and the result written through, so
/// the file list and Get Info share one source of truth (and one
/// invalidation path). Runs on the caller's background thread. A
/// cancelled walk returns its partial sum and is *not* cached.
pub(crate) fn folder_size_cached(
    path: &std::path::Path,
    db: Option<&Arc<Mutex<MetadataDb>>>,
    cancel: &AtomicBool,
) -> u64 {
    let mtime_unix = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let path_str = path.to_string_lossy();
    if let Some(db) = db {
        if let Ok(guard) = db.lock() {
            if let Ok(Some(rec)) = guard.get_folder_size(&path_str) {
                if rec.mtime_unix == mtime_unix
                    && now_unix().saturating_sub(rec.computed_at_unix) < FOLDER_SIZE_TTL_SECS
                {
                    return rec.size;
                }
            }
        }
    }
    let SubtreeTotals {
        apparent: size,
        files,
        dirs,
        ..
    } = recursive_totals(path, cancel);
    if cancel.load(Ordering::Relaxed) {
        return size;
    }
    // Write through the full record: this shares the `folder_sizes`
    // cache with the file-list worker, so the counts must land too or a
    // later cache hit there would read this row as "0 files".
    if let Some(db) = db {
        if let Ok(guard) = db.lock() {
            let _ = guard.upsert_folder_size(&FolderSizeRecord {
                path: path_str.into_owned(),
                mtime_unix,
                size,
                computed_at_unix: now_unix(),
                file_count: files,
                dir_count: dirs,
            });
        }
    }
    size
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_folder_results_are_emitted_in_bounded_batches() {
        let db = Arc::new(Mutex::new(MetadataDb::in_memory().unwrap()));
        let mut seeds = Vec::new();
        let computed_at = now_unix();
        for index in 0..(SIZE_ROWS_PER_BATCH * 2 + 3) {
            let path = PathBuf::from(format!("cached-folder-{index}"));
            let mtime_unix = 123;
            db.lock()
                .unwrap()
                .upsert_folder_size(&FolderSizeRecord {
                    path: path.to_string_lossy().into_owned(),
                    mtime_unix,
                    size: index as u64,
                    computed_at_unix: computed_at,
                    file_count: 1,
                    dir_count: 0,
                })
                .unwrap();
            seeds.push(SizeSeed {
                row_ix: index,
                node: ferail_core::NodeId::from_raw(index as u64 + 1).unwrap(),
                path,
                mtime_unix,
            });
        }

        let (tx, rx) = async_channel::bounded(16);
        run_worker(seeds, Some(db), false, Arc::new(AtomicBool::new(false)), tx);
        let mut batches = Vec::new();
        while let Ok(batch) = rx.try_recv() {
            batches.push(batch);
        }

        assert_eq!(batches.len(), 3);
        assert!(
            batches
                .iter()
                .all(|batch| batch.len() <= SIZE_ROWS_PER_BATCH)
        );
        assert_eq!(batches.iter().map(Vec::len).sum::<usize>(), 131);
    }
}
