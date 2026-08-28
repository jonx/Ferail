//! Background prefetch — magic byte sniffing + quarantine xattr lookup.
//!
//! Magic detection and quarantine lookup are fused into one viewport-owned
//! background pass. The worker hydrates those visible entries
//! from the metadata DB cache where possible, falls back to
//! `ferail_fs_native::{detect_magic, fetch_quarantine_info}` on
//! cache miss, writes through to the DB, and posts a bounded batch
//! of mutations back to the foreground executor. It never opens every file
//! merely because a directory was displayed.
//!
//! Background → foreground bridge: `cx.background_executor().spawn`
//! does the I/O off the main thread; `this.update(cx, …)` applies
//! the result on the foreground executor (gpui's NSApp thread).
//! The Shell entity's weak handle is passed in so a closed window
//! causes the update call to fail gracefully — no leak, no crash.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ferail_core::NodeId;
use ferail_fs_native::{detect_magic_info, fetch_quarantine_info};
use ferail_meta::{FileMetaRecord, MetadataDb};

use crate::file_list::FileListDelegate;

/// Process-wide master switch for the per-row file-detail scans that run
/// on every folder load (Settings → Performance). Covers both the magic
/// byte / quarantine sniff here *and* the Finder-tag xattr reads in
/// `Shell::refresh_file_list_tags_in_tab` — the two ungated per-row disk
/// costs. Off, the Format column falls back to extension-based types and
/// tag dots don't paint, but no per-row content/xattr I/O runs. Seeded
/// from persisted settings at startup; default on.
#[derive(Clone, Copy)]
pub struct FileDetailScan(pub bool);

impl gpui::Global for FileDetailScan {}

/// Whether per-row magic sniffing + Finder-tag reads are allowed to run.
pub fn file_detail_scan_enabled(cx: &gpui::App) -> bool {
    cx.try_global::<FileDetailScan>()
        .map(|g| g.0)
        .unwrap_or(true)
}

/// One row's worth of prefetched data, returned by the worker.
/// Keyed by `NodeId` — stable across re-sorts and re-enumerations —
/// so a batch can never land on the wrong row (raw indices shift
/// whenever the model changes under an in-flight pass).
pub(crate) struct PrefetchRow {
    /// Model row captured with the seed. Applying a viewport batch uses this
    /// direct index (plus the NodeId guard below), never a whole-model scan.
    row_ix: usize,
    pub(crate) node: NodeId,
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
#[derive(Clone)]
pub(crate) struct PrefetchSeed {
    pub(crate) row_ix: usize,
    pub(crate) node: NodeId,
    pub(crate) path: PathBuf,
    pub(crate) mtime_unix: i64,
    pub(crate) size: u64,
    /// Directories skip the magic/description derive entirely. Not
    /// just an optimisation: on cd9660 (and other legacy filesystems)
    /// `open()`+`read()` on a directory *succeeds* and returns raw
    /// directory records, so the sniffer would confidently label every
    /// folder "Binary". The folder-size worker owns folder
    /// descriptions; the Format column falls back to the kind label.
    pub(crate) is_dir: bool,
    pub(crate) details_loaded: bool,
}

/// Derive details for a small viewport-owned seed set without persisting its
/// paths. Flat View uses this instead of the whole-list pass: scrolling keeps
/// Format, Description, and quarantine badges functional while never opening
/// millions of off-screen files or retaining their paths in the metadata DB.
pub(crate) fn run_viewport(seeds: Vec<PrefetchSeed>) -> Vec<PrefetchRow> {
    run_worker(seeds, None, false, Arc::new(AtomicBool::new(false)))
}

/// Derive a bounded ordinary-listing viewport, using and updating the
/// persistent metadata cache. `force` preserves Refresh semantics: every row
/// is re-sniffed the first time that refreshed viewport reaches it.
pub(crate) fn run_cached_viewport(
    seeds: Vec<PrefetchSeed>,
    db: Option<Arc<Mutex<MetadataDb>>>,
    force: bool,
    cancel: Arc<AtomicBool>,
) -> Vec<PrefetchRow> {
    run_worker(seeds, db, force, cancel)
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
        if !force && seed.details_loaded {
            continue;
        }
        let path_str = seed.path.to_string_lossy().into_owned();

        // Try DB cache. A row whose stored mtime doesn't match the
        // live file describes different bytes — serving it would keep
        // a stale label/description (and quarantine state) forever
        // after an in-place edit, since nothing else re-validates.
        // Drop it and let the fresh derive write through (caches buy
        // latency, never correctness).
        let cached = db
            .as_ref()
            .and_then(|db| {
                let guard = db.lock().ok()?;
                guard.get_file(&path_str).ok().flatten()
            })
            .filter(|r| r.mtime_unix == seed.mtime_unix);

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
        // Track whether the description had to be derived this pass (vs.
        // served from the DB cache) — only a fresh derive does the extra
        // audio-tag read, since the cache already holds the media line.
        let desc_was_cached = cached_desc.is_some();
        let mut detected_magic = None;
        let (magic_label, mut description) = if seed.is_dir {
            // See `PrefetchSeed::is_dir` — never sniff a directory,
            // and don't resurrect a label an older build sniffed for
            // one. Empty values mean the Format column shows the kind
            // ("Folder") and the folder-size worker owns Description.
            (String::new(), String::new())
        } else {
            match (cached_label, cached_desc) {
                (Some(l), Some(d)) => (l, d),
                (cached_l, cached_d) => {
                    let info = detect_magic_info(&seed.path);
                    detected_magic = info.clone();
                    let label = cached_l.unwrap_or_else(|| {
                        info.as_ref()
                            .map(|i| i.magic_type.display_name().to_string())
                            .unwrap_or_default()
                    });
                    let desc = cached_d.unwrap_or_else(|| {
                        info.as_ref().map(|i| i.description()).unwrap_or_default()
                    });
                    (label, desc)
                }
            }
        };

        // Audio files: replace the generic magic description ("MPEG audio,
        // layer III") with the rich media line ("MP3 · stereo · 44.1 kHz ·
        // 192 kbps · 03:24"). Only on a fresh derive — the cached value is
        // already this line (both magic and media descriptions persist to
        // the same `description` field), so a revisit never re-reads tags.
        // lofty reads only the header/tag regions, not the whole file.
        if !desc_was_cached
            && ferail_fs_native::media::is_audio_candidate(&seed.path, detected_magic.as_ref())
        {
            if let Some(media_desc) = ferail_fs_native::media::read_media_tags_with_magic(
                &seed.path,
                detected_magic.as_ref(),
            )
            .map(|t| t.description())
            .filter(|d| !d.is_empty())
            {
                description = media_desc;
            }
        }

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
            row_ix: seed.row_ix,
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

/// Apply a viewport worker's batch directly to its captured row slots.
/// `FileListDelegate::detail_revision` rejects a structurally changed model;
/// the per-slot `NodeId` check below is the final identity guard. Work is
/// therefore O(batch), not O(the potentially multi-million-row listing).
pub(crate) fn apply_viewport_batch(delegate: &mut FileListDelegate, batch: Vec<PrefetchRow>) {
    for row in batch {
        let quarantine_change = {
            let Some(e) = delegate.entries.get_mut(row.row_ix) else {
                continue;
            };
            // A sort/filter/load may have moved another file into the captured
            // slot while I/O was running. Never apply across that identity guard;
            // the delegate's model revision schedules the live viewport again.
            if e.id != row.node {
                continue;
            }
            // Belt-and-suspenders (mirrors `format_label`'s folder guard):
            // a directory row never takes a magic label or description, even
            // if a stale path-keyed cache row arrives carrying one — the
            // Format column shows the kind ("Folder") and the folder-size
            // worker owns Description for directories.
            let is_dir = matches!(e.kind, ferail_core::EntryKind::Directory);
            if !is_dir && !row.magic_label.is_empty() {
                e.display_magic = row.magic_label.into();
            }
            if !is_dir && !row.description.is_empty() {
                e.display_description = row.description.into();
            }
            let was_quarantined = e.is_quarantined;
            e.is_quarantined = row.is_quarantined;
            e.details_loaded = true;
            // Provenance rides along so the preview pane can show
            // where a marked file came from without touching xattrs.
            e.quarantine = if row.is_quarantined {
                Some(Box::new(ferail_core::QuarantineDetails {
                    agent: row.quarantine_agent,
                    downloaded_iso: row.quarantine_iso,
                    where_from: row
                        .quarantine_where_from
                        .map(|s| s.lines().map(str::to_owned).collect())
                        .unwrap_or_default(),
                }))
            } else {
                None
            };
            (was_quarantined, row.is_quarantined)
        };
        delegate.note_quarantine_change(quarantine_change.0, quarantine_change.1);
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
    use std::sync::atomic::AtomicU64;

    /// AROS `exec.library` ELF prefix (64-bit LSB relocatable, aarch64,
    /// ELFOSABI_AROS) — enough bytes for the sniffer's header parse.
    const AROS_ELF: &[u8] = &[
        0x7f, b'E', b'L', b'F', 0x02, 0x01, 0x01, 0x0f, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x01, 0x00, 0xb7, 0x00, 0x01, 0x00, 0x00, 0x00,
    ];

    fn write_temp(bytes: &[u8]) -> PathBuf {
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "ferail-prefetch-test-{}-{}.bin",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn seed_for(path: &std::path::Path) -> PrefetchSeed {
        PrefetchSeed {
            row_ix: 0,
            node: NodeId::from(7u64),
            path: path.to_path_buf(),
            mtime_unix: 100,
            size: AROS_ELF.len() as u64,
            is_dir: false,
            // Fresh enumeration: no derived data carried on the entry, so
            // the worker always reaches the cache/sniff decision.
            details_loaded: false,
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
        db.lock()
            .unwrap()
            .upsert_file(&stale_record(&path_str))
            .unwrap();
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

    #[test]
    fn viewport_pass_derives_description_without_a_database() {
        let path = write_temp(AROS_ELF);
        let rows = run_viewport(vec![seed_for(&path)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].magic_label, "ELF executable");
        assert!(rows[0].description.ends_with("AROS"));
        let _ = std::fs::remove_file(&path);
    }

    /// The directory magic-label bug (TODO.md): a path-keyed cache row
    /// written while the path was a *file* must never resurface on a
    /// *directory* at that path. The worker's `is_dir` arm returns empty
    /// values regardless of what the poisoned row carries, cache-first
    /// or forced.
    #[test]
    fn directory_never_takes_a_cached_magic_label() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("ferail-prefetch-dir-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir_str = dir.to_string_lossy().into_owned();

        let db = Arc::new(Mutex::new(MetadataDb::in_memory().unwrap()));
        // Poisoned row: the path used to be a ZIP file, same mtime.
        let mut poisoned = stale_record(&dir_str);
        poisoned.magic_label = Some("ZIP archive".into());
        poisoned.description = Some("ZIP archive \u{b7} 3 files".into());
        db.lock().unwrap().upsert_file(&poisoned).unwrap();

        let seed = PrefetchSeed {
            is_dir: true,
            ..seed_for(&dir)
        };
        for force in [false, true] {
            let rows = run_worker(
                vec![seed.clone()],
                Some(db.clone()),
                force,
                Arc::new(AtomicBool::new(false)),
            );
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].magic_label, "", "force={force}");
            assert_eq!(rows[0].description, "", "force={force}");
        }

        let _ = std::fs::remove_dir(&dir);
    }
}
