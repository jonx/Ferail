use ferail_ntfs::{
    bootstrap_mft, parse_file_record, ByteReader as _, CompactNtfsIndex, ErrorKind, IndexBuilder,
    NtfsError, RecordParseOptions,
};

use crate::{RawVolumeError, RawVolumeReader, Result};

const PROGRESS_RECORD_INTERVAL: u64 = 4096;
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
    mut on_progress: impl FnMut(RawScanSummary),
) -> Result<(CompactNtfsIndex, RawScanSummary)> {
    let geometry = reader.geometry();
    let mft = bootstrap_mft(reader, geometry.parsed, geometry.mft_valid_bytes)?;
    let record_bytes = u64::from(geometry.parsed.bytes_per_file_record);
    let total_records = geometry.mft_valid_bytes / record_bytes;
    if total_records == 0 {
        return Err(RawVolumeError::Geometry("MFT contains no complete record"));
    }

    let mut summary = RawScanSummary {
        total_records,
        ..RawScanSummary::default()
    };
    let mut builder = IndexBuilder::default();
    let mut buffer = vec![0u8; record_bytes as usize];
    for record_number in 0..total_records {
        if is_cancelled() {
            return Err(RawVolumeError::Parser(NtfsError::cancelled("MFT scan")));
        }
        let offset = record_number.checked_mul(record_bytes).ok_or_else(|| {
            RawVolumeError::Parser(NtfsError::source(
                record_number,
                "MFT record offset overflow",
            ))
        })?;
        mft.read_exact_at(offset, &mut buffer)?;
        summary.records_seen = summary.records_seen.saturating_add(1);
        if buffer.iter().all(|byte| *byte == 0) {
            if summary.records_seen % PROGRESS_RECORD_INTERVAL == 0 {
                on_progress(summary);
            }
            continue;
        }
        match parse_file_record(
            &buffer,
            RecordParseOptions {
                bytes_per_sector: geometry.parsed.bytes_per_sector as usize,
                expected_record_number: Some(record_number),
            },
        ) {
            Ok(record) => {
                if record.in_use {
                    summary.live_records = summary.live_records.saturating_add(1);
                }
                // NTFS metadata records are not returned by an ordinary
                // directory enumeration. Keep only root #5 plus user records
                // so Fast and Portable expose the same visible contract.
                if record_number == ROOT_DIRECTORY_RECORD || record_number >= FIRST_USER_RECORD {
                    builder.push(record)?;
                }
            }
            Err(error) if error.kind == ErrorKind::SourceIo => {
                return Err(RawVolumeError::Parser(error));
            }
            Err(_) => {
                summary.corrupt_records = summary.corrupt_records.saturating_add(1);
                if corrupt_threshold_exceeded(summary) {
                    return Err(RawVolumeError::Geometry(
                        "live MFT record corruption threshold exceeded",
                    ));
                }
            }
        }
        if summary.records_seen % PROGRESS_RECORD_INTERVAL == 0 {
            on_progress(summary);
        }
    }
    let index = builder.finish()?;
    on_progress(summary);
    Ok((index, summary))
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
