//! Per-volume cache of the compact MFT index inside the elevated helper.
//!
//! Disk Usage (`Start`) and Search (`Search`) both obtain their index through
//! [`acquire`]. A cached index is keyed by volume GUID, USN journal id and the
//! journal position it reflects, and is reused only when the journal proves
//! it is still exact:
//!
//! - journal unchanged and nothing left to settle: reuse as is;
//! - journal advanced: replay every entry since the cursor, re-read each
//!   touched MFT record and apply them through
//!   [`ferail_ntfs::CompactNtfsIndex::apply_changes`];
//! - no journal, a new journal id, purged entries (wrap), a malformed or
//!   failed journal read, too many touched records, an unreadable record or
//!   any change the overlay refuses: full rebuild from the MFT.
//!
//! The cache only ever changes latency. A volume without a journal is never
//! cached, because nothing could prove a later request sees the same state.
//!
//! Settling: raw volume reads can return an MFT record before NTFS flushed
//! its newest version. The cursor therefore keeps `replay_from` at the oldest
//! journal entry younger than [`SETTLE_WINDOW_SECONDS`], and those records are
//! re-read on every request until they age out. For a fresh build the same
//! rule needs the entries from just before the build started, which a
//! bounded scan of the journal head finds; when the journal is too large to
//! scan, the build is served once and not cached.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ferail_ntfs::{
    bootstrap_mft, freshness, parse_file_record, unix_seconds_to_filetime, ApplyOutcome,
    AttributeList, ByteReader as _, CompactNtfsIndex, FileRecord, FileReference, Freshness,
    IndexChange, JournalCursor, JournalState, MappedStream, NtfsError, Progress,
    RecordParseOptions, ReplayCollector, ScanPhase,
};

use crate::raw::DirectVolumeReader;
use crate::{scan_mft, RawScanSummary, RawVolumeError, RawVolumeReader, Result};

/// A cached index unused for this long is dropped by the helper's janitor.
pub const IDLE_RELEASE: Duration = Duration::from_secs(5 * 60);
/// At most this many volumes keep an index at once (least recently used
/// goes first).
pub const MAX_CACHED_VOLUMES: usize = 2;
/// Journal entries younger than this are re-read on the next request too.
pub const SETTLE_WINDOW_SECONDS: i64 = 60;
/// Above this many distinct touched records a rebuild is cheaper.
pub const MAX_REPLAY_RECORDS: usize = 100_000;
/// Most journal bytes one replay or one build-time head scan may read.
pub const MAX_JOURNAL_READ_BYTES: u64 = 256 * 1024 * 1024;
const JOURNAL_BUFFER_BYTES: usize = 1024 * 1024;
const PROGRESS_EVERY_RECORDS: usize = 1024;

struct CachedIndex {
    volume_key: Vec<u16>,
    cursor: JournalCursor,
    index: Arc<CompactNtfsIndex>,
    raw_summary: RawScanSummary,
    last_used: Instant,
}

/// Owned by the helper for its whole life; guarded by one mutex that is
/// only held for bookkeeping, never across volume I/O.
#[derive(Default)]
pub struct IndexCache {
    entries: Vec<CachedIndex>,
}

impl IndexCache {
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Drops entries idle for `idle` that no request is using.
    pub fn release_idle(&mut self, idle: Duration) {
        self.entries.retain(|entry| {
            entry.last_used.elapsed() < idle || Arc::strong_count(&entry.index) > 1
        });
    }

    pub fn touch(&mut self, volume_guid: &[u16]) {
        let key = volume_key(volume_guid);
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.volume_key == key)
        {
            entry.last_used = Instant::now();
        }
    }

    fn remove(&mut self, key: &[u16]) {
        self.entries.retain(|entry| entry.volume_key != key);
    }

    fn insert(&mut self, entry: CachedIndex) {
        self.remove(&entry.volume_key);
        self.entries.push(entry);
        while self.entries.len() > MAX_CACHED_VOLUMES {
            let oldest = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(index, _)| index)
                .expect("non-empty cache");
            self.entries.remove(oldest);
        }
    }
}

pub struct AcquiredIndex {
    pub index: Arc<CompactNtfsIndex>,
    /// The cached index served this request (possibly after a replay).
    pub reused: bool,
    pub raw_summary: RawScanSummary,
    /// The journal position the index reflects, when there is a journal.
    pub reflects: Option<(u64, i64)>,
}

/// Get-or-build. `force_rebuild` discards the cached entry first; callers use
/// it once when a reused index disagrees with the live root identity.
pub fn acquire(
    cache: &Mutex<IndexCache>,
    volume_guid: &[u16],
    reader: &RawVolumeReader,
    force_rebuild: bool,
    is_cancelled: &dyn Fn() -> bool,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<AcquiredIndex> {
    let key = volume_key(volume_guid);
    let cached = {
        let mut cache = lock(cache);
        if force_rebuild {
            cache.remove(&key);
        }
        cache
            .entries
            .iter_mut()
            .find(|entry| entry.volume_key == key)
            .map(|entry| {
                entry.last_used = Instant::now();
                (entry.cursor, entry.index.clone(), entry.raw_summary)
            })
    };

    if let Some((cursor, index, raw_summary)) = cached {
        match freshness(cursor, reader.journal_state()) {
            Freshness::Current => {
                return Ok(AcquiredIndex {
                    index,
                    reused: true,
                    raw_summary,
                    reflects: Some((cursor.journal_id, cursor.synced_usn)),
                });
            }
            Freshness::Replay { from, to } => {
                if let Some((updated, cursor)) = replay(
                    reader,
                    &index,
                    cursor.journal_id,
                    from,
                    to,
                    is_cancelled,
                    on_progress,
                )? {
                    let updated = Arc::new(updated);
                    drop(index);
                    lock(cache).insert(CachedIndex {
                        volume_key: key,
                        cursor,
                        index: updated.clone(),
                        raw_summary,
                        last_used: Instant::now(),
                    });
                    return Ok(AcquiredIndex {
                        index: updated,
                        reused: true,
                        raw_summary,
                        reflects: Some((cursor.journal_id, cursor.synced_usn)),
                    });
                }
            }
            Freshness::Rebuild(_) => {}
        }
    }

    // Free the stale index before reading the MFT again so the helper never
    // holds two full-volume indexes while building.
    lock(cache).remove(&key);
    let before = reader.journal_state();
    let build_started = unix_now();
    let (index, raw_summary) = scan_mft(reader, is_cancelled, &mut *on_progress)?;
    let index = Arc::new(index);
    let reflects = before.map(|state| (state.journal_id, state.next_usn));
    if let Some(state) = before {
        let cutoff = unix_seconds_to_filetime(build_started.saturating_sub(SETTLE_WINDOW_SECONDS));
        if let Some(replay_from) = head_replay_start(reader, state, cutoff, is_cancelled)? {
            lock(cache).insert(CachedIndex {
                volume_key: key,
                cursor: JournalCursor {
                    journal_id: state.journal_id,
                    synced_usn: state.next_usn,
                    replay_from,
                },
                index: index.clone(),
                raw_summary,
                last_used: Instant::now(),
            });
        }
    }
    Ok(AcquiredIndex {
        index,
        reused: false,
        raw_summary,
        reflects,
    })
}

/// Finds the first journal entry at or after `cutoff` that precedes the
/// build. `None` means the head could not be scanned within the byte cap
/// (or the journal could not be read): the build must not be cached.
fn head_replay_start(
    reader: &RawVolumeReader,
    state: JournalState,
    cutoff: i64,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<i64>> {
    let oldest = state.oldest_readable();
    if state.next_usn < oldest {
        return Ok(None);
    }
    if u64::try_from(state.next_usn - oldest).unwrap_or(u64::MAX) > MAX_JOURNAL_READ_BYTES {
        return Ok(None);
    }
    let mut buffer = vec![0u8; JOURNAL_BUFFER_BYTES];
    let mut start = oldest;
    while start < state.next_usn {
        if is_cancelled() {
            return Err(RawVolumeError::Parser(NtfsError::cancelled("journal head")));
        }
        let Ok(page) = reader.read_journal_page(state.journal_id, start, &mut buffer) else {
            return Ok(None);
        };
        for entry in &page.entries {
            if entry.usn >= state.next_usn {
                return Ok(Some(state.next_usn));
            }
            if entry.timestamp >= cutoff {
                return Ok(Some(entry.usn));
            }
        }
        if page.next_usn <= start {
            break;
        }
        start = page.next_usn;
    }
    Ok(Some(state.next_usn))
}

/// Replays `[from, to)` and returns the updated index and cursor, or `None`
/// when a rebuild is required.
fn replay(
    reader: &RawVolumeReader,
    index: &CompactNtfsIndex,
    journal_id: u64,
    from: i64,
    to: i64,
    is_cancelled: &dyn Fn() -> bool,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<Option<(CompactNtfsIndex, JournalCursor)>> {
    let cutoff = unix_seconds_to_filetime(unix_now().saturating_sub(SETTLE_WINDOW_SECONDS));
    let mut collector = ReplayCollector::new(cutoff, MAX_REPLAY_RECORDS);
    let mut buffer = vec![0u8; JOURNAL_BUFFER_BYTES];
    let mut start = from;
    let mut read_bytes = 0u64;
    while start < to {
        if is_cancelled() {
            return Err(RawVolumeError::Parser(NtfsError::cancelled(
                "journal replay",
            )));
        }
        let Ok(page) = reader.read_journal_page(journal_id, start, &mut buffer) else {
            return Ok(None);
        };
        for entry in &page.entries {
            collector.observe(entry);
        }
        if collector.overflowed() {
            return Ok(None);
        }
        if page.next_usn <= start {
            break;
        }
        read_bytes = read_bytes.saturating_add((page.next_usn - start) as u64);
        if read_bytes > MAX_JOURNAL_READ_BYTES {
            return Ok(None);
        }
        start = page.next_usn;
    }
    let synced_usn = start;
    let Some(plan) = collector.finish(synced_usn) else {
        return Ok(None);
    };

    let geometry = reader.geometry();
    let direct = DirectVolumeReader(reader);
    let Ok(mft) = bootstrap_mft(&direct, geometry.parsed, geometry.mft_valid_bytes) else {
        return Ok(None);
    };
    let total = plan.records.len() as u64;
    let mut changes = Vec::with_capacity(plan.records.len());
    for (done, record) in plan.records.iter().copied().enumerate() {
        if is_cancelled() {
            return Err(RawVolumeError::Parser(NtfsError::cancelled(
                "record re-read",
            )));
        }
        if done % PROGRESS_EVERY_RECORDS == 0 {
            on_progress(Progress {
                phase: ScanPhase::ReadingRecords,
                completed: done as u64,
                total,
                live_records: 0,
                corrupt_records: 0,
            });
        }
        let Some(change) = reread(
            &mft,
            geometry.parsed.bytes_per_file_record,
            geometry.parsed.bytes_per_sector,
            record,
        ) else {
            return Ok(None);
        };
        changes.push(change);
    }
    on_progress(Progress {
        phase: ScanPhase::BuildingIndex,
        completed: total,
        total,
        live_records: 0,
        corrupt_records: 0,
    });
    match index.apply_changes(&changes) {
        ApplyOutcome::Applied(updated) => Ok(Some((
            updated,
            JournalCursor {
                journal_id,
                synced_usn,
                replay_from: plan.next_replay_from,
            },
        ))),
        ApplyOutcome::RebuildRequired(_) => Ok(None),
    }
}

/// Re-reads one base record and the extension records its resident
/// attribute list names. `None` (rebuild) on any read or parse failure.
fn reread(
    mft: &MappedStream<'_, DirectVolumeReader<'_>>,
    record_bytes: u32,
    sector_bytes: u32,
    record: u64,
) -> Option<IndexChange> {
    let base = read_record(mft, record_bytes, sector_bytes, record)?;
    if !base.in_use || base.base_reference.is_some() {
        // Free, or reused as another file's extension: no longer a base file.
        return Some(IndexChange::Reread {
            base: vacant(record, base.sequence),
            extensions: Vec::new(),
        });
    }
    let own = FileReference {
        record: base.record_number,
        sequence: base.sequence,
    };
    let mut wanted: Vec<u64> = Vec::new();
    for list in &base.attribute_lists {
        if let AttributeList::Resident(entries) = list {
            for entry in entries {
                if entry.record != own && !wanted.contains(&entry.record.record) {
                    wanted.push(entry.record.record);
                }
            }
        }
        // A non-resident list is left for the overlay to refuse.
    }
    let mut extensions = Vec::with_capacity(wanted.len());
    for extension in wanted {
        extensions.push(read_record(mft, record_bytes, sector_bytes, extension)?);
    }
    Some(IndexChange::Reread { base, extensions })
}

fn read_record(
    mft: &MappedStream<'_, DirectVolumeReader<'_>>,
    record_bytes: u32,
    sector_bytes: u32,
    record: u64,
) -> Option<FileRecord> {
    let size = u64::from(record_bytes);
    let offset = record.checked_mul(size)?;
    if offset.checked_add(size)? > mft.len() {
        return None;
    }
    let mut bytes = vec![0u8; record_bytes as usize];
    mft.read_exact_at(offset, &mut bytes).ok()?;
    if bytes.iter().all(|byte| *byte == 0) {
        return Some(vacant(record, 0));
    }
    parse_file_record(
        &bytes,
        RecordParseOptions {
            bytes_per_sector: sector_bytes as usize,
            expected_record_number: Some(record),
            include_data_runs: false,
        },
    )
    .ok()
}

fn vacant(record: u64, sequence: u16) -> FileRecord {
    FileRecord {
        record_number: record,
        sequence,
        hard_link_count: 0,
        in_use: false,
        directory: false,
        reparse_point: false,
        reparse_tag: None,
        base_reference: None,
        modified_ticks: None,
        created_ticks: None,
        file_attributes: None,
        names: Vec::new(),
        data: Vec::new(),
        attribute_lists: Vec::new(),
        named_data_attributes: 0,
    }
}

fn volume_key(volume_guid: &[u16]) -> Vec<u16> {
    volume_guid
        .iter()
        .map(|unit| match u8::try_from(*unit) {
            Ok(byte) => u16::from(byte.to_ascii_lowercase()),
            Err(_) => *unit,
        })
        .collect()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

pub fn lock(cache: &Mutex<IndexCache>) -> MutexGuard<'_, IndexCache> {
    cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
