//! Feraille — GPUI shell entry point.
//!
//! Dispatches between the live GUI and the headless `--screenshot`
//! capture path. All real view code lives in `crate::shell`.

use anyhow::Result;
use feraille_gpui::{
    screenshot,
    settings::{category_from_arg, SettingsView},
    shell::Shell,
};
use gpui::*;
use gpui_component::Theme;
use gpui_component_assets::Assets;

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
