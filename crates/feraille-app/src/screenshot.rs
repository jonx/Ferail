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

#[derive(Debug)]
pub struct Args {
    pub screenshot: Option<PathBuf>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub scale: Option<f32>,
    pub theme: Option<Theme>,
    pub navigate: Vec<PathBuf>,
    pub new_tabs: Vec<PathBuf>,
    pub expand: Vec<PathBuf>,
    pub select_row: Option<usize>,
    pub select_name: Option<String>,
    pub splitter: Option<f32>,
    pub scroll: Option<f32>,
    pub tab: Option<usize>,
    pub edit_mode: bool,
    pub show_hidden: bool,
    pub filter: Option<String>, // current-folder name/kind/magic filter
    pub search: bool,           // open the filter dialog
    pub preview: bool,          // show the preview pane
    pub sort: Option<(String, bool)>, // (column_name, ascending)
    pub properties: bool,       // open Get-Info panel for the selected row
    pub mac_chrome: bool,       // simulate macOS native chrome (traffic-light inset on tabstrip)
    pub rename: bool,           // open the rename dialog with the cursor entry
    pub inline_rename: bool,    // start in-row rename for the cursor entry
    pub new_folder: bool,       // open the new-folder dialog
    pub simulate_toast: Option<String>, // push a fake error toast
    /// Force-show the footer ProgressStrip (ignoring debounce). Used to
    /// verify the visual under deterministic conditions.
    pub simulate_progress: Option<f32>,
    /// Open the task panel and seed it with two representative tasks so
    /// the popover and status-bar hint can be verified without a slow
    /// folder. v1 is a fixture for visual review.
    pub simulate_task_panel: bool,
    /// Open the keyboard-shortcuts overlay with the given filter
    /// pre-populated (`Some(String::new())` = open with empty filter,
    /// `Some("zoom")` = open with that text already in the filter).
    /// `None` = don't open.
    pub shortcuts_help: Option<String>,
    /// User-facing UI scale (text, spacing, hit, icon, layout
    /// dimensions are multiplied by this in `Tokens::scaled`). Used to
    /// render screenshot fixtures at non-default scales without
    /// launching the GUI. None falls through to `FERAILLE_UI_SCALE` or
    /// 1.0 via `initial_ui_scale`.
    pub ui_scale: Option<f32>,
    /// When `Some`, run the headless disk-usage path: walk the path
    /// synchronously, build the treemap, paint a single frame with a
    /// thin volume header on top. Bypasses the App entirely — iter-6.1
    /// proves the visual control without the multi-window plumbing
    /// that lands in iter-6.2.
    pub disk_usage: Option<PathBuf>,
    /// Treemap recursion depth for `--disk-usage`. Default 4.
    pub disk_usage_depth: u32,
    /// Coloring mode for `--disk-usage`. `category` (default) or `depth`.
    pub disk_usage_coloring: feraille_controls::TreemapColoring,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            screenshot: None,
            width: None,
            height: None,
            scale: None,
            theme: None,
            navigate: Vec::new(),
            new_tabs: Vec::new(),
            expand: Vec::new(),
            select_row: None,
            select_name: None,
            splitter: None,
            scroll: None,
            tab: None,
            edit_mode: false,
            show_hidden: false,
            filter: None,
            search: false,
            preview: false,
            sort: None,
            properties: false,
            mac_chrome: false,
            rename: false,
            inline_rename: false,
            new_folder: false,
            simulate_toast: None,
            simulate_progress: None,
            simulate_task_panel: false,
            shortcuts_help: None,
            ui_scale: None,
            disk_usage: None,
            disk_usage_depth: 4,
            disk_usage_coloring: feraille_controls::TreemapColoring::Category,
        }
    }
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
            "--navigate" => {
                if let Some(p) = iter.next() {
                    args.navigate.push(PathBuf::from(p));
                }
            }
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
            "--properties" => args.properties = true,
            "--mac-chrome" => args.mac_chrome = true,
            "--rename" => args.rename = true,
            "--inline-rename" => args.inline_rename = true,
            "--new-folder" => args.new_folder = true,
            "--simulate-toast" => args.simulate_toast = iter.next(),
            "--simulate-progress" => {
                args.simulate_progress = Some(
                    iter.next()
                        .and_then(|v| v.parse::<f32>().ok())
                        .unwrap_or(-1.0),
                );
            }
            "--simulate-task-panel" => args.simulate_task_panel = true,
            "--shortcuts-help" => args.shortcuts_help = Some(String::new()),
            "--shortcuts-help-filter" => args.shortcuts_help = iter.next(),
            "--ui-scale" => args.ui_scale = iter.next().and_then(|s| s.parse().ok()),
            "--disk-usage" => args.disk_usage = iter.next().map(PathBuf::from),
            "--du-depth" => {
                if let Some(n) = iter.next().and_then(|s| s.parse().ok()) {
                    args.disk_usage_depth = n;
                }
            }
            "--du-coloring" => {
                args.disk_usage_coloring = match iter.next().as_deref() {
                    Some("depth") => feraille_controls::TreemapColoring::DepthOnly,
                    _ => feraille_controls::TreemapColoring::Category,
                };
            }
            "--show-hidden" => args.show_hidden = true,
            "--filter" => args.filter = iter.next(),
            "--search" => args.search = true,
            "--preview" => args.preview = true,
            "--sort" => {
                let raw = iter.next().unwrap_or_default();
                let (col, asc) = if let Some(c) = raw.strip_suffix("-desc") {
                    (c.to_string(), false)
                } else {
                    (raw, true)
                };
                args.sort = Some((col, asc));
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
  --sort <column[-desc]>   Sort by name, size, kind, modified/mtime, or magic.
                           Add -desc for descending order.
  --show-hidden            Include dotfiles / hidden entries.
  --filter <text>          Filter current folder by name, kind, or magic text.
  --search                 Open the filter dialog using the current filter.
  --preview                Show the selected-item preview pane.
  --edit-mode              Put breadcrumb in edit mode (for screenshotting).
  --properties             Open the Get Info panel for the selected row.
  --rename                 Open the modal rename dialog for the selected row.
  --inline-rename          Start in-row rename for the selected row.
  --simulate-toast <text>  Push an error toast with the given message.
  --new-folder             Open the new-folder dialog.
  --mac-chrome             Simulate macOS traffic-light inset in the tabstrip.
  --disk-usage <path>      Render the disk-usage treemap of <path> instead of
                           the file list. Bypasses the full App; useful for
                           regression-testing the treemap visual in isolation.
  --du-depth <N>           Treemap recursion depth (default 4).
  --du-coloring <mode>     'category' (default) or 'depth'.
  -h, --help               Print this help.

EXAMPLES
  feraille --screenshot home.png --navigate ~ --width 1400 --height 900
  feraille --screenshot tree.png --expand ~/Source/Feraille --select-name Cargo.toml
  feraille --screenshot dark.png --theme dark --navigate ~/Documents
  feraille --screenshot props.png --navigate ~/Source/Feraille --select-name Cargo.toml --properties
  feraille --screenshot rename.png --navigate ~/Source/Feraille --select-name README.md --rename
  feraille --screenshot search.png --navigate ~/Source/Feraille --filter toml --search
  feraille --screenshot preview.png --navigate ~/Source/Feraille --select-name Cargo.toml --preview
  feraille --screenshot du.png --disk-usage ~/Source/Feraille --width 1400 --height 900
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

    // Iter-6.1: dedicated disk-usage rendering path that walks the
    // tree synchronously and paints the treemap without touching App.
    // Iter-6.2 will replace this with the proper DU window + live
    // worker, but keeping a no-App path here means the visual control
    // stays regression-testable on its own.
    if let Some(du_root) = args.disk_usage.clone() {
        return run_disk_usage(
            args,
            &path,
            width_dips,
            height_dips,
            width_px,
            height_px,
            scale,
            &du_root,
        );
    }

    // Build app + drive state.
    let mut app = App::new_for_headless(args.theme.unwrap_or(Theme::Light));
    app.set_dimensions(width_px, height_px, scale);
    if let Some(s) = args.ui_scale {
        app.ui_scale = s.clamp(
            feraille_design::UI_SCALE_MIN,
            feraille_design::UI_SCALE_MAX,
        );
        app.apply_theme();
    }
    app.show_hidden = args.show_hidden;

    // Each --navigate is applied in order, so repeating one (or chaining
    // several) seeds the ant trail with realistic visit counts.
    for p in &args.navigate {
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
    if let Some((col, asc)) = args.sort.clone() {
        let id = match col.as_str() {
            "name" => Some(feraille_controls::ColumnId::Name),
            "size" => Some(feraille_controls::ColumnId::Size),
            "kind" => Some(feraille_controls::ColumnId::Kind),
            "magic" => Some(feraille_controls::ColumnId::Magic),
            "modified" | "mtime" => Some(feraille_controls::ColumnId::Modified),
            _ => None,
        };
        if let Some(id) = id {
            // toggle_sort flips when re-clicking the same column, so set
            // ascending explicitly afterward.
            app.list.toggle_sort(id);
            app.list.sort = feraille_controls::SortKey {
                column: id,
                ascending: asc,
            };
            let key = app.list.sort;
            feraille_controls::sort_entries(&mut app.tabs[app.active].entries, key);
        }
    }
    if let Some(s) = args.splitter {
        app.set_splitter(s);
    }
    if let Some(y) = args.scroll {
        app.set_scroll(y);
    }
    if let Some(filter) = args.filter.clone() {
        app.set_filter_text(filter);
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
    if args.properties {
        app.toggle_properties();
    }
    if args.mac_chrome {
        app.tabstrip.inset_left = feraille_shell_mac::TRAFFIC_LIGHT_INSET;
    }
    if args.rename {
        app.open_rename();
    }
    if args.inline_rename {
        app.start_inline_rename();
    }
    if args.new_folder {
        app.open_new_folder();
    }
    if let Some(text) = args.simulate_toast.clone() {
        app.toasts
            .push(feraille_controls::primitives::toast::Toast::new(
                feraille_controls::primitives::toast::ToastKind::Error,
                text,
            ));
    }
    if args.search {
        app.open_search();
    }
    if args.preview {
        app.set_preview_visible(true);
    }
    if let Some(p) = args.simulate_progress {
        if p < 0.0 {
            let _ = app.progress.start_indeterminate();
        } else {
            let _ = app.progress.start_determinate(p);
        }
        // Bypass the 50ms debounce so the strip shows in the screenshot.
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
    if args.simulate_task_panel {
        // Two representative tasks: one cancellable enumeration with
        // determinate progress, one non-cancellable indexing job.
        let id = app
            .tasks
            .begin(crate::tasks::TaskKind::Enumeration, "Reading folder…", true);
        app.tasks.update(id, 0.42);
        app.tasks.begin(
            crate::tasks::TaskKind::MagicPrefetch,
            "Indexing files…",
            false,
        );
        let _ = app.progress.start_indeterminate();
        std::thread::sleep(std::time::Duration::from_millis(60));
        app.task_panel_open = true;
    }
    if let Some(filter) = args.shortcuts_help.clone() {
        app.show_help_shortcuts();
        if !filter.is_empty() {
            if let Some(modal) = app.shortcuts_modal.as_mut() {
                modal.filter.set_value(&filter);
            }
        }
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

/// Headless disk-usage path: synchronous scan, build tree + layout,
/// paint the full DU window chrome (volume header + treemap + Top-N
/// + splitter rule), write PNG. Uses the same `paint_du` function the
/// live window does so the headless image matches the GUI by
/// construction.
#[allow(clippy::too_many_arguments)]
fn run_disk_usage(
    args: Args,
    out_path: &Path,
    width_dips: f32,
    height_dips: f32,
    width_px: u32,
    height_px: u32,
    scale: f32,
    du_root: &Path,
) -> Result<()> {
    use std::sync::atomic::AtomicBool;

    use feraille_controls::primitives::splitter::Splitter;
    use feraille_design::{Theme, Tokens};
    use feraille_fs_native::{NativeFs, DEFAULT_DU_BATCH};
    use feraille_render::Rect;

    use crate::disk_usage_state::DiskUsageState;
    use crate::disk_usage_window;

    let canonical = canonicalize_or_passthrough(du_root);
    let theme = args.theme.unwrap_or(Theme::Light);
    let tokens = Tokens::for_theme(theme).scaled(args.ui_scale.unwrap_or(1.0));

    let fs_native = NativeFs::new();
    let root_id = fs_native.id_for_path(&canonical);
    let cancel = AtomicBool::new(false);

    // Build a synthetic state, walk to completion, populate volume +
    // top-N. This mirrors what the live window does after a scan.
    let mut state = DiskUsageState::new(canonical.clone(), root_id, 1);
    let err = fs_native.scan_disk_usage(
        &canonical,
        DEFAULT_DU_BATCH,
        &cancel,
        false,
        |batch| state.tree.apply_facts(&batch),
        |_| {},
    );
    if let Some(e) = err {
        anyhow::bail!("disk-usage scan failed for {}: {e:?}", canonical.display());
    }
    state.tree.complete = true;
    state.scan_complete = true;
    state.coloring = args.disk_usage_coloring;
    state.volume = crate::lookup_volume_for_path(&canonical);
    state.rebuild_topn();

    let font_bytes = load_default_font().context("load default font")?;
    let mut renderer = feraille_render::SoftRenderer::new(width_px, height_px, scale, font_bytes);
    let viewport = Rect::new(0.0, 0.0, width_dips, height_dips);
    let mut splitter = Splitter::new(0.0, 0.0);

    disk_usage_window::paint_du(
        &mut state,
        viewport,
        &mut splitter,
        &mut renderer,
        &tokens,
        disk_usage_window::ButtonState::Idle,
    );

    write_png(out_path, renderer.pixels(), width_px, height_px)?;
    eprintln!("wrote {}", out_path.display());
    Ok(())
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
