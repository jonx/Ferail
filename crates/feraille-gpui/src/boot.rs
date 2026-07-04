//! GUI boot: everything `main.rs` used to do after CLI parsing —
//! application construction, theme resolution, app-level actions,
//! process-state setup, menus, and the first window.
//!
//! Lives in the library (not the binary) so every entry point shares it:
//! the desktop `main()`, and the AROS port's staticlib wrapper
//! (`feraille-aros-app`), where a C harness owns `main()` and calls in.

use crate::{
    assets::FeraAssets,
    screenshot,
    settings::{SettingsView, category_from_arg},
    shell::{
        ClearRecents, CloseTab, CloseWindow, CopyPath, DeleteImmediately, EmptyTrash,
        FindDuplicates, FocusFilter, GoHome, MoveToTrash, NavigateBack, NavigateForward,
        NavigateParent, NewFolder, NewTab, OpenDiskUsage, OpenSelected, OpenSettings, Refresh,
        RenameSelected, RevealInFinder, Shell, ShowDesktop, ToggleHidden, TogglePreview,
    },
};
use feraille_core::commands::{CommandId, find};
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


pub fn run_gui(args: screenshot::Args) {
    // Windows shell: assign our own AppUserModelID so the taskbar
    // groups Feraille windows under their own icon/label instead of
    // inheriting the launching console's identity (PowerShell, etc.).
    // No-op on macOS. Must run before any window is created — the
    // shell caches the ID on first-window-show.
    crate::platform_shell::set_app_user_model_id("Knipper.Feraille");

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
        crate::shell::init(cx);
        // Replace the dock / About icon. Has to happen after gpui
        // has built its NSApplication — calling from `main()` panics
        // ("Ivar platform not found on class NSApplication").
        let icon_result = crate::platform_shell::set_app_icon_from_png_bytes(
            crate::app_icon::PNG,
        );
        crate::log_info!(90, "set_app_icon: {:?}", icon_result);
        // Populate the About-panel dictionary so OpenAbout brings up a
        // dialog with our name + version instead of the AppKit bare
        // fallback. We're not calling install_app_menu (gpui drives the
        // menu via cx.set_menus), so this is the lightweight path.
        crate::platform_shell::set_about_options(
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
        crate::platform_shell::set_app_appearance(matches!(
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

        // Build the singleton ProcessState before opening any window
        // and stash it as a GPUI Global. Every Shell::new (this window,
        // future Cmd+N windows, screenshot path) reads the same Rc
        // through `process_state::process_state(cx)`.
        let process = crate::shell::Shell::build_process_state(cx);
        cx.set_global(crate::process_state::ProcessStateGlobal(process));

        // Resolve the sidebar Locations for the persisted special-folder
        // mode (Windows/OneDrive) once, before any window paints. Render
        // reads this cached global — it must never stat (Prime Directive).
        crate::special_folders::seed(cx);

        // Live volume mount/unmount watch: NSWorkspace notifications
        // [mac] feed a coalescing channel; the drain task re-lists
        // volumes off-thread and fans the change out to every window's
        // sidebar + the Favorites mount states.
        crate::process_state::start_volume_watch(cx);

        // Live sleep/wake watch: pause video + slideshow when the
        // machine or its displays sleep; re-list volumes and reload
        // directory tabs on wake (docs/features/POWER.md).
        crate::process_state::start_power_watch(cx);

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
pub fn open_shell_window(cx: &mut App) {
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
        // The main window failing to open is fatal to a useful session, but a
        // logged error beats an abort — the panic hook would have nothing more
        // to add than this line.
        if let Err(e) = cx.open_window(opts, |window, cx| {
            let process = crate::process_state::process_state(cx);
            let view = cx.new(|cx| Shell::new(process, window, cx));
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        }) {
            crate::log_error!(90, "could not open main window: {e}");
        }
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
    let show_desktop_available = crate::platform_shell::show_desktop_available();
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
                // Ellipsis: each opens a confirmation dialog (macOS HIG).
                // Delete Immediately is Finder's "Delete Immediately…" — a
                // targeted permanent delete with no undo.
                MenuItem::action("Delete Immediately\u{2026}", DeleteImmediately),
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
