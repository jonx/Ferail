// Feraille — GPUI shell entry point.
//
// Dispatches between the live GUI and the headless `--screenshot`
// capture path. All real view code lives in `crate::shell`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use feraille_core::commands::{CommandId, find};
use feraille_disk_usage::{DiskUsageTree, NodeKind, build_layout_node};
use feraille_fs_native::{DEFAULT_DU_BATCH, NativeFs, detect_magic};
use feraille_gpui::{
    assets::FeraAssets,
    screenshot,
    settings::{SettingsView, category_from_arg},
    shell::{
        CloseTab, CloseWindow, CopyPath, EmptyTrash, FindDuplicates, FocusFilter, GoHome,
        MoveToTrash, NavigateBack, NavigateForward, NavigateParent, NewFolder, NewTab,
        OpenDiskUsage, OpenSelected, OpenSettings, Refresh, RenameSelected, RevealInFinder, Shell,
        ShowDesktop, ToggleHidden, TogglePreview,
    },
};
use gpui::*;
use gpui_component::Theme;

// App-scoped actions. These fire regardless of whether a Shell window
// owns focus, so they're bound via `cx.on_action` at the App level
// rather than under the `SHELL_CONTEXT` keymap.
//
// - `Quit`        — Cmd+Q
// - `OpenAbout`   — About menu item
// - `NewWindow`   — Cmd+N. Opens a fresh window sharing the singleton
//                   `ProcessState`. See `open_new_window`.
actions!(app, [Quit, OpenAbout, NewWindow]);

fn main() -> Result<()> {
    // Pre-event-loop CLI handlers — run before the window opens.
    if let Some(code) = feraille_gpui::reset_db::handle_reset_db_cli() {
        std::process::exit(code);
    }
    if let Some(code) = handle_cli_subcommand()? {
        std::process::exit(code);
    }
    feraille_gpui::obs::init();
    let args = screenshot::parse_args();
    if args.screenshot.is_some() {
        feraille_gpui::log_info!(90, "headless screenshot path");
        return screenshot::run(args);
    }
    feraille_gpui::log_info!(90, "event loop starting");
    run_gui(args);
    feraille_gpui::log_info!(90, "event loop exited");
    Ok(())
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
            eprintln!("feraille: unknown subcommand: {other:?}\n");
            print_cli_help();
            Ok(Some(2))
        }
        _ => Ok(None),
    }
}

fn print_cli_help() {
    println!(
        "Feraille\n\nUsage:\n  feraille                 Open the GPUI file manager\n  feraille magic [path]...  Print magic-byte format (defaults to current directory; directories are listed shallow)\n  feraille du [options] <path>  Print disk-usage summary\n  feraille thumb <path> [--out <png>] [--size N]  Extract a file's thumbnail/preview to a PNG\n\nDisk usage options:\n  --top <n>        Number of entries to show (default: 20)\n  --packages       Descend into macOS package directories\n\nThumb options:\n  --out <path>     Output PNG path (default: thumb.png)\n  --size <px>      Max edge in pixels (default: 512)"
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
                    eprintln!("feraille magic: {}: {}", path.display(), e);
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
                    eprintln!("feraille du: --top needs a number");
                    return Ok(2);
                };
                top = value.parse().unwrap_or(top).clamp(1, 200);
            }
            s if s.starts_with("--top=") => {
                top = s["--top=".len()..].parse().unwrap_or(top).clamp(1, 200);
            }
            "-h" | "--help" => {
                println!("usage: feraille du [--top N] [--packages] <path>");
                return Ok(0);
            }
            other => paths.push(PathBuf::from(other)),
        }
        i += 1;
    }
    if paths.is_empty() {
        eprintln!("usage: feraille du [--top N] [--packages] <path>");
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

/// `feraille thumb <path> [--out <png>] [--size N]`
///
/// Calls `platform_shell::fetch_quick_look_thumbnail` (which runs the
/// full thumbnail / preview-handler pipeline on Windows; macOS
/// `qlmanage -t` shell-out on Mac) and writes the result as a PNG.
/// Useful for testing the preview pipeline without launching the GUI
/// and for scripting (batch thumbnail extraction).
fn run_thumb_cli(args: &[String]) -> Result<i32> {
    let mut out: Option<PathBuf> = None;
    let mut size: u32 = 512;
    let mut input: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                let Some(value) = args.get(i) else {
                    eprintln!("feraille thumb: --out needs a path");
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
                    eprintln!("feraille thumb: --size needs a number");
                    return Ok(2);
                };
                size = value.parse().unwrap_or(size).clamp(16, 4096);
            }
            s if s.starts_with("--size=") => {
                size = s["--size=".len()..].parse().unwrap_or(size).clamp(16, 4096);
            }
            "-h" | "--help" => {
                println!("usage: feraille thumb <path> [--out <png>] [--size N]");
                return Ok(0);
            }
            other => {
                if input.is_some() {
                    eprintln!("feraille thumb: extra positional argument {other:?}");
                    return Ok(2);
                }
                input = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }
    let Some(path) = input else {
        eprintln!("usage: feraille thumb <path> [--out <png>] [--size N]");
        return Ok(2);
    };
    let out = out.unwrap_or_else(|| PathBuf::from("thumb.png"));

    match feraille_gpui::platform_shell::fetch_quick_look_thumbnail(&path, size) {
        Some((rgba, w, h)) => {
            let buf = image::RgbaImage::from_raw(w, h, rgba)
                .ok_or_else(|| anyhow::anyhow!("thumbnail RGBA dimensions don't match"))?;
            buf.save(&out).context("write PNG")?;
            println!("{}\t{}x{}\t{}", path.display(), w, h, out.display());
            Ok(0)
        }
        None => {
            eprintln!(
                "feraille thumb: no thumbnail/preview available for {}",
                path.display()
            );
            Ok(1)
        }
    }
}

fn print_disk_usage(fs: &NativeFs, path: &Path, top: usize, descend_packages: bool) -> Result<()> {
    let root = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let root_id = fs.id_for_path(&root);
    let cancel = AtomicBool::new(false);
    let mut tree = DiskUsageTree::new(root_id);
    let mut latest_stats = feraille_disk_usage::DiskUsageStats::default();
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
    // Windows shell: assign our own AppUserModelID so the taskbar
    // groups Feraille windows under their own icon/label instead of
    // inheriting the launching console's identity (PowerShell, etc.).
    // No-op on macOS. Must run before any window is created — the
    // shell caches the ID on first-window-show.
    feraille_gpui::platform_shell::set_app_user_model_id("Knipper.Feraille");

    // FeraAssets stacks our local SVG bundle (file-type icons, etc.)
    // in front of the upstream gpui-component icon pack. Both surface
    // through one `icons/X.svg` namespace.
    let app = gpui_platform::application().with_assets(FeraAssets);
    let width = args.width.unwrap_or(1180) as f32;
    let height = args.height.unwrap_or(760) as f32;
    let theme_mode = args.theme;
    let settings_page = args.settings;

    app.run(move |cx| {
        gpui_component::init(cx);
        feraille_gpui::shell::init(cx);
        // Replace the dock / About icon. Has to happen after gpui
        // has built its NSApplication — calling from `main()` panics
        // ("Ivar platform not found on class NSApplication").
        let icon_result = feraille_gpui::platform_shell::set_app_icon_from_png_bytes(
            feraille_gpui::app_icon::PNG,
        );
        feraille_gpui::log_info!(90, "set_app_icon: {:?}", icon_result);
        // Populate the About-panel dictionary so OpenAbout brings up a
        // dialog with our name + version instead of the AppKit bare
        // fallback. We're not calling install_app_menu (gpui drives the
        // menu via cx.set_menus), so this is the lightweight path.
        feraille_gpui::platform_shell::set_about_options(
            "Feraille",
            "macOS file explorer",
            env!("CARGO_PKG_VERSION"),
            "Copyright \u{00A9} 2026 John Knipper",
        );
        // Initial theme resolution order (highest wins):
        //   1. `--theme {light,dark}` CLI flag
        //   2. `FERAILLE_THEME` env var (light / dark / system)
        //   3. Persisted `theme_pref` in app_state
        //   4. macOS Appearance via `system_is_dark()`
        let env_theme = std::env::var("FERAILLE_THEME")
            .ok()
            .map(|s| s.to_lowercase());
        let persisted_theme = feraille_gpui::app_state::load().theme_pref;
        let resolve_string = |s: &str| -> Option<gpui_component::ThemeMode> {
            match s {
                "light" => Some(gpui_component::ThemeMode::Light),
                "dark" => Some(gpui_component::ThemeMode::Dark),
                "system" | "auto" => Some(if feraille_gpui::platform_shell::system_is_dark() {
                    gpui_component::ThemeMode::Dark
                } else {
                    gpui_component::ThemeMode::Light
                }),
                _ => None,
            }
        };
        let mode = theme_mode
            .or_else(|| env_theme.as_deref().and_then(resolve_string))
            .or_else(|| persisted_theme.as_deref().and_then(resolve_string))
            .unwrap_or_else(|| {
                if feraille_gpui::platform_shell::system_is_dark() {
                    gpui_component::ThemeMode::Dark
                } else {
                    gpui_component::ThemeMode::Light
                }
            });
        Theme::change(mode, None, cx);
        // Sync native chrome (titlebars, traffic lights, menus) to the
        // app theme so secondary windows don't show system-dark chrome
        // under a light theme (or vice versa).
        feraille_gpui::platform_shell::set_app_appearance(matches!(
            mode,
            gpui_component::ThemeMode::Dark
        ));

        // Phase 10 polish: System-Appearance follow via a global
        // AtomicBool the Shell polls on each render. The native
        // observer fires on the main thread but doesn't get an
        // `&mut App`, so direct Theme::change isn't reachable —
        // instead we publish to a shared cell and let the Shell
        // pick it up on its next paint (Render is already invoked
        // for any frame, so the lag is single-digit milliseconds).
        // Skipped when the user explicitly chose Light or Dark so
        // we don't override their pick on every system tick.
        let follow_system = matches!(
            feraille_gpui::app_state::load().theme_pref.as_deref(),
            None | Some("system") | Some("auto") | Some("")
        );
        if follow_system {
            feraille_gpui::platform_shell::start_system_theme_observer(Box::new(|is_dark| {
                feraille_gpui::shell::set_system_theme_pending(is_dark);
            }));
        }

        // Quit action so Cmd+Q routes through gpui's normal app
        // shutdown rather than relying on the platform's default
        // (which still works, but having it as an Action means
        // the menu item below can advertise the shortcut hint).
        cx.bind_keys([KeyBinding::new("secondary-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        // Phase C: process stays resident at zero windows (Finder /
        // Safari model). Quit only via Cmd+Q or the app menu. A future
        // preference may toggle this back to "quit on last window."
        // (Phase I will also wire dock-icon-with-zero-windows to
        // reopen a window via NSApplicationDelegate.)
        // App-level OpenSettings handler so the menu-bar item is
        // always enabled, not just when a Shell window has focus.
        // The Shell-context listener for OpenSettings still wins
        // when present (lower precedence first); this is the
        // fallback that fires from the app menu / About dialog.
        cx.on_action(|_: &OpenSettings, cx| {
            feraille_gpui::settings::open_settings_window(cx);
        });
        // OpenAbout opens our custom About dialog as a modal overlay
        // on the active window via gpui-component's Dialog primitive
        // (ESC, click-outside, close button all built in). Replaces
        // both the Mac NSAboutPanel and the Windows MessageBoxW. The
        // latter had a quirk where the menu stayed visually pinned
        // because the system-modal MessageBox took focus before the
        // menu finished dismissing — an overlay dialog inside the
        // existing window has none of that interaction with the
        // menu.
        cx.on_action(|_: &OpenAbout, cx| {
            feraille_gpui::about::open_about_dialog(cx);
        });

        // Build the singleton ProcessState before opening any window
        // and stash it as a GPUI Global. Every Shell::new (this window,
        // future Cmd+N windows, screenshot path) reads the same Rc
        // through `process_state::process_state(cx)`.
        let process = feraille_gpui::shell::Shell::build_process_state(cx);
        cx.set_global(feraille_gpui::process_state::ProcessStateGlobal(process));

        // Resolve the sidebar Locations for the persisted special-folder
        // mode (Windows/OneDrive) once, before any window paints. Render
        // reads this cached global — it must never stat (Prime Directive).
        feraille_gpui::special_folders::seed(cx);

        // Live volume mount/unmount watch: NSWorkspace notifications
        // [mac] feed a coalescing channel; the drain task re-lists
        // volumes off-thread and fans the change out to every window's
        // sidebar + the Favorites mount states.
        feraille_gpui::process_state::start_volume_watch(cx);

        // Live sleep/wake watch: pause video + slideshow when the
        // machine or its displays sleep; re-list volumes and reload
        // directory tabs on wake (docs/features/POWER.md).
        feraille_gpui::process_state::start_power_watch(cx);

        // Cmd+N → new window. The handler runs at App level so the
        // binding works regardless of which window holds focus, and
        // works with zero windows (after the last window closes the
        // process stays resident, Cmd+N reopens).
        cx.bind_keys([KeyBinding::new("secondary-n", NewWindow, None)]);
        cx.on_action(|_: &NewWindow, cx| {
            open_shell_window(cx);
        });

        install_app_menus(cx);

        // Windows/Linux: quit the process when the last window closes.
        // macOS keeps the process resident (Finder/Safari model) and
        // relies on Cmd+Q or the app menu to exit; this matches the
        // platform's convention. The subscription leaks intentionally
        // (lives the whole app run).
        #[cfg(not(target_os = "macos"))]
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        // [NSApp activateIgnoringOtherApps:YES] — without this the
        // terminal that invoked us keeps key-window status and our
        // window opens unfocused behind it.
        cx.activate(true);

        // First window. The size hints from `--width` / `--height`
        // apply only to this initial window; future Cmd+N windows
        // use defaults from `open_shell_window`.
        if let Some(page) = settings_page.clone() {
            // Direct-into-settings boot path (CLI). Skips the shell.
            let opts = settings_window_opts(width, height);
            cx.spawn(async move |cx| {
                cx.open_window(opts, |window, cx| {
                    let cat = category_from_arg(if page.is_empty() {
                        None
                    } else {
                        Some(page.as_str())
                    });
                    let view = cx.new(|cx| SettingsView::new(cat, window, cx));
                    cx.new(|cx| gpui_component::Root::new(view, window, cx))
                })
                .expect("failed to open feraille settings window");
            })
            .detach();
        } else {
            let initial_size = (width, height);
            cx.spawn(async move |cx| {
                cx.update(|cx| {
                    open_shell_window_sized(cx, Some(initial_size));
                });
            })
            .detach();
        }
    });
}

/// Bounds for the initial settings-only boot path. Uses windowed
/// geometry at the requested size — `WindowBounds::centered` needs
/// an `&mut App` (display metrics) which we don't have inside the
/// async spawn, and the settings boot path is rare enough that
/// top-left positioning is fine.
fn settings_window_opts(width: f32, height: f32) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: Default::default(),
            size: size(px(width), px(height)),
        })),
        titlebar: Some(gpui_component::TitleBar::title_bar_options()),
        ..Default::default()
    }
}

/// Spawn a new Shell window using the singleton `ProcessState`. The
/// handler bound to `Cmd+N` (and the initial-window boot path) both
/// route through this so there's one place that owns window options
/// and process-state hookup.
fn open_shell_window(cx: &mut App) {
    open_shell_window_sized(cx, None);
}

fn open_shell_window_sized(cx: &mut App, size_hint: Option<(f32, f32)>) {
    let (w, h) = size_hint.unwrap_or((1180.0, 760.0));
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(w), px(h)), cx)),
        // gpui-component's TitleBar replaces the macOS default title
        // text + adopts the traffic-light area so our custom title-
        // bar content (brand + filter + nav) sits flush across the top.
        titlebar: Some(gpui_component::TitleBar::title_bar_options()),
        ..Default::default()
    };
    cx.spawn(async move |cx| {
        cx.open_window(opts, |window, cx| {
            let process = feraille_gpui::process_state::process_state(cx);
            let view = cx.new(|cx| Shell::new(process, window, cx));
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        })
        .expect("failed to open feraille-gpui window");
    })
    .detach();
}

/// Build and install the macOS application menu bar. Titles for
/// every item are pulled from `feraille_core::commands` so the
/// menu, the keymap (Stage 3), and the future Keyboard-Shortcuts
/// dialog all read from the same source of truth. Items whose
/// action handler isn't implemented yet are omitted; they show up
/// as a startup-log warning via keymap::install instead.
///
/// Per-item dynamic disable (e.g. Move to Trash when nothing is
/// selected) is deferred to a polish iter — the action handler
/// no-ops silently in that case today.
fn install_app_menus(cx: &mut App) {
    // Show Desktop is gated on the private Dock symbol resolving on a
    // supported macOS. Resolving here also warms the cache before the
    // first render reads it (keeps the render-time check nonblocking).
    let show_desktop_available = feraille_gpui::platform_shell::show_desktop_available();
    let mut view_items = vec![
        MenuItem::action(title("view.search", "Find"), FocusFilter),
        MenuItem::action(
            title("view.find_duplicates", "Find Duplicates"),
            FindDuplicates,
        ),
        MenuItem::action(title("view.disk_usage", "Disk Usage"), OpenDiskUsage),
    ];
    if show_desktop_available {
        view_items.push(MenuItem::separator());
        view_items.push(MenuItem::action(
            title("view.show_desktop", "Show Desktop"),
            ShowDesktop,
        ));
    }
    view_items.push(MenuItem::separator());
    view_items.push(MenuItem::action(
        title("view.toggle_preview", "Show Preview Pane"),
        TogglePreview,
    ));
    view_items.push(MenuItem::action(
        title("view.toggle_hidden", "Show Hidden Files"),
        ToggleHidden,
    ));
    view_items.push(MenuItem::separator());
    view_items.push(MenuItem::action(title("file.refresh", "Refresh"), Refresh));

    cx.set_menus([
        Menu {
            name: "Feraille".into(),
            items: vec![
                MenuItem::action(title("app.about", "About Feraille"), OpenAbout),
                MenuItem::separator(),
                MenuItem::action(title("app.settings", "Settings\u{2026}"), OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Quit Feraille", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action(title("window.new_window", "New Window"), NewWindow),
                MenuItem::action(title("file.new_tab", "New Tab"), NewTab),
                MenuItem::action(title("file.close_tab", "Close Tab"), CloseTab),
                MenuItem::action(title("window.close_window", "Close Window"), CloseWindow),
                MenuItem::separator(),
                MenuItem::action(title("file.new_folder", "New Folder"), NewFolder),
                MenuItem::action(title("selection.rename", "Rename"), RenameSelected),
                MenuItem::separator(),
                MenuItem::action(title("selection.activate", "Open"), OpenSelected),
                MenuItem::action(
                    title(
                        "file.reveal_in_finder",
                        feraille_core::commands::REVEAL_LABEL,
                    ),
                    RevealInFinder,
                ),
                MenuItem::separator(),
                MenuItem::action(
                    title("file.move_to_trash", feraille_core::commands::TRASH_LABEL),
                    MoveToTrash,
                ),
                // Ellipsis: opens a confirmation dialog (macOS HIG), matching
                // the pane context menu's wording.
                MenuItem::action("Empty Trash\u{2026}", EmptyTrash),
            ],
            disabled: false,
        },
        Menu {
            name: "Edit".into(),
            items: vec![MenuItem::action(
                title("file.copy_path", "Copy Path"),
                CopyPath,
            )],
            disabled: false,
        },
        Menu {
            name: "Go".into(),
            items: vec![
                MenuItem::action(title("go.back", "Back"), NavigateBack),
                MenuItem::action(title("go.forward", "Forward"), NavigateForward),
                MenuItem::action(title("go.parent", "Enclosing Folder"), NavigateParent),
                MenuItem::separator(),
                MenuItem::action(title("go.home", "Home"), GoHome),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: view_items,
            disabled: false,
        },
    ]);

    // Mirror the menu list into gpui-component's GlobalState. macOS
    // ignores this (uses its NSApp menu); Windows/Linux's AppMenuBar
    // reads from here. One source of truth for the menu spec.
    if let Some(menus) = cx.get_menus() {
        gpui_component::global_state::GlobalState::global_mut(cx).set_app_menus(menus);
    }
}

/// Look up a command's title in `feraille_core::commands`, falling
/// back to the provided default when the lookup fails. Keeping a
/// default means a typo in the CommandId string surfaces as the
/// wrong title rather than panicking — the catalogue still drives
/// every wired item.
fn title(id: &'static str, fallback: &'static str) -> SharedString {
    find(CommandId(id))
        .map(|spec| SharedString::from(spec.title))
        .unwrap_or_else(|| SharedString::from(fallback))
}
