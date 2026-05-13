//! Feraille — GPUI shell entry point.
//!
//! Dispatches between the live GUI and the headless `--screenshot`
//! capture path. All real view code lives in `crate::shell`.

use anyhow::Result;
use feraille_core::commands::{find, CommandId};
use feraille_gpui::{
    screenshot,
    settings::{category_from_arg, SettingsView},
    shell::{
        CloseTab, CopyPath, FocusFilter, GoHome, MoveToTrash, NavigateBack, NavigateForward,
        NavigateParent, NewFolder, NewTab, OpenSelected, OpenSettings, Refresh,
        RenameSelected, RevealInFinder, Shell, ToggleHidden,
    },
};
use gpui::*;
use gpui_component::Theme;
use gpui_component_assets::Assets;

actions!(app, [Quit]);

fn main() -> Result<()> {
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

fn run_gui(args: screenshot::Args) {
    let app = gpui_platform::application().with_assets(Assets);
    let width = args.width.unwrap_or(1180) as f32;
    let height = args.height.unwrap_or(760) as f32;
    let theme_mode = args.theme;
    let settings_page = args.settings;

    app.run(move |cx| {
        gpui_component::init(cx);
        feraille_gpui::shell::init(cx);
        // Initial theme: explicit --theme flag wins; otherwise
        // detect macOS Appearance via feraille_shell_mac. Live
        // observer (responding to System Settings → Appearance
        // changes after launch) lands with the Stage 9 "Auto"
        // preference work; this just establishes the default.
        let mode = theme_mode.unwrap_or_else(|| {
            if feraille_shell_mac::system_is_dark() {
                gpui_component::ThemeMode::Dark
            } else {
                gpui_component::ThemeMode::Light
            }
        });
        Theme::change(mode, None, cx);

        // Quit action so Cmd+Q routes through gpui's normal app
        // shutdown rather than relying on the platform's default
        // (which still works, but having it as an Action means
        // the menu item below can advertise the shortcut hint).
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());

        install_app_menus(cx);

        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(width), px(height)), cx)),
            ..Default::default()
        };

        let settings_page = settings_page.clone();
        cx.spawn(async move |cx| {
            cx.open_window(opts, |window, cx| {
                if let Some(page) = settings_page.as_deref() {
                    let cat = category_from_arg(if page.is_empty() {
                        None
                    } else {
                        Some(page)
                    });
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
                // app.about not yet implemented — repurpose Settings.
                MenuItem::action(title("app.about", "About Feraille"), OpenSettings),
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
            items: vec![MenuItem::action(title("file.copy_path", "Copy Path"), CopyPath)],
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
