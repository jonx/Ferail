//! App-footprint sampler behind the status bar's stats segment
//! (`up 3d 4h · CPU 6.8% · MEM 184 MB · 58 fps`).
//!
//! Everything shown is **app-centric** — what Ferail itself costs, not
//! the machine: time since this process launched, the process's CPU
//! share (Activity Monitor convention — % of one core, so it can
//! exceed 100 on multi-core), its resident memory, and how fast the
//! window draws **while it is drawing**. Fps comes from gpui's
//! frame-timing profiler, which records every `Window::draw`, and is
//! computed over *bursts* — runs of consecutive frames closer than
//! [`BURST_GAP`] apart. Averaging over the whole sample window would
//! dilute half a second of buttery scrolling into a meaningless "12
//! fps"; the burst rate reports the scroll's real ~60. An idle window
//! has no bursts and honestly reads `0 fps`, so the figure doubles as
//! a repaint-leak tripwire (a notify-loop shows its true loop rate).
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

/// Two frames further apart than this belong to different bursts of
/// drawing. 250 ms (= 4 fps) comfortably separates real animation
/// (16.7 ms at 60 Hz) and even janky redraws from isolated
/// one-off paints — including this sampler's own 0.5 Hz status-bar
/// ticks, which land far outside any burst and thus never register.
const BURST_GAP: Duration = Duration::from_millis(250);

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
    /// Burst draw rate (frames per second while actually drawing)
    /// over the last sample window, keyed by gpui window. Windows
    /// that drew no bursts are simply absent.
    pub fps: HashMap<WindowId, f32>,
}

impl StatsSnapshot {
    /// The status-bar segment for `window_id`, one pre-formatted
    /// string per figure so the bar can give each a fixed-width box.
    pub fn segment_parts(&self, window_id: WindowId) -> SegmentParts {
        let fps = self.fps.get(&window_id).copied().unwrap_or(0.0);
        SegmentParts::from_values(self.run_secs, self.cpu_pct, self.mem_bytes, fps)
    }
}

/// The four status-bar figures, pre-formatted. Split (rather than one
/// joined string) so the status bar can sit each in a fixed-min-width
/// box — a live readout whose width breathes on every tick makes the
/// whole bar jitter.
#[derive(Clone, Debug)]
pub struct SegmentParts {
    /// `up 3d 4h`
    pub up: SharedString,
    /// `CPU 6.8%`
    pub cpu: SharedString,
    /// `MEM 184.0 MB`
    pub mem: SharedString,
    /// `58 fps`
    pub fps: SharedString,
}

impl SegmentParts {
    pub fn from_values(run_secs: u64, cpu_pct: f32, mem_bytes: u64, fps: f32) -> Self {
        Self {
            up: format!("up {}", format_uptime(run_secs)).into(),
            cpu: format!("CPU {}", format_cpu(cpu_pct)).into(),
            mem: format!("MEM {}", crate::status_bar::humanize_bytes(mem_bytes)).into(),
            // Round: burst rate already excludes isolated one-off
            // paints, so 59.94 from timer jitter should read 60.
            fps: format!("{} fps", fps.max(0.0).round() as u64).into(),
        }
    }

    /// Fixed reference values for `--simulate-stats` screenshots.
    pub fn simulated() -> Self {
        Self::from_values(3 * 86_400 + 4 * 3_600, 6.8, 184 * 1024 * 1024, 58.0)
    }
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
        let mut primed = false;
        loop {
            cx.background_executor().timer(SAMPLE_INTERVAL).await;
            let (returned_sys, returned_collector, snapshot) = cx
                .background_executor()
                .spawn(async move {
                    let mut sys = sys;
                    let mut collector = collector;
                    let snapshot = sample(&mut sys, &mut collector, pid);
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

    // Group draw timestamps per window (the ring is chronological,
    // so each per-window list stays sorted), then take the burst
    // rate of each.
    let mut draws: HashMap<WindowId, Vec<Instant>> = HashMap::new();
    for timing in &frames {
        draws.entry(timing.window_id).or_default().push(timing.draw_end);
    }
    let fps: HashMap<WindowId, f32> = draws
        .into_iter()
        .map(|(id, ends)| (id, burst_fps(&ends)))
        .collect();

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

/// Frames per second *while drawing*: only gaps between consecutive
/// frames closer than [`BURST_GAP`] count as drawing time. Averaging
/// over wall-clock instead would report "12 fps" for half a second of
/// perfect 60 Hz scrolling inside a 2 s window — a dilution artifact,
/// not a frame rate. Isolated paints (no neighbour within the gap)
/// contribute nothing, so an idle window — including one repainted
/// only by the sampler's own ticks — reads 0.
///
/// `draw_ends` must be chronological (the profiler ring is).
fn burst_fps(draw_ends: &[Instant]) -> f32 {
    let mut drawing = Duration::ZERO;
    let mut intervals = 0u32;
    for pair in draw_ends.windows(2) {
        let delta = pair[1].duration_since(pair[0]);
        if delta <= BURST_GAP {
            drawing += delta;
            intervals += 1;
        }
    }
    if drawing.is_zero() {
        return 0.0;
    }
    intervals as f32 / drawing.as_secs_f32()
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
    fn segment_parts_compose() {
        let p = SegmentParts::from_values(3 * 86_400 + 4 * 3_600, 6.8, 184 * 1024 * 1024, 58.9);
        assert_eq!(&*p.up, "up 3d 4h");
        assert_eq!(&*p.cpu, "CPU 6.8%");
        assert_eq!(&*p.mem, "MEM 184.0 MB");
        // Burst rate rounds (59.94 from timer jitter should read 60;
        // isolated paints were already excluded upstream).
        assert_eq!(&*p.fps, "59 fps");
        let p = SegmentParts::from_values(30, 0.2, 3_774_874, 0.0);
        assert_eq!(&*p.up, "up 30s");
        assert_eq!(&*p.mem, "MEM 3.6 MB");
        assert_eq!(&*p.fps, "0 fps");
    }

    #[test]
    fn snapshot_parts_missing_window_reads_zero_fps() {
        let snap = StatsSnapshot {
            run_secs: 61,
            cpu_pct: 1.0,
            mem_bytes: 1024,
            fps: HashMap::new(),
        };
        let parts = snap.segment_parts(WindowId::default());
        assert_eq!(&*parts.fps, "0 fps");
        assert_eq!(&*parts.up, "up 1m");
    }

    #[test]
    fn burst_fps_reports_rate_while_drawing_not_wall_clock() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // Half a second of 60 Hz scrolling inside an otherwise idle
        // 2 s window: the old wall-clock average said ~15 fps; the
        // burst rate must say ~60.
        let mut ends: Vec<Instant> = (0..30).map(|i| ms(1_000 + i * 16)).collect();
        ends.insert(0, ms(0)); // an isolated paint long before the burst
        let fps = burst_fps(&ends);
        assert!((59.0..=64.0).contains(&fps), "{fps}");
        // Idle: only the sampler's own ~0.5 Hz repaint ticks — no
        // pair is closer than BURST_GAP, so no drawing time at all.
        let ends: Vec<Instant> = (0..3).map(|i| ms(i * 2_000)).collect();
        assert_eq!(burst_fps(&ends), 0.0);
        // Degenerate inputs.
        assert_eq!(burst_fps(&[]), 0.0);
        assert_eq!(burst_fps(&[t0]), 0.0);
    }
}
