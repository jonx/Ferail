//! Tiny observability layer for crash investigation. Stdlib only.
//!
//! Call-sites below `LOG_THRESHOLD` (90) stay muted. The `log_info!` /
//! `log_warn!` / `log_error!` macros are `#[macro_export]` here so any
//! module in `ferail-gpui` can reach them via `crate::log_info!(...)`.
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
    // ...but never let that leak into *library* error construction.
    // `std::backtrace::Backtrace::capture()`: which `anyhow::Error` calls on
    // every `anyhow!`/`bail!`/`.context()`: consults RUST_LIB_BACKTRACE
    // first and falls back to RUST_BACKTRACE, so the default above would
    // silently make every anyhow error in the process capture a full stack
    // walk. Hot paths build those errors by the hundreds per frame (gpui's
    // font lookup re-wraps a cached miss in a fresh `anyhow!` per text run;
    // on Linux that walk is ~9 misses deep), and on Linux each capture is a
    // libgcc `_Unwind_Find_FDE` crawl over a 100 MB binary: measured at 78%
    // of the UI thread's time on the Ubuntu VM, the app crawling at seconds
    // per frame. Panics are unaffected: std's panic path honours
    // RUST_BACKTRACE, and our hook uses `Backtrace::force_capture()` anyway.
    if std::env::var_os("RUST_LIB_BACKTRACE").is_none() {
        // SAFETY: as above: startup, before any worker spawn.
        unsafe { std::env::set_var("RUST_LIB_BACKTRACE", "0") };
    }

    install_panic_hook();
    // Native exceptions (access violations in drivers, shell extensions,
    // GPU code) never reach the panic hook; on Windows a minidump lands
    // next to the text report instead, and the OS keeps its default
    // handling afterwards. See `ferail_shell_win32::install_crash_dump_handler`.
    #[cfg(windows)]
    if let Some(dir) = crate::app_state::config_dir() {
        crate::platform_shell::install_crash_dump_handler(&dir.join("reports"), "crash", false);
    }
    print_startup_banner();

    // On booted AROS a shell command's stderr goes to the AROS console, not the
    // host process, so `eprintln!` never reaches the `aros-ctl` log. And gpui's
    // `.log_err()` (e.g. the swallowed `paint_svg` failure) routes through the
    // `log` crate, which is a no-op until a logger is installed, nothing
    // installs one on AROS. Bridge the `log` facade into the MacRW file sink so
    // both are actually observable on the host (~/AROS/Shared/ferail-log.txt).
    #[cfg(target_os = "aros")]
    {
        // Info by default: at Trace, cosmic_text shapes and notify rescans
        // flood both sinks (tens of lines per frame: real I/O cost, and the
        // volume that used to overflow the boot console). FERAIL_LOG_TRACE=1
        // restores the firehose for icon/text debugging sessions.
        let max = if std::env::var_os("FERAIL_LOG_TRACE").is_some() {
            log::LevelFilter::Trace
        } else {
            log::LevelFilter::Info
        };
        let _ = log::set_logger(&AROS_LOGGER).map(|()| log::set_max_level(max));
        line(
            "info",
            format_args!("MacRW:ferail-log.txt sink open; `log` crate bridged (max={max})"),
        );
    }
}

pub fn elapsed_secs() -> f64 {
    START
        .get()
        .map(|t| t.elapsed().as_secs_f64())
        .unwrap_or(0.0)
}

pub fn line(level: &str, args: std::fmt::Arguments) {
    let msg = format!("[+{:7.3}s][{}] {}", elapsed_secs(), level, args);
    // On AROS, routine logs must NOT touch stderr: stderr is the boot
    // console, the console lives on its own Intuition screen, and a write
    // can yank that screen in front of the app's (observed 2026-07-21:
    // every `navigate:` line swapped screens mid-click, which reads as
    // "the listview stopped updating" and eats the next clicks). The
    // MacRW file sink is the log surface there; stderr stays for panics,
    // where the app is dying anyway and surfacing the console is fine.
    #[cfg(target_os = "aros")]
    aros_sink(&msg);
    #[cfg(not(target_os = "aros"))]
    stderr_line(&msg);
}

/// Non-panicking stderr write. `eprintln!` panics when stderr fails, and on
/// AROS the boot-console handler does fail partial writes under sustained
/// output, with panic=abort that cascaded into a whole-OS deadend reboot
/// (the "spontaneous silent reboot": notify-rs TRACE spam → eprintln! →
/// "failed printing to stderr" panic). Diagnostics must never kill the app.
pub fn stderr_line(msg: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{msg}");
}

/// AROS-only host-visible diagnostic sink. Appends each line to
/// `MacRW:ferail-log.txt`: the host-shared volume, readable on macOS as
/// `~/AROS/Shared/ferail-log.txt`. Truncated on first write each boot. This
/// is the same host-durable trick the panic hook uses (`ferail-panic.txt`),
/// needed because AROS stderr never reaches the `aros-ctl` log.
#[cfg(target_os = "aros")]
pub fn aros_sink(msg: &str) {
    use std::io::Write;
    static SINK: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();
    let cell = SINK.get_or_init(|| {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open("MacRW:ferail-log.txt")
            .ok();
        Mutex::new(f)
    });
    let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(file) = guard.as_mut() {
        let _ = writeln!(file, "{msg}");
        let _ = file.flush();
    }
}

/// Forwards the `log` crate facade into [`line`] (stderr + the MacRW sink).
/// Installed only on AROS; on the desktop the app's own binary owns logging.
#[cfg(target_os = "aros")]
struct ArosLogger;

#[cfg(target_os = "aros")]
impl log::Log for ArosLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        line(
            "log",
            format_args!("{} {}: {}", record.level(), record.target(), record.args()),
        );
    }

    fn flush(&self) {}
}

#[cfg(target_os = "aros")]
static AROS_LOGGER: ArosLogger = ArosLogger;

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

    stderr_line(&format!(
        "--- Ferail (gpui) {version} ({profile}, {target}) pid={pid} log>={LOG_THRESHOLD} {wall} ---",
    ));
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

/// How much of the crash lands on stderr vs. in the report file. A
/// terminal crash used to dump everything (up to 64 breadcrumbs + 28
/// backtrace lines) on the console; now stderr gets a digest sized to
/// these caps and the report file gets the whole story: every
/// breadcrumb plus the complete raw backtrace.
const STDERR_BREADCRUMBS: usize = 8;
const STDERR_FRAME_LINES: usize = 12;
const REPORT_FRAME_LINES: usize = 28;

fn print_crash_report(thread_name: &str, location: &str, payload: &str) {
    use std::fmt::Write as _;

    // Persist the small, allocation-light core before capturing a backtrace.
    // A panic hook has no second chance: if symbolization itself fails, this
    // file still proves where and why the process panicked.
    let essential = format!(
        "Ferail {} crash\npid: {}\nthread: {thread_name}\nlocation: {location}\nmessage: {payload}\n",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
    );
    let crash_path = persist_crash_report(&essential);

    // Persist the essential facts FIRST, before anything that can fail. A
    // panic inside this hook aborts immediately (panic-in-hook cannot
    // unwind), and on this port Backtrace::force_capture() is the prime
    // suspect: reports were lost entirely when it came before the write.
    #[cfg(target_os = "aros")]
    {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("MacRW:ferail-panic.txt")
        {
            let _ = writeln!(
                f,
                "PANIC +{:.3}s thread '{thread_name}' at {location}: {payload}",
                elapsed_secs()
            );
            let _ = f.flush();
        }
    }

    let backtrace = std::backtrace::Backtrace::force_capture();
    let backtrace_text = format!("{backtrace}");
    let full = std::env::var_os("FERAIL_FULL_BACKTRACE").is_some()
        || std::env::var_os("RUST_BACKTRACE")
            .and_then(|v| v.into_string().ok())
            .map(|v| v == "full")
            .unwrap_or(false);

    // The report file gets the whole story, unconditionally: every
    // breadcrumb, the compacted Ferail/GPUI frames for a quick read, and
    // the complete raw backtrace. Appended: persist_crash_report already
    // seeded the file, and a second panic (or a native-exception sidecar
    // line) must never overwrite the first, informative report.
    let mut detailed = String::with_capacity(4096);
    write_report_header(&mut detailed, thread_name, location, payload);
    dump_breadcrumbs_for_panic(&mut detailed, usize::MAX);
    let _ = writeln!(detailed);
    let _ = writeln!(detailed, "relevant frames:");
    write_compact_backtrace(&mut detailed, &backtrace_text, REPORT_FRAME_LINES);
    let _ = writeln!(detailed);
    let _ = writeln!(detailed, "backtrace :");
    let _ = writeln!(detailed, "{backtrace_text}");
    let _ = writeln!(
        detailed,
        "==============================================================="
    );

    if let Some(path) = &crash_path {
        append_report(path, &detailed);
    }

    // stderr gets a console-sized digest: the last few breadcrumbs, the
    // relevant frames, and a pointer to the report file for the rest.
    // Two cases still print everything: the user asked for the full
    // backtrace inline, or no report file could be written (stderr is
    // then the only copy).
    match &crash_path {
        Some(path) if !full => {
            let mut brief = String::with_capacity(2048);
            write_report_header(&mut brief, thread_name, location, payload);
            dump_breadcrumbs_for_panic(&mut brief, STDERR_BREADCRUMBS);
            let _ = writeln!(brief);
            let _ = writeln!(brief, "relevant frames:");
            write_compact_backtrace(&mut brief, &backtrace_text, STDERR_FRAME_LINES);
            let _ = writeln!(
                brief,
                "full report: {} (every breadcrumb + the complete backtrace; \
                 set FERAIL_FULL_BACKTRACE=1 to print it here instead)",
                path.display()
            );
            let _ = writeln!(
                brief,
                "==============================================================="
            );
            stderr_line(&brief);
        }
        _ => stderr_line(&detailed),
    }

    // On AROS, stderr is lost (console-bound) and with panic=abort the whole
    // OS may reboot before anything drains: persist the report to the
    // host-shared volume with a dedicated file handle. Deliberately NOT the
    // shared aros_sink: the panicking thread may hold its (non-reentrant)
    // mutex, and a deadlocked panic hook reports nothing at all.
    #[cfg(target_os = "aros")]
    {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("MacRW:ferail-panic.txt")
        {
            let _ = writeln!(f, "{detailed}");
            let _ = f.flush();
        }
    }
}

fn write_report_header(out: &mut String, thread_name: &str, location: &str, payload: &str) {
    use std::fmt::Write as _;
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "==================== Ferail (gpui) Crash ===================="
    );
    let _ = writeln!(out, "time      : +{:.3}s", elapsed_secs());
    let _ = writeln!(out, "thread    : {thread_name}");
    let _ = writeln!(out, "location  : {location}");
    let _ = writeln!(out, "message   : {payload}");
    let _ = writeln!(out);
}

/// Append `text` to the crash-report file. Append, never truncate: the
/// same double-panic rationale as [`persist_crash_report`], plus the
/// Windows native-exception filter shares this file for its sidecar line.
fn append_report(path: &std::path::Path, text: &str) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(text.as_bytes());
    }
}

#[cfg(not(target_os = "aros"))]
fn persist_crash_report(essential: &str) -> Option<std::path::PathBuf> {
    let dir = crate::app_state::config_dir()?.join("reports");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("ferail-crash-{}.txt", std::process::id()));
    // Append, never truncate: a panic that unwinds into an `extern "C"`
    // frame raises a second, generic "panic in a function that cannot
    // unwind" panic that would otherwise overwrite the informative first one.
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()?;
        file.write_all(essential.as_bytes()).ok()?;
        file.write_all(b"\n").ok()?;
    }
    Some(path)
}

#[cfg(target_os = "aros")]
fn persist_crash_report(_essential: &str) -> Option<std::path::PathBuf> {
    None
}

fn write_compact_backtrace(out: &mut String, backtrace: &str, max_lines: usize) {
    use std::fmt::Write as _;

    // Backslash-normalized so Windows backtraces (`.\crates\ferail-gpui\...`)
    // match too: they used to slip past the `/`-only patterns and every
    // Windows report said "<no Ferail/GPUI frames found>". A frame counts if
    // either its symbol line or its `at path:line` line matches; the panic
    // hook's own frames are noise on every report and are skipped.
    fn is_relevant(line: &str) -> bool {
        let norm = line.replace('\\', "/");
        if norm.contains("obs::print_crash_report") || norm.contains("obs::install_panic_hook") {
            return false;
        }
        norm.contains("ferail_")
            || norm.contains("crates/ferail-")
            || norm.contains("gpui::app::entity_map")
            || norm.contains("gpui/src/app.rs")
            || norm.contains("gpui/src/app/context.rs")
            || norm.contains("gpui/src/app/entity_map.rs")
    }

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
        // Drop the hook's own frame together with its location line:
        // `is_relevant` can't do it alone, since the location (`at
        // ...crates/ferail-gpui/src/obs.rs`) matches the ferail patterns.
        if pending_frame
            .map(|f| f.contains("obs::print_crash_report") || f.contains("obs::install_panic_hook"))
            .unwrap_or(false)
        {
            pending_frame = None;
            continue;
        }
        let relevant = is_relevant(line) || pending_frame.map(is_relevant).unwrap_or(false);
        if relevant {
            if let Some(frame) = pending_frame.take() {
                let _ = writeln!(out, "    {frame}");
                printed += 1;
            }
            let _ = writeln!(out, "    {line}");
            printed += 1;
        }
        if printed >= max_lines {
            let _ = writeln!(out, "    ... compacted ...");
            return;
        }
    }
    if printed == 0 {
        let _ = writeln!(out, "    <no Ferail/GPUI frames found>");
    }
}

/// Breadcrumb ring contents, oldest → newest. Shared by the panic report
/// and the freeze watchdog's hang report (`crate::watchdog`).
pub fn breadcrumb_lines() -> Vec<String> {
    breadcrumbs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect()
}

/// Non-blocking snapshot for the freeze watchdog. If the stalled thread held
/// the breadcrumb mutex at the instant it wedged, waiting for that same mutex
/// would freeze the watchdog too and defeat the report.
pub fn try_breadcrumb_lines() -> Option<Vec<String>> {
    use std::sync::TryLockError;
    let guard = match breadcrumbs().try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => return None,
    };
    Some(guard.iter().cloned().collect())
}

/// Write the breadcrumb ring's last `limit` entries. The report file
/// passes `usize::MAX` (everything); stderr passes a console-sized cap
/// and notes how many earlier entries the report file holds.
fn dump_breadcrumbs_for_panic(out: &mut String, limit: usize) {
    use std::fmt::Write as _;
    let Some(guard) = try_breadcrumb_lines() else {
        let _ = writeln!(out, "breadcrumbs:");
        let _ = writeln!(out, "    <unavailable: held by panicking thread>");
        return;
    };
    if guard.is_empty() {
        let _ = writeln!(out, "breadcrumbs:");
        let _ = writeln!(out, "    <none>");
        return;
    }
    let _ = writeln!(out, "breadcrumbs:");
    let skipped = guard.len().saturating_sub(limit);
    if skipped > 0 {
        let _ = writeln!(out, "    ... {skipped} earlier (in the report file) ...");
    }
    for (idx, entry) in guard.iter().enumerate().skip(skipped) {
        let _ = writeln!(out, "    {:>2}. {entry}", idx + 1);
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
// Macros, `#[macro_export]`ed so any ferail-gpui code can reach them via
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

#[cfg(test)]
mod tests {
    // Trimmed from a real Windows report (leaked-handle assertion, pid 6948):
    // backslash paths, `impl$45`-style MSVC symbols, the hook's own frames.
    const WINDOWS_BACKTRACE: &str = "\
   3: std::backtrace::Backtrace::force_capture
             at /rustc/59807616/library\\std\\src\\backtrace.rs:312
   4: ferail_gpui::obs::print_crash_report
             at .\\crates\\ferail-gpui\\src\\obs.rs:283
  10: core::panicking::panic_fmt
             at /rustc/59807616/library\\core\\src\\panicking.rs:80
  11: gpui::app::entity_map::impl$45::drop
             at C:\\Users\\x\\.cargo\\git\\checkouts\\zed-a70e\\38ca910\\crates\\gpui\\src\\app\\entity_map.rs:1116
  12: core::ptr::drop_in_place
             at /rustc/59807616/library\\core\\src\\ptr\\mod.rs:805
";

    #[test]
    fn compact_backtrace_matches_windows_paths() {
        let mut out = String::new();
        super::write_compact_backtrace(&mut out, WINDOWS_BACKTRACE, 28);
        assert!(
            out.contains("gpui::app::entity_map::impl$45::drop"),
            "entity_map frame missing from: {out}"
        );
        // The hook's own capture frames are plumbing, not the crash site.
        assert!(
            !out.contains("print_crash_report"),
            "panic-hook frame should be filtered: {out}"
        );
        assert!(!out.contains("<no Ferail/GPUI frames found>"), "{out}");
    }

    #[test]
    fn compact_backtrace_caps_lines() {
        let mut out = String::new();
        super::write_compact_backtrace(&mut out, WINDOWS_BACKTRACE, 1);
        assert!(out.contains("... compacted ..."), "{out}");
    }
}
