use ferail_ntfs::{
    bootstrap_mft, parse_file_record_in_place, ByteReader as _, CompactNtfsIndex, ErrorKind,
    IndexBuilder, NtfsError, Progress, RecordParseOptions, ScanPhase,
};
use rayon::prelude::*;

use crate::{RawVolumeError, RawVolumeReader, Result};

const SCAN_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const CORRUPT_RATIO_DENOMINATOR: u64 = 100;
const CORRUPT_ABSOLUTE_FLOOR: u64 = 1024;
const FIRST_USER_RECORD: u64 = 16;
const ROOT_DIRECTORY_RECORD: u64 = 5;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RawScanSummary {
    pub records_seen: u64,
    pub live_records: u64,
    pub corrupt_records: u64,
    pub total_records: u64,
}

pub fn scan_mft(
    reader: &RawVolumeReader,
    mut is_cancelled: impl FnMut() -> bool,
    mut on_progress: impl FnMut(Progress),
) -> Result<(CompactNtfsIndex, RawScanSummary)> {
    let geometry = reader.geometry();
    let mft = bootstrap_mft(reader, geometry.parsed, geometry.mft_valid_bytes)?;
    let record_bytes = u64::from(geometry.parsed.bytes_per_file_record);
    let record_bytes_usize = usize::try_from(record_bytes)
        .map_err(|_| RawVolumeError::Geometry("MFT record size does not fit memory"))?;
    let total_records = geometry.mft_valid_bytes / record_bytes;
    if total_records == 0 {
        return Err(RawVolumeError::Geometry("MFT contains no complete record"));
    }

    let mut summary = RawScanSummary {
        total_records,
        ..RawScanSummary::default()
    };
    let mut builder = IndexBuilder::default();
    let records_per_chunk = (SCAN_CHUNK_BYTES / record_bytes_usize).max(1);
    let mut buffer = vec![0u8; records_per_chunk * record_bytes_usize];
    let mut first_record = 0u64;
    while first_record < total_records {
        if is_cancelled() {
            return Err(RawVolumeError::Parser(NtfsError::cancelled("MFT scan")));
        }
        let remaining = total_records - first_record;
        let record_count = usize::try_from(remaining.min(records_per_chunk as u64))
            .map_err(|_| RawVolumeError::Geometry("MFT chunk count does not fit memory"))?;
        let byte_count = record_count
            .checked_mul(record_bytes_usize)
            .ok_or(RawVolumeError::Geometry("MFT chunk byte count overflow"))?;
        let offset = first_record.checked_mul(record_bytes).ok_or_else(|| {
            RawVolumeError::Parser(NtfsError::source(
                first_record,
                "MFT record offset overflow",
            ))
        })?;
        mft.read_exact_at(offset, &mut buffer[..byte_count])?;
        let parsed: Vec<_> = buffer[..byte_count]
            .par_chunks_exact_mut(record_bytes_usize)
            .enumerate()
            .map(|(index, bytes)| {
                if bytes.iter().all(|byte| *byte == 0) {
                    return None;
                }
                let record_number = first_record + index as u64;
                Some(parse_file_record_in_place(
                    bytes,
                    RecordParseOptions {
                        bytes_per_sector: geometry.parsed.bytes_per_sector as usize,
                        expected_record_number: Some(record_number),
                        include_data_runs: false,
                    },
                ))
            })
            .collect();

        for (index, parsed) in parsed.into_iter().enumerate() {
            if is_cancelled() {
                return Err(RawVolumeError::Parser(NtfsError::cancelled("MFT scan")));
            }
            let record_number = first_record + index as u64;
            summary.records_seen = summary.records_seen.saturating_add(1);
            match parsed {
                None => {}
                Some(Ok(record)) => {
                    if record.in_use {
                        summary.live_records = summary.live_records.saturating_add(1);
                    }
                    // NTFS metadata records are not returned by an ordinary
                    // directory enumeration. Keep only root #5 plus user
                    // records so Fast and Portable expose the same contract.
                    if record_number == ROOT_DIRECTORY_RECORD || record_number >= FIRST_USER_RECORD
                    {
                        builder.push(record)?;
                    }
                }
                Some(Err(error)) if error.kind == ErrorKind::SourceIo => {
                    return Err(RawVolumeError::Parser(error));
                }
                Some(Err(_)) => {
                    summary.corrupt_records = summary.corrupt_records.saturating_add(1);
                    if corrupt_threshold_exceeded(summary) {
                        return Err(RawVolumeError::Geometry(
                            "live MFT record corruption threshold exceeded",
                        ));
                    }
                }
            }
        }
        first_record = first_record.saturating_add(record_count as u64);
        on_progress(wire_progress(ScanPhase::ReadingRecords, summary));
    }
    on_progress(wire_progress(ScanPhase::BuildingIndex, summary));
    let index = builder.finish()?;
    Ok((index, summary))
}

fn wire_progress(phase: ScanPhase, summary: RawScanSummary) -> Progress {
    Progress {
        phase,
        completed: summary.records_seen,
        total: summary.total_records,
        live_records: summary.live_records,
        corrupt_records: summary.corrupt_records,
    }
}

fn corrupt_threshold_exceeded(summary: RawScanSummary) -> bool {
    summary.corrupt_records > CORRUPT_ABSOLUTE_FLOOR
        && summary
            .corrupt_records
            .saturating_mul(CORRUPT_RATIO_DENOMINATOR)
            > summary.records_seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corruption_gate_needs_both_floor_and_ratio() {
        assert!(!corrupt_threshold_exceeded(RawScanSummary {
            records_seen: 10_000,
            corrupt_records: 1000,
            ..RawScanSummary::default()
        }));
        assert!(!corrupt_threshold_exceeded(RawScanSummary {
            records_seen: 1_000_000,
            corrupt_records: 1025,
            ..RawScanSummary::default()
        }));
        assert!(corrupt_threshold_exceeded(RawScanSummary {
            records_seen: 10_000,
            corrupt_records: 1025,
            ..RawScanSummary::default()
        }));
    }
}
