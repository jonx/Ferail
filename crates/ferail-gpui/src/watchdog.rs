//! Freeze watchdog + hang reports (docs/features/FREEZE_DIAGNOSTICS.md).
//!
//! The Prime Directive says the UI thread never blocks. This module is the
//! field instrumentation for when that fails anyway: it notices a wedged UI
//! thread and turns an opaque "the app froze" into a report a user can
//! attach to an issue. Three cooperating pieces, core is std-only so every
//! port (macOS / Windows / Linux / AROS) gets at least the in-process data:
//!
//! - **Heartbeat** — a foreground-executor loop bumps an atomic counter
//!   once a second. The continuation only runs while the UI thread pumps
//!   its event loop, so "counter stopped" ⇔ "UI thread wedged". Each beat
//!   also snapshots the active background tasks into a thread-safe cell;
//!   the last snapshot before a freeze usually names the culprit.
//! - **Watchdog thread** — a plain `std::thread` (the GPUI pool could be
//!   starved by the very bug under diagnosis) that watches the counter and
//!   writes a hang report automatically after ~10 s of silence, no user
//!   action needed. Re-arms if the UI recovers.
//! - **Kill interception** — on macOS/Linux, `Ctrl+\` (SIGQUIT) always
//!   dumps a report and exits; SIGINT/SIGTERM dump one only when the UI
//!   was already stalled, then exit as usual. The signal handler itself
//!   only writes a byte to a self-pipe (the only async-signal-safe part);
//!   a dedicated thread does the real work. On Windows, a console-control
//!   handler gives Ctrl+Break / Ctrl+C the same behavior.
//!
//! The report carries: the last task snapshot, breadcrumbs (`crate::obs`),
//! the activity trail (`crate::trail`, path-redacted), and — where a
//! platform tool exists — full symbolized stacks of every thread
//! (`/usr/bin/sample` on macOS; `eu-stack`/`gdb` on Linux). It is written
//! to the same `reports/` folder as the issue bundle *before* the stack
//! capture is attempted, so a misbehaving capture tool can't lose it.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui::App;

/// Heartbeat period; also the watchdog thread's check period.
const BEAT_INTERVAL: Duration = Duration::from_secs(1);
/// Consecutive silent checks before the automatic report fires (~10 s —
/// long enough that a slow-but-alive frame never false-alarms).
const STALL_REPORT_CHECKS: u32 = 10;
/// Consecutive silent checks before SIGINT/SIGTERM count as "killed while
/// frozen" and dump on the way out (~3 s).
const STALL_SUSPECT_CHECKS: u32 = 3;

static BEAT: AtomicU64 = AtomicU64::new(0);
/// UI thread silent for at least [`STALL_SUSPECT_CHECKS`]; maintained by
/// the watchdog thread, read by the signal/console handlers.
static STALLED: AtomicBool = AtomicBool::new(false);
/// Heartbeat loop lost its App (normal quit) — the watchdog must not
/// mistake shutdown for a freeze.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);
static TASK_SNAPSHOT: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
static HANG_SEQ: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchdogAction {
    None,
    Suspect,
    Report,
    Recovered,
    Rebaseline,
}

struct StallTracker {
    baseline: u64,
    stale_checks: u32,
    reported: bool,
}

impl StallTracker {
    fn new(baseline: u64) -> Self {
        Self {
            baseline,
            stale_checks: 0,
            reported: false,
        }
    }

    fn observe(&mut self, beat: u64, check_gap: Duration) -> WatchdogAction {
        if check_gap > BEAT_INTERVAL * 5 {
            self.baseline = beat;
            self.stale_checks = 0;
            return WatchdogAction::Rebaseline;
        }
        if beat != self.baseline {
            self.baseline = beat;
            self.stale_checks = 0;
            let recovered = self.reported;
            self.reported = false;
            return if recovered {
                WatchdogAction::Recovered
            } else {
                WatchdogAction::None
            };
        }
        self.stale_checks = self.stale_checks.saturating_add(1);
        if self.stale_checks >= STALL_REPORT_CHECKS && !self.reported {
            self.reported = true;
            WatchdogAction::Report
        } else if self.stale_checks == STALL_SUSPECT_CHECKS {
            WatchdogAction::Suspect
        } else {
            WatchdogAction::None
        }
    }

    fn stalled(&self) -> bool {
        self.stale_checks >= STALL_SUSPECT_CHECKS
    }
}

/// Install every piece. Called once from `boot::run_gui`, after the
/// `ProcessState` global exists (the snapshot reads it) and before any
/// window opens. Deliberately NOT called on the screenshot path — a
/// headless capture has no event loop to monitor.
pub fn start(cx: &mut App) {
    // Quit teardown stops the heartbeat without being a freeze; tell the
    // watchdog so it can't misfire in a slow shutdown. Leaks the
    // subscription intentionally (lives the whole app run).
    cx.on_app_quit(|_| {
        SHUTDOWN.store(true, Ordering::Release);
        async {}
    })
    .detach();
    start_heartbeat(cx);
    start_watchdog_thread();
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    unix_signals::install();
    #[cfg(windows)]
    windows_console::install();
    install_debug_freeze(cx);
}

/// Test hook: `FERAIL_DEBUG_FREEZE=<secs>` deliberately wedges the UI
/// thread for that many seconds, three seconds after boot — the only
/// sanctioned Prime Directive violation, existing so the watchdog, the
/// hang report, and the kill interception can be verified end-to-end
/// (by us and by a user asking "would this catch *my* freeze?").
fn install_debug_freeze(cx: &mut App) {
    let Some(secs) = std::env::var("FERAIL_DEBUG_FREEZE")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s > 0)
    else {
        return;
    };
    crate::log_warn!(
        90,
        "FERAIL_DEBUG_FREEZE: will block the UI thread for {secs}s (test hook)"
    );
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs(3)).await;
        cx.update(|_| {
            // Blocking the UI thread is the entire point here.
            std::thread::sleep(Duration::from_secs(secs));
        });
    })
    .detach();
}

/// Whether the UI thread has been unresponsive for a few seconds — the
/// "should a kill also dump?" question.
pub fn ui_thread_stalled() -> bool {
    STALLED.load(Ordering::Acquire)
}

fn snapshot_cell() -> &'static Mutex<Vec<String>> {
    TASK_SNAPSHOT.get_or_init(|| Mutex::new(Vec::new()))
}

fn start_heartbeat(cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(BEAT_INTERVAL).await;
            // The update closure runs on the UI thread; *reaching it at
            // all* is the liveness signal the watchdog consumes. At quit
            // the loop simply never resumes — `on_app_quit` above flags
            // the watchdog before that can look like a stall.
            cx.update(|cx| {
                BEAT.fetch_add(1, Ordering::Release);
                publish_task_snapshot(cx);
            });
        }
    })
    .detach();
}

/// Runs on the UI thread once per beat: copy the live task registry into
/// the cell the watchdog / signal threads can read. A few string formats
/// per second — no I/O, no path resolution (Prime Directive-clean).
fn publish_task_snapshot(cx: &mut App) {
    let Some(global) = cx.try_global::<crate::process_state::ProcessStateGlobal>() else {
        return;
    };
    let process = global.0.clone();
    let tasks = process.tasks.borrow();
    let mut lines = Vec::with_capacity(tasks.len());
    for t in tasks.iter() {
        // Labels can carry file names ("Copying report.pdf…"); scrub
        // path-shaped tokens so the report stays shareable under the
        // privacy toggle, same policy as the activity trail.
        lines.push(format!(
            "{:?} · running {:.1}s · {}",
            t.kind,
            t.started_at.elapsed().as_secs_f32(),
            crate::redact::scrub_text(&t.label),
        ));
    }
    // Never make the UI heartbeat wait for the reporting thread. A missed
    // snapshot is harmless; blocking here would manufacture the very stall
    // this mechanism is meant to diagnose.
    match snapshot_cell().try_lock() {
        Ok(mut snapshot) => *snapshot = lines,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => {
            *poisoned.into_inner() = lines;
        }
        Err(std::sync::TryLockError::WouldBlock) => {}
    }
}

fn start_watchdog_thread() {
    crate::obs::spawn_logged("freeze-watchdog", || {
        let mut last_check = Instant::now();
        let mut tracker = StallTracker::new(BEAT.load(Ordering::Acquire));
        loop {
            std::thread::sleep(BEAT_INTERVAL);
            if SHUTDOWN.load(Ordering::Acquire) {
                return;
            }
            let gap = last_check.elapsed();
            last_check = Instant::now();
            let beat = BEAT.load(Ordering::Acquire);
            match tracker.observe(beat, gap) {
                WatchdogAction::Recovered => {
                    crate::log_warn!(90, "freeze-watchdog: UI thread recovered; re-armed");
                }
                WatchdogAction::Report => write_hang_report(&format!(
                    "UI thread unresponsive for ~{}s (heartbeat stalled)",
                    tracker.stale_checks
                )),
                WatchdogAction::None | WatchdogAction::Suspect | WatchdogAction::Rebaseline => {}
            }
            STALLED.store(tracker.stalled(), Ordering::Release);
        }
    });
}

/// Assemble the in-process half of a hang report: header, last task
/// snapshot, breadcrumbs, activity trail. Pure string building — split
/// out so tests can exercise it without a frozen app.
fn render_report_body(reason: &str) -> String {
    use std::fmt::Write as _;
    let mut r = String::with_capacity(8 * 1024);
    let _ = writeln!(
        r,
        "==================== Ferail (gpui) Hang Report ===================="
    );
    let _ = writeln!(
        r,
        "version   : {} ({}/{})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let _ = writeln!(r, "pid       : {}", std::process::id());
    let _ = writeln!(r, "uptime    : +{:.3}s", crate::obs::elapsed_secs());
    let _ = writeln!(r, "reason    : {reason}");
    let _ = writeln!(r, "safe mode : {}", crate::safe_mode::enabled());
    let _ = writeln!(r);

    let _ = writeln!(r, "background tasks (last snapshot before the stall):");
    match snapshot_cell().try_lock() {
        Ok(snapshot) => {
            if snapshot.is_empty() {
                let _ = writeln!(r, "    <none>");
            } else {
                for line in snapshot.iter() {
                    let _ = writeln!(r, "    {line}");
                }
            }
        }
        Err(std::sync::TryLockError::Poisoned(poisoned)) => {
            for line in poisoned.into_inner().iter() {
                let _ = writeln!(r, "    {line}");
            }
        }
        Err(std::sync::TryLockError::WouldBlock) => {
            let _ = writeln!(r, "    <unavailable: lock held by stalled thread>");
        }
    }
    let _ = writeln!(r);

    let _ = writeln!(r, "breadcrumbs:");
    match crate::obs::try_breadcrumb_lines() {
        Some(crumbs) if crumbs.is_empty() => {
            let _ = writeln!(r, "    <none>");
        }
        Some(crumbs) => {
            for line in &crumbs {
                let _ = writeln!(r, "    {line}");
            }
        }
        None => {
            let _ = writeln!(r, "    <unavailable: lock held by stalled thread>");
        }
    }
    let _ = writeln!(r);

    // Redaction-honoring render — a hang report must be as shareable as
    // the issue bundle. Tail only: the freeze context is the recent past.
    const TRAIL_TAIL: usize = 40;
    let Some(trail) = crate::trail::try_render_lines_sanitized() else {
        let _ = writeln!(r, "activity trail:");
        let _ = writeln!(r, "    <unavailable: lock held by stalled thread>");
        let _ = writeln!(
            r,
            "===================================================================="
        );
        return r;
    };
    let skipped = trail.len().saturating_sub(TRAIL_TAIL);
    let _ = writeln!(
        r,
        "activity trail (last {} of {}):",
        trail.len() - skipped,
        trail.len()
    );
    if trail.is_empty() {
        let _ = writeln!(r, "    <none>");
    } else {
        for line in &trail[skipped..] {
            let _ = writeln!(r, "    {line}");
        }
    }
    let _ = writeln!(
        r,
        "===================================================================="
    );
    r
}

/// Write a hang report: persist the in-process half first (a report on
/// disk beats a prettier one lost), then attempt the per-platform
/// whole-process stack capture and append it. The console gets a short
/// digest, not the report — a screenful of dyld image addresses scrolls
/// the one line that matters (where the report landed) out of view.
/// Set `FERAIL_FULL_HANG_REPORT=1` to echo everything to stderr as well.
/// Callable from any thread except the (presumably wedged) UI thread.
pub fn write_hang_report(reason: &str) {
    let verbose = std::env::var_os("FERAIL_FULL_HANG_REPORT").is_some();
    let body = render_report_body(reason);
    let path = report_file_path();
    if let Some(path) = &path {
        if let Err(e) = std::fs::write(path, &body) {
            crate::log_warn!(90, "hang report write failed ({}): {e}", path.display());
        }
    }
    // One line before the (seconds-long, possibly wedged) stack capture,
    // so a user watching the terminal knows the freeze was noticed even
    // if the capture never returns.
    crate::obs::stderr_line(&format!("─── Ferail hang: {reason} ───"));
    if verbose {
        crate::obs::stderr_line(&body);
    }
    // AROS: stderr is console-bound and the config dir may not exist —
    // persist to the host-shared volume like the panic hook does.
    #[cfg(target_os = "aros")]
    {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("MacRW:ferail-hang.txt")
        {
            let _ = writeln!(f, "{body}");
            let _ = f.flush();
        }
    }

    let captured = capture_thread_stacks(path.as_deref());
    match &captured {
        Some((tool, stacks)) => {
            let section = format!("\nthread stacks (via {tool}):\n{stacks}\n");
            if let Some(path) = &path {
                append_to_file(path, &section);
            }
            if verbose {
                crate::obs::stderr_line(&section);
            }
        }
        None => {
            let hint = format!("\nthread stacks : <unavailable> — {}\n", stack_hint());
            if let Some(path) = &path {
                append_to_file(path, &hint);
            }
            if verbose {
                crate::obs::stderr_line(&hint);
            }
        }
    }

    crate::obs::stderr_line(&render_console_digest(
        captured.as_ref().map(|(_, s)| s.as_str()),
        path.as_deref(),
    ));
}

/// The console half of a hang report: the few facts that identify the
/// freeze — where the UI thread is stuck, what was running, what the
/// user last did — plus the path to the file that holds the rest. Kept
/// to well under a screenful on purpose.
fn render_console_digest(stacks: Option<&str>, path: Option<&std::path::Path>) -> String {
    use std::fmt::Write as _;
    /// Innermost UI-thread frames worth showing before the file takes over.
    const FRAMES: usize = 3;

    let mut d = String::with_capacity(1024);
    let _ = write!(
        d,
        "  where   : {} {}/{} · pid {} · up {:.1}s",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::process::id(),
        crate::obs::elapsed_secs()
    );
    if crate::safe_mode::enabled() {
        let _ = write!(d, " · safe mode");
    }
    let _ = writeln!(d);

    match stacks.map(|s| ui_thread_frames(s, FRAMES)) {
        Some(frames) if !frames.is_empty() => {
            for (i, frame) in frames.iter().enumerate() {
                let label = if i == 0 {
                    "  ui stack:"
                } else {
                    "          ←"
                };
                let _ = writeln!(d, "{label} {frame}");
            }
        }
        Some(_) => {
            let _ = writeln!(d, "  ui stack: <captured — see the report>");
        }
        None => {
            let _ = writeln!(d, "  ui stack: <unavailable> — {}", stack_hint());
        }
    }

    let tasks = match snapshot_cell().try_lock() {
        Ok(snapshot) => snapshot.clone(),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner().clone(),
        Err(std::sync::TryLockError::WouldBlock) => Vec::new(),
    };
    let _ = writeln!(d, "  tasks   : {}", summarize(&tasks, "none running"));

    let trail = crate::trail::try_render_lines_sanitized().unwrap_or_default();
    let last = trail.last().cloned().unwrap_or_default();
    let _ = writeln!(
        d,
        "  last    : {}",
        if last.is_empty() {
            "<no activity recorded>"
        } else {
            &last
        }
    );

    match path {
        Some(path) => {
            let _ = writeln!(
                d,
                "  report  : {} — attach it when reporting the freeze",
                path.display()
            );
            let _ = write!(
                d,
                "            (stack capture location, breadcrumbs and trail are in there; FERAIL_FULL_HANG_REPORT=1 echoes them here)"
            );
        }
        None => {
            let _ = write!(
                d,
                "  report  : <could not be saved> — set FERAIL_FULL_HANG_REPORT=1 to get the details on stderr"
            );
        }
    }
    d
}

/// First entry of a list plus a `(+N more)` tail — the digest names the
/// likely culprit without turning into the list the report already has.
fn summarize(lines: &[String], empty: &str) -> String {
    match lines.split_first() {
        None => empty.to_string(),
        Some((first, [])) => first.clone(),
        Some((first, rest)) => format!("{first}  (+{} more)", rest.len()),
    }
}

/// Innermost frames of the UI thread, pulled out of a whole-process
/// stack capture for the console digest. Innermost first — the wedged
/// call is what a reader wants on line one, the rest is context.
///
/// macOS `sample` prints an indented call graph per thread; the UI
/// thread's block starts at a header naming the main thread and its
/// deepest (last) line is the call that is stuck. `gdb` / `eu-stack`
/// print `#0`-numbered frames already innermost-first, the first block
/// being the main thread. Anything unrecognized yields nothing — the
/// digest says so, and the file still has the raw capture.
fn ui_thread_frames(stacks: &str, max: usize) -> Vec<String> {
    let sample = sample_main_thread_frames(stacks, max);
    if !sample.is_empty() {
        return sample;
    }
    numbered_first_thread_frames(stacks, max)
}

fn sample_main_thread_frames(text: &str, max: usize) -> Vec<String> {
    /// Depth in `sample`'s call graph is the indentation *after* the
    /// tree glyphs it draws in the left gutter (`+ `, `! `, `: `), so a
    /// raw `trim_start()` would read every frame as depth 4 and end the
    /// block on its first line.
    fn depth(line: &str) -> usize {
        line.len() - line.trim_start_matches([' ', '+', '!', ':', '|']).len()
    }
    let mut header_indent: Option<usize> = None;
    let mut frames: Vec<&str> = Vec::new();
    for line in text.lines() {
        let indent = depth(line);
        match header_indent {
            None => {
                let lower = line.to_ascii_lowercase();
                if lower.contains("thread_")
                    && (lower.contains("main-thread") || lower.contains("main thread"))
                {
                    header_indent = Some(indent);
                }
            }
            Some(header) => {
                if line.trim().is_empty() {
                    continue;
                }
                if indent <= header {
                    break; // next thread's header — this block is done
                }
                frames.push(line);
            }
        }
    }
    frames
        .iter()
        .rev()
        .map(|l| clean_frame(l))
        .filter(|l| !l.is_empty())
        .take(max)
        .collect()
}

fn numbered_first_thread_frames(text: &str, max: usize) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('#'))
        .map(clean_frame)
        .filter(|l| !l.is_empty())
        .take(max)
        .collect()
}

/// One capture-tool line → one readable frame: drop the tree glyphs and
/// per-frame sample count `sample` prefixes, the `#N` and raw pc of a
/// numbered backtrace, and the trailing address, then bound the width so
/// one deep C++ symbol cannot blow the digest back up to a screenful.
/// Drop the `::h<16 hex>` rustc appends to legacy-mangled symbols —
/// pure noise in a three-line digest. Whatever `sample` printed after
/// the symbol (`+ 44`, a source location) is kept.
fn strip_symbol_hash(s: &str) -> String {
    let Some(at) = s.rfind("::h") else {
        return s.to_string();
    };
    let tail = &s[at + 3..];
    let hash_len = tail
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(tail.len());
    if hash_len != 16 {
        return s.to_string();
    }
    format!("{}{}", &s[..at], &tail[hash_len..])
}

fn clean_frame(line: &str) -> String {
    const MAX_WIDTH: usize = 96;
    let mut s = line.trim_matches(|c: char| c.is_whitespace() || "+!:|".contains(c));
    // `sample`: leading per-frame sample count. `gdb`/`eu-stack`: `#3`.
    if let Some((head, rest)) = s.split_once(' ') {
        let head = head.trim_start_matches('#');
        if !head.is_empty() && head.chars().all(|c| c.is_ascii_digit()) {
            s = rest.trim_start();
        }
    }
    // `gdb`/`eu-stack` then lead with the raw pc: `0x0000… in foo ()`.
    if let Some(rest) = s.strip_prefix("0x") {
        let rest = rest
            .trim_start_matches(|c: char| c.is_ascii_hexdigit())
            .trim_start();
        s = rest.strip_prefix("in ").unwrap_or(rest);
    }
    if let Some(at) = s.rfind(" [0x") {
        s = s[..at].trim_end();
    }
    let s = strip_symbol_hash(s.trim());
    if s.chars().count() > MAX_WIDTH {
        let cut: String = s.chars().take(MAX_WIDTH - 1).collect();
        return format!("{cut}…");
    }
    s
}

/// `<config>/reports/ferail-hang-<pid>-<seq>.txt` — the same folder the
/// issue bundle uses, so users find both in one place.
fn report_file_path() -> Option<std::path::PathBuf> {
    let dir = crate::app_state::config_dir()?.join("reports");
    std::fs::create_dir_all(&dir).ok()?;
    // A PID can be reused after a reboot. Never overwrite earlier diagnostic
    // evidence (and on Windows an existing sibling `.dmp` would also make the
    // broker's final atomic rename fail).
    loop {
        let seq = HANG_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("ferail-hang-{}-{seq}.txt", std::process::id()));
        if !path.exists() && !path.with_extension("dmp").exists() {
            return Some(path);
        }
    }
}

fn append_to_file(path: &std::path::Path, text: &str) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// What a user (or the report itself) can still do when no capture tool
/// produced stacks on this platform.
fn stack_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "run `sample Ferail 3` in Terminal (or Activity Monitor → Sample Process) while the app is frozen"
    } else if cfg!(windows) {
        "in Task Manager → Details, right-click ferail.exe → Create dump file, and attach the .dmp"
    } else if cfg!(target_os = "linux") {
        "install `elfutils` (eu-stack) or `gdb` and future reports will include all-thread stacks automatically"
    } else {
        "no whole-process stack tool on this platform"
    }
}

/// macOS: `/usr/bin/sample` gives symbolized call stacks of every thread
/// of a hung process, no root needed for our own pid.
#[cfg(target_os = "macos")]
fn capture_thread_stacks(_report_path: Option<&std::path::Path>) -> Option<(&'static str, String)> {
    // Blocking child wait — allowed: this runs on the watchdog /
    // signal-dump thread, never the UI thread (which is wedged anyway;
    // sampling it is the point).
    #[allow(clippy::disallowed_methods)]
    let out = std::process::Command::new("/usr/bin/sample")
        .args([std::process::id().to_string().as_str(), "1", "-mayDie"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        return None;
    }
    Some(("sample", text))
}

/// Linux: prefer `eu-stack` (fast, elfutils), fall back to `gdb`. Yama's
/// default `ptrace_scope=1` blocks a child from attaching to its parent,
/// so open a `PR_SET_PTRACER` window for the duration of the capture.
#[cfg(target_os = "linux")]
fn capture_thread_stacks(_report_path: Option<&std::path::Path>) -> Option<(&'static str, String)> {
    unsafe {
        libc::prctl(libc::PR_SET_PTRACER, libc::PR_SET_PTRACER_ANY, 0, 0, 0);
    }
    let pid = std::process::id().to_string();
    let result = run_stack_tool("eu-stack", &["-p", &pid])
        .map(|text| ("eu-stack", text))
        .or_else(|| {
            run_stack_tool(
                "gdb",
                &["--batch", "-p", &pid, "-ex", "thread apply all bt"],
            )
            .map(|text| ("gdb", text))
        });
    unsafe {
        libc::prctl(libc::PR_SET_PTRACER, 0, 0, 0, 0);
    }
    result
}

#[cfg(target_os = "linux")]
fn run_stack_tool(tool: &str, args: &[&str]) -> Option<String> {
    // Blocking child wait — allowed: watchdog / signal-dump thread only,
    // never the UI thread (which is wedged anyway).
    #[allow(clippy::disallowed_methods)]
    let out = std::process::Command::new(tool).args(args).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        return None;
    }
    Some(text)
}

/// Windows: launch a pristine copy of Ferail which opens this process and
/// writes a minidump containing every thread's context, stack pages, loaded
/// and unloaded modules, and handle/thread metadata. The matching release PDB
/// turns it into source-level stacks in WinDbg. Keeping MiniDumpWriteDump in a
/// child avoids depending on locks held by the frozen UI process.
#[cfg(windows)]
fn capture_thread_stacks(report_path: Option<&std::path::Path>) -> Option<(&'static str, String)> {
    let report_path = report_path?;
    let dump_path = report_path.with_extension("dmp");
    match crate::platform_shell::capture_hang_dump(&dump_path) {
        Ok(()) => Some((
            "MiniDumpWriteDump broker",
            format!(
                "All-thread minidump: {}\nAttach this .dmp together with this report; symbolize it with the exact matching Ferail PDB bundle.",
                dump_path.display()
            ),
        )),
        Err(error) => {
            append_to_file(
                report_path,
                &format!("\nWindows minidump capture FAILED: {error}\n"),
            );
            crate::log_warn!(
                90,
                "freeze-watchdog: Windows minidump capture failed: {error}"
            );
            None
        }
    }
}

/// AROS has no process/thread snapshot facility.
#[cfg(target_os = "aros")]
fn capture_thread_stacks(_report_path: Option<&std::path::Path>) -> Option<(&'static str, String)> {
    None
}

/// POSIX kill interception via the self-pipe trick. A signal handler may
/// only do async-signal-safe work; `write(2)` to a pipe is on that list,
/// and everything else happens on the `signal-dump` thread — which stays
/// schedulable while the UI thread is frozen, because only the UI thread
/// is stuck.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod unix_signals {
    use std::sync::atomic::{AtomicI32, Ordering};

    static PIPE_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

    extern "C" fn on_signal(signo: libc::c_int) {
        let fd = PIPE_WRITE_FD.load(Ordering::Relaxed);
        if fd >= 0 {
            let byte = signo as u8;
            // Async-signal-safe; O_NONBLOCK so a (never-expected) full
            // pipe can't wedge the handler.
            unsafe {
                libc::write(fd, &byte as *const u8 as *const libc::c_void, 1);
            }
        }
    }

    pub fn install() {
        let mut fds = [0 as libc::c_int; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            crate::log_warn!(90, "signal-dump: pipe() failed; kill interception disabled");
            return;
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);
        unsafe {
            libc::fcntl(write_fd, libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(write_fd, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(read_fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        PIPE_WRITE_FD.store(write_fd, Ordering::Relaxed);
        crate::obs::spawn_logged("signal-dump", move || dump_loop(read_fd));
        unsafe {
            libc::signal(libc::SIGQUIT, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        }
    }

    fn dump_loop(read_fd: libc::c_int) {
        loop {
            let mut byte = 0u8;
            let n = unsafe { libc::read(read_fd, &mut byte as *mut u8 as *mut libc::c_void, 1) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            if n == 0 {
                return; // write end closed — process is tearing down
            }
            let signo = byte as libc::c_int;
            match signo {
                // Ctrl+\ in a terminal. The documented "the app is frozen,
                // give me a dump" gesture: always report, then exit with
                // the conventional 128+signal code.
                libc::SIGQUIT => {
                    super::write_hang_report("SIGQUIT received (freeze dump requested)");
                    std::process::exit(128 + libc::SIGQUIT);
                }
                // Ctrl+C / plain kill: dump only when the UI thread was
                // already stalled — a healthy quit stays quiet — then exit
                // like the default disposition would have.
                libc::SIGINT | libc::SIGTERM => {
                    if super::ui_thread_stalled() {
                        let name = if signo == libc::SIGINT {
                            "SIGINT"
                        } else {
                            "SIGTERM"
                        };
                        super::write_hang_report(&format!(
                            "{name} received while the UI thread was unresponsive"
                        ));
                    }
                    std::process::exit(128 + signo);
                }
                _ => {}
            }
        }
    }
}

/// Windows console-control interception. Unlike a POSIX signal handler,
/// the system runs this on its own injected thread, so it may do real
/// work directly. Returning FALSE afterwards hands the event to the
/// default handler, which terminates the process — same exit-after-dump
/// shape as the Unix path. Only fires for console launches; a frozen
/// GUI-launched app is covered by the automatic watchdog report.
#[cfg(windows)]
mod windows_console {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };

    unsafe extern "system" fn handler(ctrl_type: u32) -> i32 {
        match ctrl_type {
            // Ctrl+Break is the SIGQUIT analog: always dump.
            CTRL_BREAK_EVENT => {
                super::write_hang_report("Ctrl+Break received (freeze dump requested)");
            }
            // Ctrl+C: dump only when already frozen; healthy quits stay quiet.
            CTRL_C_EVENT if super::ui_thread_stalled() => {
                super::write_hang_report("Ctrl+C received while the UI thread was unresponsive");
            }
            _ => {}
        }
        0 // FALSE: continue to the default handler (terminate)
    }

    pub fn install() {
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_body_carries_reason_and_sections() {
        let body = render_report_body("test reason: synthetic stall");
        assert!(body.contains("Hang Report"));
        assert!(body.contains("test reason: synthetic stall"));
        assert!(body.contains("background tasks"));
        assert!(body.contains("breadcrumbs:"));
        assert!(body.contains("activity trail"));
        assert!(body.contains(env!("CARGO_PKG_VERSION")));
    }

    /// A real `sample` call graph is indented, count-prefixed and
    /// address-suffixed; the digest wants the wedged call first, clean.
    #[test]
    fn digest_picks_innermost_main_thread_frames_from_sample() {
        let text = "\
Call graph:
    884 Thread_8593187   DispatchQueue_1: com.apple.main-thread  (serial)
    + 884 start  (in dyld) + 6992  [0x18c5bc4e4]
    +   884 main  (in ferail-gpui) + 52  [0x10430bb08]
    +     884 ferail_gpui::boot::run_gui::hcdc44e5e9cb935bb  (in ferail-gpui) + 44  [0x1000c2d10]
    +       884 __psynch_cvwait  (in libsystem_kernel.dylib) + 8  [0x18a0c1f30]
    884 Thread_8593200
    + 884 thread_start  (in libsystem_pthread.dylib) + 8  [0x18a0f0e10]
";
        let frames = ui_thread_frames(text, 3);
        assert_eq!(
            frames,
            vec![
                "__psynch_cvwait  (in libsystem_kernel.dylib) + 8",
                "ferail_gpui::boot::run_gui  (in ferail-gpui) + 44",
                "main  (in ferail-gpui) + 52",
            ]
        );
    }

    /// `gdb` / `eu-stack` numbered frames are already innermost-first.
    #[test]
    fn digest_reads_numbered_backtrace_frames() {
        let text = "\
Thread 1 (LWP 4242):
#0  0x00007f1a2b3c4d5e in __futex_abstimed_wait () from /lib/libc.so.6
#1  0x00007f1a2b3c9999 in ferail_fs_native::read_dir ()
";
        let frames = ui_thread_frames(text, 3);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], "__futex_abstimed_wait () from /lib/libc.so.6");
        assert!(frames[1].contains("read_dir"));
    }

    #[test]
    fn digest_is_short_and_names_the_report() {
        let path = std::path::Path::new("/tmp/ferail-hang-1-0.txt");
        let digest = render_console_digest(None, Some(path));
        assert!(digest.lines().count() <= 8, "digest too long:\n{digest}");
        assert!(digest.contains("/tmp/ferail-hang-1-0.txt"));
        assert!(digest.contains("ui stack:"));
    }

    #[test]
    fn digest_frame_width_is_bounded() {
        let long = format!(
            "      2001 {}  (in ferail-gpui) + 8  [0x1]",
            "a".repeat(400)
        );
        assert!(clean_frame(&long).chars().count() <= 96);
    }

    #[test]
    fn stack_hint_is_never_empty_on_shipping_targets() {
        assert!(!stack_hint().is_empty());
    }

    #[test]
    fn stall_tracker_reports_once_recovers_and_rearms() {
        let mut tracker = StallTracker::new(7);
        for _ in 0..STALL_SUSPECT_CHECKS - 1 {
            assert_eq!(tracker.observe(7, BEAT_INTERVAL), WatchdogAction::None);
            assert!(!tracker.stalled());
        }
        assert_eq!(tracker.observe(7, BEAT_INTERVAL), WatchdogAction::Suspect);
        assert!(tracker.stalled());
        for _ in STALL_SUSPECT_CHECKS..STALL_REPORT_CHECKS - 1 {
            assert_eq!(tracker.observe(7, BEAT_INTERVAL), WatchdogAction::None);
        }
        assert_eq!(tracker.observe(7, BEAT_INTERVAL), WatchdogAction::Report);
        assert_eq!(tracker.observe(7, BEAT_INTERVAL), WatchdogAction::None);

        assert_eq!(tracker.observe(8, BEAT_INTERVAL), WatchdogAction::Recovered);
        assert!(!tracker.stalled());
        for _ in 0..STALL_REPORT_CHECKS - 1 {
            let _ = tracker.observe(8, BEAT_INTERVAL);
        }
        assert_eq!(tracker.observe(8, BEAT_INTERVAL), WatchdogAction::Report);
    }

    #[test]
    fn stall_tracker_does_not_report_system_sleep() {
        let mut tracker = StallTracker::new(1);
        assert_eq!(
            tracker.observe(1, BEAT_INTERVAL * 6),
            WatchdogAction::Rebaseline
        );
        assert!(!tracker.stalled());
        assert_eq!(tracker.observe(2, BEAT_INTERVAL), WatchdogAction::None);
    }
}
