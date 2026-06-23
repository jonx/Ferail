//! Headless screenshot CLI for the GPUI shell.
//!
//! Lets the developer (and Claude) iterate on the UI without manual
//! screen-capture. Renders one frame off-screen via
//! `Window::render_to_image` (gated behind gpui's `test-support`
//! feature; enabled in the workspace `Cargo.toml`), writes a PNG,
//! then quits.
//!
//! ```sh
//! cargo run --bin feraille-gpui -- --screenshot screenshots/foo.png \
//!     --theme dark --width 1180 --height 760 --navigate ~/Documents
//! ```
//!
//! Roughly 20 flags drive navigation, selection, and overlays so a
//! single off-screen frame can be captured for visual verification.

use std::path::PathBuf;

use crate::assets::FeraAssets;
use anyhow::{Context as _, Result};
use feraille_fs_native::home_dir;
use gpui::*;
use gpui_component::{Theme, ThemeMode, WindowExt as _};

use crate::settings::{SettingsView, category_from_arg};
use crate::shell::Shell;

#[derive(Debug, Default)]
pub struct Args {
    /// Path to write the PNG. None ⇒ run the GUI normally.
    pub screenshot: Option<PathBuf>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub scale: Option<f32>,
    pub theme: Option<ThemeMode>,
    /// Folders to navigate to, in order. Each is applied to the
    /// active tab; chaining seeds the ant trail with realistic
    /// visit counts.
    pub navigate: Vec<PathBuf>,
    /// Extra tabs to open after the initial one.
    pub new_tabs: Vec<PathBuf>,
    /// Tab index to make active after `--new-tab` flags apply.
    pub tab: Option<usize>,
    /// Tree paths to reveal/expand. Each path's ancestors are added
    /// to `Shell::expanded` and pre-enumerated, so the sidebar tree
    /// shows the path unfurled by the time the screenshot renders.
    pub expand: Vec<PathBuf>,
    /// Set cursor to this row index in the active tab's file list.
    pub select_row: Option<usize>,
    /// Set cursor to the first row whose name matches.
    pub select_name: Option<String>,
    /// Enter breadcrumb edit mode (Cmd+L) and simulate typing this
    /// text, so the path-autocomplete menu renders for capture.
    pub breadcrumb: Option<String>,
    /// Whitespace-separated keystrokes (gpui DSL, e.g. "down down
    /// enter") dispatched through the real window key path after the
    /// other flags apply. Verifies focus/keybinding routing headlessly.
    pub keys: Option<String>,
    /// Seed a multi-row selection by row index (comma-separated on
    /// the CLI: `--select-rows 0,2,5`). The first index becomes
    /// the anchor; the last becomes the lead. Drives screenshot
    /// verification of the selection-set rendering without
    /// simulating click+modifier sequences.
    pub select_rows: Vec<usize>,
    /// Force the active tab's view mode (`--view grid` / `--view list`)
    /// before capture, so the icon grid can be screenshotted without
    /// mutating the user's persisted default.
    pub view: Option<crate::grid::ViewMode>,
    /// Sidebar splitter position (DIPs). N/A in the GPUI shell —
    /// the sidebar width is currently a Settings choice, not a
    /// drag-resizable splitter.
    pub splitter: Option<f32>,
    /// List scroll offset (DIPs). Stage-2 stub.
    pub scroll: Option<f32>,
    /// In-row breadcrumb edit mode (Cmd+L). Lands in Stage 9.
    pub edit_mode: bool,
    /// Show dotfiles.
    pub show_hidden: bool,
    /// Active filter text (Cmd+F input field's content).
    pub filter: Option<String>,
    /// Open / focus the filter widget.
    pub search: bool,
    /// Launch a recursive / global search of the active tab's directory
    /// for this needle (docs/features/SEARCH.md), streaming results into
    /// the list. Verifies the search-results tab headlessly.
    pub search_subtree: Option<String>,
    /// Launch a duplicate-finder scan of the active tab's directory
    /// (docs/features/DUPLICATES.md). Verifies the duplicates tab.
    pub find_duplicates: bool,
    /// Force the duplicate scan into the dedicated panel presentation
    /// (`DupePresentation::Panel`) regardless of the saved setting, so the
    /// card view can be captured headlessly. Implies `--find-duplicates`.
    pub dupe_panel: bool,
    /// Show the preview pane (`preview_visible` defaults to off; the
    /// pane also auto-hides under the viewport-width threshold, so
    /// pair with `--width` ≥ 900 when capturing it).
    pub preview: bool,
    /// Column sort: `(column_name, ascending)` where column is one
    /// of name / size / kind / magic / modified | mtime.
    pub sort: Option<(String, bool)>,
    /// Open Get Info / properties panel. Lands in Stage 8.
    pub properties: bool,
    /// Simulate macOS traffic-light inset on the tabstrip. N/A —
    /// the GPUI shell already has native window chrome.
    pub mac_chrome: bool,
    /// Open the rename dialog for the selected row.
    pub rename: bool,
    /// Start inline (in-row) rename. The GPUI shell uses the modal
    /// rename only; falls back to `--rename` semantics.
    pub inline_rename: bool,
    /// Open the new-folder dialog.
    pub new_folder: bool,
    /// Push a fake toast with the given message. Lands in Stage 5.
    pub simulate_toast: Option<String>,
    /// Show the footer progress strip: <0 → indeterminate, ≥0 →
    /// determinate at that fraction. Lands in Stage 5.
    pub simulate_progress: Option<f32>,
    /// Open the task panel pre-populated with two representative
    /// tasks. Lands in Stage 5.
    pub simulate_task_panel: bool,
    /// `Some(_)` opens the keyboard-shortcuts help overlay with
    /// the given filter text pre-populated (empty string = open
    /// with no filter). Lands in Stage 9.
    pub shortcuts_help: Option<String>,
    /// User UI zoom scale. Lands in Stage 9 alongside the
    /// `feraille_design::Tokens::scaled` integration.
    pub ui_scale: Option<f32>,
    /// Open the Disk Usage window at this path and render its
    /// treemap headless. Lands in Stage 7.
    pub disk_usage: Option<PathBuf>,
    /// Treemap recursion depth for `--disk-usage`. Default 4.
    pub disk_usage_depth: u32,
    /// Coloring mode for `--disk-usage`. `category` (default) or
    /// `depth`. Stored as a string until Stage 7 wires the actual
    /// enum.
    pub disk_usage_coloring: String,
    /// `Some(page)` opens the Settings view at that page (kept from
    /// Phase 3 / 5).
    pub settings: Option<String>,
    /// Open the viewer window (docs/features/VIEWER.md) instead of the
    /// shell. A directory renders its first file with the full
    /// playlist; a single file renders a one-entry playlist.
    pub viewer: Option<PathBuf>,
    /// Open the viewer's colour/enhance adjustments panel for the capture.
    pub viewer_adjust: bool,
    /// Screenshot-only: render the viewer adjustments panel in full mpv-video
    /// mode (all colour/enhance/transparent-colour controls) without opening a
    /// live stream — for capturing the panel layout.
    pub viewer_adjust_video: bool,
    /// Render the drag ghost ([`crate::file_list::DragBadge`]) for a
    /// drag of N items, in isolation, against a neutral backdrop —
    /// the only way to capture the cursor ghost headlessly (it never
    /// exists outside a live drag). Uses placeholder coloured tiles
    /// for the item images.
    pub drag_ghost: Option<usize>,
    /// Render the favorite icon-picker window
    /// ([`crate::favorite_icon_picker`]) instead of the shell, so the
    /// Lucide glyph grid can be captured headlessly.
    pub icon_picker: bool,
}

pub fn parse_args() -> Args {
    let mut args = Args {
        disk_usage_depth: 4,
        disk_usage_coloring: "category".to_string(),
        ..Args::default()
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--screenshot" => args.screenshot = iter.next().map(PathBuf::from),
            "--width" => args.width = iter.next().and_then(|s| s.parse().ok()),
            "--height" => args.height = iter.next().and_then(|s| s.parse().ok()),
            "--scale" => args.scale = iter.next().and_then(|s| s.parse().ok()),
            "--theme" => {
                args.theme = iter.next().and_then(|s| match s.as_str() {
                    "light" => Some(ThemeMode::Light),
                    "dark" => Some(ThemeMode::Dark),
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
            "--tab" => args.tab = iter.next().and_then(|s| s.parse().ok()),
            "--expand" => {
                if let Some(p) = iter.next() {
                    args.expand.push(PathBuf::from(p));
                }
            }
            "--view" => args.view = iter.next().map(|s| crate::grid::ViewMode::from_str(&s)),
            "--select-row" => args.select_row = iter.next().and_then(|s| s.parse().ok()),
            "--select-name" => args.select_name = iter.next(),
            "--breadcrumb" => args.breadcrumb = iter.next(),
            "--keys" => args.keys = iter.next(),
            "--select-rows" => {
                if let Some(raw) = iter.next() {
                    args.select_rows = raw
                        .split(',')
                        .filter_map(|s| s.trim().parse::<usize>().ok())
                        .collect();
                }
            }
            "--splitter" => args.splitter = iter.next().and_then(|s| s.parse().ok()),
            "--scroll" => args.scroll = iter.next().and_then(|s| s.parse().ok()),
            "--edit-mode" => args.edit_mode = true,
            "--show-hidden" => args.show_hidden = true,
            "--filter" => args.filter = iter.next(),
            "--search" => args.search = true,
            "--search-subtree" => args.search_subtree = iter.next(),
            "--find-duplicates" => args.find_duplicates = true,
            "--dupe-panel" => {
                args.find_duplicates = true;
                args.dupe_panel = true;
            }
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
                if let Some(s) = iter.next() {
                    args.disk_usage_coloring = s;
                }
            }
            "--settings" => {
                args.settings = Some(iter.next().unwrap_or_default());
            }
            "--viewer" => args.viewer = iter.next().map(PathBuf::from),
            "--viewer-adjust" => args.viewer_adjust = true,
            "--viewer-adjust-video" => args.viewer_adjust_video = true,
            "--icon-picker" => args.icon_picker = true,
            "--drag-ghost" => args.drag_ghost = iter.next().and_then(|s| s.parse().ok()),
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
  --screenshot <path>      Write a PNG to <path> and exit (no visible window).
  --width <N>              Logical width in DIPs (default 1180).
  --height <N>             Logical height in DIPs (default 760).
  --scale <factor>         Display scale factor (default 2.0).
  --theme light|dark       Theme (default: follow system appearance).
  --navigate <path>        Navigate the active tab to <path>. Repeatable.
  --new-tab <path>         Open an additional tab at <path>. Repeatable.
  --tab <idx>              Set active tab index after --new-tab(s) apply.
  --expand <path>          Reveal & expand <path> in the sidebar tree. Repeatable.
  --select-row <N>         Set cursor to row N in the file pane.
  --select-name <name>     Set cursor to first row whose name equals <name>.
  --select-rows <a,b,c>    Multi-select rows by index. First = anchor, last = lead.
  --splitter <x>           Sidebar splitter position. Stage-2 stub.
  --scroll <y>             List scroll offset. Stage-2 stub.
  --show-hidden            Include dotfiles.
  --filter <text>          Set the filter input value.
  --search                 Focus the filter input (Cmd+F).
  --preview                Show preview pane (always on today).
  --sort <column[-desc]>   Sort by name | size | kind | magic | mtime ± desc.
  --properties             Open Get Info pane. Lands in Stage 8.
  --rename                 Open the rename dialog for the selected row.
  --inline-rename          Start inline rename. Falls back to modal in the GPUI shell.
  --new-folder             Open the new-folder dialog.
  --edit-mode              Open breadcrumb edit mode. Lands in Stage 9.
  --mac-chrome             N/A in the GPUI shell (native chrome already).
  --simulate-toast <text>  Push an error toast. Lands in Stage 5.
  --simulate-progress <p>  Force-show the progress strip. Lands in Stage 5.
  --simulate-task-panel    Open task panel with fixtures. Lands in Stage 5.
  --shortcuts-help[-filter] Open keyboard help overlay. Lands in Stage 9.
  --ui-scale <factor>      Apply UI zoom. Lands in Stage 9.
  --disk-usage <path>      Render disk-usage treemap. Lands in Stage 7.
  --du-depth <N>           Treemap recursion depth (default 4).
  --du-coloring <mode>     'category' (default) or 'depth'.
  --settings <page>        Open Settings instead of Shell.
                           appearance / files / layout / about.
  --viewer <path>          Render the viewer window for <path> (file or
                           folder) instead of the shell.
  --viewer-adjust          Open the viewer's colour/enhance panel for capture.
  --drag-ghost <N>         Render the drag cursor ghost for an N-item drag
                           (placeholder tiles) against a neutral backdrop.
  -h, --help               Print this help.

EXAMPLES
  feraille-gpui --screenshot home.png --navigate ~/Documents
  feraille-gpui --screenshot multi.png --new-tab ~/Documents --new-tab ~/Downloads --tab 1
  feraille-gpui --screenshot filter.png --navigate . --filter toml --search
  feraille-gpui --screenshot dark.png --theme dark --navigate ~/Documents
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
    let settings_page = args.settings.clone();
    let disk_usage_root = args.disk_usage.clone();
    let viewer_target = args.viewer.clone();
    let viewer_adjust = args.viewer_adjust;
    let viewer_adjust_video = args.viewer_adjust_video;
    let drag_ghost = args.drag_ghost;
    let icon_picker = args.icon_picker;

    let shell_args = ShellArgs::from(&args);

    let app = gpui_platform::application().with_assets(FeraAssets);
    app.run(move |cx| {
        gpui_component::init(cx);
        crate::shell::init(cx);
        // Register the dock icon — see comment in main.rs::run_gui;
        // must happen post-NSApplication-init.
        let _ = crate::platform_shell::set_app_icon_from_png_bytes(crate::app_icon::PNG);
        if let Some(mode) = theme_mode {
            Theme::change(mode, None, cx);
        }
        // Build the singleton ProcessState and register it as a
        // GPUI Global so the open_window factory (and any
        // subsequent Shell::new) can read it back.
        let process = Shell::build_process_state(cx);
        cx.set_global(crate::process_state::ProcessStateGlobal(process));

        let path = path.clone();
        let settings_page = settings_page.clone();
        let shell_args = shell_args.clone();
        let disk_usage_root = disk_usage_root.clone();
        let viewer_target = viewer_target.clone();
        cx.spawn(async move |cx| {
            // Headless capture: `Window::render_to_image` samples an offscreen
            // render target with the window hidden — macOS via gpui_macos's
            // MetalRenderer, Windows via gpui_windows's D3D11 staging readback
            // (our render_to_image patch; docs/GPUI-UPSTREAM.md item 7). So the
            // window stays hidden on every platform — truly headless, no flash.
            let opts = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: gpui::Point::default(),
                    size: gpui::size(px(width), px(height)),
                })),
                // Match the live GUI's title-bar mode (see
                // main.rs::open_shell_window_sized) so the screenshot
                // captures the same chrome the user actually sees —
                // gpui-component's TitleBar widget replaces the OS
                // default and hosts our brand + filter + nav row.
                titlebar: Some(gpui_component::TitleBar::title_bar_options()),
                show: false,
                focus: false,
                ..Default::default()
            };
            // Open the window. Whether we hand back an
            // `Option<Entity<Shell>>` so the CLI flags can drive it
            // depends on whether this is the Shell or Settings path.
            #[allow(clippy::type_complexity)]
            let mut shell_entity: Option<Entity<Shell>> = None;
            let handle = cx
                .open_window(opts, |window, cx| {
                    if let Some(du_root) = disk_usage_root.clone() {
                        // Stage 7 headless DU window: skip the shell
                        // entirely, render the treemap straight into
                        // the screenshot frame.
                        let fs = std::sync::Arc::new(feraille_fs_native::NativeFs::new());
                        let canonical = std::fs::canonicalize(&du_root).unwrap_or(du_root.clone());
                        // Screenshot path has no owning Shell, so use
                        // a fresh standalone task registry + no notify.
                        let tasks = std::rc::Rc::new(std::cell::RefCell::new(
                            crate::tasks::TaskRegistry::new(),
                        ));
                        let view = cx.new(|cx| {
                            crate::disk_usage::DiskUsageView::new(
                                canonical, fs, tasks, None, None, cx,
                            )
                        });
                        cx.new(|cx| gpui_component::Root::new(view, window, cx))
                    } else if let Some(target) = viewer_target.clone() {
                        // Headless viewer window: build the playlist
                        // straight from the filesystem (setup phase,
                        // not a render path).
                        let process = crate::process_state::process_state(cx);
                        let canonical = std::fs::canonicalize(&target).unwrap_or(target.clone());
                        let mut playlist = Vec::new();
                        if canonical.is_dir() {
                            let mut files: Vec<PathBuf> = std::fs::read_dir(&canonical)
                                .map(|rd| {
                                    rd.filter_map(|e| e.ok())
                                        .map(|e| e.path())
                                        .filter(|p| p.is_file())
                                        .collect()
                                })
                                .unwrap_or_default();
                            files.sort();
                            playlist.extend(files.into_iter().map(|p| {
                                let name = p
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                crate::viewer::PlaylistEntry { path: p, name }
                            }));
                        } else {
                            let name = canonical
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            playlist.push(crate::viewer::PlaylistEntry {
                                path: canonical.clone(),
                                name,
                            });
                        }
                        let view = cx.new(|cx| {
                            crate::viewer::ViewerWindow::new(
                                playlist, 0, false, process, window, cx,
                            )
                        });
                        if viewer_adjust {
                            view.update(cx, |w, _| w.open_adjust_panel());
                        }
                        if viewer_adjust_video {
                            view.update(cx, |w, _| w.sim_full_adjust_panel());
                        }
                        cx.new(|cx| gpui_component::Root::new(view, window, cx))
                    } else if icon_picker {
                        // Headless icon-picker window: a standalone
                        // Favorites entity + dummy id is enough to paint the
                        // glyph grid (the grid is built from the asset
                        // bundle, independent of any favorite data).
                        let favorites = cx.new(|_| crate::favorites::Favorites::new(None));
                        let id = feraille_core::favorites::FavoriteId::new();
                        let view = cx.new(|cx| {
                            crate::favorite_icon_picker::IconPickerView::new(
                                favorites, id, window, cx,
                            )
                        });
                        cx.new(|cx| gpui_component::Root::new(view, window, cx))
                    } else if let Some(n) = drag_ghost {
                        // Isolated drag-ghost preview: build a DragBadge
                        // for an N-item drag with placeholder tiles and
                        // render it against a neutral backdrop.
                        let count = n.max(1);
                        let palette = [
                            0x4f8cff_u32,
                            0xff8c42,
                            0x36c275,
                            0xc061ff,
                            0xffd23f,
                            0xff5d5d,
                        ];
                        let icons = (0..count.min(crate::file_list::GHOST_STACK_CAP))
                            .map(|i| placeholder_icon(palette[i % palette.len()]))
                            .collect();
                        let sample = [
                            "Annual Report.pdf",
                            "Q3 Budget.xlsx",
                            "Team Photo.jpg",
                            "Roadmap.key",
                        ];
                        let names = (0..count.min(crate::file_list::GHOST_STACK_CAP))
                            .map(|i| sample[i % sample.len()].into())
                            .collect();
                        let badge = cx.new(|_| crate::file_list::DragBadge {
                            names,
                            icons,
                            count,
                            offset: gpui::Point::default(),
                        });
                        let view = cx.new(|_| DragGhostPreview { badge });
                        cx.new(|cx| gpui_component::Root::new(view, window, cx))
                    } else if let Some(page) = settings_page.as_deref() {
                        let cat =
                            category_from_arg(if page.is_empty() { None } else { Some(page) });
                        let view = cx.new(|cx| SettingsView::new(cat, window, cx));
                        cx.new(|cx| gpui_component::Root::new(view, window, cx))
                    } else {
                        let process = crate::process_state::process_state(cx);
                        let view = cx.new(|cx| Shell::new(process, window, cx));
                        shell_entity = Some(view.clone());
                        cx.new(|cx| gpui_component::Root::new(view, window, cx))
                    }
                })
                .expect("failed to open window for screenshot");

            if let Some(shell) = shell_entity {
                shell_args.apply(&shell, &handle, cx).await;
            }

            // Give async prefetch (magic / quarantine) time to land
            // before render_to_image samples. 2500ms covers a few
            // hundred entries' prefetch + a qlmanage round-trip for
            // the Quick Look thumbnail (which can take ~1s for
            // images, longer for PDFs).
            cx.background_executor()
                .timer(std::time::Duration::from_millis(2500))
                .await;

            let img = capture_window(&handle, cx).expect("screenshot capture failed");

            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            img.save(&path).expect("write PNG");
            eprintln!("wrote {}", path.display());

            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Headless capture — unified across platforms.
// ---------------------------------------------------------------------------
//
// Both platforms go through gpui's `Window::render_to_image`, which samples an
// offscreen render target with the window hidden: macOS via gpui_macos's
// MetalRenderer, Windows via gpui_windows's D3D11 staging-texture readback (our
// render_to_image patch — docs/GPUI-UPSTREAM.md item 7). No window is ever
// shown, so there is no flash and nothing to capture off-screen.

fn capture_window(handle: &AnyWindowHandle, cx: &mut AsyncApp) -> Result<image::RgbaImage> {
    cx.update_window(*handle, |_, window, _| window.render_to_image())
        .map_err(|e| anyhow::anyhow!("update_window failed: {e}"))?
        .map_err(|e| anyhow::anyhow!("render_to_image failed: {e}"))
}

// ---------------------------------------------------------------------------
// Shell-state mutation driven by CLI flags.
// ---------------------------------------------------------------------------

/// Subset of `Args` that mutates Shell state. Clone-safe so the
/// closure capture doesn't need to take `Args` ownership twice.
#[derive(Clone, Debug, Default)]
struct ShellArgs {
    navigate: Vec<PathBuf>,
    new_tabs: Vec<PathBuf>,
    tab: Option<usize>,
    show_hidden: bool,
    filter: Option<String>,
    search: bool,
    search_subtree: Option<String>,
    find_duplicates: bool,
    dupe_panel: bool,
    select_row: Option<usize>,
    select_name: Option<String>,
    select_rows: Vec<usize>,
    view: Option<crate::grid::ViewMode>,
    breadcrumb: Option<String>,
    keys: Option<String>,
    preview: bool,
    sort: Option<(String, bool)>,
    rename: bool,
    new_folder: bool,
    expand: Vec<PathBuf>,
    // Stage-deferred flags. Recorded so the apply step can emit a
    // single "stage X not yet wired" log warning per use, rather
    // than silently dropping the flag.
    properties: bool,
    edit_mode: bool,
    ui_scale: Option<f32>,
    simulate_toast: Option<String>,
    simulate_progress: Option<f32>,
    simulate_task_panel: bool,
    shortcuts_help: Option<String>,
    splitter: Option<f32>,
    scroll: Option<f32>,
}

impl From<&Args> for ShellArgs {
    fn from(a: &Args) -> Self {
        Self {
            navigate: a.navigate.clone(),
            new_tabs: a.new_tabs.clone(),
            tab: a.tab,
            show_hidden: a.show_hidden,
            filter: a.filter.clone(),
            search: a.search,
            search_subtree: a.search_subtree.clone(),
            find_duplicates: a.find_duplicates,
            dupe_panel: a.dupe_panel,
            select_row: a.select_row,
            select_name: a.select_name.clone(),
            select_rows: a.select_rows.clone(),
            view: a.view,
            breadcrumb: a.breadcrumb.clone(),
            keys: a.keys.clone(),
            preview: a.preview,
            sort: a.sort.clone(),
            rename: a.rename || a.inline_rename,
            new_folder: a.new_folder,
            expand: a.expand.clone(),
            properties: a.properties,
            edit_mode: a.edit_mode,
            ui_scale: a.ui_scale,
            simulate_toast: a.simulate_toast.clone(),
            simulate_progress: a.simulate_progress,
            simulate_task_panel: a.simulate_task_panel,
            shortcuts_help: a.shortcuts_help.clone(),
            splitter: a.splitter,
            scroll: a.scroll,
        }
    }
}

impl ShellArgs {
    /// Apply each flag onto the live Shell entity in the headless
    /// window. Each mutation runs on the foreground executor via
    /// `update`. Flags blocked on a later harvest stage emit a log
    /// warning via the `crate::log_warn!` macro and don't fail.
    async fn apply(
        self,
        shell: &Entity<Shell>,
        handle: &WindowHandle<gpui_component::Root>,
        cx: &mut AsyncApp,
    ) {
        for path in self.navigate.iter() {
            let p = canonicalize_or_passthrough(path);
            shell.update(cx, |s, cx| s.navigate(p, cx));
        }
        for path in self.new_tabs.iter() {
            let p = canonicalize_or_passthrough(path);
            // `make_tab` needs `&mut Window` to build the TableState
            // entity and subscribe to it, so route through the
            // window handle rather than the shell entity directly.
            let shell_for_tab = shell.clone();
            let _ = handle.update(cx, move |_root, window, cx| {
                shell_for_tab.update(cx, |s, cx| {
                    let id = s.process.fs.id_for_path(&p);
                    s.process
                        .node_store
                        .borrow_mut()
                        .get_or_create_path_with_id(p.clone(), id);
                    let tab = s.make_tab(p.clone(), id, window, cx);
                    s.tabs.push(tab);
                    s.active = s.tabs.len() - 1;
                    let cur = s.active_tab().current_dir.clone();
                    s.load_path(cur, cx);
                });
            });
        }
        if let Some(idx) = self.tab {
            shell.update(cx, |s, cx| s.select_tab(idx, cx));
        }
        if let Some(mode) = self.view {
            // `set_view_mode` focuses the grid, which needs `&mut Window`.
            let shell_for_view = shell.clone();
            let _ = handle.update(cx, move |_root, window, cx| {
                shell_for_view.update(cx, |s, cx| s.set_view_mode(mode, window, cx));
            });
        }
        for path in self.expand.iter() {
            let p = canonicalize_or_passthrough(path);
            shell.update(cx, |s, cx| {
                s.reveal_path_in_tree(&p);
                cx.notify();
            });
        }
        if self.show_hidden {
            shell.update(cx, |s, cx| {
                if !s.show_hidden {
                    s.toggle_hidden(cx);
                }
            });
        }
        if let Some(text) = self.filter.clone() {
            // Sync the InputState (visual) AND drive load_path
            // (data). InputState::set_value deliberately suppresses
            // InputEvent::Change so the subscription doesn't fire —
            // we have to plumb both ends manually from the CLI.
            let text_for_input = text.clone();
            let text_for_data = text.clone();
            let _ = cx.update_window((*handle).into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    let input = s.active_tab().filter_input.clone();
                    input.update(cx, |state, cx| {
                        state.set_value(text_for_input.clone(), window, cx);
                    });
                    s.active_tab_mut().filter_text = text_for_data.clone();
                    let path = s.active_tab().current_dir.clone();
                    s.load_path(path, cx);
                });
            });
        }
        if self.search {
            // Focus the filter input. cx.update_window threads the
            // Window through; we have to use update_window_in here
            // so the input gets a real window reference.
            let _ = cx.update_window((*handle).into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    s.focus_filter_input(window, cx);
                });
            });
        }
        if let Some(needle) = self.search_subtree.clone() {
            // Sync the filter input (visual) then launch the recursive
            // search, exactly as Enter-in-the-filter-box does.
            let text_for_input = needle.clone();
            let _ = cx.update_window((*handle).into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    let tab_id = s.active_tab().id;
                    let input = s.active_tab().filter_input.clone();
                    input.update(cx, |state, cx| {
                        state.set_value(text_for_input.clone(), window, cx);
                    });
                    s.start_subtree_search(tab_id, needle.clone(), None, cx);
                });
            });
        }
        if self.find_duplicates {
            let force_panel = self.dupe_panel;
            shell.update(cx, |s, cx| {
                let tab_id = s.active_tab().id;
                s.start_duplicate_scan(tab_id, None, cx);
                if force_panel {
                    if let Some(dm) = s
                        .active_tab_mut()
                        .tool_result
                        .as_mut()
                        .and_then(|surface| surface.dupe_mode_mut())
                    {
                        dm.presentation = crate::feature_settings::DupePresentation::Panel;
                    }
                }
            });
        }
        if self.preview {
            shell.update(cx, |s, cx| {
                s.preview_visible = true;
                cx.notify();
            });
        }
        if let Some(text) = self.breadcrumb.clone() {
            // Enter breadcrumb edit mode and SIMULATE TYPING `text`
            // (via the EntityInputHandler trait method, so the
            // completion-provider trigger path runs exactly as it
            // does for real keystrokes). Used to capture the Cmd+L
            // autocomplete menu.
            let shell_for_edit = shell.clone();
            let _ = cx.update_window((*handle).into(), |_, window, cx| {
                shell_for_edit.update(cx, |s, cx| {
                    s.breadcrumb_editing = true;
                    let input = s.breadcrumb_input.clone();
                    input.update(cx, |state, cx| {
                        state.focus(window, cx);
                        use gpui::EntityInputHandler;
                        let end = state.value().encode_utf16().count();
                        state.replace_text_in_range(Some(0..end), &text, window, cx);
                    });
                    cx.notify();
                });
            });
        }
        // Open the shortcuts/command-palette overlay BEFORE `--keys`
        // so a test can drive it with the keyboard (type is seeded via
        // the filter flag; Enter runs the top match).
        if let Some(initial_filter) = self.shortcuts_help.clone() {
            let _ = cx.update_window((*handle).into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    s.open_shortcuts_help(initial_filter, window, cx);
                });
            });
        }
        if let Some(keys) = self.keys.clone() {
            // Let async UI (e.g. the completion menu's provider task)
            // land before dispatching, then send each keystroke
            // through the window's REAL dispatch path — focus,
            // contexts, and keymap all behave exactly as a physical
            // key press would.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(400))
                .await;
            for k in keys.split_whitespace() {
                // "pause" waits out async UI between keys (e.g. the
                // completion menu's accept inserts on a spawned task —
                // a human's next keystroke lands after it).
                if k == "pause" {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(300))
                        .await;
                    continue;
                }
                let _ =
                    cx.update_window(
                        (*handle).into(),
                        |_, window, cx| match gpui::Keystroke::parse(k) {
                            Ok(ks) => {
                                window.dispatch_keystroke(ks, cx);
                            }
                            Err(e) => crate::log_warn!(90, "--keys: bad keystroke {k:?}: {e}"),
                        },
                    );
            }
        }
        if self.select_row.is_some() || self.select_name.is_some() || !self.select_rows.is_empty() {
            // Selection flags resolve against the loaded entry list,
            // but `navigate` streams its enumeration — give the
            // batches (and the magic/quarantine prefetch they kick
            // off) a beat to apply before resolving rows by index or
            // name. The main settle timer only runs AFTER apply().
            cx.background_executor()
                .timer(std::time::Duration::from_millis(700))
                .await;
        }
        if let Some(row) = self.select_row {
            shell.update(cx, |s, cx| {
                s.select_row_index(row, cx);
                if let Some(p) = s.path_for_row(row, cx) {
                    crate::preview::request(s, p, cx);
                }
            });
        }
        if !self.select_rows.is_empty() {
            let rows = self.select_rows.clone();
            shell.update(cx, |s, cx| {
                s.select_row_indices(&rows, cx);
            });
        }
        if let Some(name) = self.select_name.clone() {
            shell.update(cx, |s, cx| {
                let idx = s
                    .active_tab()
                    .table
                    .read(cx)
                    .delegate()
                    .entries
                    .iter()
                    .position(|e| e.name == name);
                if let Some(i) = idx {
                    s.select_row_index(i, cx);
                    if let Some(p) = s.path_for_row(i, cx) {
                        crate::preview::request(s, p, cx);
                    }
                }
            });
        }
        if let Some((col, asc)) = self.sort.clone() {
            shell.update(cx, |s, cx| {
                crate::file_list::apply_sort(&s.active_tab().table, &col, asc, cx);
            });
        }
        if self.rename {
            let _ = cx.update_window((*handle).into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    // RenameSelected handler reads target_row; need
                    // a selection (the lead row in particular).
                    if s.active_tab().lead.is_none() {
                        s.select_row_index(0, cx);
                    }
                    s.trigger_rename(window, cx);
                });
            });
        }
        if self.new_folder {
            let _ = cx.update_window((*handle).into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    s.trigger_new_folder(window, cx);
                });
            });
        }

        // Stage 5.b: status-bar progress / task panel simulation.
        if let Some(p) = self.simulate_progress {
            shell.update(cx, |s, cx| {
                s.simulated_progress = Some(p);
                cx.notify();
            });
        }
        if self.simulate_task_panel {
            shell.update(cx, |s, cx| {
                s.task_panel_open = true;
                let mut reg = s.process.tasks.borrow_mut();
                let _ = reg.begin(
                    crate::tasks::TaskKind::Enumeration,
                    "Indexing 12,318 entries\u{2026}",
                    true,
                );
                let _ = reg.begin(
                    crate::tasks::TaskKind::DiskUsage,
                    "Computing disk usage for ~/Source\u{2026}",
                    true,
                );
                // Seed the "Recent" history with a few representative
                // finished tasks (one of each outcome) so the panel's
                // history section renders in the screenshot.
                let done = reg.begin(crate::tasks::TaskKind::FileOp, "Copied 1,204 items", false);
                reg.end(done);
                let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
                let cancelled = reg.begin_with_cancel(
                    crate::tasks::TaskKind::Search,
                    "Searched \u{201C}invoice\u{201D}",
                    flag,
                );
                reg.end(cancelled);
                let failed = reg.begin(
                    crate::tasks::TaskKind::FileOp,
                    "Compress to archive.zip",
                    false,
                );
                reg.end_failed(failed, "No space left on device");
                drop(reg);
                cx.notify();
            });
        }

        // Stage 9.b: open breadcrumb edit mode (Cmd+L).
        if self.edit_mode {
            let _ = cx.update_window((*handle).into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    s.on_edit_breadcrumb(&crate::shell::EditBreadcrumb, window, cx);
                });
            });
        }

        // Open the Get Info popup for the current target (a selected row
        // via --select-row, else the folder). The 2500ms pre-capture wait
        // below lets the background gather land before render_to_image.
        if self.properties {
            let _ = cx.update_window((*handle).into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    s.on_get_info(&crate::shell::GetInfo, window, cx);
                });
            });
        }
        // UI zoom: the render path multiplies the window rem size by
        // `ui_scale`, and all text is rem-relative through the design
        // tokens, so this scales the whole UI. Clamp to the same range
        // as the interactive Cmd+= / Cmd+- bindings.
        if let Some(scale) = self.ui_scale {
            shell.update(cx, |s, cx| {
                s.ui_scale = scale.clamp(0.6, 2.0);
                s.apply_ui_zoom(cx);
            });
        }
        // Stage 5.c: push a toast notification via gpui-component's
        // built-in Notification primitive. The Root view renders the
        // notification list overlay automatically.
        // `autohide(false)` because the screenshot harness waits up
        // to ~1.2 s for prefetch to settle; the default 5 s autohide
        // is fine for interactive use but the test path wants the
        // toast pinned visible while we capture.
        //
        // Caveat: gpui's `render_to_image` doesn't fully composite
        // absolute-positioned overlay layers (dialogs + notification
        // list both bleed through partial state in headless capture).
        // The toast still surfaces correctly in the live window.
        if let Some(text) = self.simulate_toast.clone() {
            let _ = cx.update_window((*handle).into(), |_, window, cx| {
                window.push_notification(crate::shell::error_notification(text), cx);
            });
        }
        if self.splitter.is_some() {
            crate::log_warn!(
                90,
                "--splitter flag: sidebar is fixed-width in the GPUI shell today"
            );
        }
        if self.scroll.is_some() {
            crate::log_warn!(90, "--scroll flag: Table scroll API not yet wired");
        }
    }
}

/// Expands `~` to `$HOME` and canonicalises; on failure returns the
/// input as-is so the caller still gets a path it can navigate to.
fn canonicalize_or_passthrough(p: &std::path::Path) -> PathBuf {
    let expanded = if let Some(rest) = p.to_string_lossy().strip_prefix("~") {
        let mut h = home_dir();
        let suffix = rest.trim_start_matches('/');
        if !suffix.is_empty() {
            h.push(suffix);
        }
        h
    } else {
        p.to_path_buf()
    };
    std::fs::canonicalize(&expanded).unwrap_or(expanded)
}

/// Root view for `--drag-ghost`: centres a [`crate::file_list::DragBadge`]
/// over a neutral backdrop so the cursor ghost can be captured headlessly.
struct DragGhostPreview {
    badge: Entity<crate::file_list::DragBadge>,
}

impl Render for DragGhostPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(0x2b2b30))
            .child(self.badge.clone())
    }
}

/// A solid RGB tile as a stand-in file image for the drag-ghost preview.
fn placeholder_icon(color: u32) -> std::sync::Arc<RenderImage> {
    const W: u32 = 64;
    const H: u32 = 64;
    let r = ((color >> 16) & 0xff) as u8;
    let g = ((color >> 8) & 0xff) as u8;
    let b = (color & 0xff) as u8;
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for _ in 0..(W * H) {
        rgba.extend_from_slice(&[r, g, b, 255]);
    }
    std::sync::Arc::new(crate::icons::build_render_image(rgba, W, H))
}
