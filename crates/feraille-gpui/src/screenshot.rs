//! Headless screenshot CLI for the GPUI shell.
//!
//! Mirrors the CLI surface from `feraille-app::screenshot` so the
//! developer (and Claude) can iterate on the new UI without manual
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
//! Harvest Stage 2 ports the ~20 flags that existed in the old app.
//! Flags whose handler depends on a not-yet-ported feature accept
//! the value at parse time but log a warning when applied — they
//! become functional as the relevant stage lands.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use feraille_fs_native::home_dir;
use gpui::*;
use gpui_component::{Theme, ThemeMode, WindowExt as _, notification::Notification};
use gpui_component_assets::Assets;

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
    /// Show the preview pane (always on in the GPUI shell today;
    /// flag kept for CLI parity).
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
}

pub fn parse_args() -> Args {
    let mut args = Args::default();
    args.disk_usage_depth = 4;
    args.disk_usage_coloring = "category".to_string();
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
            "--select-row" => args.select_row = iter.next().and_then(|s| s.parse().ok()),
            "--select-name" => args.select_name = iter.next(),
            "--splitter" => args.splitter = iter.next().and_then(|s| s.parse().ok()),
            "--scroll" => args.scroll = iter.next().and_then(|s| s.parse().ok()),
            "--edit-mode" => args.edit_mode = true,
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
  -h, --help               Print this help.

EXAMPLES
  feraille-gpui --screenshot home.png --navigate ~/Documents
  feraille-gpui --screenshot multi.png --new-tab ~/Documents --new-tab ~/Downloads --tab 1
  feraille-gpui --screenshot filter.png --navigate ~/Source/Feraille --filter toml --search
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

    let shell_args = ShellArgs::from(&args);

    let app = gpui_platform::application().with_assets(Assets);
    app.run(move |cx| {
        gpui_component::init(cx);
        crate::shell::init(cx);
        // Register the dock icon — see comment in main.rs::run_gui;
        // must happen post-NSApplication-init.
        const APP_ICON_PNG: &[u8] = include_bytes!("../resources/feraille.png");
        let _ = feraille_shell_mac::set_app_icon_from_png_bytes(APP_ICON_PNG);
        if let Some(mode) = theme_mode {
            Theme::change(mode, None, cx);
        }

        let path = path.clone();
        let settings_page = settings_page.clone();
        let shell_args = shell_args.clone();
        let disk_usage_root = disk_usage_root.clone();
        cx.spawn(async move |cx| {
            let opts = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: gpui::Point::default(),
                    size: gpui::size(px(width), px(height)),
                })),
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
                            crate::disk_usage::DiskUsageView::new(canonical, fs, tasks, None, cx)
                        });
                        cx.new(|cx| gpui_component::Root::new(view, window, cx))
                    } else if let Some(page) = settings_page.as_deref() {
                        let cat =
                            category_from_arg(if page.is_empty() { None } else { Some(page) });
                        let view = cx.new(|_| SettingsView::new(cat));
                        cx.new(|cx| gpui_component::Root::new(view, window, cx))
                    } else {
                        let view = cx.new(|cx| Shell::new(window, cx));
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

            let img = cx
                .update_window(handle.into(), |_, window, _| window.render_to_image())
                .and_then(|r| r)
                .expect("render_to_image failed");

            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            img.save(&path).expect("write PNG");
            eprintln!("wrote {}", path.display());

            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
    Ok(())
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
    select_row: Option<usize>,
    select_name: Option<String>,
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
            select_row: a.select_row,
            select_name: a.select_name.clone(),
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
        for path in self.navigate.iter().cloned() {
            let p = canonicalize_or_passthrough(&path);
            let _ = shell.update(cx, |s, cx| s.navigate(p, cx));
        }
        for path in self.new_tabs.iter().cloned() {
            let p = canonicalize_or_passthrough(&path);
            let _ = shell.update(cx, |s, cx| {
                let id = s.fs.id_for_path(&p);
                s.node_store.get_or_create_path_with_id(p.clone(), id);
                s.tabs.push(crate::shell::Tab::new(p, id));
                s.active = s.tabs.len() - 1;
                let cur = s.active_tab().current_dir.clone();
                s.load_path(cur, cx);
            });
        }
        if let Some(idx) = self.tab {
            let _ = shell.update(cx, |s, cx| s.select_tab(idx, cx));
        }
        for path in self.expand.iter().cloned() {
            let p = canonicalize_or_passthrough(&path);
            let _ = shell.update(cx, |s, cx| {
                s.reveal_path_in_tree(&p);
                cx.notify();
            });
        }
        if self.show_hidden {
            let _ = shell.update(cx, |s, cx| {
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
            let _ = cx.update_window(handle.clone().into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    s.filter_input.update(cx, |state, cx| {
                        state.set_value(text_for_input.clone(), window, cx);
                    });
                    s.filter_text = text_for_data.clone();
                    let path = s.active_tab().current_dir.clone();
                    s.load_path(path, cx);
                });
            });
        }
        if self.search {
            // Focus the filter input. cx.update_window threads the
            // Window through; we have to use update_window_in here
            // so the input gets a real window reference.
            let _ = cx.update_window(handle.clone().into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    s.focus_filter_input(window, cx);
                });
            });
        }
        if let Some(row) = self.select_row {
            let _ = shell.update(cx, |s, cx| {
                s.active_tab_mut().selected = Some(row);
                if let Some(p) = s.path_for_row(row, cx) {
                    crate::preview::request(s, p, cx);
                }
            });
        }
        if let Some(name) = self.select_name.clone() {
            let _ = shell.update(cx, |s, cx| {
                let idx = s
                    .table
                    .read(cx)
                    .delegate()
                    .entries
                    .iter()
                    .position(|e| e.name == name);
                if let Some(i) = idx {
                    s.active_tab_mut().selected = Some(i);
                    if let Some(p) = s.path_for_row(i, cx) {
                        crate::preview::request(s, p, cx);
                    }
                }
            });
        }
        if let Some((col, asc)) = self.sort.clone() {
            let _ = shell.update(cx, |s, cx| {
                crate::file_list::apply_sort(&s.table, &col, asc, cx);
            });
        }
        if self.rename {
            let _ = cx.update_window(handle.clone().into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    // RenameSelected handler reads target_row; need
                    // a selection.
                    if s.active_tab().selected.is_none() {
                        s.active_tab_mut().selected = Some(0);
                    }
                    s.trigger_rename(window, cx);
                });
            });
        }
        if self.new_folder {
            let _ = cx.update_window(handle.clone().into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    s.trigger_new_folder(window, cx);
                });
            });
        }

        // Stage 5.b: status-bar progress / task panel simulation.
        if let Some(p) = self.simulate_progress {
            let _ = shell.update(cx, |s, cx| {
                s.simulated_progress = Some(p);
                cx.notify();
            });
        }
        if self.simulate_task_panel {
            let _ = shell.update(cx, |s, cx| {
                let mut reg = s.tasks.borrow_mut();
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
                drop(reg);
                cx.notify();
            });
        }

        // Stage 9.b: open breadcrumb edit mode (Cmd+L).
        if self.edit_mode {
            let _ = cx.update_window(handle.clone().into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    s.on_edit_breadcrumb(&crate::shell::EditBreadcrumb, window, cx);
                });
            });
        }

        // ---- Stage-deferred flags. Log + skip. -------------------
        if self.properties {
            crate::log_warn!(90, "--properties flag: Get Info pane lands in Stage 8");
        }
        if self.ui_scale.is_some() {
            crate::log_warn!(90, "--ui-scale flag: UI zoom lands in Stage 9");
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
            let _ = cx.update_window(handle.clone().into(), |_, window, cx| {
                window.push_notification(Notification::error(text).autohide(false), cx);
            });
        }
        // Stage 9.b: open keyboard-shortcuts help overlay.
        if let Some(initial_filter) = self.shortcuts_help.clone() {
            let _ = cx.update_window(handle.clone().into(), |_, window, cx| {
                shell.update(cx, |s, cx| {
                    s.open_shortcuts_help(initial_filter, window, cx);
                });
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

/// Mirror of `feraille-app::screenshot::canonicalize_or_passthrough`.
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
