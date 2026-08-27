#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    use ferail_ntfs::SizingMode;
    use ferail_ntfs_win32::{run_fast_ntfs, FastNtfsEvent, FastNtfsRequest};

    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: ferail-ntfs-client-diag <local-ntfs-directory>")?;
    let cancel = AtomicBool::new(false);
    let request = FastNtfsRequest {
        root,
        sizing_mode: SizingMode::Apparent,
        descend_packages: false,
        root_id: (1 << 62) + 1,
        first_child_id: (1 << 62) + 2,
        request_id: 1,
    };
    let mut batches = 0u64;
    let mut rows = 0u64;
    let mut completion = None;
    run_fast_ntfs(request, &cancel, |event| match event {
        FastNtfsEvent::Ready => println!("ready"),
        FastNtfsEvent::Batch(batch) => {
            batches = batches.saturating_add(1);
            rows = rows.saturating_add(batch.len() as u64);
        }
        FastNtfsEvent::Progress(progress) => println!(
            "phase={:?} completed={} total={} live={} corrupt={}",
            progress.phase,
            progress.completed,
            progress.total,
            progress.live_records,
            progress.corrupt_records
        ),
        FastNtfsEvent::Complete(summary) => completion = Some(summary),
    })?;
    let summary = completion.ok_or("helper exited without completion")?;
    println!(
        "complete batches={batches} rows={rows} logical={} allocated={} corrupt={} skipped={} best_effort_live={}",
        summary.logical_bytes,
        summary.allocated_bytes,
        summary.corrupt_records,
        summary.skipped_records,
        summary.best_effort_live
    );
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ferail-ntfs-client-diag is Windows-only");
    std::process::exit(2);
}
