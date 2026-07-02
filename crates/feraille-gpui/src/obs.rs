//! Tiny observability layer for crash investigation. Stdlib only.
//!
//! Call-sites below `LOG_THRESHOLD` (90) stay muted. The `log_info!` /
//! `log_warn!` / `log_error!` macros are `#[macro_export]` here so any
//! module in `feraille-gpui` can reach them via `crate::log_info!(...)`.
//!
//! Crash-diagnostic output (startup banner, panic hook, worker-panic
//! line) does **not** flow through the macros and is always printed.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Instant;

/// Iteration-tagged log threshold. Lines whose ID is **greater than or
/// equal to** `LOG_THRESHOLD` are printed; everything else is silently
/// dropped. Bumped at the start of each iteration to make stale
/// diagnostic noise disappear without having to delete the log
/// statements.
///
/// New log calls should be tagged with the current iter. Harvest-phase
/// commits use IDs in the 90–99 range; the next polish iter would use
/// 100+.
pub const LOG_THRESHOLD: u32 = 90;
const BREADCRUMB_CAP: usize = 64;

static START: OnceLock<Instant> = OnceLock::new();
static BREADCRUMBS: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

pub fn init() {
    START.get_or_init(Instant::now);

    // Always backtrace on panic unless the user explicitly opted out.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        // SAFETY: single-threaded process startup, before any worker spawn.
        unsafe { std::env::set_var("RUST_BACKTRACE", "1") };
    }

    install_panic_hook();
    print_startup_banner();
}

pub fn elapsed_secs() -> f64 {
    START
        .get()
        .map(|t| t.elapsed().as_secs_f64())
        .unwrap_or(0.0)
}

pub fn line(level: &str, args: std::fmt::Arguments) {
    eprintln!("[+{:7.3}s][{}] {}", elapsed_secs(), level, args);
}

pub fn breadcrumb(args: std::fmt::Arguments) {
    let entry = format!("[+{:7.3}s] {}", elapsed_secs(), args);
    // Poison-tolerant: a panic in another thread must not turn the crash
    // reporter itself into a second, report-less abort.
    let mut guard = breadcrumbs().lock().unwrap_or_else(|e| e.into_inner());
    if guard.len() == BREADCRUMB_CAP {
        guard.pop_front();
    }
    guard.push_back(entry);
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = panic_payload_string(info.payload());
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        print_crash_report(thread_name, &location, &payload);
    }));
}

fn panic_payload_string(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

fn print_startup_banner() {
    let version = env!("CARGO_PKG_VERSION");
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let target = format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<?>".to_string());
    let pid = std::process::id();
    let wall = wall_clock_iso();

    eprintln!(
        "--- Feraille (gpui) {version} ({profile}, {target}) pid={pid} log>={LOG_THRESHOLD} {wall} ---",
    );
    line("info", format_args!("cwd: {cwd}"));
    if args.is_empty() {
        line("info", format_args!("args: <none>"));
    } else {
        line("info", format_args!("args: {}", args.join(" ")));
    }
    line(
        "info",
        format_args!("logs on stderr; backtrace via RUST_BACKTRACE (default=1)"),
    );
}

fn breadcrumbs() -> &'static Mutex<VecDeque<String>> {
    BREADCRUMBS.get_or_init(|| Mutex::new(VecDeque::with_capacity(BREADCRUMB_CAP)))
}

fn print_crash_report(thread_name: &str, location: &str, payload: &str) {
    let backtrace = std::backtrace::Backtrace::force_capture();
    let backtrace_text = format!("{backtrace}");
    let full = std::env::var_os("FERAILLE_FULL_BACKTRACE").is_some()
        || std::env::var_os("RUST_BACKTRACE")
            .and_then(|v| v.into_string().ok())
            .map(|v| v == "full")
            .unwrap_or(false);
    eprintln!();
    eprintln!("==================== Feraille (gpui) Crash ====================");
    eprintln!("time      : +{:.3}s", elapsed_secs());
    eprintln!("thread    : {thread_name}");
    eprintln!("location  : {location}");
    eprintln!("message   : {payload}");
    eprintln!();
    dump_breadcrumbs_for_panic();
    eprintln!();
    if full {
        eprintln!("backtrace :");
        eprintln!("{backtrace_text}");
    } else {
        eprintln!("relevant frames:");
        print_compact_backtrace(&backtrace_text);
        eprintln!("hint      : set FERAILLE_FULL_BACKTRACE=1 for the full raw backtrace");
    }
    eprintln!("===============================================================");
    eprintln!();
}

fn print_compact_backtrace(backtrace: &str) {
    let mut printed = 0usize;
    let mut pending_frame: Option<&str> = None;
    for line in backtrace.lines() {
        let trimmed = line.trim_start();
        let is_frame = trimmed
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
            && trimmed.contains(':');
        if is_frame {
            pending_frame = Some(line);
            continue;
        }
        let relevant = line.contains("feraille_")
            || line.contains("crates/feraille-")
            || line.contains("gpui::app::entity_map")
            || line.contains("gpui/src/app.rs")
            || line.contains("gpui/src/app/context.rs");
        if relevant {
            if let Some(frame) = pending_frame.take() {
                eprintln!("    {frame}");
                printed += 1;
            }
            eprintln!("    {line}");
            printed += 1;
        }
        if printed >= 28 {
            eprintln!("    ... compacted ...");
            return;
        }
    }
    if printed == 0 {
        eprintln!("    <no Feraille/GPUI frames found>");
    }
}

fn dump_breadcrumbs_for_panic() {
    let guard = breadcrumbs().lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_empty() {
        eprintln!("breadcrumbs:");
        eprintln!("    <none>");
        return;
    }
    eprintln!("breadcrumbs:");
    for (idx, entry) in guard.iter().enumerate() {
        eprintln!("    {:>2}. {entry}", idx + 1);
    }
}

/// Best-effort wall-clock UTC timestamp without pulling in chrono.
fn wall_clock_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}Z (epoch {secs})")
}

/// Spawn a thread whose panic is logged with the given task name instead
/// of vanishing silently. The shared panic hook still prints the panic
/// site; this just adds a "worker X failed" line on top so the operator
/// knows which task crashed.
pub fn spawn_logged<F>(task_name: &'static str, f: F) -> std::thread::JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    std::thread::Builder::new()
        .name(task_name.to_string())
        .spawn(move || {
            if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
                let payload = panic_payload_string(panic.as_ref());
                line(
                    "error",
                    format_args!("worker '{task_name}' panicked: {payload}"),
                );
            }
        })
        .expect("spawn worker thread")
}

// =============================================================================
// Macros — `#[macro_export]`ed so any feraille-gpui code can reach them via
// `crate::log_info!`, `crate::log_warn!`, `crate::log_error!`.
// =============================================================================

#[macro_export]
macro_rules! log_info {
    ($id:expr, $($arg:tt)*) => {
        if $id >= $crate::obs::LOG_THRESHOLD {
            $crate::obs::line("info", format_args!($($arg)*))
        }
    };
}

#[macro_export]
macro_rules! log_warn {
    ($id:expr, $($arg:tt)*) => {
        if $id >= $crate::obs::LOG_THRESHOLD {
            $crate::obs::line("warn", format_args!($($arg)*))
        }
    };
}

#[macro_export]
macro_rules! log_error {
    ($id:expr, $($arg:tt)*) => {
        if $id >= $crate::obs::LOG_THRESHOLD {
            $crate::obs::line("error", format_args!($($arg)*))
        }
    };
}
