//! Background folder-size worker — recursive sizes for the Size
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
//!   - **In-app work** invalidates exactly — every mutation deletes
//!     the cached size for the mutated path and all its ancestors
//!     (`Shell::invalidate_folder_size_ancestors`), so the next visit
//!     recomputes.
//!   - **External (3rd-party) work** is caught lazily — the TTL
//!     bounds how long a deep external change can hide, and an
//!     activation/forced pass (`start(.., force = true)`) recomputes
//!     visible rows when the user returns to the app.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use feraille_core::{EntryKind, NodeId};
use feraille_fs_native::{NativeFs, humanize_bytes, recursive_size};
use feraille_meta::{FolderSizeRecord, MetadataDb};
use gpui::Entity;

use crate::file_list::{FileListDelegate, SortColumn};
use crate::multi_table::TableState;
use crate::shell::{Shell, TabId};
use crate::tasks::{TaskKind, TaskRegistry};

/// How long a cached folder size is trusted before a revisit
/// recomputes it. This is the lazy safety net for deep *external*
/// changes the folder's own mtime can't reveal (an in-app mutation
/// invalidates the exact ancestor chain immediately and doesn't wait
/// for this). A recompute only happens when the folder is actually
/// loaded again, off the UI thread — so a longer TTL trades a little
/// staleness for fewer re-walks of big trees. Tune here, one place.
const FOLDER_SIZE_TTL_SECS: i64 = 10 * 60;

/// Snapshot used to seed the worker — `Send` copies of the bits it
/// needs, like `PrefetchSeed`.
struct SizeSeed {
    node: NodeId,
    path: PathBuf,
    /// The folder's own mtime at enumerate time; stored alongside
    /// the computed size so the next visit can validate the row.
    mtime_unix: i64,
}

/// One resolved folder size. Keyed by `NodeId`, not row index —
/// rows can re-sort while the walk is in flight.
struct SizeRow {
    node: NodeId,
    size: u64,
}

/// Spawn a folder-size pass over the current entries of `table`.
/// Called from `Shell::finish_directory_load_in_tab` after the
/// final enumeration snapshot lands. Cheap: returns immediately.
///
/// `cancel` is owned by the tab (`Tab::folder_size_cancel`) and
/// flipped on the next navigation/reload, which both stops the
/// in-flight `recursive_size` walk at its next dirent and marks
/// any partial sum as not-cacheable.
/// `force` bypasses the cache fast-path entirely — every folder is
/// re-walked and written through. Used by the activation refresh to
/// pick up deep external changes the cache still considers valid; the
/// normal directory-load path passes `false`.
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
    // Snapshot the directory rows on the foreground executor. The
    // worker gets `Send` data only. Symlinks-to-directories are
    // `EntryKind::Symlink` and stay excluded — we never follow them.
    let seeds: Vec<SizeSeed> = table
        .read(cx)
        .delegate()
        .entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::Directory))
        .filter_map(|e| {
            let path = fs.path_for(e.id)?;
            Some(SizeSeed {
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
        format!("Sizing {seed_count} folders\u{2026}"),
        false,
    );

    let (tx, rx) = async_channel::unbounded::<Vec<SizeRow>>();
    let worker_cancel = cancel.clone();
    cx.background_executor()
        .spawn(async move {
            run_worker(seeds, db, force, worker_cancel, tx);
        })
        .detach();

    let table_for_apply = table.clone();
    let tasks_for_end = tasks.clone();
    cx.spawn(async move |this, cx| {
        while let Ok(batch) = rx.recv().await {
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
                        state.refresh(cx);
                    });
                    cx.notify();
                    false
                })
                .unwrap_or(true);
            if stale {
                break;
            }
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
/// answers every mtime-valid folder in one cheap batch, then a
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
    let mut hits: Vec<SizeRow> = Vec::new();
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
                hits.push(SizeRow {
                    node: seed.node,
                    size: rec.size,
                })
            }
            _ => misses.push(seed),
        }
    }
    if !hits.is_empty() && tx.send_blocking(hits).is_err() {
        return;
    }

    for seed in misses {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let size = recursive_size(&seed.path, &cancel);
        // A cancelled walk returns a partial sum — neither cacheable
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
                });
            }
        }
        if tx
            .send_blocking(vec![SizeRow {
                node: seed.node,
                size,
            }])
            .is_err()
        {
            return;
        }
    }
}

/// Apply resolved sizes to the live `FileEntry` rows, matched by
/// `NodeId`. Rows that vanished (re-enumeration, filter) skip
/// silently. Sets both `size` (so Size-sorting orders folders
/// correctly) and the pre-formatted `display_size` (per the
/// no-alloc-on-paint contract).
fn apply_batch(delegate: &mut FileListDelegate, batch: Vec<SizeRow>) {
    // Sizes change → the status bar's cached totals are stale.
    delegate.invalidate_drag_snapshot();
    for row in batch {
        if let Some(e) = delegate.entries.iter_mut().find(|e| e.id == row.node) {
            e.size = row.size;
            // "0 B" on an empty folder reads like a measurement
            // error; Finder-style "--" reads as "nothing in here".
            e.display_size = if row.size == 0 {
                "--".to_string()
            } else {
                humanize_bytes(row.size)
            };
        }
    }
}

/// Resolve one folder's recursive size through the shared
/// `folder_sizes` cache — the single-path analogue of the worker's
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
    let size = recursive_size(path, cancel);
    if cancel.load(Ordering::Relaxed) {
        return size;
    }
    if let Some(db) = db {
        if let Ok(guard) = db.lock() {
            let _ = guard.upsert_folder_size(&FolderSizeRecord {
                path: path_str.into_owned(),
                mtime_unix,
                size,
                computed_at_unix: now_unix(),
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
