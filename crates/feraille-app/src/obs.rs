//! Tiny observability layer for crash investigation. Stdlib only.
//!
//! - `init()` installs a panic hook, defaults `RUST_BACKTRACE=1`, and prints
//!   a one-shot startup banner so we know which build was running.
//! - `line()` formats stderr lines with elapsed-since-start timestamps so
//!   the seconds-before-crash window is easy to read.
//! - `spawn_logged()` wraps `thread::spawn` with `catch_unwind` so worker
//!   panics surface in the terminal instead of vanishing.
//!
//! Macros `log_info!`, `log_warn!`, `log_error!` live in `main.rs` and
//! delegate here.

use std::sync::OnceLock;
use std::time::Instant;

/// Iteration-tagged log threshold.
///
/// Every `log_info!` / `log_warn!` / `log_error!` call carries an integer
/// ID. Lines whose ID is **greater than or equal to** `LOG_THRESHOLD` are
/// printed; everything else is silently dropped. Bump this constant at the
/// start of each iteration to make stale diagnostic noise disappear without
/// having to delete the log statements.
///
/// New log calls should be tagged with the current iter (e.g. iter-5.7 →
/// `57`). Crash-diagnostic output (startup banner, panic hook, worker
/// panic line) does **not** flow through the macros and is always printed.
pub const LOG_THRESHOLD: u32 = 57;

static START: OnceLock<Instant> = OnceLock::new();

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
    START.get().map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0)
}

pub fn line(level: &str, args: std::fmt::Arguments) {
    eprintln!("[+{:7.3}s][{}] {}", elapsed_secs(), level, args);
}

fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = panic_payload_string(info.payload());
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        eprintln!(
            "[+{:7.3}s][PANIC] thread \"{}\" at {}: {}",
            elapsed_secs(),
            thread_name,
            location,
            payload,
        );
        original(info);
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
        "--- Feraille {version} ({profile}, {target}) pid={pid} log>={LOG_THRESHOLD} {wall} ---",
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

/// Best-effort wall-clock UTC timestamp without pulling in chrono.
fn wall_clock_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Crude but useful: H:M:S UTC, plus epoch seconds for unambiguous correlation.
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
