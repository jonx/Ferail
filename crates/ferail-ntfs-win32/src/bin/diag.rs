#[cfg(windows)]
fn main() {
    use std::path::Path;

    use ferail_ntfs::{parse_file_record, ByteReader as _, RecordParseOptions};
    use ferail_ntfs_win32::{probe_fast_ntfs, scan_mft, RawVolumeReader};

    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: ferail-ntfs-diag <local-ntfs-path>");
        std::process::exit(2);
    };
    let probe = match probe_fast_ntfs(Path::new(&path)) {
        Ok(probe) => probe,
        Err(error) => fail(&format!("probe failed: {error}")),
    };
    let reader = match RawVolumeReader::open(&probe) {
        Ok(reader) => reader,
        Err(error) => fail(&format!("raw open failed (run elevated): {error}")),
    };
    let geometry = reader.geometry();
    let mut record_zero = vec![0u8; geometry.parsed.bytes_per_file_record as usize];
    let offset = geometry.parsed.mft_start_lcn * u64::from(geometry.parsed.bytes_per_cluster);
    if let Err(error) = reader.read_exact_at(offset, &mut record_zero) {
        fail(&format!("record-zero read failed: {error}"));
    }
    let record = match parse_file_record(
        &record_zero,
        RecordParseOptions {
            bytes_per_sector: geometry.parsed.bytes_per_sector as usize,
            expected_record_number: Some(0),
        },
    ) {
        Ok(record) => record,
        Err(error) => fail(&format!("record-zero parse failed: {error}")),
    };

    // Counts and geometry only. Never print the requested path, volume GUID,
    // names, runlists or record bytes.
    println!("bytes_per_sector={}", geometry.parsed.bytes_per_sector);
    println!("bytes_per_cluster={}", geometry.parsed.bytes_per_cluster);
    println!(
        "bytes_per_file_record={}",
        geometry.parsed.bytes_per_file_record
    );
    println!("mft_valid_bytes={}", geometry.mft_valid_bytes);
    println!("total_clusters={}", geometry.total_clusters);
    println!("free_clusters={}", geometry.free_clusters);
    println!("record_zero_names={}", record.names.len());
    println!("record_zero_data_attributes={}", record.data.len());
    println!(
        "record_zero_attribute_lists={}",
        record.attribute_lists.len()
    );
    let (index, scan) = match scan_mft(&reader, || false, |_| {}) {
        Ok(result) => result,
        Err(error) => fail(&format!("MFT scan failed: {error}")),
    };
    println!("records_seen={}", scan.records_seen);
    println!("live_records={}", scan.live_records);
    println!("corrupt_records={}", scan.corrupt_records);
    println!("indexed_base_records={}", index.files().len());
    println!("indexed_name_links={}", index.links().len());
}

#[cfg(windows)]
fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ferail-ntfs-diag is available only on Windows");
    std::process::exit(2);
}
