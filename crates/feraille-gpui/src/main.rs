// Feraille — GPUI shell entry point.
//
// Dispatches between the live GUI and the headless `--screenshot`
// capture path. All real view code lives in `crate::shell`.

use anyhow::Result;
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
        CloseTab, CopyPath, FocusFilter, GoHome, MoveToTrash, NavigateBack, NavigateForward,
        NavigateParent, NewFolder, NewTab, OpenSelected, OpenSettings, Refresh, RenameSelected,
        RevealInFinder, Shell, ToggleHidden,
    },
};
use gpui::*;
use gpui_component::Theme;

actions!(app, [Quit, OpenAbout]);

/// macOS Dock icon — set early via `NSApplication.setApplicationIconImage:`
/// (the same call the old app makes). Bytes embedded so the binary is
/// self-contained.
const APP_ICON_PNG: &[u8] = include_bytes!("../resources/feraille.png");

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
        "help" | "-h" | "--help" => {
            print_cli_help();
            Ok(Some(0))
        }
        _ => Ok(None),
    }
}

fn print_cli_help() {
    println!(
        "Feraille\n\nUsage:\n  feraille                 Open the GPUI file manager\n  feraille magic <path>...  Print magic-byte format, with extension fallback\n  feraille du [options] <path>  Print disk-usage summary\n\nDisk usage options:\n  --top <n>        Number of entries to show (default: 20)\n  --packages       Descend into macOS package directories"
    );
}

fn run_magic_cli(args: &[String]) -> Result<i32> {
    if args.is_empty() {
        eprintln!("usage: feraille magic <path>...");
        return Ok(2);
    }
    for raw in args {
        let path = PathBuf::from(raw);
        let label = detect_magic(&path)
            .map(str::to_string)
            .unwrap_or_else(|| extension_fallback_label(&path));
        println!("{}\t{}", path.display(), label);
    }
    Ok(0)
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
    files.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
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
        // ("Ivar platform not found on class NSApplication"). The
        // old soft-renderer app did the same thing from winit's
        // `resumed()` for the same reason.
        let icon_result = feraille_shell_mac::set_app_icon_from_png_bytes(APP_ICON_PNG);
        feraille_gpui::log_info!(90, "set_app_icon: {:?}", icon_result);
        // Populate the About-panel dictionary so OpenAbout brings up a
        // dialog with our name + version instead of the AppKit bare
        // fallback. We're not calling install_app_menu (gpui drives the
        // menu via cx.set_menus), so this is the lightweight path.
        feraille_shell_mac::set_about_options(
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
                "system" | "auto" => Some(if feraille_shell_mac::system_is_dark() {
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
                if feraille_shell_mac::system_is_dark() {
                    gpui_component::ThemeMode::Dark
                } else {
                    gpui_component::ThemeMode::Light
                }
            });
        Theme::change(mode, None, cx);

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
            feraille_shell_mac::start_system_theme_observer(Box::new(|is_dark| {
                feraille_gpui::shell::set_system_theme_pending(is_dark);
            }));
        }

        // Quit action so Cmd+Q routes through gpui's normal app
        // shutdown rather than relying on the platform's default
        // (which still works, but having it as an Action means
        // the menu item below can advertise the shortcut hint).
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        // GPUI keeps the app loop running after the last window
        // closes; on macOS that leaves a zombie process with no UI.
        // on_window_closed fires *after* removal, so an empty
        // cx.windows() means this was the final window.
        cx.on_window_closed(|cx, _id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        // App-level OpenSettings handler so the menu-bar item is
        // always enabled, not just when a Shell window has focus.
        // The Shell-context listener for OpenSettings still wins
        // when present (lower precedence first); this is the
        // fallback that fires from the app menu / About dialog.
        cx.on_action(|_: &OpenSettings, cx| {
            feraille_gpui::settings::open_settings_window(cx);
        });
        // OpenAbout brings up the standard macOS About panel using the
        // dictionary populated above. Stays an App-level fallback so
        // the menu item is always live.
        cx.on_action(|_: &OpenAbout, _cx| {
            feraille_shell_mac::show_about_panel();
        });

        install_app_menus(cx);

        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(width), px(height)), cx)),
            // Phase 7: gpui-component's TitleBar replaces the macOS
            // default title text + adopts the traffic-light area so
            // our custom title-bar content (name + filter + nav)
            // sits flush across the top of the window.
            titlebar: Some(gpui_component::TitleBar::title_bar_options()),
            ..Default::default()
        };

        // [NSApp activateIgnoringOtherApps:YES] — without this the
        // terminal that invoked us keeps key-window status and our
        // window opens unfocused behind it.
        cx.activate(true);

        let settings_page = settings_page.clone();
        cx.spawn(async move |cx| {
            cx.open_window(opts, |window, cx| {
                if let Some(page) = settings_page.as_deref() {
                    let cat = category_from_arg(if page.is_empty() { None } else { Some(page) });
                    let view = cx.new(|_| SettingsView::new(cat));
                    cx.new(|cx| gpui_component::Root::new(view, window, cx))
                } else {
                    let view = cx.new(|cx| Shell::new(window, cx));
                    cx.new(|cx| gpui_component::Root::new(view, window, cx))
                }
            })
            .expect("failed to open feraille-gpui window");
        })
        .detach();
    });
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
                MenuItem::action(title("file.new_tab", "New Tab"), NewTab),
                MenuItem::action(title("file.close_tab", "Close Tab"), CloseTab),
                MenuItem::separator(),
                MenuItem::action(title("file.new_folder", "New Folder"), NewFolder),
                MenuItem::action(title("selection.rename", "Rename"), RenameSelected),
                MenuItem::separator(),
                MenuItem::action(title("selection.activate", "Open"), OpenSelected),
                MenuItem::action(
                    title("file.reveal_in_finder", "Reveal in Finder"),
                    RevealInFinder,
                ),
                MenuItem::separator(),
                MenuItem::action(title("file.move_to_trash", "Move to Trash"), MoveToTrash),
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
            items: vec![
                MenuItem::action(title("view.search", "Find"), FocusFilter),
                MenuItem::separator(),
                MenuItem::action(
                    title("view.toggle_hidden", "Show Hidden Files"),
                    ToggleHidden,
                ),
                MenuItem::separator(),
                MenuItem::action(title("file.refresh", "Refresh"), Refresh),
            ],
            disabled: false,
        },
    ]);
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
