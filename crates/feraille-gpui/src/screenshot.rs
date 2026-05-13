//! Headless screenshot CLI for the GPUI shell.
//!
//! Mirrors the pattern from `feraille-app::screenshot` so the developer
//! (and Claude) can iterate on the new UI without manual screen-capture.
//! Renders one frame off-screen via `Window::render_to_image` (gated
//! behind gpui's `test-support` feature; enabled in the workspace
//! `Cargo.toml`), writes a PNG, then quits.
//!
//! ```
//! cargo run --bin feraille-gpui -- --screenshot screenshots/foo.png \
//!     --theme dark --width 1180 --height 760
//! ```

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use gpui::*;
use gpui_component::{Theme, ThemeMode};
use gpui_component_assets::Assets;

use crate::settings::{category_from_arg, SettingsView};
use crate::shell::Shell;

#[derive(Debug, Default)]
pub struct Args {
    /// Path to write the PNG. None ⇒ run the GUI normally.
    pub screenshot: Option<PathBuf>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub theme: Option<ThemeMode>,
    /// `Some("appearance" | "files" | "layout" | "about")` opens
    /// the Settings view at that page instead of the file-manager
    /// Shell. `Some("")` opens Settings at the default page.
    pub settings: Option<String>,
}

pub fn parse_args() -> Args {
    let mut args = Args::default();
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--screenshot" => args.screenshot = iter.next().map(PathBuf::from),
            "--width" => args.width = iter.next().and_then(|s| s.parse().ok()),
            "--height" => args.height = iter.next().and_then(|s| s.parse().ok()),
            "--theme" => {
                args.theme = iter.next().and_then(|s| match s.as_str() {
                    "light" => Some(ThemeMode::Light),
                    "dark" => Some(ThemeMode::Dark),
                    _ => None,
                });
            }
            "--settings" => {
                args.settings = Some(iter.next().unwrap_or_default());
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            _ => {}
        }
    }
    args
}

fn print_help() {
    println!(
        "feraille-gpui — GPUI-stack file explorer

Without --screenshot, opens the GUI. With --screenshot <path>, renders
one frame off-screen, writes the PNG, and exits.

OPTIONS
  --screenshot <path>     Write a PNG to <path> and exit (no visible window).
  --width <N>             Logical width in DIPs (default 1180).
  --height <N>            Logical height in DIPs (default 760).
  --theme light|dark      Theme (default: follow system appearance).
  --settings <page>       Open the Settings view instead of the Shell.
                          <page> is one of: appearance, files, layout, about.
  -h, --help              Print this help.
"
    );
}

/// Run the headless screenshot path. Opens an invisible window, lets
/// one frame render, captures the framebuffer, writes a PNG, quits.
pub fn run(args: Args) -> Result<()> {
    let path = args
        .screenshot
        .clone()
        .context("--screenshot path required for headless mode")?;
    let width = args.width.unwrap_or(1180) as f32;
    let height = args.height.unwrap_or(760) as f32;
    let theme_mode = args.theme;

    let app = gpui_platform::application().with_assets(Assets);
    let settings_page = args.settings.clone();
    app.run(move |cx| {
        gpui_component::init(cx);
        if let Some(mode) = theme_mode {
            Theme::change(mode, None, cx);
        }

        let path = path.clone();
        let settings_page = settings_page.clone();
        cx.spawn(async move |cx| {
            // Open invisibly. show=false keeps the window off-screen
            // and out of the user's face during automated capture.
            let opts = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: gpui::Point::default(),
                    size: gpui::size(px(width), px(height)),
                })),
                show: false,
                focus: false,
                ..Default::default()
            };
            let handle = cx
                .open_window(opts, |window, cx| {
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
                .expect("failed to open window for screenshot");

            // Let one frame render. Without this, render_to_image
            // captures the not-yet-populated initial scene.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(120))
                .await;

            let img = cx
                .update_window(handle.into(), |_, window, _| window.render_to_image())
                .and_then(|r| r)
                .expect("render_to_image failed");

            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            img.save(&path).expect("write PNG");
            eprintln!("wrote {}", path.display());

            // Quit the app — otherwise the run loop spins forever
            // after we've gotten what we came for.
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
    Ok(())
}
