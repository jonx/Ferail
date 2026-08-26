// Ferail — GPUI shell entry point.
//
// Dispatches between the live GUI and the headless `--screenshot`
// capture path. All real view code lives in `crate::shell`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ferail_disk_usage::{DiskUsageTree, NodeKind, build_layout_node};
use ferail_fs_native::{DEFAULT_DU_BATCH, NativeFs, detect_magic};
use ferail_gpui::screenshot;

fn main() -> Result<()> {
    // Disposable Windows Shell-menu broker. Dispatch before String-based CLI
    // parsing so every NTFS path survives as its original OsString and before
    // GPUI/observability startup so third-party extensions stay isolated.
    if let Some(code) = run_windows_context_menu_broker() {
        std::process::exit(code);
    }
    if let Some(code) = run_windows_namespace_broker() {
        std::process::exit(code);
    }
    // Pre-event-loop CLI handlers — run before the window opens.
    if let Some(code) = ferail_gpui::reset_db::handle_reset_db_cli() {
        std::process::exit(code);
    }
    if let Some(code) = handle_cli_subcommand()? {
        std::process::exit(code);
    }
    ferail_gpui::obs::init();
    // Honor the persisted diagnostics-privacy preference (defaults to on) before
    // anything can record a report-bound trail.
    ferail_gpui::redact::set_enabled(
        ferail_gpui::app_state::load()
            .redact_diagnostics
            .unwrap_or(true),
    );
    let args = screenshot::parse_args();
    if args.screenshot.is_some() {
        ferail_gpui::log_info!(90, "headless screenshot path");
        return screenshot::run(args);
    }
    ferail_gpui::log_info!(90, "event loop starting");
    run_gui(args);
    ferail_gpui::log_info!(90, "event loop exited");
    Ok(())
}

fn run_windows_namespace_broker() -> Option<i32> {
    #[cfg(windows)]
    {
        (std::env::args_os().nth(1).as_deref()
            == Some(std::ffi::OsStr::new("--windows-namespace-broker")))
        .then(ferail_gpui::platform_shell::namespace_broker_main)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn run_windows_context_menu_broker() -> Option<i32> {
    #[cfg(windows)]
    {
        let mut args = std::env::args_os().skip(1);
        if args.next().as_deref() != Some(std::ffi::OsStr::new("--windows-context-menu-broker")) {
            return None;
        }
        if let Some(dir) = ferail_gpui::app_state::config_dir() {
            ferail_gpui::platform_shell::install_crash_dump_handler(
                &dir.join("reports"),
                "context-menu-broker",
                true,
            );
        }
        let args = args.collect::<Vec<_>>();
        Some(ferail_gpui::platform_shell::context_menu_broker_main(&args))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn handle_cli_subcommand() -> Result<Option<i32>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        return Ok(None);
    };
    match cmd {
        "magic" => run_magic_cli(&args[1..]).map(Some),
        "du" | "disk-usage" => run_disk_usage_cli(&args[1..]).map(Some),
        "thumb" | "thumbnail" => run_thumb_cli(&args[1..]).map(Some),
        // Health check. `--doctor` is also accepted (it would otherwise fall
        // through to the GUI as an unknown flag).
        "doctor" | "--doctor" => Ok(Some(run_doctor_cli())),
        // Privileged file-op worker: a re-launch of this binary (usually
        // elevated) that performs one copy/move descriptor headlessly and
        // writes a result file, then exits. Never opens a window. Runs before
        // any GUI init so it works as root with no window-server access.
        "--elevated-op" => Ok(Some(ferail_gpui::elevation::run_elevated_op_worker(&args))),
        // Privileged trash/delete worker: same re-launch-as-root contract as
        // `--elevated-op`, but moves protected items into the user's Trash (or
        // removes them outright for permanent-delete / Empty Trash).
        "--elevated-trash" => Ok(Some(ferail_gpui::elevation::run_elevated_trash_op_worker(
            &args,
        ))),
        // Preview-broker worker (Windows): a disposable re-launch of this
        // binary that hosts one third-party IPreviewHandler off-process and
        // writes the captured frame to stdout, so a crashing or hanging
        // provider (the 0.6.5 pdfprevhndlr.dll access violation) can only
        // ever take down this short-lived helper. Runs before any GUI or
        // observability init.
        "--preview-broker" => Ok(Some(run_preview_broker(&args[1..]))),
        "help" | "-h" | "--help" => {
            print_cli_help();
            Ok(Some(0))
        }
        // Flags that belong to the GUI / screenshot path (`--screenshot`,
        // `--theme`, `--width`, etc.) pass through to `screenshot::parse_args`.
        // Anything else that looks like a subcommand attempt — a bare word
        // or unknown flag — surfaces the help text and exits with code 2
        // rather than silently launching the GUI with confusing args.
        other if !other.starts_with('-') => {
            eprintln!("ferail: unknown subcommand: {other:?}\n");
            print_cli_help();
            Ok(Some(2))
        }
        _ => Ok(None),
    }
}

/// `--preview-broker` worker mode — Windows-only crash containment for
/// third-party preview handlers (see `ferail_shell_win32::preview_broker_main`).
/// The cfg-gate lives here at the single call site rather than as stubs in the
/// other platform shell crates: this is a worker mode of the binary, not part
/// of the shared `platform_shell` surface.
fn run_preview_broker(args: &[String]) -> i32 {
    #[cfg(windows)]
    {
        // A faulting third-party handler should leave a minidump that names
        // the module (WIN-001) — quietly, so a broken previewer can't pop
        // Windows Error Reporting UI for every file it is asked about.
        if let Some(dir) = ferail_gpui::app_state::config_dir() {
            ferail_gpui::platform_shell::install_crash_dump_handler(
                &dir.join("reports"),
                "preview-broker",
                true,
            );
        }
        ferail_gpui::platform_shell::preview_broker_main(args)
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        eprintln!("ferail: --preview-broker is a Windows-only worker mode");
        2
    }
}

/// `ferail doctor` / `ferail --doctor` — print the diagnostics health
/// report and exit. Runs before the event loop, so it works even when the GUI
/// can't start (the common "nothing happens" case). Exit code 1 if any check
/// FAILs, else 0 — so it's scriptable / pasteable into a bug report.
fn run_doctor_cli() -> i32 {
    let report = ferail_gpui::diagnostics::run_checks();
    print!("{}", ferail_gpui::diagnostics::render_text(&report));
    match report.worst() {
        ferail_gpui::diagnostics::Status::Fail => 1,
        _ => 0,
    }
}

fn print_cli_help() {
    println!(
        "Ferail\n\nUsage:\n  ferail                 Open the GPUI file manager\n  ferail magic [path]...  Print magic-byte format (defaults to current directory; directories are listed shallow)\n  ferail du [options] <path>  Print disk-usage summary\n  ferail thumb <path> [--out <png>] [--size N] [--preview]  Extract a file's thumbnail/preview to a PNG\n  ferail doctor          Print a health check (config / storage / deps) and exit\n\nDisk usage options:\n  --top <n>        Number of entries to show (default: 20)\n  --packages       Descend into macOS package directories\n\nThumb options:\n  --out <path>     Output PNG path (default: thumb.png)\n  --size <px>      Max edge in pixels (default: 512)\n  --preview        Fetch what the preview pane would show instead of the grid thumbnail\n                   (on Windows this allows the brokered preview-handler capture)"
    );
}

fn run_magic_cli(args: &[String]) -> Result<i32> {
    // No paths → scan the current directory's files. A single
    // directory argument behaves the same way. File arguments are
    // probed individually as before.
    let paths: Vec<PathBuf> = if args.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.iter().map(PathBuf::from).collect()
    };
    for path in paths {
        if path.is_dir() {
            let entries = match std::fs::read_dir(&path) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("ferail magic: {}: {}", path.display(), e);
                    continue;
                }
            };
            let mut files: Vec<PathBuf> = entries
                .flatten()
                .filter(|d| d.file_type().map(|t| t.is_file()).unwrap_or(false))
                .map(|d| d.path())
                .collect();
            files.sort();
            for f in files {
                print_magic_line(&f);
            }
        } else {
            print_magic_line(&path);
        }
    }
    Ok(0)
}

fn print_magic_line(path: &Path) {
    let label = detect_magic(path)
        .map(str::to_string)
        .unwrap_or_else(|| extension_fallback_label(path));
    println!("{}\t{}", path.display(), label);
}

fn run_disk_usage_cli(args: &[String]) -> Result<i32> {
    let mut top = 20usize;
    let mut descend_packages = false;
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--packages" => descend_packages = true,
            "--top" => {
                i += 1;
                let Some(value) = args.get(i) else {
                    eprintln!("ferail du: --top needs a number");
                    return Ok(2);
                };
                top = value.parse().unwrap_or(top).clamp(1, 200);
            }
            s if s.starts_with("--top=") => {
                top = s["--top=".len()..].parse().unwrap_or(top).clamp(1, 200);
            }
            "-h" | "--help" => {
                println!("usage: ferail du [--top N] [--packages] <path>");
                return Ok(0);
            }
            other => paths.push(PathBuf::from(other)),
        }
        i += 1;
    }
    if paths.is_empty() {
        eprintln!("usage: ferail du [--top N] [--packages] <path>");
        return Ok(2);
    }

    let fs = NativeFs::new();
    for (idx, path) in paths.iter().enumerate() {
        if idx > 0 {
            println!();
        }
        print_disk_usage(&fs, path, top, descend_packages)?;
    }
    Ok(0)
}

/// `ferail thumb <path> [--out <png>] [--size N] [--preview]`
///
/// Calls `video_poster::fetch_content` — the same fetch every in-app
/// thumbnail warm path uses (the platform shell first, then the mpv
/// poster fallback for videos) — and writes the result as a PNG. Useful
/// for testing the preview pipeline without launching the GUI and for
/// scripting (batch thumbnail extraction). `--preview` asks for the
/// preview pane's tier rather than the grid's; on Windows that is the
/// only way to exercise the brokered `IPreviewHandler` capture.
fn run_thumb_cli(args: &[String]) -> Result<i32> {
    use ferail_gpui::video_poster::Tier;

    let mut out: Option<PathBuf> = None;
    let mut size: u32 = 512;
    let mut tier = Tier::Thumbnail;
    let mut input: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                let Some(value) = args.get(i) else {
                    eprintln!("ferail thumb: --out needs a path");
                    return Ok(2);
                };
                out = Some(PathBuf::from(value));
            }
            s if s.starts_with("--out=") => {
                out = Some(PathBuf::from(&s["--out=".len()..]));
            }
            "--size" => {
                i += 1;
                let Some(value) = args.get(i) else {
                    eprintln!("ferail thumb: --size needs a number");
                    return Ok(2);
                };
                size = value.parse().unwrap_or(size).clamp(16, 4096);
            }
            s if s.starts_with("--size=") => {
                size = s["--size=".len()..].parse().unwrap_or(size).clamp(16, 4096);
            }
            "--preview" => {
                tier = Tier::Preview;
            }
            "-h" | "--help" => {
                println!("usage: ferail thumb <path> [--out <png>] [--size N] [--preview]");
                return Ok(0);
            }
            other => {
                if input.is_some() {
                    eprintln!("ferail thumb: extra positional argument {other:?}");
                    return Ok(2);
                }
                input = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }
    let Some(path) = input else {
        eprintln!("usage: ferail thumb <path> [--out <png>] [--size N] [--preview]");
        return Ok(2);
    };
    let out = out.unwrap_or_else(|| PathBuf::from("thumb.png"));

    match ferail_gpui::video_poster::fetch_content_blocking(&path, size, tier) {
        Some((rgba, w, h)) => {
            let buf = image::RgbaImage::from_raw(w, h, rgba)
                .ok_or_else(|| anyhow::anyhow!("thumbnail RGBA dimensions don't match"))?;
            buf.save(&out).context("write PNG")?;
            println!("{}\t{}x{}\t{}", path.display(), w, h, out.display());
            Ok(0)
        }
        None => {
            eprintln!(
                "ferail thumb: no thumbnail/preview available for {}",
                path.display()
            );
            Ok(1)
        }
    }
}

fn print_disk_usage(fs: &NativeFs, path: &Path, top: usize, descend_packages: bool) -> Result<()> {
    // CLI utility path — no UI thread to freeze.
    #[allow(clippy::disallowed_methods)]
    let root = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let root_id = fs.id_for_path(&root);
    let cancel = AtomicBool::new(false);
    let mut tree = DiskUsageTree::new(root_id);
    let mut latest_stats = ferail_disk_usage::DiskUsageStats::default();
    let err = fs.scan_disk_usage(
        &root,
        DEFAULT_DU_BATCH,
        &cancel,
        descend_packages,
        |facts| tree.apply_facts(&facts),
        |stats| latest_stats = stats,
    );
    if let Some(err) = err {
        anyhow::bail!("disk usage scan failed for {}: {:?}", root.display(), err);
    }

    let layout = build_layout_node(&tree, root_id, 3);
    println!("Disk usage: {}", root.display());
    println!(
        "Total: {}  Files: {}  Folders: {}",
        humanize_bytes(layout.size_bytes),
        latest_stats.files_scanned,
        latest_stats.dirs_scanned,
    );
    println!();
    println!("Largest children:");
    for child in layout.children.iter().take(top) {
        let name = tree
            .nodes
            .get(&child.node_id)
            .map(|n| n.display_name.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("(unnamed)");
        let pct = if layout.size_bytes == 0 {
            0.0
        } else {
            child.size_bytes as f64 * 100.0 / layout.size_bytes as f64
        };
        println!(
            "  {:>10}  {:>5.1}%  {}",
            humanize_bytes(child.size_bytes),
            pct,
            name
        );
    }

    let mut files: Vec<_> = tree
        .nodes
        .values()
        .filter(|n| n.kind == NodeKind::File && n.size_bytes > 0)
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.size_bytes));
    println!();
    println!("Largest files:");
    for file in files.into_iter().take(top) {
        let name = if file.display_name.is_empty() {
            "(unnamed)"
        } else {
            file.display_name.as_str()
        };
        println!("  {:>10}  {}", humanize_bytes(file.size_bytes), name);
    }
    Ok(())
}

fn extension_fallback_label(path: &Path) -> String {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => "Folder".to_string(),
        Ok(meta) if meta.file_type().is_symlink() => "Symlink".to_string(),
        _ => path
            .extension()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_uppercase())
            .unwrap_or_else(|| "File".to_string()),
    }
}

fn humanize_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn run_gui(args: screenshot::Args) {
    ferail_gpui::boot::run_gui(args)
}
