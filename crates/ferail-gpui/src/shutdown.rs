//! Shutdown diagnostics: why the process is still here after its last window.
//!
//! The failure this exists for: a user closes Ferail to install an update, the
//! taskbar icon disappears, and the process is still running. Nothing is on
//! screen to close, so the only way out is Task Manager, and the new `.exe`
//! cannot replace the running one until then.
//!
//! The mechanism behind it is not a mystery, it is just invisible. Outside
//! macOS, Ferail quits when GPUI reports no windows left ([`crate::boot`]'s
//! `on_window_closed`). Anything that keeps one window registered, an orphaned
//! sub-window, a close callback that never fired, keeps the process alive with
//! no surface to interact with. Nothing forces the exit afterwards.
//!
//! So this module does three things, in order of increasing violence:
//!
//! 1. counts what is still registered, cheaply, from the UI thread;
//! 2. writes a report naming it if the process outlives its grace period,
//!    so the next bug report carries evidence instead of a guess;
//! 3. exits anyway at a hard deadline, because a process the user has to hunt
//!    down in Task Manager is worse than one that stopped a few seconds late.
//!
//! Step 3 can be turned off with `FERAIL_NO_SHUTDOWN_EXIT=1` when the point is
//! to attach a debugger to the stuck process rather than to be rid of it.
//!
//! Note which half of this needs a terminal. The report is written whatever
//! launched Ferail, Explorer and the Dock included, because the user hitting
//! this bug is not running the app from a shell. The environment variables
//! here, and the `--stuck-shutdown` probe, have to be set before launch, so
//! they are command-line only; so is the log line naming the report path,
//! which goes to stderr and is lost when no console is attached. When asking a
//! user for evidence, point at the file, not at the message.
//! [FREEZE_DIAGNOSTICS.md](../../../docs/features/FREEZE_DIAGNOSTICS.md) has
//! the per-shell command lines.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// How long a normal quit is allowed to take before the process is presumed
/// stuck and a report is written. Generous: teardown legitimately drains
/// callbacks, saves state, and closes the metadata database.
const REPORT_AFTER: Duration = Duration::from_secs(5);

/// How long after the report before the process exits on its own. The window
/// between the two exists so the report describes a still-stuck process, not
/// one that was about to finish.
const EXIT_AFTER: Duration = Duration::from_secs(15);

/// Both delays, with a testing override.
///
/// `FERAIL_SHUTDOWN_GRACE_MS` collapses them to one short interval so this
/// path can be exercised deliberately: a quit that hangs is not something you
/// can reproduce on demand, and waiting twenty seconds to find out whether the
/// report is written is how a diagnostic goes unverified.
fn delays() -> (Duration, Duration) {
    match std::env::var("FERAIL_SHUTDOWN_GRACE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        Some(ms) => {
            let grace = Duration::from_millis(ms.clamp(50, 60_000));
            (grace, grace)
        }
        None => (REPORT_AFTER, EXIT_AFTER),
    }
}

static ARMED: AtomicBool = AtomicBool::new(false);
static WINDOWS_OPEN: AtomicUsize = AtomicUsize::new(0);

/// Labels of the auxiliary windows still alive, published from the UI thread.
/// Scrubbed at the publishing site, so this can go straight into a report.
static AUX_WINDOWS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Publish what GPUI still tracks. Called from the watchdog heartbeat (once a
/// second, on the UI thread) and from every window close, so the watchdog
/// thread can read the count without touching `App`.
pub(crate) fn note_windows(open: usize, aux_labels: Vec<String>) {
    WINDOWS_OPEN.store(open, Ordering::Release);
    // Never let the reporting bookkeeping block the UI thread. A missed
    // publish costs one stale line in a report that may never be written.
    if let Ok(mut labels) = AUX_WINDOWS.try_lock() {
        *labels = aux_labels;
    }
}

/// Note a window closing, with the number GPUI still has after it.
///
/// This is the breadcrumb that makes the report readable in hindsight: a run
/// of closes that never reaches zero is the whole diagnosis.
pub(crate) fn note_window_closed(remaining: usize) {
    crate::obs::breadcrumb(format_args!("window closed; {remaining} still registered"));
}

/// Arm the shutdown watchdog. Idempotent: the first arming wins, so a quit
/// that also closes the last window does not start two threads.
///
/// `trigger` says what asked the process to end, and is the first line of any
/// report that follows.
pub fn arm(trigger: &'static str) {
    if ARMED.swap(true, Ordering::AcqRel) {
        return;
    }
    crate::obs::breadcrumb(format_args!("shutdown armed: {trigger}"));
    let (report_after, exit_after) = delays();
    crate::obs::spawn_logged("shutdown-watchdog", move || {
        std::thread::sleep(report_after);
        // Reaching here at all means the process outlived its own quit: a
        // clean exit would have taken this thread down with it.
        let details = render_details(trigger);
        let path = crate::watchdog::write_shutdown_report(
            &format!("still running {report_after:?} after {trigger}"),
            &details,
        );
        match &path {
            Some(path) => crate::log_warn!(
                90,
                "shutdown: still running after {trigger}; report written to {}",
                path.display()
            ),
            None => crate::log_warn!(90, "shutdown: still running after {trigger}; no report"),
        }

        if std::env::var_os("FERAIL_NO_SHUTDOWN_EXIT").is_some() {
            crate::log_warn!(90, "shutdown: exit suppressed by FERAIL_NO_SHUTDOWN_EXIT");
            return;
        }
        std::thread::sleep(exit_after);
        crate::log_warn!(
            90,
            "shutdown: forcing exit; {} window(s) never closed",
            WINDOWS_OPEN.load(Ordering::Acquire)
        );
        // The alternative is a process with no window, no taskbar entry, and
        // no way out but Task Manager, holding its own .exe against an
        // update. Everything that owns unsaved work has already been asked to
        // finish: this path is only reached because it did not.
        std::process::exit(0);
    });
}

/// The shutdown-specific half of the report: what is still registered, and
/// which named sub-windows are among it.
fn render_details(trigger: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(512);
    let _ = writeln!(out, "shutdown  : {trigger}");
    let open = WINDOWS_OPEN.load(Ordering::Acquire);
    let _ = writeln!(out, "windows   : {open} still registered with gpui");
    let labels = match AUX_WINDOWS.try_lock() {
        Ok(labels) => labels.clone(),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner().clone(),
        Err(std::sync::TryLockError::WouldBlock) => {
            let _ = writeln!(out, "aux       : <unavailable: lock held>");
            return out;
        }
    };
    if labels.is_empty() {
        // Worth stating plainly: with no auxiliary window left, a non-zero
        // count above means a main window or an orphan gpui never dropped.
        let _ = writeln!(
            out,
            "aux       : <none> (a non-zero count above is a main or orphaned window)"
        );
    } else {
        let _ = writeln!(out, "aux       :");
        for label in &labels {
            let _ = writeln!(out, "    {label}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{note_windows, render_details, AUX_WINDOWS};

    // One test, not two: the published state is process-global, and two tests
    // writing it in parallel would race each other rather than the code.
    #[test]
    fn details_name_what_outlived_the_quit() {
        note_windows(2, vec!["Viewer: photo".into(), "Editor: notes".into()]);
        let rendered = render_details("last window closed");
        assert!(rendered.contains("last window closed"));
        assert!(rendered.contains("2 still registered"));
        assert!(rendered.contains("Viewer: photo"));
        assert!(rendered.contains("Editor: notes"));

        // A stale label list would blame a sub-window that has already gone,
        // so publishing an empty list must clear the previous one.
        note_windows(1, Vec::new());
        assert!(AUX_WINDOWS.lock().unwrap().is_empty());
        let rendered = render_details("quit requested");
        assert!(rendered.contains("main or orphaned window"));
    }
}
