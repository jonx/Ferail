//! GUI boot: everything `main.rs` used to do after CLI parsing —
//! application construction, theme resolution, app-level actions,
//! process-state setup, menus, and the first window.
//!
//! Lives in the library (not the binary) so every entry point shares it:
//! the desktop `main()`, and the AROS port's staticlib wrapper
//! (`ferail-aros-app`), where a C harness owns `main()` and calls in.

use crate::{
    assets::FeraAssets,
    screenshot,
    settings::{SettingsView, category_from_arg},
    shell::{
        ClearRecents, CloseTab, CloseWindow, CopyPath, DeleteImmediately, EmptyTrash,
        FindDuplicates, FindSimilarImages, FocusFilter, GoHome, GoToFolder, MoveToTrash,
        NavigateBack, NavigateForward, NavigateParent, NewFolder, NewTab, OpenDiskUsage,
        OpenSelected, OpenSettings, Refresh, RenameSelected, RevealInFinder, Shell, ShowDesktop,
        ToggleFavoriteForTarget, ToggleHidden, TogglePreview,
    },
};
use ferail_core::commands::{CommandId, find};
use gpui::*;
use gpui_component::Theme;

// App-scoped actions. These fire regardless of whether a Shell window
// owns focus, so they're bound via `cx.on_action` at the App level
// rather than under the `SHELL_CONTEXT` keymap.
//
// - `Quit`            — Cmd+Q
// - `OpenAbout`       — About menu item
// - `NewWindow`       — Cmd+N. Opens a fresh window sharing the singleton
//                       `ProcessState`. See `open_new_window`.
// - `BringAllToFront` — Window ▸ Bring All to Front. Raises every open
//                       window (shells, viewers, tool windows) above
//                       other apps'. See `bring_all_to_front`.
actions!(
    app,
    [Quit, OpenAbout, CheckForUpdates, NewWindow, BringAllToFront]
);

pub fn run_gui(args: screenshot::Args) {
    // Windows shell: assign our own AppUserModelID so the taskbar
    // groups Ferail windows under their own icon/label instead of
    // inheriting the launching console's identity (PowerShell, etc.).
    // No-op on macOS. Must run before any window is created — the
    // shell caches the ID on first-window-show.
    crate::platform_shell::set_app_user_model_id("Knipper.Ferail");

    // Safe mode (--safe-mode / FERAIL_SAFE_MODE=1): freeze-bisection
    // launch with every optional background subsystem off. Resolved
    // before anything below consults it (docs/features/FREEZE_DIAGNOSTICS.md).
    crate::safe_mode::set(args.safe_mode || crate::safe_mode::from_env());
    if crate::safe_mode::enabled() {
        crate::log_info!(
            90,
            "safe mode: background subsystems disabled for this session"
        );
    }

    // Archive previews stage entries into a per-process scratch directory.
    // A clean exit removes it, but a crash or a kill runs no destructor, and
    // archive contents are not something to leave lying around — so sweep any
    // scratch directory whose owning process is gone. One temp-dir listing —
    // cheap on a local SSD, but a temp dir can live on slow media, so it runs
    // on its own thread, not the soon-to-be-UI thread (Prime Directive).
    // Skipped in safe mode like every other optional background task.
    if !crate::safe_mode::enabled() {
        crate::obs::spawn_logged("scratch-sweep", || {
            ferail_fs_native::scratch::sweep_stale_scratch();
        });
    }

    // FeraAssets stacks our local SVG bundle (file-type icons, etc.)
    // in front of the upstream gpui-component icon pack. Both surface
    // through one `icons/X.svg` namespace.
    let app = gpui_platform::application().with_assets(FeraAssets);
    let width = args.width.unwrap_or(1180) as f32;
    let height = args.height.unwrap_or(760) as f32;
    let theme_mode = args.theme;
    let settings_page = args.settings;
    let initial_archive = args.archive;

    app.run(move |cx| {
        // Arm the Prime Directive's debug-build tripwire: from here on,
        // known-blocking filesystem/shell entry points panic if they
        // ever run on this (the UI) thread. See
        // `ferail_core::path_guard::assert_off_ui_thread`.
        ferail_core::path_guard::mark_ui_thread();
        gpui_component::init(cx);
        crate::text::install_platform_font_families(cx);
        // gpui boots with a NullHttpClient (every request errors); give
        // `cx.http_client()` a real TLS-capable client so the update
        // check can reach GitHub. Nothing else in the app talks HTTP
        // today, and this client never sends a request on its own.
        match reqwest_client::ReqwestClient::user_agent(concat!(
            "Ferail/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/jonx/Ferail)"
        )) {
            Ok(client) => cx.set_http_client(std::sync::Arc::new(client)),
            Err(e) => crate::log_warn!(90, "http client init failed: {e}"),
        }
        // UI language: read the persisted choice and install the matching
        // catalog *before* any window renders or the menu bar is built, so
        // there is no flash of English (docs/features/LOCALIZATION.md).
        crate::i18n::init(cx);
        crate::shell::init(cx);
        // Replace the dock / About icon. Has to happen after gpui
        // has built its NSApplication — calling from `main()` panics
        // ("Ivar platform not found on class NSApplication").

        let icon_result = crate::platform_shell::set_app_icon_from_png_bytes(crate::app_icon::PNG);
        crate::log_info!(90, "set_app_icon: {:?}", icon_result);
        // Populate the About-panel dictionary so OpenAbout brings up a
        // dialog with our name + version instead of the AppKit bare
        // fallback. We're not calling install_app_menu (gpui drives the
        // menu via cx.set_menus), so this is the lightweight path.
        crate::platform_shell::set_about_options(
            "Ferail",
            &tr!("macOS file explorer"),
            env!("CARGO_PKG_VERSION"),
            "Copyright \u{00A9} 2026 John Knipper",
        );
        // Initial theme resolution order (highest wins):
        //   1. `--theme {light,dark}` CLI flag
        //   2. `FERAIL_THEME` env var (light / dark / system)
        //   3. Persisted `theme_pref` in app_state
        //   4. macOS Appearance via `system_is_dark()`
        let env_theme = std::env::var("FERAIL_THEME").ok().map(|s| s.to_lowercase());
        let persisted_theme = crate::app_state::load().theme_pref;
        let resolve_string = |s: &str| -> Option<gpui_component::ThemeMode> {
            match s {
                "light" => Some(gpui_component::ThemeMode::Light),
                "dark" => Some(gpui_component::ThemeMode::Dark),
                "system" | "auto" => Some(if crate::platform_shell::system_is_dark() {
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
                if crate::platform_shell::system_is_dark() {
                    gpui_component::ThemeMode::Dark
                } else {
                    gpui_component::ThemeMode::Light
                }
            });
        Theme::change(mode, None, cx);
        // Sync native chrome (titlebars, traffic lights, menus) to the
        // app theme so secondary windows don't show system-dark chrome
        // under a light theme (or vice versa).
        crate::platform_shell::set_app_appearance(matches!(mode, gpui_component::ThemeMode::Dark));

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
            crate::app_state::load().theme_pref.as_deref(),
            None | Some("system") | Some("auto") | Some("")
        );
        if follow_system {
            crate::platform_shell::start_system_theme_observer(Box::new(|is_dark| {
                crate::shell::set_system_theme_pending(is_dark);
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
            crate::settings::open_settings_window(cx);
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
            crate::about::open_about_dialog(cx);
        });
        // Software Update dialog + a fresh check (docs/features/UPDATES.md).
        // Manual by definition — works whether or not the automatic daily
        // check is enabled in Settings.
        cx.on_action(|_: &CheckForUpdates, cx| {
            crate::log_info!(90, "manual update check requested from app menu");
            crate::update_check::manual_check(cx);
        });

        // Build the singleton ProcessState before opening any window
        // and stash it as a GPUI Global. Every Shell::new (this window,
        // future Cmd+N windows, screenshot path) reads the same Rc
        // through `process_state::process_state(cx)`.
        let process = crate::shell::Shell::build_process_state(cx);
        crate::process_state::install(cx, process);

        // Freeze watchdog + hang reports: heartbeat on the UI thread,
        // watchdog thread, kill-signal interception. After the
        // ProcessState global (its per-beat snapshot reads the task
        // registry), before any window can freeze. Stays on in safe
        // mode — diagnosing freezes is what both exist for.
        crate::watchdog::start(cx);

        // Resolve the sidebar Locations for the persisted special-folder
        // mode (Windows/OneDrive) once, before any window paints. Render
        // reads this cached global — it must never stat (Prime Directive).
        crate::special_folders::seed(cx);

        // The three process-wide watchers below are all skipped in safe
        // mode — each one talks to the OS off-thread, and safe mode's
        // whole point is a session where no background subsystem runs.
        if !crate::safe_mode::enabled() {
            // Live volume mount/unmount watch: NSWorkspace notifications
            // [mac] feed a coalescing channel; the drain task re-lists
            // volumes off-thread and fans the change out to every window's
            // sidebar + the Favorites mount states.
            crate::process_state::start_volume_watch(cx);

            // Live sleep/wake watch: pause video + slideshow when the
            // machine or its displays sleep; re-list volumes and reload
            // directory tabs on wake (docs/features/POWER.md).
            crate::process_state::start_power_watch(cx);

            // App-footprint sampler behind the status bar's
            // "up · CPU · MEM · rps" segment. Not started on the
            // screenshot path (screenshot::run) — captures use the
            // deterministic `--simulate-stats` label instead
            // (docs/features/SYSTEM_STATS.md).
            crate::system_stats::start_sampler(cx);

            // Daily update check — a no-op unless the user opted in
            // (Settings ▸ About ▸ Updates; off by default). The loop
            // re-reads the setting each wake, so the toggle works
            // without a relaunch.
            crate::update_check::start_auto_loop(cx);
        }

        // Cmd+N → new window. The handler runs at App level so the
        // binding works regardless of which window holds focus, and
        // works with zero windows (after the last window closes the
        // process stays resident, Cmd+N reopens).
        cx.bind_keys([KeyBinding::new("secondary-n", NewWindow, None)]);
        cx.on_action(|_: &NewWindow, cx| {
            open_shell_window(cx);
        });

        // Cmd+G → Go to Folder with nothing open. A focused Shell
        // handles the action itself (its element listener wins the
        // bubble phase and stops propagation); this global fallback
        // only runs when the action reached nobody. It acts *only* at
        // zero windows — the state where the menu-bar item is the
        // user's single entry point — so the Go prompt still works
        // after the last window closed. It opens a window and shows
        // the prompt in it, navigating that window's own tab rather
        // than stacking a second one on a folder the user never asked
        // for.
        cx.on_action(|_: &GoToFolder, cx| {
            if !cx.windows().is_empty() {
                return;
            }
            open_shell_window_then(cx, |shell, window, cx| {
                shell.open_go_to_folder_prompt(false, window, cx);
            });
        });

        // Window ▸ Bring All to Front. App-level like NewWindow: the
        // menu item must fire no matter which window (or none) is key.
        cx.on_action(|_: &BringAllToFront, cx| {
            bring_all_to_front(cx);
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
                if let Err(e) = cx.open_window(opts, |window, cx| {
                    let cat = category_from_arg(if page.is_empty() {
                        None
                    } else {
                        Some(page.as_str())
                    });
                    let view = cx.new(|cx| SettingsView::new(cat, window, cx));
                    cx.new(|cx| gpui_component::Root::new(view, window, cx))
                }) {
                    crate::log_error!(90, "could not open settings window: {e}");
                }
            })
            .detach();
        } else {
            let initial_size = (width, height);
            cx.spawn(async move |cx| {
                cx.update(|cx| {
                    open_shell_window_sized(cx, Some(initial_size), initial_archive);
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
        ..crate::base_window_options()
    }
}

/// Spawn a new Shell window using the singleton `ProcessState`. The
/// handler bound to `Cmd+N` (and the initial-window boot path) both
/// route through this so there's one place that owns window options
/// and process-state hookup.
pub fn open_shell_window(cx: &mut App) {
    open_shell_window_sized(cx, None, None);
}

fn open_shell_window_sized(
    cx: &mut App,
    size_hint: Option<(f32, f32)>,
    initial_archive: Option<std::path::PathBuf>,
) {
    let (w, h) = size_hint.unwrap_or((1180.0, 760.0));
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(w), px(h)), cx)),
        // gpui-component's TitleBar replaces the macOS default title
        // text + adopts the traffic-light area so our custom title-
        // bar content (brand + filter + nav) sits flush across the top,
        // and claims the titlebar drag so that bar's own controls work.
        ..crate::shell_window_options()
    };
    cx.spawn(async move |cx| {
        // The main window failing to open is fatal to a useful session, but a
        // logged error beats an abort — the panic hook would have nothing more
        // to add than this line.
        if let Err(e) = cx.open_window(opts, |window, cx| {
            let process = crate::process_state::process_state(cx);
            let view = cx.new(|cx| Shell::new(process, window, cx));
            if let Some(archive) = initial_archive.clone() {
                view.update(cx, |shell, cx| {
                    shell.open_archive_path(archive, window, cx);
                });
            }
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        }) {
            crate::log_error!(90, "could not open main window: {e}");
        } else {
            install_native_drag_operations();
        }
    })
    .detach();
}

/// Widen gpui's outbound drag mask to Finder parity (move/copy/alias by
/// destination + modifier keys, with the system's “+” copy badge). Must
/// run after a gpui window exists — the window classes it patches are
/// registered lazily on first window construction. Idempotent, so every
/// shell-window open calls it. No-op off macOS: drag-out on other
/// platforms uses upstream's masks as-is.
fn install_native_drag_operations() {
    #[cfg(target_os = "macos")]
    if !crate::platform_shell::install_native_drag_operations() {
        crate::log_warn!(
            60,
            "native drag-operations override not installed (gpui window classes not found); \
             external drags fall back to copy-only"
        );
    }
}

/// Open a new Shell window and run `after` against its Shell once the
/// window (and its `Root`, which hosts dialogs and notifications)
/// exists. Backs the zero-window Cmd+G path: there is no Shell to
/// dispatch to, so we make one and hand it the prompt.
fn open_shell_window_then(
    cx: &mut App,
    after: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
) {
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1180.0), px(760.0)), cx)),
        ..crate::shell_window_options()
    };
    cx.spawn(async move |cx| {
        match cx.open_window(opts, |window, cx| {
            let process = crate::process_state::process_state(cx);
            let view = cx.new(|cx| Shell::new(process, window, cx));
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        }) {
            Ok(handle) => {
                install_native_drag_operations();
                let _ = handle.update(cx, |root, window, cx| {
                    let Ok(shell) = root.view().clone().downcast::<Shell>() else {
                        return;
                    };
                    shell.update(cx, |shell, cx| after(shell, window, cx));
                });
            }
            Err(e) => crate::log_error!(90, "could not open window: {e}"),
        }
    })
    .detach();
}

/// Build and install the macOS application menu bar. Titles for
/// every item are pulled from `ferail_core::commands` so the
/// menu, the keymap (Stage 3), and the future Keyboard-Shortcuts
/// dialog all read from the same source of truth. Items whose
/// action handler isn't implemented yet are omitted; they show up
/// as a startup-log warning via keymap::install instead.
///
/// Per-item dynamic disable (e.g. Move to Trash when nothing is
/// selected) is deferred to a polish iter — the action handler
/// no-ops silently in that case today.
pub(crate) fn install_app_menus(cx: &mut App) {
    // Show Desktop is gated on the private Dock symbol resolving on a
    // supported macOS. Resolving here also warms the cache before the
    // first render reads it (keeps the render-time check nonblocking).
    let show_desktop_available = crate::platform_shell::show_desktop_available();
    let mut view_items = vec![
        MenuItem::action(title("view.search", "Find"), FocusFilter),
        MenuItem::action(
            title("view.find_duplicates", "Find Duplicates"),
            FindDuplicates,
        ),
        MenuItem::action(
            title("view.find_similar_images", "Find Similar Images"),
            FindSimilarImages,
        ),
        MenuItem::action(title("view.disk_usage", "Disk Usage"), OpenDiskUsage),
    ];
    if show_desktop_available {
        view_items.push(MenuItem::separator());
        view_items.push(MenuItem::action(
            title("view.show_desktop", ferail_core::msgid!("Show Desktop")),
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
            name: "Ferail".into(),
            items: vec![
                MenuItem::action(title("app.about", "About Ferail"), OpenAbout),
                MenuItem::action(
                    title("app.check_updates", "Check for Updates\u{2026}"),
                    CheckForUpdates,
                ),
                MenuItem::separator(),
                MenuItem::action(title("app.settings", "Settings\u{2026}"), OpenSettings),
                MenuItem::separator(),
                MenuItem::action(tr!("Quit Ferail"), Quit),
            ],
            disabled: false,
        },
        Menu {
            name: tr!("File"),
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
                    title("file.reveal_in_finder", ferail_core::commands::REVEAL_LABEL),
                    RevealInFinder,
                ),
                MenuItem::separator(),
                // Favorites (docs/features/FAVORITES.md). The Cmd+D toggle
                // acts on the selected folder, else the current folder —
                // the menu-bar twin of the row context menu's "Add to
                // Favorites", so the command is discoverable without
                // knowing the shortcut. Wording matches the context menu;
                // on an already-favorited target it removes (toggle).
                MenuItem::action(tr!("Add to Favorites"), ToggleFavoriteForTarget),
                MenuItem::separator(),
                MenuItem::action(
                    title("file.move_to_trash", ferail_core::commands::TRASH_LABEL),
                    MoveToTrash,
                ),
                // Ellipsis: each opens a confirmation dialog (macOS HIG).
                // Delete Immediately is Finder's "Delete Immediately…" — a
                // targeted permanent delete with no undo.
                MenuItem::action(tr!("Delete Immediately\u{2026}"), DeleteImmediately),
                MenuItem::action(tr!("Empty Trash\u{2026}"), EmptyTrash),
            ],
            disabled: false,
        },
        Menu {
            name: tr!("Edit"),
            items: vec![MenuItem::action(
                title("file.copy_path", "Copy Path"),
                CopyPath,
            )],
            disabled: false,
        },
        Menu {
            name: tr!("Go"),
            items: vec![
                MenuItem::action(title("go.back", "Back"), NavigateBack),
                MenuItem::action(title("go.forward", "Forward"), NavigateForward),
                MenuItem::action(title("go.parent", "Enclosing Folder"), NavigateParent),
                MenuItem::separator(),
                MenuItem::action(title("go.home", "Home"), GoHome),
                MenuItem::separator(),
                // Ellipsis: opens the path prompt (macOS HIG).
                MenuItem::action(title("go.go_to_folder", "Go to Folder\u{2026}"), GoToFolder),
                MenuItem::separator(),
                // Ellipsis: opens a confirmation dialog (macOS HIG).
                MenuItem::action(
                    title("go.clear_recents", "Clear Recents\u{2026}"),
                    ClearRecents,
                ),
            ],
            disabled: false,
        },
        Menu {
            name: tr!("View"),
            items: view_items,
            disabled: false,
        },
        Menu {
            // gpui registers a top-level menu literally named "Window"
            // as NSApp's windows menu, so on macOS AppKit automatically
            // appends the live list of open windows below these items —
            // every titled window (shells, viewers, Settings, Disk
            // Usage, …), kept fresh as they open/close/retitle, with
            // the checkmark on the key window; selecting one brings it
            // to front. The trailing separator divides our items from
            // that appended list. Windows/Linux's AppMenuBar renders
            // only the explicit items; the window list is a macOS
            // freebie until a cross-platform list is built by hand.
            name: tr!("Window"),
            items: vec![
                MenuItem::action(
                    title("window.bring_all_to_front", "Bring All to Front"),
                    BringAllToFront,
                ),
                MenuItem::separator(),
            ],
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

/// Raise every open window above other apps' windows — Finder's
/// Window ▸ Bring All to Front. On macOS this is the native
/// `arrangeInFront:`, which preserves the windows' relative z-order
/// and leaves the key window unchanged. Elsewhere it falls back to
/// activating each window through gpui: `activate_window` queues its
/// platform raise on the foreground executor in call order, so the
/// windows come up in `cx.windows()` order with the last one key.
fn bring_all_to_front(cx: &mut App) {
    if crate::platform_shell::bring_all_windows_to_front() {
        return;
    }
    for handle in cx.windows() {
        let _ = handle.update(cx, |_, window, _| window.activate_window());
    }
}

/// Look up a command's title in `ferail_core::commands`, falling
/// back to the provided default when the lookup fails. Keeping a
/// default means a typo in the CommandId string surfaces as the
/// wrong title rather than panicking — the catalogue still drives
/// every wired item.
fn title(id: &'static str, fallback: &'static str) -> SharedString {
    find(CommandId(id))
        .map(|spec| crate::i18n::tr_static(spec.title))
        .unwrap_or_else(|| crate::i18n::tr_static(fallback))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    /// The drag-operations override patches gpui's `GPUIWindow` /
    /// `GPUIPanel` classes by name (registered in a `#[ctor]` at load, so
    /// they exist here too). If upstream renames them this fails — the
    /// override would otherwise silently degrade to copy-only external
    /// drags. Second call checks idempotence.
    #[test]
    fn native_drag_operations_override_installs() {
        assert!(crate::platform_shell::install_native_drag_operations());
        assert!(crate::platform_shell::install_native_drag_operations());
    }
}
