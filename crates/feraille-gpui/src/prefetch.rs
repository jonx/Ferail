//! Background prefetch — magic byte sniffing + quarantine xattr lookup.
//!
//! Old-app pattern (`feraille-app::App::start_magic_prefetch` +
//! `start_quarantine_prefetch`) had two separate worker pipelines.
//! The new app fuses them into one cx.spawn call per `load_path`:
//! a background task iterates the active tab's entries, hydrates
//! from the metadata DB cache where possible, falls back to
//! `feraille_fs_native::{detect_magic, fetch_quarantine_info}` on
//! cache miss, writes through to the DB, and posts a single batch
//! of mutations back to the foreground executor.
//!
//! Background → foreground bridge: `cx.background_executor().spawn`
//! does the I/O off the main thread; `this.update(cx, …)` applies
//! the result on the foreground executor (gpui's NSApp thread).
//! The Shell entity's weak handle is passed in so a closed window
//! causes the update call to fail gracefully — no leak, no crash.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use feraille_core::FileEntry;
use feraille_fs_native::{detect_magic, fetch_quarantine_info, NativeFs};
use feraille_meta::{FileMetaRecord, MetadataDb};
use gpui::Entity;
use gpui_component::table::TableState;

use crate::file_list::FileListDelegate;
use crate::shell::Shell;
use crate::tasks::{TaskKind, TaskRegistry};

/// One row's worth of prefetched data, returned by the worker.
/// The index points into the snapshot the worker started with;
/// the foreground applier checks bounds in case the directory
/// changed under us before the batch arrived.
struct PrefetchRow {
    row_ix: usize,
    /// Same-or-newer snapshot of `display_magic`. Empty string when
    /// we couldn't determine.
    magic_label: String,
    /// Same-or-newer snapshot of `is_quarantined`.
    is_quarantined: bool,
}

/// Snapshot used to seed the worker. We can't capture `&FileEntry`
/// (not Send + lifetime); copy the bits the worker needs.
struct PrefetchSeed {
    row_ix: usize,
    path: PathBuf,
    mtime_unix: i64,
    size: u64,
    has_magic: bool,
    has_quarantine: bool,
}

/// Spawn a prefetch pass over the current entries. Called from
/// `Shell::load_path` after the table refresh. Cheap: returns
/// immediately; the worker runs on the background executor.
///
/// Field references (table, fs, db, weak handle) come in by
/// parameter rather than being looked up via `shell.read(cx)`,
/// because `load_path` runs inside its own `&mut self` borrow —
/// reading the Shell entity again from the same context would
/// trigger the gpui "cannot read while already being updated"
/// panic.
pub fn start(
    table: Entity<TableState<FileListDelegate>>,
    fs: Arc<NativeFs>,
    db: Option<Arc<Mutex<MetadataDb>>>,
    tasks: Rc<RefCell<TaskRegistry>>,
    shell_weak: gpui::WeakEntity<Shell>,
    cx: &mut gpui::Context<Shell>,
) {
    // Snapshot the entries on the foreground executor. The worker
    // gets `Send` data only.
    let seeds: Vec<PrefetchSeed> = table
        .read(cx)
        .delegate()
        .entries
        .iter()
        .enumerate()
        .filter_map(|(row_ix, e)| {
            let path = fs.path_for(e.id)?;
            Some(PrefetchSeed {
                row_ix,
                path,
                mtime_unix: e.mtime_unix,
                size: e.size,
                has_magic: !e.display_magic.is_empty(),
                has_quarantine: e.is_quarantined,
            })
        })
        .collect();

    if seeds.is_empty() {
        return;
    }
    let seed_count = seeds.len();
    crate::log_info!(90, "prefetch: starting for {seed_count} rows");

    // Stage 5.a: register a Task in the shared registry so the
    // status bar reflects the in-flight work. We fuse magic +
    // quarantine into one MagicPrefetch entry (the old app
    // registered two — separate workers); the label still mentions
    // both so the panel is honest about what's happening.
    let task_id = tasks.borrow_mut().begin(
        TaskKind::MagicPrefetch,
        format!("Indexing {seed_count} entries\u{2026}"),
        false,
    );

    let table_for_apply = table.clone();
    let tasks_for_end = tasks.clone();
    cx.spawn(async move |_this, cx| {
        let batch: Vec<PrefetchRow> = cx
            .background_executor()
            .spawn(async move { run_worker(seeds, db) })
            .await;
        let n = batch.len();
        crate::log_info!(90, "prefetch: worker returned {n} rows");
        if let Some(shell) = shell_weak.upgrade() {
            shell.update(cx, |_, cx| {
                if !batch.is_empty() {
                    table_for_apply.update(cx, |state, cx| {
                        apply_batch(state.delegate_mut(), batch);
                        state.refresh(cx);
                    });
                }
                tasks_for_end.borrow_mut().end(task_id);
                // Force the Shell to re-render. `state.refresh` on
                // the inner TableState alone doesn't propagate up
                // to the outer view tree in all cases.
                cx.notify();
            });
            crate::log_info!(90, "prefetch: apply ran");
        } else {
            // Shell already gone: still drop the task so its row
            // doesn't leak in the registry singleton (though in
            // practice the whole Shell is going away).
            tasks_for_end.borrow_mut().end(task_id);
            crate::log_warn!(90, "prefetch: shell entity gone before apply");
        }
    })
    .detach();
}

/// Body of the background pass. For each seed: cache lookup first
/// (cheap, hits the SQLite WAL); on miss, sniff + xattr-read; write
/// through to DB; produce a result row.
fn run_worker(
    seeds: Vec<PrefetchSeed>,
    db: Option<Arc<Mutex<MetadataDb>>>,
) -> Vec<PrefetchRow> {
    let mut out = Vec::with_capacity(seeds.len());
    for seed in seeds {
        // If FileEntry already carries the data (rare — only when
        // hydrate-from-DB on enumerate gets implemented), skip.
        if seed.has_magic && seed.has_quarantine {
            continue;
        }
        let path_str = seed.path.to_string_lossy().into_owned();

        // Try DB cache.
        let cached = db.as_ref().and_then(|db| {
            let guard = db.lock().ok()?;
            guard.get_file(&path_str).ok().flatten()
        });

        // Determine magic label.
        let magic_label = match cached.as_ref().and_then(|r| r.magic_label.clone()) {
            Some(s) => s,
            None => {
                let detected = detect_magic(&seed.path).unwrap_or("").to_string();
                detected
            }
        };

        // Determine quarantine state.
        let (quarantined, agent, iso, where_from) =
            match cached.as_ref().and_then(|r| r.quarantined) {
                Some(q) => (
                    q,
                    cached.as_ref().and_then(|r| r.quarantine_agent.clone()),
                    cached.as_ref().and_then(|r| r.quarantine_iso.clone()),
                    cached.as_ref().and_then(|r| r.quarantine_where_from.clone()),
                ),
                None => {
                    let info = fetch_quarantine_info(&seed.path);
                    let iso = info.downloaded_at.map(format_iso_minute);
                    let where_from = if info.where_from.is_empty() {
                        None
                    } else {
                        Some(info.where_from.join("\n"))
                    };
                    (info.quarantined, info.agent, iso, where_from)
                }
            };

        // Write through to DB.
        if let Some(db) = db.as_ref() {
            if let Ok(guard) = db.lock() {
                let rec = FileMetaRecord {
                    path: path_str.clone(),
                    mtime_unix: seed.mtime_unix,
                    size: seed.size,
                    magic_label: if magic_label.is_empty() {
                        None
                    } else {
                        Some(magic_label.clone())
                    },
                    partial_hash: cached.as_ref().and_then(|r| r.partial_hash.clone()),
                    full_hash: cached.as_ref().and_then(|r| r.full_hash.clone()),
                    mime: cached.as_ref().and_then(|r| r.mime.clone()),
                    quarantined: Some(quarantined),
                    quarantine_agent: agent,
                    quarantine_iso: iso,
                    quarantine_where_from: where_from,
                    indexed_at_unix: now_unix(),
                };
                let _ = guard.upsert_file(&rec);
            }
        }

        out.push(PrefetchRow {
            row_ix: seed.row_ix,
            magic_label,
            is_quarantined: quarantined,
        });
    }
    out
}

/// Apply the worker's batch back to the live `FileEntry` slice.
/// Bounds-checked per-row: if the directory was re-enumerated and
/// the row count shrank, stale indices skip silently.
fn apply_batch(delegate: &mut FileListDelegate, batch: Vec<PrefetchRow>) {
    let entries: &mut [FileEntry] = &mut delegate.entries;
    for row in batch {
        if let Some(e) = entries.get_mut(row.row_ix) {
            if !row.magic_label.is_empty() {
                e.display_magic = row.magic_label;
            }
            e.is_quarantined = row.is_quarantined;
        }
    }
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Minute-resolution ISO-8601 in UTC. Mirrors the old app's
/// formatter so DB rows are interoperable across binaries during
/// the migration.
fn format_iso_minute(unix: i64) -> String {
    // Crude UTC formatter: avoids pulling chrono in.
    let secs = unix.max(0) as u64;
    let days = secs / 86_400;
    let h = ((secs / 3600) % 24) as u32;
    let m = ((secs / 60) % 60) as u32;
    // Approx date from Unix epoch.
    let (y, mo, d) = epoch_days_to_ymd(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}Z")
}

/// Convert "days since 1970-01-01" into (year, month, day). Plain
/// proleptic Gregorian; good enough for ISO labels on quarantine
/// metadata.
fn epoch_days_to_ymd(days: i64) -> (i32, u32, u32) {
    let mut y: i32 = 1970;
    let mut d: i64 = days;
    loop {
        let y_days = if is_leap(y) { 366 } else { 365 };
        if d < y_days {
            break;
        }
        d -= y_days;
        y += 1;
    }
    while d < 0 {
        y -= 1;
        let y_days = if is_leap(y) { 366 } else { 365 };
        d += y_days;
    }
    let months = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo: u32 = 1;
    for &md in &months {
        if d < md {
            break;
        }
        d -= md;
        mo += 1;
    }
    (y, mo, (d + 1) as u32)
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
