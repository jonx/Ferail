//! Feraille — GPUI shell entry point.
//!
//! Dispatches between the live GUI and the headless `--screenshot`
//! capture path. All real view code lives in `crate::shell`.

use anyhow::Result;
use feraille_gpui::{
    screenshot,
    settings::{category_from_arg, SettingsView},
    shell::{
        CopyPath, MoveToTrash, NavigateBack, NavigateForward, NavigateParent, OpenSelected,
        OpenSettings, Refresh, RevealInFinder, Shell, ToggleHidden,
    },
};
use gpui::*;
use gpui_component::Theme;
use gpui_component_assets::Assets;

actions!(app, [Quit]);

fn main() -> Result<()> {
    let args = screenshot::parse_args();
    if args.screenshot.is_some() {
        return screenshot::run(args);
    }
    run_gui(args);
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
        if let Some(mode) = theme_mode {
            Theme::change(mode, None, cx);
        }

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

/// Build and install the macOS application menu bar. Items are
/// bound to the Actions defined in `shell::*` so keyboard shortcuts
/// and menu clicks fire the same code path.
///
/// The bar shows the standard macOS order: app menu (with
/// Preferences + Quit), File, Edit, View, Help. Items that operate
/// on a selected row are unconditionally enabled in the menu —
/// when nothing's selected they no-op silently. Per-item disable
/// based on dynamic state arrives with the proper validation pass
/// in a polish iter.
fn install_app_menus(cx: &mut App) {
    cx.set_menus([
        Menu {
            name: "Feraille".into(),
            items: vec![
                MenuItem::action("About Feraille", OpenSettings), // placeholder for now
                MenuItem::separator(),
                MenuItem::action("Preferences\u{2026}", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Quit Feraille", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Open", OpenSelected),
                MenuItem::action("Reveal in Finder", RevealInFinder),
                MenuItem::separator(),
                MenuItem::action("Move to Trash", MoveToTrash),
            ],
            disabled: false,
        },
        Menu {
            name: "Edit".into(),
            items: vec![MenuItem::action("Copy Path", CopyPath)],
            disabled: false,
        },
        Menu {
            name: "Go".into(),
            items: vec![
                MenuItem::action("Back", NavigateBack),
                MenuItem::action("Forward", NavigateForward),
                MenuItem::action("Enclosing Folder", NavigateParent),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Show Hidden Files", ToggleHidden),
                MenuItem::separator(),
                MenuItem::action("Refresh", Refresh),
            ],
            disabled: false,
        },
    ]);
}
