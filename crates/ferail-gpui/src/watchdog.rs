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

/// Install every piece. Called once from `boot::run_gui`, after the
/// `ProcessState` global exists (the snapshot reads it) and before any
/// window opens. Deliberately NOT called on the screenshot path — a
/// headless capture has no event loop to monitor.
pub fn start(cx: &mut App) {
    // Quit teardown stops the heartbeat without being a freeze; tell the
    // watchdog so it can't misfire in a slow shutdown. Leaks the
    // subscription intentionally (lives the whole app run).
    cx.on_app_quit(|_| {
        SHUTDOWN.store(true, Ordering::Relaxed);
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
        cx.background_executor()
            .timer(Duration::from_secs(3))
            .await;
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
    STALLED.load(Ordering::Relaxed)
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
                BEAT.fetch_add(1, Ordering::Relaxed);
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
    *snapshot_cell().lock().unwrap_or_else(|e| e.into_inner()) = lines;
}

fn start_watchdog_thread() {
    crate::obs::spawn_logged("freeze-watchdog", || {
        let mut last_check = Instant::now();
        let mut baseline = BEAT.load(Ordering::Relaxed);
        let mut stale_checks: u32 = 0;
        let mut reported = false;
        loop {
            std::thread::sleep(BEAT_INTERVAL);
            if SHUTDOWN.load(Ordering::Relaxed) {
                return;
            }
            let gap = last_check.elapsed();
            last_check = Instant::now();
            let beat = BEAT.load(Ordering::Relaxed);
            // A check gap far past the interval means the machine slept or
            // the whole process was stopped — both threads froze together,
            // so re-baseline rather than counting it as a UI stall.
            if gap > BEAT_INTERVAL * 5 {
                baseline = beat;
                stale_checks = 0;
                STALLED.store(false, Ordering::Relaxed);
                continue;
            }
            if beat != baseline {
                baseline = beat;
                stale_checks = 0;
                STALLED.store(false, Ordering::Relaxed);
                if reported {
                    reported = false;
                    crate::log_warn!(90, "freeze-watchdog: UI thread recovered; re-armed");
                }
                continue;
            }
            stale_checks += 1;
            if stale_checks >= STALL_SUSPECT_CHECKS {
                STALLED.store(true, Ordering::Relaxed);
            }
            if stale_checks >= STALL_REPORT_CHECKS && !reported {
                reported = true;
                write_hang_report(&format!(
                    "UI thread unresponsive for ~{stale_checks}s (heartbeat stalled)"
                ));
            }
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
    let snapshot = snapshot_cell()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if snapshot.is_empty() {
        let _ = writeln!(r, "    <none>");
    } else {
        for line in &snapshot {
            let _ = writeln!(r, "    {line}");
        }
    }
    let _ = writeln!(r);

    let _ = writeln!(r, "breadcrumbs:");
    let crumbs = crate::obs::breadcrumb_lines();
    if crumbs.is_empty() {
        let _ = writeln!(r, "    <none>");
    } else {
        for line in &crumbs {
            let _ = writeln!(r, "    {line}");
        }
    }
    let _ = writeln!(r);

    // Redaction-honoring render — a hang report must be as shareable as
    // the issue bundle. Tail only: the freeze context is the recent past.
    const TRAIL_TAIL: usize = 40;
    let trail = crate::trail::render_lines_sanitized();
    let skipped = trail.len().saturating_sub(TRAIL_TAIL);
    let _ = writeln!(r, "activity trail (last {} of {}):", trail.len() - skipped, trail.len());
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
/// disk beats a prettier one lost), echo it to stderr, then attempt the
/// per-platform whole-process stack capture and append it. Callable from
/// any thread except the (presumably wedged) UI thread.
pub fn write_hang_report(reason: &str) {
    let body = render_report_body(reason);
    let path = report_file_path();
    if let Some(path) = &path {
        if let Err(e) = std::fs::write(path, &body) {
            crate::log_warn!(90, "hang report write failed ({}): {e}", path.display());
        }
    }
    crate::obs::stderr_line(&body);
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

    match capture_thread_stacks() {
        Some((tool, stacks)) => {
            let section = format!("\nthread stacks (via {tool}):\n{stacks}\n");
            if let Some(path) = &path {
                append_to_file(path, &section);
            }
            crate::obs::stderr_line(&section);
        }
        None => {
            let hint = format!("\nthread stacks : <unavailable> — {}\n", stack_hint());
            if let Some(path) = &path {
                append_to_file(path, &hint);
            }
            crate::obs::stderr_line(&hint);
        }
    }

    if let Some(path) = &path {
        crate::obs::stderr_line(&format!(
            "hang report saved: {} — please attach it when reporting the freeze",
            path.display()
        ));
    }
}

/// `<config>/reports/ferail-hang-<pid>-<seq>.txt` — the same folder the
/// issue bundle uses, so users find both in one place.
fn report_file_path() -> Option<std::path::PathBuf> {
    let dir = crate::app_state::config_dir()?.join("reports");
    std::fs::create_dir_all(&dir).ok()?;
    let seq = HANG_SEQ.fetch_add(1, Ordering::Relaxed);
    Some(dir.join(format!("ferail-hang-{}-{seq}.txt", std::process::id())))
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
fn capture_thread_stacks() -> Option<(&'static str, String)> {
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
fn capture_thread_stacks() -> Option<(&'static str, String)> {
    unsafe {
        libc::prctl(libc::PR_SET_PTRACER, libc::PR_SET_PTRACER_ANY, 0, 0, 0);
    }
    let pid = std::process::id().to_string();
    let result = run_stack_tool("eu-stack", &["-p", &pid])
        .map(|text| ("eu-stack", text))
        .or_else(|| {
            run_stack_tool("gdb", &["--batch", "-p", &pid, "-ex", "thread apply all bt"])
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

/// Windows / AROS: no in-process whole-thread stack tool wired yet; the
/// report explains what to capture instead (see [`stack_hint`]).
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn capture_thread_stacks() -> Option<(&'static str, String)> {
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
                        let name = if signo == libc::SIGINT { "SIGINT" } else { "SIGTERM" };
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
            CTRL_C_EVENT => {
                if super::ui_thread_stalled() {
                    super::write_hang_report(
                        "Ctrl+C received while the UI thread was unresponsive",
                    );
                }
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

    #[test]
    fn stack_hint_is_never_empty_on_shipping_targets() {
        assert!(!stack_hint().is_empty());
    }
}
