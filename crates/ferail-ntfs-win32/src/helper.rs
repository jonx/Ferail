use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferail_ntfs::{
    decode_frame, encode_frame, Completion, DuMessage, ErrorKind, FailureCode, Progress, ScanPhase,
    StartRequest, TraversalOptions, PROTOCOL_VERSION,
};
use windows::Win32::System::Threading::GetCurrentProcessId;

use crate::pipe::{connect_client, never_cancelled, Pipe};
use crate::{file_identity, probe_fast_ntfs, scan_mft, RawVolumeError, RawVolumeReader};

const START_TIMEOUT: Duration = Duration::from_secs(120);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const CANCEL_READ_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

pub fn helper_main() -> i32 {
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

    let start_frame = pipe
        .read_frame(Instant::now() + START_TIMEOUT, &no_cancel)
        .map_err(|_| FailureCode::Protocol)?;
    let (request_id, message) =
        decode_frame(&start_frame, None).map_err(|_| FailureCode::Protocol)?;
    if request_id == 0 {
        return Err(FailureCode::Protocol);
    }
    let DuMessage::Start(start) = message else {
        return Err(FailureCode::Protocol);
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let stop_reader = Arc::new(AtomicBool::new(false));
    let reader = spawn_cancel_reader(
        pipe.clone(),
        request_id,
        cancel.clone(),
        stop_reader.clone(),
    );
    let result = execute_scan(&pipe, request_id, &start, &cancel);
    stop_reader.store(true, Ordering::Release);
    pipe.cancel_all();
    let _ = reader.join();
    if let Err(code) = result {
        send_failed(&pipe, request_id, code);
        return Err(code);
    }
    Ok(())
}

fn execute_scan(
    pipe: &Pipe,
    request_id: u64,
    start: &StartRequest,
    cancel: &AtomicBool,
) -> Result<(), FailureCode> {
    let root = PathBuf::from(OsString::from_wide(&start.root));
    let probe = probe_fast_ntfs(&root).map_err(map_raw_failure)?;
    if !utf16_ascii_eq(&probe.volume_guid, &start.volume_guid) {
        return Err(FailureCode::Validation);
    }
    let (identity, root_record) = file_identity(&root).map_err(map_raw_failure)?;
    if identity != start.root_identity {
        return Err(FailureCode::Validation);
    }
    let reader = RawVolumeReader::open(&probe).map_err(map_raw_failure)?;
    send(pipe, request_id, &DuMessage::Ready, cancel)?;
    send(
        pipe,
        request_id,
        &DuMessage::Progress(Progress {
            phase: ScanPhase::MappingMft,
            completed: 0,
            total: reader.geometry().mft_valid_bytes,
            live_records: 0,
            corrupt_records: 0,
        }),
        cancel,
    )?;
    let start_journal = reader.journal_position();
    let write_failed = AtomicBool::new(false);
    let (index, raw_summary) = scan_mft(
        &reader,
        || cancel.load(Ordering::Acquire) || write_failed.load(Ordering::Acquire),
        |progress| {
            if send(
                pipe,
                request_id,
                &DuMessage::Progress(Progress {
                    phase: ScanPhase::ReadingRecords,
                    completed: progress.records_seen,
                    total: progress.total_records,
                    live_records: progress.live_records,
                    corrupt_records: progress.corrupt_records,
                }),
                cancel,
            )
            .is_err()
            {
                write_failed.store(true, Ordering::Release);
            }
        },
    )
    .map_err(map_raw_failure)?;
    if write_failed.load(Ordering::Acquire) {
        return Err(FailureCode::Cancelled);
    }
    let (_, root_meta) = index
        .file_by_record_number(root_record)
        .ok_or(FailureCode::Validation)?;
    let root_reference = root_meta.reference();
    let traversal = index
        .walk_subtree(
            root_reference,
            TraversalOptions {
                root_id: start.root_id,
                first_child_id: start.first_child_id,
                batch_rows: 256,
                descend_packages: start.descend_packages,
            },
            || cancel.load(Ordering::Acquire) || write_failed.load(Ordering::Acquire),
            |batch| {
                if send(pipe, request_id, &DuMessage::Batch(batch), cancel).is_err() {
                    write_failed.store(true, Ordering::Release);
                }
            },
        )
        .map_err(|error| {
            if error.kind == ErrorKind::Cancelled {
                FailureCode::Cancelled
            } else {
                FailureCode::CorruptVolume
            }
        })?;
    if write_failed.load(Ordering::Acquire) {
        return Err(FailureCode::Cancelled);
    }
    let end_journal = reader.journal_position();
    let best_effort_live = start_journal
        .zip(end_journal)
        .is_some_and(|(start, end)| start != end);
    let (start_journal_id, start_next_usn) = start_journal.unwrap_or_default();
    let (end_journal_id, end_next_usn) = end_journal.unwrap_or_default();
    let index_stats = index.stats();
    send(
        pipe,
        request_id,
        &DuMessage::Complete(Completion {
            rows: traversal.rows,
            logical_bytes: traversal.logical_bytes,
            allocated_bytes: traversal.allocated_bytes,
            corrupt_records: raw_summary.corrupt_records,
            skipped_records: index_stats
                .skipped_deleted
                .saturating_add(index_stats.skipped_unlisted_extensions)
                .saturating_add(index_stats.stale_or_missing_parent_links),
            start_journal_id,
            start_next_usn,
            end_journal_id,
            end_next_usn,
            best_effort_live,
        }),
        cancel,
    )
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

fn send(
    pipe: &Pipe,
    request_id: u64,
    message: &DuMessage,
    cancel: &AtomicBool,
) -> Result<(), FailureCode> {
    let frame = encode_frame(request_id, message).map_err(|_| FailureCode::Protocol)?;
    pipe.write_frame(&frame, Instant::now() + WRITE_TIMEOUT, cancel)
        .map_err(|_| FailureCode::Cancelled)
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
    fn volume_guid_comparison_is_ascii_only() {
        let left: Vec<u16> = r"\\?\Volume{ABCD}\".encode_utf16().collect();
        let right: Vec<u16> = r"\\?\volume{abcd}\".encode_utf16().collect();
        assert!(utf16_ascii_eq(&left, &right));
        assert!(!utf16_ascii_eq(&[0xd800], &[0xd800]));
    }
}
