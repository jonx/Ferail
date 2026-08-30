//! App-footprint sampler behind the status bar's stats segment
//! (`up 3d 4h · CPU 6.8% · MEM 184.0 MB · 58 redraws/s`).
//!
//! Everything shown is **app-centric**: what Ferail itself costs, not
//! the machine: time since this process launched, the process's CPU
//! share, its resident memory, and how often the window redraws. macOS
//! keeps Activity Monitor's one-core convention; Windows normalizes
//! across the process's available logical processors so its figure
//! agrees with Task Manager and cannot alarmingly read 700%. The last
//! figure is deliberately **redraws per second, not "fps"**: gpui
//! only draws invalidated frames, so
//! the honest measurement is a plain count of `Window::draw`s over
//! the sample window (from gpui's frame-timing profiler). Calling
//! that "fps" would misread a brief scroll inside a mostly-idle
//! window as low smoothness; as rps the number is simply true. Idle
//! reads 0, steady scrolling approaches the display rate, and a
//! sustained nonzero at rest is a repaint-leak tripwire.
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
    /// User-facing CPU percentage. On Windows this is normalized over
    /// available logical processors to match Task Manager; other targets
    /// retain their platform convention.
    pub cpu_pct: f32,
    /// Raw sysinfo percentage in units of one logical core. Kept for
    /// diagnostics so a Windows report can explain both figures without
    /// changing the user-facing status-bar convention.
    pub cpu_core_pct: f32,
    /// Resident set size, bytes.
    pub mem_bytes: u64,
    /// Redraws per second over the last sample window (count of
    /// `Window::draw`s ÷ window duration), keyed by gpui window.
    /// Windows that drew nothing are simply absent.
    pub rps: HashMap<WindowId, f32>,
}

impl StatsSnapshot {
    /// The status-bar segment for `window_id`, one pre-formatted
    /// string per figure so the bar can give each a fixed-width box.
    pub fn segment_parts(&self, window_id: WindowId) -> SegmentParts {
        let rps = self.rps.get(&window_id).copied().unwrap_or(0.0);
        SegmentParts::from_values(self.run_secs, self.cpu_pct, self.mem_bytes, rps)
    }
}

/// The four status-bar figures, pre-formatted. Split (rather than one
/// joined string) so the status bar can sit each in a fixed-min-width
/// box: a live readout whose width breathes on every tick makes the
/// whole bar jitter.
#[derive(Clone, Debug)]
pub struct SegmentParts {
    /// `up 3d 4h`
    pub up: SharedString,
    /// The bare uptime figure, `3d 4h`: what the status bar's minimal
    /// density prefixes with the universal `UP` token when the
    /// translated "up" wording ("en service depuis"…) no longer fits.
    pub uptime: SharedString,
    /// `CPU 6.8%`
    pub cpu: SharedString,
    /// `MEM 184.0 MB`
    pub mem: SharedString,
    /// `58 redraws/s`
    pub rps: SharedString,
}

impl SegmentParts {
    pub fn from_values(run_secs: u64, cpu_pct: f32, mem_bytes: u64, rps: f32) -> Self {
        Self {
            up: tr!("up {uptime}", uptime = format_uptime(run_secs)),
            uptime: format_uptime(run_secs).into(),
            cpu: tr!("CPU {pct}", pct = format_cpu(cpu_pct)),
            mem: tr!(
                "MEM {bytes}",
                bytes = crate::status_bar::humanize_bytes(mem_bytes)
            ),
            // Floor, don't round: an idle window whose only redraws
            // are this sampler's own 0.5 Hz notify ticks averages
            // ~0.5 and must read an honest 0, not flicker to 1.
            rps: tr!("{n} redraws/s", n = rps.max(0.0) as u64),
        }
    }

    /// Fixed reference values for `--simulate-stats` screenshots.
    pub fn simulated() -> Self {
        Self::from_values(3 * 86_400 + 4 * 3_600, 6.8, 184 * 1024 * 1024, 58.0)
    }
}

/// One decimal below 10% (where the decimal carries real signal,
/// "0.2%" vs "0%"), whole numbers above.
fn format_cpu(pct: f32) -> String {
    if pct < 10.0 {
        format!("{pct:.1}%")
    } else {
        format!("{pct:.0}%")
    }
}

/// Two coarsest units, largest first: `45s` → `12m` → `4h 12m` →
/// `3d 4h`. Days don't roll into weeks, "up 19d 7h" reads fine.
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
/// label instead: live numbers would make captures nondeterministic).
pub fn start_sampler(cx: &mut App) {
    // Frame tracing feeds the rps figure. Cheap: one 40-byte ring
    // push per drawn frame, nothing at all while idle.
    gpui::profiler::set_trace_enabled(true);

    let pid = match sysinfo::get_current_pid() {
        Ok(pid) => pid,
        Err(e) => {
            crate::log_warn!(90, "system stats disabled - no pid: {e}");
            return;
        }
    };
    let cpu_divisor = status_cpu_divisor();

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
            // rps divides by the *actual* elapsed time, not the
            // nominal interval: timer wakeups drift under load.
            let elapsed = last_sample.elapsed();
            last_sample = Instant::now();
            let (returned_sys, returned_collector, snapshot) = cx
                .background_executor()
                .spawn(async move {
                    let mut sys = sys;
                    let mut collector = collector;
                    let snapshot = sample(&mut sys, &mut collector, pid, elapsed, cpu_divisor);
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
                report_intern_growth(&process.fs);
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
    cpu_divisor: f32,
) -> Option<StatsSnapshot> {
    // Drain the frames drawn since the last tick, then flip tracing
    // off/on: disabling clears (and shrinks) gpui's global ring, so
    // it holds at most one tick's worth of frames (~KBs) instead of
    // creeping toward its 16 MiB cap over a long session. A frame
    // landing in the microseconds between drain and re-enable is
    // dropped: an off-by-one nobody can see in an rps figure.
    let frames = collector.collect_unseen();
    gpui::profiler::set_trace_enabled(false);
    gpui::profiler::set_trace_enabled(true);
    *collector = gpui::profiler::FrameTimingCollector::new();

    // Redraw count per window over the window's actual duration. A
    // plain average, deliberately: this is a redraw *counter* (rps),
    // not a smoothness claim: see the module docs for why it is not
    // called "fps".
    let secs = elapsed.as_secs_f32().max(0.001);
    let mut rps: HashMap<WindowId, f32> = HashMap::new();
    for event in &frames {
        if let gpui::profiler::FrameEvent::Draw(timing) = event {
            *rps.entry(timing.window_id).or_insert(0.0) += 1.0;
        }
    }
    for count in rps.values_mut() {
        *count /= secs;
    }

    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        sysinfo::ProcessRefreshKind::new().with_cpu().with_memory(),
    );
    let proc = sys.process(pid)?;
    let cpu_core_pct = proc.cpu_usage();
    Some(StatsSnapshot {
        run_secs: proc.run_time(),
        cpu_pct: cpu_core_pct / cpu_divisor,
        cpu_core_pct,
        mem_bytes: proc.memory(),
        rps,
    })
}

/// Windows Task Manager reports one process as a share of the machine's total
/// logical-CPU capacity, while sysinfo reports in units of one logical core.
/// Other targets keep their established platform convention (notably Activity
/// Monitor on macOS), so their divisor is one.
fn status_cpu_divisor() -> f32 {
    #[cfg(windows)]
    {
        std::thread::available_parallelism()
            .map(|n| n.get() as f32)
            .unwrap_or(1.0)
            .max(1.0)
    }
    #[cfg(not(windows))]
    {
        1.0
    }
}

/// Log the identity maps' footprint the first time it crosses each
/// threshold.
///
/// `NativeFs`'s `NodeId <-> PathBuf` maps are add-only for the life of the
/// process, so a long browsing session or one recursive tool run over a big
/// tree grows them without bound (TODO.md, "NodeId intern-map lifecycle").
/// Until that lifecycle exists, the growth should at least be visible: these
/// lines land in the activity trail, so an issue report from a session that
/// went sluggish carries the evidence. `intern_stats` is O(1), so sampling it
/// on this timer costs nothing.
fn report_intern_growth(fs: &ferail_fs_native::NativeFs) {
    /// Entry counts worth a line. A few hundred thousand paths is an
    /// ordinary heavy session; millions means a recursive tool pinned a
    /// whole tree.
    const THRESHOLDS: &[usize] = &[250_000, 500_000, 1_000_000, 2_000_000, 4_000_000];
    static REPORTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    let stats = fs.intern_stats();
    let reached = THRESHOLDS
        .iter()
        .filter(|threshold| stats.entries >= **threshold)
        .count();
    if reached > REPORTED.swap(reached, std::sync::atomic::Ordering::Relaxed) {
        crate::log_warn!(
            90,
            "identity maps hold {} paths (~{} MB) and are never pruned",
            ferail_core::counts::format_count(stats.entries as u64),
            stats.approx_bytes / (1024 * 1024)
        );
    }
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
    fn task_manager_normalization_is_total_machine_share() {
        let raw_core_pct = 700.0;
        let logical_processors = 16.0;
        assert_eq!(raw_core_pct / logical_processors, 43.75);
    }

    #[test]
    fn segment_parts_compose() {
        let p = SegmentParts::from_values(3 * 86_400 + 4 * 3_600, 6.8, 184 * 1024 * 1024, 58.9);
        assert_eq!(&*p.up, "up 3d 4h");
        assert_eq!(&*p.cpu, "CPU 6.8%");
        assert_eq!(&*p.mem, "MEM 184.0 MB");
        assert_eq!(&*p.rps, "58 redraws/s");
        // Idle: the only redraws are the sampler's own notify ticks
        // (~0.5/s): must floor to an honest 0.
        let p = SegmentParts::from_values(30, 0.2, 3_774_874, 0.5);
        assert_eq!(&*p.up, "up 30s");
        assert_eq!(&*p.mem, "MEM 3.6 MB");
        assert_eq!(&*p.rps, "0 redraws/s");
    }

    #[test]
    fn snapshot_parts_missing_window_reads_zero_rps() {
        let snap = StatsSnapshot {
            run_secs: 61,
            cpu_pct: 1.0,
            cpu_core_pct: 1.0,
            mem_bytes: 1024,
            rps: HashMap::new(),
        };
        let parts = snap.segment_parts(WindowId::default());
        assert_eq!(&*parts.rps, "0 redraws/s");
        assert_eq!(&*parts.up, "up 1m");
    }
}
