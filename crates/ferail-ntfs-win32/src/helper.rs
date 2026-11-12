use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferail_ntfs::{
    decode_frame, encode_frame, Completion, DuMessage, ErrorKind, FailureCode, FileIdentity,
    FileReference, Progress, ScanPhase, SearchCompletion, SearchQuery, SearchRequest, StartRequest,
    TraversalOptions, PROTOCOL_VERSION,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
};

use crate::index_cache::{self, AcquiredIndex, IndexCache, IDLE_RELEASE};
use crate::pipe::{connect_client, never_cancelled, Pipe};
use crate::{file_identity, probe_fast_ntfs, FastNtfsProbe, RawVolumeError, RawVolumeReader};

const START_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const CANCEL_READ_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
/// Longest silence the helper allows itself during a request. The parent's
/// inactivity timeout is 30 s; a long MFT read, index build, journal replay
/// or a search with few matches re-sends the last Progress well before it.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const HEARTBEAT_POLL: Duration = Duration::from_millis(250);
const JANITOR_INTERVAL: Duration = Duration::from_secs(15);

pub fn helper_main() -> i32 {
    // The helper parses the MFT on every core. Run it below normal priority
    // so the app's own threads win whenever both want the CPU: a scan takes
    // the machine's spare capacity and never the window's frame time. I/O
    // priority is left alone; a request the user is waiting on still reads
    // at full speed.
    // SAFETY: the pseudo-handle from GetCurrentProcess is always valid.
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
    }
    match run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn run() -> Result<(), FailureCode> {
    let mut args = std::env::args();
    let _exe = args.next();
    let version = args.next().ok_or(FailureCode::Protocol)?;
    let pipe_name = args.next().ok_or(FailureCode::Protocol)?;
    if args.next().is_some()
        || version.parse::<u16>().ok() != Some(PROTOCOL_VERSION)
        || !valid_pipe_name(&pipe_name)
    {
        return Err(FailureCode::Protocol);
    }
    let pipe = connect_client(&pipe_name).map_err(|_| FailureCode::Protocol)?;
    let no_cancel = never_cancelled();
    let hello = encode_frame(
        0,
        &DuMessage::Hello {
            helper_pid: unsafe { GetCurrentProcessId() },
        },
    )
    .map_err(|_| FailureCode::Protocol)?;
    pipe.write_frame(&hello, Instant::now() + WRITE_TIMEOUT, &no_cancel)
        .map_err(|_| FailureCode::Protocol)?;

    let cache = Arc::new(Mutex::new(IndexCache::default()));
    let janitor_stop = Arc::new(AtomicBool::new(false));
    let janitor = spawn_janitor(cache.clone(), janitor_stop.clone());
    let result = serve(&pipe, &cache);
    janitor_stop.store(true, Ordering::Release);
    let _ = janitor.join();
    result
}

enum Request {
    DiskUsage(StartRequest),
    Search(SearchRequest),
}

fn serve(pipe: &Pipe, cache: &Mutex<IndexCache>) -> Result<(), FailureCode> {
    let no_cancel = never_cancelled();
    loop {
        let request_frame = pipe
            .read_frame(Instant::now() + START_TIMEOUT, &no_cancel)
            .map_err(|_| FailureCode::Protocol)?;
        let (request_id, message) =
            decode_frame(&request_frame, None).map_err(|_| FailureCode::Protocol)?;
        if request_id == 0 {
            return Err(FailureCode::Protocol);
        }
        let request = match message {
            DuMessage::Start(start) => Request::DiskUsage(start),
            DuMessage::Search(search) => Request::Search(search),
            DuMessage::Release => {
                index_cache::lock(cache).clear();
                continue;
            }
            // A Cancel that crossed the previous request's terminal frame.
            // The parent drains to that terminal frame, so nothing is owed.
            DuMessage::Cancel => continue,
            _ => return Err(FailureCode::Protocol),
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let stop_reader = Arc::new(AtomicBool::new(false));
        let reader = spawn_cancel_reader(
            pipe.clone(),
            request_id,
            cancel.clone(),
            stop_reader.clone(),
        );
        let sink = FrameSink::new(pipe.clone(), request_id);
        let heartbeat_stop = AtomicBool::new(false);
        let result = std::thread::scope(|scope| {
            scope.spawn(|| heartbeat(&sink, &heartbeat_stop, &cancel));
            let result = match &request {
                Request::DiskUsage(start) => execute_scan(&sink, start, &cancel, cache),
                Request::Search(search) => execute_search(&sink, search, &cancel, cache),
            };
            // The scope joins the heartbeat before returning, so it is gone
            // before the terminal frame is written.
            heartbeat_stop.store(true, Ordering::Release);
            result
        });
        // No subsequent request may race with the cancellation reader. Stop
        // it before emitting the terminal frame that releases the client-side
        // session lock.
        stop_reader.store(true, Ordering::Release);
        pipe.cancel_all();
        let _ = reader.join();
        match result {
            Ok(terminal) => sink.send(&terminal, &no_cancel)?,
            Err(code) => send_failed(pipe, request_id, code),
        }
    }
}

/// Serializes every frame of one request (the worker and the heartbeat
/// thread both write) and remembers what the heartbeat may repeat.
struct FrameSink {
    pipe: Pipe,
    request_id: u64,
    state: Mutex<SinkState>,
}

struct SinkState {
    last_write: Instant,
    ready: bool,
    failed: bool,
    last_progress: Option<Progress>,
}

impl FrameSink {
    fn new(pipe: Pipe, request_id: u64) -> Self {
        Self {
            pipe,
            request_id,
            state: Mutex::new(SinkState {
                last_write: Instant::now(),
                ready: false,
                failed: false,
                last_progress: None,
            }),
        }
    }

    /// Cancellation is checked before a frame starts, never during it: an
    /// aborted overlapped write could leave half a frame on the pipe, and the
    /// parent drains a cancelled request up to its terminal frame. A gone
    /// parent still fails the write at once (disconnect) or at the timeout.
    fn send(&self, message: &DuMessage, cancel: &AtomicBool) -> Result<(), FailureCode> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.failed || cancel.load(Ordering::Acquire) {
            state.failed = true;
            return Err(FailureCode::Cancelled);
        }
        let frame = encode_frame(self.request_id, message).map_err(|_| FailureCode::Protocol)?;
        if self
            .pipe
            .write_frame(&frame, Instant::now() + WRITE_TIMEOUT, &never_cancelled())
            .is_err()
        {
            state.failed = true;
            return Err(FailureCode::Cancelled);
        }
        state.last_write = Instant::now();
        match message {
            DuMessage::Ready => state.ready = true,
            DuMessage::Progress(progress) => state.last_progress = Some(*progress),
            _ => {}
        }
        Ok(())
    }

    fn failed(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .failed
    }

    /// The last Progress, when the request has been silent long enough.
    fn heartbeat_due(&self) -> Option<Progress> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (state.ready && !state.failed && state.last_write.elapsed() >= HEARTBEAT_INTERVAL)
            .then_some(state.last_progress)
            .flatten()
    }
}

fn heartbeat(sink: &FrameSink, stop: &AtomicBool, cancel: &AtomicBool) {
    while !stop.load(Ordering::Acquire) {
        std::thread::sleep(HEARTBEAT_POLL);
        if stop.load(Ordering::Acquire) || cancel.load(Ordering::Acquire) {
            continue;
        }
        if let Some(progress) = sink.heartbeat_due() {
            let _ = sink.send(&DuMessage::Progress(progress), cancel);
        }
    }
}

fn spawn_janitor(
    cache: Arc<Mutex<IndexCache>>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut last_sweep = Instant::now();
        while !stop.load(Ordering::Acquire) {
            std::thread::sleep(HEARTBEAT_POLL);
            if last_sweep.elapsed() >= JANITOR_INTERVAL {
                index_cache::lock(&cache).release_idle(IDLE_RELEASE);
                last_sweep = Instant::now();
            }
        }
    })
}

/// Shared validation for Start and Search: the helper re-resolves the root
/// itself and requires the same volume GUID and root file identity.
fn open_validated(
    volume_guid: &[u16],
    root: &[u16],
    expected: FileIdentity,
) -> Result<(FastNtfsProbe, RawVolumeReader, u64, u16), FailureCode> {
    let root = PathBuf::from(OsString::from_wide(root));
    let probe = probe_fast_ntfs(&root).map_err(map_raw_failure)?;
    if !utf16_ascii_eq(&probe.volume_guid, volume_guid) {
        return Err(FailureCode::Validation);
    }
    let (identity, root_record) = file_identity(&root).map_err(map_raw_failure)?;
    if identity != expected {
        return Err(FailureCode::Validation);
    }
    let root_sequence = identity_sequence(&identity);
    let reader = RawVolumeReader::open(&probe).map_err(map_raw_failure)?;
    Ok((probe, reader, root_record, root_sequence))
}

/// The NTFS sequence number in the high 16 bits of the 64-bit file
/// reference that `FILE_ID_INFO` carries in its low 8 bytes.
fn identity_sequence(identity: &FileIdentity) -> u16 {
    let reference = u64::from_le_bytes(identity.file_id[..8].try_into().expect("FILE_ID prefix"));
    (reference >> 48) as u16
}

/// Gets the index through the cache and resolves the root in it. A reused
/// index whose root record disagrees with the live root identity is thrown
/// away and rebuilt once; a fresh index that disagrees fails validation.
fn acquire_with_root(
    sink: &FrameSink,
    cache: &Mutex<IndexCache>,
    probe: &FastNtfsProbe,
    reader: &RawVolumeReader,
    cancel: &AtomicBool,
    root_record: u64,
    root_sequence: u16,
) -> Result<(AcquiredIndex, FileReference), FailureCode> {
    let is_cancelled = || cancel.load(Ordering::Acquire) || sink.failed();
    let mut force_rebuild = false;
    loop {
        let acquired = index_cache::acquire(
            cache,
            &probe.volume_guid,
            reader,
            force_rebuild,
            &is_cancelled,
            &mut |progress| {
                let _ = sink.send(&DuMessage::Progress(progress), cancel);
            },
        )
        .map_err(map_raw_failure)?;
        if sink.failed() {
            return Err(FailureCode::Cancelled);
        }
        let root = acquired
            .index
            .file_by_record_number(root_record)
            .filter(|(_, meta)| meta.sequence == root_sequence && meta.is_directory())
            .map(|(_, meta)| meta.reference());
        match root {
            Some(root) => return Ok((acquired, root)),
            None if acquired.reused && !force_rebuild => force_rebuild = true,
            None => return Err(FailureCode::Validation),
        }
    }
}

fn execute_scan(
    sink: &FrameSink,
    start: &StartRequest,
    cancel: &AtomicBool,
    cache: &Mutex<IndexCache>,
) -> Result<DuMessage, FailureCode> {
    let (probe, reader, root_record, root_sequence) =
        open_validated(&start.volume_guid, &start.root, start.root_identity)?;
    sink.send(&DuMessage::Ready, cancel)?;
    sink.send(
        &DuMessage::Progress(Progress {
            phase: ScanPhase::MappingMft,
            completed: 0,
            total: reader.geometry().mft_valid_bytes,
            live_records: 0,
            corrupt_records: 0,
        }),
        cancel,
    )?;
    let (acquired, root_reference) = acquire_with_root(
        sink,
        cache,
        &probe,
        &reader,
        cancel,
        root_record,
        root_sequence,
    )?;
    let index = &acquired.index;
    let raw_summary = acquired.raw_summary;
    sink.send(
        &DuMessage::Progress(Progress {
            phase: ScanPhase::Traversing,
            completed: 0,
            total: index.links().len() as u64,
            live_records: raw_summary.live_records,
            corrupt_records: raw_summary.corrupt_records,
        }),
        cancel,
    )?;
    let traversal = index
        .walk_subtree(
            root_reference,
            TraversalOptions {
                root_id: start.root_id,
                first_child_id: start.first_child_id,
                batch_rows: 256,
                descend_packages: start.descend_packages,
            },
            || cancel.load(Ordering::Acquire) || sink.failed(),
            |batch| {
                let _ = sink.send(&DuMessage::Batch(batch), cancel);
            },
        )
        .map_err(|error| {
            if error.kind == ErrorKind::Cancelled {
                FailureCode::Cancelled
            } else {
                FailureCode::CorruptVolume
            }
        })?;
    if sink.failed() {
        return Err(FailureCode::Cancelled);
    }
    index_cache::lock(cache).touch(&probe.volume_guid);
    let end_journal = reader.journal_position();
    let start_journal = acquired.reflects;
    let index_stats = index.stats();
    Ok(DuMessage::Complete(Completion {
        rows: traversal.rows,
        logical_bytes: traversal.logical_bytes,
        allocated_bytes: traversal.allocated_bytes,
        corrupt_records: raw_summary.corrupt_records,
        skipped_records: index_stats
            .skipped_deleted
            .saturating_add(index_stats.skipped_unlisted_extensions)
            .saturating_add(index_stats.stale_or_missing_parent_links),
        start_journal_id: start_journal.unwrap_or_default().0,
        start_next_usn: start_journal.unwrap_or_default().1,
        end_journal_id: end_journal.unwrap_or_default().0,
        end_next_usn: end_journal.unwrap_or_default().1,
        best_effort_live: best_effort_live(start_journal, end_journal),
    }))
}

fn execute_search(
    sink: &FrameSink,
    search: &SearchRequest,
    cancel: &AtomicBool,
    cache: &Mutex<IndexCache>,
) -> Result<DuMessage, FailureCode> {
    let (probe, reader, root_record, root_sequence) =
        open_validated(&search.volume_guid, &search.root, search.root_identity)?;
    sink.send(&DuMessage::Ready, cancel)?;
    sink.send(
        &DuMessage::Progress(Progress {
            phase: ScanPhase::MappingMft,
            completed: 0,
            total: reader.geometry().mft_valid_bytes,
            live_records: 0,
            corrupt_records: 0,
        }),
        cancel,
    )?;
    let (acquired, root_reference) = acquire_with_root(
        sink,
        cache,
        &probe,
        &reader,
        cancel,
        root_record,
        root_sequence,
    )?;
    let index = &acquired.index;
    sink.send(
        &DuMessage::Progress(Progress {
            phase: ScanPhase::Traversing,
            completed: 0,
            total: index.links().len() as u64,
            live_records: acquired.raw_summary.live_records,
            corrupt_records: acquired.raw_summary.corrupt_records,
        }),
        cancel,
    )?;
    let query = SearchQuery {
        terms: search.terms.clone(),
        match_path: search.match_path,
        include_hidden: search.include_hidden,
        descend_packages: search.descend_packages,
    };
    let summary = index
        .search_subtree(
            root_reference,
            &query,
            || cancel.load(Ordering::Acquire) || sink.failed(),
            |batch| {
                let _ = sink.send(&DuMessage::SearchBatch(batch), cancel);
            },
        )
        .map_err(|error| {
            if error.kind == ErrorKind::Cancelled {
                FailureCode::Cancelled
            } else {
                FailureCode::CorruptVolume
            }
        })?;
    if sink.failed() {
        return Err(FailureCode::Cancelled);
    }
    index_cache::lock(cache).touch(&probe.volume_guid);
    let end_journal = reader.journal_position();
    let start_journal = acquired.reflects;
    Ok(DuMessage::SearchComplete(SearchCompletion {
        matches: summary.matches,
        entries_visited: summary.entries_visited,
        directories_descended: summary.directories_descended,
        skipped_paths: summary.skipped_paths,
        cycles_skipped: summary.cycles_skipped,
        corrupt_records: acquired.raw_summary.corrupt_records,
        start_journal_id: start_journal.unwrap_or_default().0,
        start_next_usn: start_journal.unwrap_or_default().1,
        end_journal_id: end_journal.unwrap_or_default().0,
        end_next_usn: end_journal.unwrap_or_default().1,
        index_reused: acquired.reused,
        best_effort_live: best_effort_live(start_journal, end_journal),
    }))
}

/// No journal at either end means the snapshot cannot be proven current
/// either way; it is reported as live only when both ends differ.
fn best_effort_live(start: Option<(u64, i64)>, end: Option<(u64, i64)>) -> bool {
    start.zip(end).is_some_and(|(start, end)| start != end)
}

fn spawn_cancel_reader(
    pipe: Pipe,
    request_id: u64,
    cancel: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let no_cancel = never_cancelled();
        let result = pipe
            .read_frame(Instant::now() + CANCEL_READ_TIMEOUT, &no_cancel)
            .ok()
            .and_then(|frame| decode_frame(&frame, Some(request_id)).ok())
            .map(|(_, message)| message);
        if !stop.load(Ordering::Acquire) || result == Some(DuMessage::Cancel) {
            cancel.store(true, Ordering::Release);
        }
    })
}

fn send_failed(pipe: &Pipe, request_id: u64, code: FailureCode) {
    let no_cancel = never_cancelled();
    if let Ok(frame) = encode_frame(request_id, &DuMessage::Failed(code)) {
        let _ = pipe.write_frame(&frame, Instant::now() + Duration::from_secs(2), &no_cancel);
    }
}

fn map_raw_failure(error: RawVolumeError) -> FailureCode {
    match error {
        RawVolumeError::Unsupported(_) => FailureCode::Unsupported,
        RawVolumeError::InvalidPath(_) => FailureCode::Validation,
        RawVolumeError::Geometry(_) => FailureCode::CorruptVolume,
        RawVolumeError::Parser(error) if error.kind == ErrorKind::Cancelled => {
            FailureCode::Cancelled
        }
        RawVolumeError::Parser(_) => FailureCode::CorruptVolume,
        RawVolumeError::Win32(_, error) if error.code().0 as u32 == 0x8007_0005 => {
            FailureCode::AccessDenied
        }
        RawVolumeError::Win32(_, _) | RawVolumeError::Poisoned => FailureCode::Internal,
    }
}

fn valid_pipe_name(name: &str) -> bool {
    name.len() <= 128
        && name
            .strip_prefix(r"\\.\pipe\Ferail.FastNtfs.")
            .is_some_and(|suffix| {
                suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
}

fn utf16_ascii_eq(left: &[u16], right: &[u16]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            u8::try_from(*left)
                .ok()
                .zip(u8::try_from(*right).ok())
                .is_some_and(|(left, right)| left.eq_ignore_ascii_case(&right))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_accepts_only_our_random_pipe_shape() {
        assert!(valid_pipe_name(
            r"\\.\pipe\Ferail.FastNtfs.0123456789abcdef0123456789ABCDEF"
        ));
        assert!(!valid_pipe_name(r"\\.\pipe\other"));
        assert!(!valid_pipe_name(
            r"\\server\pipe\Ferail.FastNtfs.0123456789abcdef0123456789abcdef"
        ));
    }

    #[test]
    fn identity_sequence_reads_the_high_reference_bits() {
        let mut file_id = [0u8; 16];
        file_id[..8].copy_from_slice(&((7u64 << 48) | 1234).to_le_bytes());
        let identity = FileIdentity {
            volume_serial: 1,
            file_id,
        };
        assert_eq!(identity_sequence(&identity), 7);
    }

    #[test]
    fn live_flag_needs_both_journal_ends() {
        assert!(!best_effort_live(None, Some((1, 2))));
        assert!(!best_effort_live(Some((1, 2)), Some((1, 2))));
        assert!(best_effort_live(Some((1, 2)), Some((1, 3))));
    }

    #[test]
    fn volume_guid_comparison_is_ascii_only() {
        let left: Vec<u16> = r"\\?\Volume{ABCD}\".encode_utf16().collect();
        let right: Vec<u16> = r"\\?\volume{abcd}\".encode_utf16().collect();
        assert!(utf16_ascii_eq(&left, &right));
        assert!(!utf16_ascii_eq(&[0xd800], &[0xd800]));
    }
}
