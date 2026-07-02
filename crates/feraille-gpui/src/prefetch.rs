//! Background prefetch — magic byte sniffing + quarantine xattr lookup.
//!
//! Magic detection and quarantine lookup are fused into one cx.spawn
//! call per `load_path`:
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use feraille_core::{FileEntry, NodeId};
use feraille_fs_native::{NativeFs, detect_magic_info, fetch_quarantine_info};
use feraille_meta::{FileMetaRecord, MetadataDb};
use gpui::Entity;

use crate::file_list::FileListDelegate;
use crate::multi_table::TableState;
use crate::shell::{Shell, TabId};
use crate::tasks::{TaskKind, TaskRegistry};

/// One row's worth of prefetched data, returned by the worker.
/// Keyed by `NodeId` — stable across re-sorts and re-enumerations —
/// so a batch can never land on the wrong row (raw indices shift
/// whenever the model changes under an in-flight pass).
struct PrefetchRow {
    node: NodeId,
    /// Same-or-newer snapshot of `display_magic`. Empty string when
    /// we couldn't determine.
    magic_label: String,
    /// Same-or-newer snapshot of `display_description`. Empty when
    /// the type has no extra facts (or we couldn't determine).
    description: String,
    /// Same-or-newer snapshot of `is_quarantined`.
    is_quarantined: bool,
    /// Display-ready provenance for quarantined rows (agent,
    /// ISO download time, newline-joined where-from URLs) — feeds
    /// `FileEntry::quarantine` so the preview pane can show where a
    /// marked file came from without re-reading xattrs.
    quarantine_agent: Option<String>,
    quarantine_iso: Option<String>,
    quarantine_where_from: Option<String>,
}

/// Snapshot used to seed the worker. We can't capture `&FileEntry`
/// (not Send + lifetime); copy the bits the worker needs.
struct PrefetchSeed {
    node: NodeId,
    path: PathBuf,
    mtime_unix: i64,
    size: u64,
    has_magic: bool,
    has_description: bool,
    has_quarantine: bool,
}

/// Spawn a prefetch pass over the current entries. Called from
/// `Shell::load_path` after the table refresh. Cheap: returns
/// immediately; the worker runs on the background executor.
///
/// `force` bypasses the metadata-DB read cache for magic/description
/// so every row is re-sniffed from disk (Refresh). The fresh result
/// is still written through, so the cache self-heals. Quarantine
/// state stays cache-first either way.
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
    tab_id: TabId,
    generation: u64,
    cancel: Arc<AtomicBool>,
    force: bool,
    cx: &mut gpui::Context<Shell>,
) {
    // Snapshot the entries on the foreground executor. The worker
    // gets `Send` data only.
    let seeds: Vec<PrefetchSeed> = table
        .read(cx)
        .delegate()
        .entries
        .iter()
        .filter_map(|e| {
            let path = fs.path_for(e.id)?;
            Some(PrefetchSeed {
                node: e.id,
                path,
                mtime_unix: e.mtime_unix,
                size: e.size,
                has_magic: !e.display_magic.is_empty(),
                has_description: !e.display_description.is_empty(),
                has_quarantine: e.is_quarantined,
            })
        })
        .collect();

    if seeds.is_empty() {
        return;
    }
    let seed_count = seeds.len();
    crate::log_info!(90, "prefetch: starting for {seed_count} rows");

    // Register a Task in the shared registry so the status bar
    // reflects the in-flight work. We fuse magic + quarantine into
    // one MagicPrefetch entry; the label still mentions both so the
    // panel is honest about what's happening.
    let task_id = tasks.borrow_mut().begin(
        TaskKind::MagicPrefetch,
        format!("Indexing {seed_count} entries\u{2026}"),
        false,
    );

    let table_for_apply = table.clone();
    let tasks_for_end = tasks.clone();
    let worker_cancel = cancel.clone();
    cx.spawn(async move |_this, cx| {
        let batch: Vec<PrefetchRow> = cx
            .background_executor()
            .spawn(async move { run_worker(seeds, db, force, worker_cancel) })
            .await;
        let n = batch.len();
        crate::log_info!(90, "prefetch: worker returned {n} rows");
        if let Some(shell) = shell_weak.upgrade() {
            shell.update(cx, |shell, cx| {
                // Staleness rule shared with folder_sizes/search/dupes:
                // the tab may have closed or navigated on. Without this
                // guard a slow pass for the previous directory would
                // stamp its magic/quarantine data onto the new one
                // (quarantine badges on the wrong files).
                let fresh = shell
                    .tabs
                    .iter()
                    .any(|t| t.id == tab_id && t.load_generation == generation);
                if fresh && !batch.is_empty() {
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
///
/// `force` skips the magic/description read cache so every row is
/// re-sniffed from disk; the fresh values still write through.
fn run_worker(
    seeds: Vec<PrefetchSeed>,
    db: Option<Arc<Mutex<MetadataDb>>>,
    force: bool,
    cancel: Arc<AtomicBool>,
) -> Vec<PrefetchRow> {
    let mut out = Vec::with_capacity(seeds.len());
    // Write-through accumulates here and lands as ONE transaction at
    // the end (`upsert_files`) — per-row autocommit upserts serialized
    // a directory's worth of fsyncs behind the connection mutex.
    let mut pending_writes: Vec<FileMetaRecord> = Vec::new();
    for seed in seeds {
        // Navigation flipped the flag: the listing this pass was
        // sniffing for is gone, stop burning 4 KB reads per file.
        // The partial batch is still returned — rows are keyed by
        // NodeId and write-through already happened per row.
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        // If FileEntry already carries everything (rare — only when
        // hydrate-from-DB on enumerate gets implemented), skip — unless
        // a forced re-sniff is in effect.
        if !force && seed.has_magic && seed.has_description && seed.has_quarantine {
            continue;
        }
        let path_str = seed.path.to_string_lossy().into_owned();

        // Try DB cache.
        let cached = db.as_ref().and_then(|db| {
            let guard = db.lock().ok()?;
            guard.get_file(&path_str).ok().flatten()
        });

        // Determine magic label + description in one shot. The new
        // detector reads 4 KB once and returns a structured info
        // struct; the label and description are derived from that
        // same parse. Cached values short-circuit the I/O — but a
        // forced re-sniff ignores them so stale derived data heals.
        let cached_label = (!force)
            .then(|| cached.as_ref().and_then(|r| r.magic_label.clone()))
            .flatten();
        let cached_desc = (!force)
            .then(|| cached.as_ref().and_then(|r| r.description.clone()))
            .flatten();
        let (magic_label, description) = match (cached_label, cached_desc) {
            (Some(l), Some(d)) => (l, d),
            (cached_l, cached_d) => {
                let info = detect_magic_info(&seed.path);
                let label = cached_l.unwrap_or_else(|| {
                    info.as_ref()
                        .map(|i| i.magic_type.display_name().to_string())
                        .unwrap_or_default()
                });
                let desc = cached_d
                    .unwrap_or_else(|| info.as_ref().map(|i| i.description()).unwrap_or_default());
                (label, desc)
            }
        };

        // Determine quarantine state.
        let (quarantined, agent, iso, where_from) =
            match cached.as_ref().and_then(|r| r.quarantined) {
                Some(q) => (
                    q,
                    cached.as_ref().and_then(|r| r.quarantine_agent.clone()),
                    cached.as_ref().and_then(|r| r.quarantine_iso.clone()),
                    cached
                        .as_ref()
                        .and_then(|r| r.quarantine_where_from.clone()),
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

        // Stage the write-through; flushed in one transaction below.
        if db.is_some() {
            pending_writes.push(FileMetaRecord {
                path: path_str.clone(),
                mtime_unix: seed.mtime_unix,
                size: seed.size,
                magic_label: if magic_label.is_empty() {
                    None
                } else {
                    Some(magic_label.clone())
                },
                description: if description.is_empty() {
                    None
                } else {
                    Some(description.clone())
                },
                partial_hash: cached.as_ref().and_then(|r| r.partial_hash.clone()),
                full_hash: cached.as_ref().and_then(|r| r.full_hash.clone()),
                mime: cached.as_ref().and_then(|r| r.mime.clone()),
                quarantined: Some(quarantined),
                quarantine_agent: agent.clone(),
                quarantine_iso: iso.clone(),
                quarantine_where_from: where_from.clone(),
                indexed_at_unix: now_unix(),
            });
        }

        out.push(PrefetchRow {
            node: seed.node,
            magic_label,
            description,
            is_quarantined: quarantined,
            quarantine_agent: agent,
            quarantine_iso: iso,
            quarantine_where_from: where_from,
        });
    }
    // One transaction for the whole pass (cancel included — whatever
    // was derived is still valid per-row data worth keeping).
    if let Some(db) = db.as_ref() {
        if let Ok(guard) = db.lock() {
            if let Err(e) = guard.upsert_files(&pending_writes) {
                crate::log_warn!(90, "prefetch: write-through failed: {e}");
            }
        }
    }
    out
}

/// Apply the worker's batch back to the live `FileEntry` slice.
/// Keyed by `NodeId`: a row whose id no longer exists in the model
/// (deleted, filtered, re-enumerated away) skips silently, and a
/// re-sort between snapshot and apply can't misdeliver a result.
fn apply_batch(delegate: &mut FileListDelegate, batch: Vec<PrefetchRow>) {
    let mut by_node: std::collections::HashMap<NodeId, PrefetchRow> =
        batch.into_iter().map(|row| (row.node, row)).collect();
    let entries: &mut [FileEntry] = &mut delegate.entries;
    for e in entries.iter_mut() {
        let Some(row) = by_node.remove(&e.id) else {
            continue;
        };
        if !row.magic_label.is_empty() {
            e.display_magic = row.magic_label;
        }
        if !row.description.is_empty() {
            e.display_description = row.description;
        }
        e.is_quarantined = row.is_quarantined;
        // Provenance rides along so the preview pane can show
        // where a marked file came from without touching xattrs.
        e.quarantine = if row.is_quarantined {
            Some(feraille_core::QuarantineDetails {
                agent: row.quarantine_agent,
                downloaded_iso: row.quarantine_iso,
                where_from: row
                    .quarantine_where_from
                    .map(|s| s.lines().map(str::to_owned).collect())
                    .unwrap_or_default(),
            })
        } else {
            None
        };
    }
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Minute-resolution ISO-8601 in UTC, matching the format the
/// metadata DB stores.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// AROS `exec.library` ELF prefix (64-bit LSB relocatable, aarch64,
    /// ELFOSABI_AROS) — enough bytes for the sniffer's header parse.
    const AROS_ELF: &[u8] = &[
        0x7f, b'E', b'L', b'F', 0x02, 0x01, 0x01, 0x0f, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x01, 0x00, 0xb7, 0x00, 0x01, 0x00, 0x00, 0x00,
    ];

    fn write_temp(bytes: &[u8]) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("feraille-prefetch-test-{}.bin", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn seed_for(path: &std::path::Path) -> PrefetchSeed {
        PrefetchSeed {
            node: NodeId::from(7u64),
            path: path.to_path_buf(),
            mtime_unix: 100,
            size: AROS_ELF.len() as u64,
            // Fresh enumeration: no derived data carried on the entry,
            // so the worker always reaches the cache/sniff decision.
            has_magic: false,
            has_description: false,
            has_quarantine: false,
        }
    }

    fn stale_record(path: &str) -> FileMetaRecord {
        FileMetaRecord {
            path: path.to_string(),
            mtime_unix: 100,
            size: AROS_ELF.len() as u64,
            magic_label: Some("STALE label".into()),
            description: Some("STALE description".into()),
            partial_hash: None,
            full_hash: None,
            mime: None,
            quarantined: Some(false),
            quarantine_agent: None,
            quarantine_iso: None,
            quarantine_where_from: None,
            indexed_at_unix: 0,
        }
    }

    #[test]
    fn cache_first_returns_stale_force_resniffs_from_disk() {
        let path = write_temp(AROS_ELF);
        let path_str = path.to_string_lossy().into_owned();

        let db = Arc::new(Mutex::new(MetadataDb::in_memory().unwrap()));
        db.lock().unwrap().upsert_file(&stale_record(&path_str)).unwrap();
        let db_opt = Some(db.clone());

        // Cache-first (force = false): the stale DB row wins, no re-sniff.
        let cached = run_worker(
            vec![seed_for(&path)],
            db_opt.clone(),
            false,
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].magic_label, "STALE label");
        assert_eq!(cached[0].description, "STALE description");

        // Forced (force = true): the file is re-sniffed from disk, so the
        // real AROS facts replace the stale cache...
        let forced = run_worker(
            vec![seed_for(&path)],
            db_opt.clone(),
            true,
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(forced.len(), 1);
        assert_eq!(forced[0].magic_label, "ELF executable");
        assert_eq!(
            forced[0].description,
            "ELF \u{b7} 64-bit \u{b7} relocatable \u{b7} ARM64 \u{b7} AROS"
        );

        // ...and the write-through heals the cache, so a subsequent
        // cache-first pass now serves the fresh values.
        let healed = run_worker(
            vec![seed_for(&path)],
            db_opt,
            false,
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(healed[0].magic_label, "ELF executable");
        assert!(healed[0].description.ends_with("AROS"));

        let _ = std::fs::remove_file(&path);
    }
}
