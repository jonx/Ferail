#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    use ferail_ntfs::SizingMode;
    use ferail_ntfs_win32::{FastNtfsEvent, FastNtfsRequest, run_fast_ntfs};

    let mut args = std::env::args_os();
    let _exe = args.next();
    let root = args
        .next()
        .map(PathBuf::from)
        .ok_or("usage: ferail-ntfs-client-diag <local-ntfs-directory> [repeat-count]")?;
    let repeat = args
        .next()
        .map(|value| value.to_string_lossy().parse::<u64>())
        .transpose()?
        .unwrap_or(1);
    if repeat == 0 || repeat > 100 || args.next().is_some() {
        return Err("repeat-count must be between 1 and 100".into());
    }
    println!("Ferail Fast NTFS client/helper diagnostic");
    println!("privacy: aggregate timings and counts only; no names or paths");
    let cancel = AtomicBool::new(false);
    for run in 1..=repeat {
        let started = Instant::now();
        println!("--- run {run}/{repeat} ---");
        let request = FastNtfsRequest {
            root: root.clone(),
            sizing_mode: SizingMode::Apparent,
            descend_packages: false,
            root_id: (1 << 62) + 1,
            first_child_id: (1 << 62) + 2,
            request_id: run,
        };
        let mut batches = 0u64;
        let mut rows = 0u64;
        let mut completion = None;
        let mut last_report = Instant::now() - Duration::from_secs(1);
        let mut last_completed = 0u64;
        let mut last_at = started;
        run_fast_ntfs(request, &cancel, |event| match event {
            FastNtfsEvent::Ready => println!("ready: {:.3}s", started.elapsed().as_secs_f64()),
            FastNtfsEvent::Batch(batch) => {
                batches = batches.saturating_add(1);
                rows = rows.saturating_add(batch.len() as u64);
            }
            FastNtfsEvent::Progress(progress) => {
                let now = Instant::now();
                if now.duration_since(last_report) >= Duration::from_millis(500)
                    || progress.completed == progress.total
                {
                    let interval = now.duration_since(last_at).as_secs_f64().max(0.001);
                    let rate = progress.completed.saturating_sub(last_completed) as f64 / interval;
                    let percent = if progress.total == 0 {
                        0.0
                    } else {
                        progress.completed as f64 * 100.0 / progress.total as f64
                    };
                    println!(
                        "phase={:?} {:5.1}% completed={}/{} live={} corrupt={} rate={:.0}/s elapsed={:.3}s",
                        progress.phase,
                        percent,
                        progress.completed,
                        progress.total,
                        progress.live_records,
                        progress.corrupt_records,
                        rate,
                        started.elapsed().as_secs_f64()
                    );
                    last_report = now;
                    last_at = now;
                    last_completed = progress.completed;
                }
            }
            FastNtfsEvent::Complete(summary) => completion = Some(summary),
        })?;
        let summary = completion.ok_or("helper exited without completion")?;
        println!(
            "complete elapsed={:.3}s batches={batches} rows={rows} logical={} allocated={} corrupt={} skipped={} best_effort_live={}",
            started.elapsed().as_secs_f64(),
            summary.logical_bytes,
            summary.allocated_bytes,
            summary.corrupt_records,
            summary.skipped_records,
            summary.best_effort_live
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ferail-ntfs-client-diag is Windows-only");
    std::process::exit(2);
}
