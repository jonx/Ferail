//! Headless screenshot CLI for iter-2.6.
//!
//! Drives `App` state from command-line flags, renders one frame into a
//! `SoftRenderer`, encodes the pixel buffer as PNG. No window opens.
//!
//! Used by Claude (and the developer) to verify visual changes without
//! manual GUI interaction. Does not script mouse drags or animations —
//! state is set directly via App's public methods.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use feraille_design::Theme;
use feraille_render::SoftRenderer;

use crate::{load_default_font, App};

#[derive(Debug, Default)]
pub struct Args {
    pub screenshot: Option<PathBuf>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub scale: Option<f32>,
    pub theme: Option<Theme>,
    pub navigate: Option<PathBuf>,
    pub new_tabs: Vec<PathBuf>,
    pub expand: Vec<PathBuf>,
    pub select_row: Option<usize>,
    pub select_name: Option<String>,
    pub splitter: Option<f32>,
    pub scroll: Option<f32>,
    pub tab: Option<usize>,
    pub edit_mode: bool,
}

pub fn parse_args() -> Args {
    let mut args = Args::default();
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--screenshot" => args.screenshot = iter.next().map(PathBuf::from),
            "--width" => args.width = iter.next().and_then(|s| s.parse().ok()),
            "--height" => args.height = iter.next().and_then(|s| s.parse().ok()),
            "--scale" => args.scale = iter.next().and_then(|s| s.parse().ok()),
            "--theme" => {
                args.theme = iter.next().and_then(|s| match s.as_str() {
                    "light" => Some(Theme::Light),
                    "dark" => Some(Theme::Dark),
                    _ => None,
                });
            }
            "--navigate" => args.navigate = iter.next().map(PathBuf::from),
            "--new-tab" => {
                if let Some(p) = iter.next() {
                    args.new_tabs.push(PathBuf::from(p));
                }
            }
            "--expand" => {
                if let Some(p) = iter.next() {
                    args.expand.push(PathBuf::from(p));
                }
            }
            "--select-row" => args.select_row = iter.next().and_then(|s| s.parse().ok()),
            "--select-name" => args.select_name = iter.next(),
            "--splitter" => args.splitter = iter.next().and_then(|s| s.parse().ok()),
            "--scroll" => args.scroll = iter.next().and_then(|s| s.parse().ok()),
            "--tab" => args.tab = iter.next().and_then(|s| s.parse().ok()),
            "--edit-mode" => args.edit_mode = true,
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
        "feraille — file explorer + screenshot CLI

Without --screenshot, opens the GUI as before. With --screenshot <path>,
runs headlessly: drives the app state from flags, renders one frame, writes
PNG, exits.

OPTIONS
  --screenshot <path>      Render PNG to <path> and exit (no window).
  --width <N>              Logical width in DIPs (default 1180).
  --height <N>             Logical height in DIPs (default 760).
  --scale <factor>         DPI scale factor (default 2.0).
  --theme light|dark       Theme (default light).
  --navigate <path>        Navigate active tab to <path>.
  --new-tab <path>         Open additional tab at <path> (repeatable).
  --tab <idx>              Set active tab index after --new-tabs apply.
  --expand <path>          Expand tree to reveal <path> (repeatable).
  --select-row <N>         Set cursor to row N in the file pane.
  --select-name <name>     Set cursor to first row whose name equals <name>.
  --splitter <x>           Move splitter to x DIPs (clamped to min/max).
  --scroll <y>             Scroll list to y DIPs.
  --edit-mode              Put breadcrumb in edit mode (for screenshotting).
  -h, --help               Print this help.

EXAMPLES
  feraille --screenshot home.png --navigate ~ --width 1400 --height 900
  feraille --screenshot tree.png --expand ~/Source/Feraille --select-name Cargo.toml
  feraille --screenshot dark.png --theme dark --navigate ~/Documents
"
    );
}

pub fn run(args: Args) -> Result<()> {
    let path = args
        .screenshot
        .clone()
        .context("--screenshot path required for headless mode")?;

    let width_dips = args.width.unwrap_or(1180) as f32;
    let height_dips = args.height.unwrap_or(760) as f32;
    let scale = args.scale.unwrap_or(2.0);
    let width_px = (width_dips * scale).round() as u32;
    let height_px = (height_dips * scale).round() as u32;

    // Build app + drive state.
    let mut app = App::new_for_headless(args.theme.unwrap_or(Theme::Light));
    app.set_dimensions(width_px, height_px, scale);

    if let Some(p) = args.navigate.as_deref() {
        app.navigate(canonicalize_or_passthrough(p));
    }
    for p in &args.new_tabs {
        app.new_tab_at(canonicalize_or_passthrough(p));
    }
    if let Some(idx) = args.tab {
        app.switch_to_tab(idx);
    }
    for p in &args.expand {
        app.reveal_in_tree(&canonicalize_or_passthrough(p));
    }
    if let Some(s) = args.splitter {
        app.set_splitter(s);
    }
    if let Some(y) = args.scroll {
        app.set_scroll(y);
    }
    if let Some(row) = args.select_row {
        app.select_row(row);
    }
    if let Some(name) = args.select_name.as_deref() {
        app.select_name(name);
    }
    if args.edit_mode {
        app.enter_breadcrumb_edit_mode();
    }

    let font_bytes = load_default_font().context("load default font")?;
    let mut renderer = SoftRenderer::new(width_px, height_px, scale, font_bytes);

    app.paint_to(&mut renderer);

    write_png(&path, renderer.pixels(), width_px, height_px)?;
    eprintln!("wrote {}", path.display());
    Ok(())
}

fn canonicalize_or_passthrough(p: &Path) -> PathBuf {
    let expanded = if let Some(rest) = p.to_string_lossy().strip_prefix("~") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut h = PathBuf::from(home);
            let suffix = rest.trim_start_matches('/');
            if !suffix.is_empty() {
                h.push(suffix);
            }
            h
        } else {
            p.to_path_buf()
        }
    } else {
        p.to_path_buf()
    };
    std::fs::canonicalize(&expanded).unwrap_or(expanded)
}

fn write_png(path: &Path, pixels: &[u32], width: u32, height: u32) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .with_context(|| format!("write header for {}", path.display()))?;
    let mut bytes: Vec<u8> = Vec::with_capacity(pixels.len() * 3);
    for p in pixels {
        bytes.push(((p >> 16) & 0xFF) as u8);
        bytes.push(((p >> 8) & 0xFF) as u8);
        bytes.push((p & 0xFF) as u8);
    }
    writer
        .write_image_data(&bytes)
        .with_context(|| format!("write image data for {}", path.display()))?;
    Ok(())
}

