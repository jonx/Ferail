//! App-footprint sampler behind the status bar's stats segment
//! (`up 3d 4h · CPU 6.8% · MEM 184 MB · 58 fps`).
//!
//! Everything shown is **app-centric** — what Ferail itself costs, not
//! the machine: time since this process launched, the process's CPU
//! share (Activity Monitor convention — % of one core, so it can
//! exceed 100 on multi-core), its resident memory, and the frames the
//! window actually drew over the last sample window. Fps comes from
//! gpui's frame-timing profiler, which records every `Window::draw`;
//! an idle window draws nothing and honestly reads `0 fps`, so the
//! figure doubles as a repaint-leak tripwire.
//!
//! Prime Directive: the sysinfo refresh is a syscall, so it runs on
//! the background executor. The UI-thread side only stores the
//! finished [`StatsSnapshot`] in `ProcessState` and notifies the
//! shells; render formats the cached snapshot and never samples.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{App, SharedString, WindowId};

/// How often the sampler wakes: fast enough for a glanceable readout,
/// slow enough to be invisible in a profile, and comfortably above
/// sysinfo's `MINIMUM_CPU_UPDATE_INTERVAL` (~200 ms) so the CPU delta
/// between two refreshes is meaningful.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// One finished sample. Stored in `ProcessState::system_stats`
/// (`None` until the sampler has published its first real reading).
#[derive(Clone, Debug, Default)]
pub struct StatsSnapshot {
    /// Seconds since this process started.
    pub run_secs: u64,
    /// CPU % of one core (may exceed 100 on multi-core).
    pub cpu_pct: f32,
    /// Resident set size, bytes.
    pub mem_bytes: u64,
    /// Frames drawn per second over the last sample window, keyed by
    /// gpui window. Windows that drew nothing are simply absent.
    pub fps: HashMap<WindowId, f32>,
}

impl StatsSnapshot {
    /// The status-bar line for `window_id`.
    pub fn segment_label(&self, window_id: WindowId) -> SharedString {
        let fps = self.fps.get(&window_id).copied().unwrap_or(0.0);
        format_segment(self.run_secs, self.cpu_pct, self.mem_bytes, fps)
    }
}

/// `up 3d 4h · CPU 6.8% · MEM 184.0 MB · 58 fps`. Also used verbatim
/// by the `--simulate-stats` screenshot flag so captures are
/// deterministic.
pub fn format_segment(run_secs: u64, cpu_pct: f32, mem_bytes: u64, fps: f32) -> SharedString {
    format!(
        "up {} \u{00B7} CPU {} \u{00B7} MEM {} \u{00B7} {} fps",
        format_uptime(run_secs),
        format_cpu(cpu_pct),
        crate::status_bar::humanize_bytes(mem_bytes),
        // Floor, don't round: an idle window whose only redraws are
        // this segment's own 0.5 Hz ticks must read 0 fps, not
        // flicker between 0 and 1.
        fps.max(0.0) as u64,
    )
    .into()
}

/// One decimal below 10% (where the decimal carries real signal —
/// "0.2%" vs "0%"), whole numbers above.
fn format_cpu(pct: f32) -> String {
    if pct < 10.0 {
        format!("{pct:.1}%")
    } else {
        format!("{pct:.0}%")
    }
}

/// Two coarsest units, largest first: `45s` → `12m` → `4h 12m` →
/// `3d 4h`. Days don't roll into weeks — "up 19d 7h" reads fine.
pub fn format_uptime(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs / 3_600) % 24;
    let m = (secs / 60) % 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}

/// Start the process-wide sampler loop. Called once at boot (skipped
/// in screenshot mode, where `--simulate-stats` supplies a fixed
/// label instead — live numbers would make captures nondeterministic).
pub fn start_sampler(cx: &mut App) {
    // Frame tracing feeds the fps figure. Cheap: one 40-byte ring
    // push per drawn frame, nothing at all while idle.
    gpui::profiler::set_frame_trace_enabled(true);

    let pid = match sysinfo::get_current_pid() {
        Ok(pid) => pid,
        Err(e) => {
            crate::log_warn!(90, "system stats disabled - no pid: {e}");
            return;
        }
    };

    cx.spawn(async move |cx| {
        // The `System` lives across ticks because process CPU% is a
        // delta between two refreshes; it commutes to the background
        // executor and back each tick (it is Send, but its refresh
        // must not run on the UI thread).
        let mut sys = sysinfo::System::new();
        let mut collector = gpui::profiler::FrameTimingCollector::new();
        let mut last_sample = Instant::now();
        let mut primed = false;
        loop {
            cx.background_executor().timer(SAMPLE_INTERVAL).await;
            // Fps divides by the *actual* elapsed time, not the
            // nominal interval — timer wakeups drift under load.
            let elapsed = last_sample.elapsed();
            last_sample = Instant::now();
            let (returned_sys, returned_collector, snapshot) = cx
                .background_executor()
                .spawn(async move {
                    let mut sys = sys;
                    let mut collector = collector;
                    let snapshot = sample(&mut sys, &mut collector, pid, elapsed);
                    (sys, collector, snapshot)
                })
                .await;
            sys = returned_sys;
            collector = returned_collector;
            // The first refresh only primes the CPU-delta baseline;
            // its cpu_usage is meaningless, so don't publish it.
            if !primed {
                primed = true;
                continue;
            }
            let Some(snapshot) = snapshot else { continue };
            cx.update(|cx| {
                let process = crate::process_state::process_state(cx);
                *process.system_stats.borrow_mut() = Some(snapshot);
                // The segment lives in each window's status bar; the
                // shells repaint from the cached snapshot.
                for weak in process.live_shells() {
                    if let Some(shell) = weak.upgrade() {
                        shell.update(cx, |_, cx| cx.notify());
                    }
                }
            });
        }
    })
    .detach();
}

/// One background-thread sample: drain the drawn-frame ring, refresh
/// this process's CPU + memory, fold into a snapshot. Returns `None`
/// if the process lookup fails (never expected for our own pid).
fn sample(
    sys: &mut sysinfo::System,
    collector: &mut gpui::profiler::FrameTimingCollector,
    pid: sysinfo::Pid,
    elapsed: Duration,
) -> Option<StatsSnapshot> {
    // Drain the frames drawn since the last tick, then flip tracing
    // off/on: disabling clears (and shrinks) gpui's global ring, so
    // it holds at most one tick's worth of frames (~KBs) instead of
    // creeping toward its 16 MiB cap over a long session. A frame
    // landing in the microseconds between drain and re-enable is
    // dropped — an off-by-one nobody can see in an fps figure.
    let frames = collector.collect_unseen();
    gpui::profiler::set_frame_trace_enabled(false);
    gpui::profiler::set_frame_trace_enabled(true);
    *collector = gpui::profiler::FrameTimingCollector::new();

    let secs = elapsed.as_secs_f32().max(0.001);
    let mut fps: HashMap<WindowId, f32> = HashMap::new();
    for timing in &frames {
        *fps.entry(timing.window_id).or_insert(0.0) += 1.0;
    }
    for count in fps.values_mut() {
        *count /= secs;
    }

    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        sysinfo::ProcessRefreshKind::new().with_cpu().with_memory(),
    );
    let proc = sys.process(pid)?;
    Some(StatsSnapshot {
        run_secs: proc.run_time(),
        cpu_pct: proc.cpu_usage(),
        mem_bytes: proc.memory(),
        fps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_picks_two_coarsest_units() {
        assert_eq!(format_uptime(45), "45s");
        assert_eq!(format_uptime(12 * 60), "12m");
        assert_eq!(format_uptime(12 * 60 + 59), "12m");
        assert_eq!(format_uptime(4 * 3_600 + 12 * 60), "4h 12m");
        assert_eq!(format_uptime(3 * 86_400 + 4 * 3_600 + 59 * 60), "3d 4h");
        assert_eq!(format_uptime(0), "0s");
    }

    #[test]
    fn cpu_keeps_decimal_only_below_ten() {
        assert_eq!(format_cpu(0.24), "0.2%");
        assert_eq!(format_cpu(6.85), "6.8%");
        assert_eq!(format_cpu(42.6), "43%");
        assert_eq!(format_cpu(142.0), "142%");
    }

    #[test]
    fn segment_floors_fps_and_composes() {
        let s = format_segment(3 * 86_400 + 4 * 3_600, 6.8, 184 * 1024 * 1024, 58.9);
        assert_eq!(&*s, "up 3d 4h \u{00B7} CPU 6.8% \u{00B7} MEM 184.0 MB \u{00B7} 58 fps");
        // Idle: the only redraws are the sampler's own ticks (~0.5/s)
        // — must floor to an honest 0.
        let s = format_segment(30, 0.2, 3_774_874, 0.5);
        assert_eq!(&*s, "up 30s \u{00B7} CPU 0.2% \u{00B7} MEM 3.6 MB \u{00B7} 0 fps");
    }

    #[test]
    fn snapshot_label_missing_window_reads_zero_fps() {
        let snap = StatsSnapshot {
            run_secs: 61,
            cpu_pct: 1.0,
            mem_bytes: 1024,
            fps: HashMap::new(),
        };
        let label = snap.segment_label(WindowId::default());
        assert!(label.contains("0 fps"), "{label}");
        assert!(label.starts_with("up 1m"), "{label}");
    }
}
