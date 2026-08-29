use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

use ferail_ntfs::{parse_file_record, ByteReader as _, RecordParseOptions, TraversalOptions};

use crate::{file_identity, probe_fast_ntfs, scan_mft, RawVolumeReader};

pub fn run_diagnostic(path: &OsStr) -> i32 {
    match report(Path::new(path)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn report(path: &Path) -> Result<(), String> {
    let total_started = Instant::now();
    println!("Ferail Fast NTFS standalone diagnostic");
    println!("privacy: aggregate geometry, timings and counts only; no names or paths");

    let phase = Instant::now();
    let probe = probe_fast_ntfs(path).map_err(|error| format!("probe failed: {error}"))?;
    let (identity, root_record) =
        file_identity(path).map_err(|error| format!("root identity failed: {error}"))?;
    println!(
        "probe: ok in {} ms (volume_serial={}, root_record={})",
        phase.elapsed().as_millis(),
        identity.volume_serial,
        root_record
    );

    let phase = Instant::now();
    let reader = RawVolumeReader::open(&probe)
        .map_err(|error| format!("raw open failed (run elevated): {error}"))?;
    let geometry = reader.geometry();
    println!("raw_open: ok in {} ms", phase.elapsed().as_millis());
    println!(
        "geometry: sector={} cluster={} record={} mft_bytes={} total_clusters={} free_clusters={}",
        geometry.parsed.bytes_per_sector,
        geometry.parsed.bytes_per_cluster,
        geometry.parsed.bytes_per_file_record,
        geometry.mft_valid_bytes,
        geometry.total_clusters,
        geometry.free_clusters
    );

    let mut record_zero = vec![0u8; geometry.parsed.bytes_per_file_record as usize];
    let offset = geometry.parsed.mft_start_lcn * u64::from(geometry.parsed.bytes_per_cluster);
    reader
        .read_exact_at(offset, &mut record_zero)
        .map_err(|error| format!("record-zero read failed: {error}"))?;
    let record = parse_file_record(
        &record_zero,
        RecordParseOptions {
            bytes_per_sector: geometry.parsed.bytes_per_sector as usize,
            expected_record_number: Some(0),
            include_data_runs: true,
        },
    )
    .map_err(|error| format!("record-zero parse failed: {error}"))?;
    println!(
        "mft_bootstrap: names={} data_attributes={} attribute_lists={}",
        record.names.len(),
        record.data.len(),
        record.attribute_lists.len()
    );

    let scan_started = Instant::now();
    let mut last_report = Instant::now() - Duration::from_secs(1);
    let mut last_records = 0u64;
    let mut last_at = scan_started;
    let (index, scan) = scan_mft(
        &reader,
        || false,
        |progress| {
            let now = Instant::now();
            if now.duration_since(last_report) < Duration::from_millis(500)
                && progress.completed != progress.total
            {
                return;
            }
            let interval = now.duration_since(last_at).as_secs_f64().max(0.001);
            let rate = progress.completed.saturating_sub(last_records) as f64 / interval;
            let percent = if progress.total == 0 {
                0.0
            } else {
                progress.completed as f64 * 100.0 / progress.total as f64
            };
            println!(
                "mft_scan: phase={:?} {:5.1}% records={}/{} live={} corrupt={} rate={:.0} records/s elapsed={:.3}s",
                progress.phase,
                percent,
                progress.completed,
                progress.total,
                progress.live_records,
                progress.corrupt_records,
                rate,
                scan_started.elapsed().as_secs_f64()
            );
            last_report = now;
            last_at = now;
            last_records = progress.completed;
        },
    )
    .map_err(|error| format!("MFT scan failed: {error}"))?;
    let scan_elapsed = scan_started.elapsed();

    let (_, root) = index
        .file_by_record_number(root_record)
        .ok_or_else(|| "selected root record is absent from the index".to_string())?;
    let traversal_started = Instant::now();
    let mut batches = 0u64;
    let traversal = index
        .walk_subtree(
            root.reference(),
            TraversalOptions {
                root_id: (1 << 62) + 1,
                first_child_id: (1 << 62) + 2,
                batch_rows: 256,
                descend_packages: false,
            },
            || false,
            |batch| {
                if !batch.is_empty() {
                    batches = batches.saturating_add(1);
                }
            },
        )
        .map_err(|error| format!("subtree traversal failed: {error}"))?;
    let traversal_elapsed = traversal_started.elapsed();
    let stats = index.stats();

    println!("--- report ---");
    println!(
        "mft: records={} live={} corrupt={} indexed_files={} name_links={}",
        scan.records_seen,
        scan.live_records,
        scan.corrupt_records,
        index.files().len(),
        index.links().len()
    );
    println!(
        "subtree: rows={} batches={} logical_bytes={} allocated_bytes={} cycles_skipped={}",
        traversal.rows,
        batches,
        traversal.logical_bytes,
        traversal.allocated_bytes,
        traversal.cycles_skipped
    );
    println!(
        "skipped: deleted={} extensions={} stale_links={} dos_aliases={}",
        stats.skipped_deleted,
        stats.skipped_unlisted_extensions,
        stats.stale_or_missing_parent_links,
        stats.suppressed_dos_aliases
    );
    println!(
        "timing: mft_scan_and_index={:.3}s subtree={:.3}s total={:.3}s",
        scan_elapsed.as_secs_f64(),
        traversal_elapsed.as_secs_f64(),
        total_started.elapsed().as_secs_f64()
    );
    Ok(())
}
