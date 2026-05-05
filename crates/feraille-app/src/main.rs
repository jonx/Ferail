//! Feraille — iter-2 final integration.
//!
//! Layout:
//!   header (40)
//!   tabstrip (32)
//!   [filetree (200, splitter-resizable) | breadcrumb (32) / list+scrollbar]
//!   status (24)
//!
//! Per-tab state: current_dir, entries, selection, list_scroll. Shared:
//! one of each control. FileTree replaces the iter-1 Sidebar.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context, Result};
use feraille_controls::primitives::{
    scrollbar::Scrollbar,
    splitter::Splitter,
    text_input::{TextInput, TextInputEvent, TextInputKey},
};
use feraille_controls::{
    sort_entries, BreadcrumbBar, BreadcrumbEvent, FileTree, Section, SectionKind, Selection,
    TabInfo, TabStrip, TabStripEvent, TreeEvent, VirtualizedList,
};
use feraille_core::{AntTrail, EntryKind, EnumerationError, FileEntry, FsBackend, NodeId};
use feraille_design::{FontWeight, Theme, Tokens};
use feraille_fs_native::{
    detect_magic, fetch_icon_rgba, home_dir, list_volumes, move_to_trash, open_with_default,
    NativeFs,
};

mod screenshot;
use feraille_render::{Bitmap, Point as FPoint, Rect as FRect, Renderer, SoftRenderer, TextStyle};
use std::collections::HashMap;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

const DRAG_THRESHOLD_DIPS: f32 = 4.0;
const DRAG_DELAY_MS: u128 = 100;

/// Which pane currently owns keyboard focus. F6 cycles between them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusedPane {
    Tree,
    List,
}

#[derive(Clone, Copy, Debug)]
struct DragWatch {
    start: FPoint,
    when: std::time::Instant,
    row: usize,
}

#[derive(Clone, Debug)]
enum AppEvent {
    MagicBatch {
        generation: u64,
        dir: PathBuf,
        results: Vec<MagicResult>,
    },
}

#[derive(Clone, Debug)]
struct MagicResult {
    name: String,
    mtime_unix: i64,
    label: String,
}

/// State for the modal rename / new-folder dialog.
pub struct TextDialog {
    pub mode: DialogMode,
    pub input: TextInput,
}

#[derive(Clone)]
pub enum DialogMode {
    Rename { original_name: String },
    NewFolder,
}

impl DialogMode {
    fn title(&self) -> &'static str {
        match self {
            DialogMode::Rename { .. } => "Rename",
            DialogMode::NewFolder => "New Folder",
        }
    }
}

const TABSTRIP_H: f32 = 32.0;
const BREADCRUMB_H: f32 = 32.0;
const STATUS_H: f32 = 24.0;
const SCROLLBAR_W: f32 = 10.0;
const PREVIEW_W: f32 = 320.0;
const SIDEBAR_DEFAULT: f32 = 220.0;
const SIDEBAR_MIN: f32 = 160.0;
const SIDEBAR_MAX: f32 = 480.0;

fn main() -> Result<()> {
    let args = screenshot::parse_args();
    if args.screenshot.is_some() {
        return screenshot::run(args);
    }
    let event_loop = EventLoop::<AppEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new();
    app.event_proxy = Some(event_loop.create_proxy());
    app.start_magic_prefetch();
    event_loop.run_app(&mut app)?;
    Ok(())
}

pub struct Tab {
    pub current_dir: PathBuf,
    /// Full current-directory listing after hidden-file filtering.
    pub all_entries: Vec<FileEntry>,
    /// Visible entries after applying `filter_text` and sort.
    pub entries: Vec<FileEntry>,
    pub filter_text: String,
    pub selection: Selection,
    pub list_scroll: f32,
    /// `Some` when the last enumeration of `current_dir` failed.
    /// Surfaces in the file pane as an empty-state message — most
    /// commonly the macOS TCC permission prompt for ~/Documents,
    /// ~/Desktop, ~/Downloads when a sandboxed launcher hasn't been
    /// granted access.
    pub error: Option<EnumerationError>,
    /// Per-tab navigation history. `history_index` points at the
    /// current location. `navigate()` truncates forward then pushes;
    /// back/forward only move the index.
    pub history: Vec<PathBuf>,
    pub history_index: usize,
}

impl Tab {
    fn label(&self) -> String {
        self.current_dir
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| self.current_dir.to_string_lossy().into_owned())
    }
}

pub struct App {
    window: Option<Rc<Window>>,
    sb_context: Option<softbuffer::Context<Rc<Window>>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    renderer: Option<SoftRenderer>,

    pub fs: Arc<NativeFs>,
    pub tabs: Vec<Tab>,
    pub active: usize,

    pub list: VirtualizedList,
    pub scrollbar: Scrollbar,
    pub splitter: Splitter,
    pub tabstrip: TabStrip,
    pub breadcrumb: BreadcrumbBar,
    pub tree: FileTree,
    pub focused_pane: FocusedPane,
    pub splitter_x: f32,
    pub show_hidden: bool,
    pub ant_trail: AntTrail,
    /// Paths the user has pinned to the tree's Favorites section. In-
    /// memory for now; SQLite-persisted in iter-6 alongside ant trail.
    pub pinned_paths: Vec<PathBuf>,
    /// `Some(idx)` when the Get-Info panel is open, pointing at an
    /// index in the active tab's entries.
    pub properties_target: Option<usize>,
    /// Modal text dialog for rename / new-folder. `Some` while open.
    pub dialog: Option<TextDialog>,
    /// Find/filter dialog. While open, text input updates the visible list live.
    pub search: Option<TextInput>,
    pub preview_visible: bool,
    /// Cache of NSWorkspace-fetched icons keyed by `cache_key_for(entry)`
    /// — extension for files (".rs", ".md"), "DIR"/"SYMLINK"/"FILE" for
    /// the rest. Populated lazily on `prefetch_icons` after each navigate.
    pub icon_cache: HashMap<String, Bitmap>,
    /// Cache of magic-detected types keyed by `(path, mtime_unix)`. Empty
    /// string = "we tried, no match".
    pub magic_cache: HashMap<(PathBuf, i64), String>,
    event_proxy: Option<EventLoopProxy<AppEvent>>,
    magic_generation: u64,
    /// `Some` when the user has mouse-down on a list row; promotes to a
    /// system drag once `(distance > 4 DIPs && time > 100 ms)`.
    drag_watch: Option<DragWatch>,
    pointer_dips: Option<FPoint>,
    modifiers: ModifiersState,

    pub tokens: Tokens,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
}

impl App {
    /// Build an `App` configured for headless screenshot use. No window
    /// is opened; the caller sets dimensions and applies scripted state
    /// before calling `paint_to`.
    pub fn new_for_headless(theme: Theme) -> Self {
        let mut a = Self::new();
        a.tokens = Tokens::for_theme(theme);
        a
    }

    pub fn set_dimensions(&mut self, width: u32, height: u32, scale: f32) {
        self.width = width.max(1);
        self.height = height.max(1);
        self.scale_factor = scale.max(0.5);
    }

    pub fn new_tab_at(&mut self, path: PathBuf) {
        self.tabs[self.active].list_scroll = self.list.scroll_offset;
        let new_index = self.tabs.len();
        self.tabs.push(Tab {
            current_dir: path.clone(),
            all_entries: Vec::new(),
            entries: Vec::new(),
            filter_text: String::new(),
            selection: Selection::new(),
            list_scroll: 0.0,
            error: None,
            history: Vec::new(),
            history_index: 0,
        });
        self.active = new_index;
        self.list.scroll_offset = 0.0;
        self.navigate(path);
    }

    pub fn switch_to_tab(&mut self, idx: usize) {
        self.switch_tab(idx);
        self.sync_window_title();
    }

    pub fn set_splitter(&mut self, x: f32) {
        self.splitter_x = x.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
    }

    pub fn set_scroll(&mut self, y: f32) {
        let viewport_h = self.list_inner_rect().size.height;
        let count = self.tabs[self.active].entries.len();
        self.list
            .scroll_by(y - self.list.scroll_offset, count, viewport_h);
    }

    pub fn select_row(&mut self, idx: usize) {
        let count = self.tabs[self.active].entries.len();
        if idx < count {
            self.tabs[self.active].selection.set_cursor(idx);
            let viewport_h = self.list_inner_rect().size.height;
            self.list.ensure_visible(idx, viewport_h);
        }
    }

    pub fn select_name(&mut self, name: &str) {
        let idx = self.tabs[self.active]
            .entries
            .iter()
            .position(|e| e.name == name);
        if let Some(i) = idx {
            self.select_row(i);
        }
    }

    pub fn enter_breadcrumb_edit_mode(&mut self) {
        let path = self.tabs[self.active].current_dir.clone();
        self.breadcrumb.enter_edit_mode(&path);
    }

    /// Copy the cursor entry's full path to the system clipboard.
    pub fn copy_cursor_path(&self) {
        let tab = &self.tabs[self.active];
        let Some(idx) = tab.selection.cursor() else {
            return;
        };
        let Some(entry) = tab.entries.get(idx) else {
            return;
        };
        let path = tab.current_dir.join(&entry.name);
        if let Some(s) = path.to_str() {
            feraille_shell_mac::copy_to_clipboard(s);
        }
    }

    /// Set the focused pane and propagate to the controls' visual state.
    pub fn set_focused_pane(&mut self, pane: FocusedPane) {
        if self.focused_pane == pane {
            return;
        }
        self.focused_pane = pane;
        self.tree.focused = matches!(pane, FocusedPane::Tree);
        self.list.focused = matches!(pane, FocusedPane::List);
        // Reset type-ahead when leaving the tree.
        if !self.tree.focused {
            self.tree.type_ahead_clear();
        }
    }

    /// F6 cycles between Tree and List. Headerless / non-focusable
    /// panes (breadcrumb, tabstrip) are not in the cycle — they take
    /// focus only via mouse.
    pub fn cycle_focus(&mut self) {
        let next = match self.focused_pane {
            FocusedPane::Tree => FocusedPane::List,
            FocusedPane::List => FocusedPane::Tree,
        };
        self.set_focused_pane(next);
    }

    /// Pin a path to Favorites if not already present, or remove it.
    /// Triggers a `rebuild_tree_sections` so the tree reflects the
    /// change.
    pub fn pin_path(&mut self, path: PathBuf) {
        if !self.pinned_paths.contains(&path) {
            self.pinned_paths.push(path);
            self.rebuild_tree_sections();
        }
    }

    pub fn unpin_path(&mut self, path: &Path) {
        let before = self.pinned_paths.len();
        self.pinned_paths.retain(|p| p != path);
        if self.pinned_paths.len() != before {
            self.rebuild_tree_sections();
        }
    }

    /// Right-click on the tree pane: build the section-appropriate menu
    /// and act on the user's choice. Mirrors `show_context_menu_at` for
    /// the list pane but with different actions.
    fn show_tree_context_menu_at(&mut self, p: FPoint) {
        let Some(target) = self.tree.right_click(self.tree_rect(), p) else {
            return;
        };
        self.set_focused_pane(FocusedPane::Tree);
        let Some(window) = self.window.as_ref().cloned() else {
            return;
        };
        let path = match self.fs.path_for(target.id) {
            Some(p) => p,
            None => return,
        };
        let in_favorites = matches!(target.kind, SectionKind::Favorites);
        let pin_label = if in_favorites {
            "Remove from Favorites"
        } else {
            "Pin to Favorites"
        };
        let titles = ["Open", "Reveal in Finder", "Copy Path", "", pin_label];
        let choice = feraille_shell_mac::show_context_menu(&window, &titles, (p.x, p.y));
        match choice {
            Some(0) => self.navigate(path.clone()),
            Some(1) => feraille_shell_mac::reveal_in_finder(&path),
            Some(2) => {
                if let Some(s) = path.to_str() {
                    feraille_shell_mac::copy_to_clipboard(s);
                }
            }
            Some(4) => {
                if in_favorites {
                    self.unpin_path(&path);
                } else {
                    self.pin_path(path);
                }
            }
            _ => {}
        }
        self.request_redraw();
    }

    /// Right-click handler: select the row and show a context menu at
    /// the click location. Synchronous — blocks the event loop while
    /// the menu is open.
    fn show_context_menu_at(&mut self, p: FPoint) {
        let inner = self.list_inner_rect();
        let count = self.tabs[self.active].entries.len();
        let Some(idx) = self.list.index_at(inner, p, count) else {
            return;
        };
        self.tabs[self.active].selection.set_cursor(idx);
        let Some(window) = self.window.as_ref().cloned() else {
            return;
        };
        // Item 4 is the empty separator string.
        let titles = [
            "Open",
            "Reveal in Finder",
            "Get Info",
            "Copy Path",
            "",
            "Move to Trash",
        ];
        let choice = feraille_shell_mac::show_context_menu(&window, &titles, (p.x, p.y));
        match choice {
            Some(0) => self.open_at_cursor(),
            Some(1) => self.reveal_cursor_in_finder(),
            Some(2) => self.toggle_properties(),
            Some(3) => self.copy_cursor_path(),
            Some(5) => self.delete_at_cursor_to_trash(),
            _ => {}
        }
        self.request_redraw();
    }

    /// Open Finder with the cursor entry selected.
    pub fn reveal_cursor_in_finder(&self) {
        let tab = &self.tabs[self.active];
        let Some(idx) = tab.selection.cursor() else {
            return;
        };
        let Some(entry) = tab.entries.get(idx) else {
            return;
        };
        let path = tab.current_dir.join(&entry.name);
        feraille_shell_mac::reveal_in_finder(&path);
    }

    pub fn open_rename(&mut self) {
        let tab = &self.tabs[self.active];
        let Some(idx) = tab.selection.cursor() else {
            return;
        };
        let Some(entry) = tab.entries.get(idx) else {
            return;
        };
        let name = entry.name.clone();
        self.dialog = Some(TextDialog {
            mode: DialogMode::Rename {
                original_name: name.clone(),
            },
            input: TextInput::new(&name),
        });
    }

    pub fn open_new_folder(&mut self) {
        self.dialog = Some(TextDialog {
            mode: DialogMode::NewFolder,
            input: TextInput::new("untitled folder"),
        });
    }

    pub fn open_search(&mut self) {
        let current = self.tabs[self.active].filter_text.clone();
        self.search = Some(TextInput::new(&current));
    }

    pub fn set_filter_text(&mut self, text: String) {
        if self.tabs[self.active].filter_text == text {
            return;
        }
        self.tabs[self.active].filter_text = text;
        self.rebuild_visible_entries(None, false);
    }

    pub fn set_preview_visible(&mut self, visible: bool) {
        self.preview_visible = visible;
    }

    pub fn toggle_preview(&mut self) {
        self.preview_visible = !self.preview_visible;
    }

    fn close_dialog(&mut self) {
        self.dialog = None;
    }

    fn submit_dialog(&mut self) {
        let Some(d) = self.dialog.take() else { return };
        let value = d.input.value();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return;
        }
        let cur_dir = self.tabs[self.active].current_dir.clone();
        let target_path = cur_dir.join(trimmed);
        match d.mode {
            DialogMode::Rename { original_name } => {
                if trimmed == original_name {
                    return; // no-op
                }
                let from = cur_dir.join(&original_name);
                if let Err(e) = std::fs::rename(&from, &target_path) {
                    eprintln!(
                        "rename({}, {}) failed: {e}",
                        from.display(),
                        target_path.display()
                    );
                    return;
                }
                self.refresh_active_tab();
                // Try to keep cursor on the renamed entry.
                let new_name = trimmed.to_string();
                let tab = &mut self.tabs[self.active];
                if let Some(idx) = tab.entries.iter().position(|e| e.name == new_name) {
                    tab.selection.set_cursor(idx);
                }
            }
            DialogMode::NewFolder => {
                if let Err(e) = std::fs::create_dir(&target_path) {
                    eprintln!("create_dir({}) failed: {e}", target_path.display());
                    return;
                }
                self.refresh_active_tab();
                let tab = &mut self.tabs[self.active];
                if let Some(idx) = tab.entries.iter().position(|e| e.name == trimmed) {
                    tab.selection.set_cursor(idx);
                }
            }
        }
    }

    pub fn toggle_properties(&mut self) {
        if self.properties_target.is_some() {
            self.properties_target = None;
        } else if let Some(idx) = self.tabs[self.active].selection.cursor() {
            if idx < self.tabs[self.active].entries.len() {
                self.properties_target = Some(idx);
            }
        }
    }

    fn close_properties(&mut self) {
        self.properties_target = None;
    }

    fn paint_dialog(
        &self,
        tokens: &Tokens,
        viewport: feraille_render::Size,
        renderer: &mut dyn Renderer,
    ) {
        let Some(d) = self.dialog.as_ref() else {
            return;
        };
        // Backdrop
        renderer.fill_rect(
            FRect::new(0.0, 0.0, viewport.width, viewport.height),
            feraille_design::Color::rgba(0, 0, 0, 90),
        );
        let panel_w = 420.0;
        let panel_h = 140.0;
        let panel_x = ((viewport.width - panel_w) / 2.0).round();
        let panel_y = ((viewport.height - panel_h) / 2.0).round();
        let panel = FRect::new(panel_x, panel_y, panel_w, panel_h);
        renderer.fill_rect(panel, tokens.bg.layer1);
        renderer.stroke_rect(panel, 1.0, tokens.border.default);
        let pad = tokens.space.lg;
        renderer.draw_text(
            FPoint::new(panel.left() + pad, panel.top() + pad),
            d.mode.title(),
            TextStyle {
                size: tokens.text.lg,
                weight: FontWeight::SemiBold,
                color: tokens.fg.primary,
            },
        );
        let input_y = panel.top() + pad + tokens.text.lg + tokens.space.md;
        let input_rect = FRect::new(
            panel.left() + pad,
            input_y,
            panel.size.width - pad * 2.0,
            32.0,
        );
        d.input.paint(input_rect, true, tokens, renderer);
        renderer.draw_text(
            FPoint::new(panel.left() + pad, panel.bottom() - pad - tokens.text.xs),
            "Enter to confirm \u{00B7} Esc to cancel",
            TextStyle {
                size: tokens.text.xs,
                weight: FontWeight::Regular,
                color: tokens.fg.disabled,
            },
        );
    }

    fn paint_search(
        &self,
        tokens: &Tokens,
        viewport: feraille_render::Size,
        renderer: &mut dyn Renderer,
    ) {
        let Some(input) = self.search.as_ref() else {
            return;
        };
        renderer.fill_rect(
            FRect::new(0.0, 0.0, viewport.width, viewport.height),
            feraille_design::Color::rgba(0, 0, 0, 70),
        );
        let panel_w = 520.0;
        let panel_h = 132.0;
        let panel_x = ((viewport.width - panel_w) / 2.0).round();
        let panel_y = (viewport.height * 0.18).round();
        let panel = FRect::new(panel_x, panel_y, panel_w, panel_h);
        renderer.fill_rect(panel, tokens.bg.layer1);
        renderer.stroke_rect(panel, 1.0, tokens.border.default);
        let pad = tokens.space.lg;
        renderer.draw_text(
            FPoint::new(panel.left() + pad, panel.top() + pad),
            "Filter",
            TextStyle {
                size: tokens.text.lg,
                weight: FontWeight::SemiBold,
                color: tokens.fg.primary,
            },
        );
        let input_rect = FRect::new(
            panel.left() + pad,
            panel.top() + pad + tokens.text.lg + tokens.space.md,
            panel.size.width - pad * 2.0,
            32.0,
        );
        input.paint(input_rect, true, tokens, renderer);
        renderer.draw_text(
            FPoint::new(panel.left() + pad, panel.bottom() - pad - tokens.text.xs),
            "Type to filter current folder \u{00B7} Enter to close \u{00B7} Esc to dismiss",
            TextStyle {
                size: tokens.text.xs,
                weight: FontWeight::Regular,
                color: tokens.fg.disabled,
            },
        );
    }

    fn paint_preview_pane(&self, bounds: FRect, tokens: &Tokens, renderer: &mut dyn Renderer) {
        if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
            return;
        }
        renderer.fill_rect(bounds, tokens.bg.layer2);
        renderer.fill_rect(
            FRect::new(bounds.left(), bounds.top(), 1.0, bounds.size.height),
            tokens.border.subtle,
        );
        renderer.push_clip(bounds);

        let pad = tokens.space.lg;
        let tab = &self.tabs[self.active];
        let selected = tab
            .selection
            .cursor()
            .and_then(|idx| tab.entries.get(idx));
        let Some(entry) = selected else {
            renderer.draw_text(
                FPoint::new(bounds.left() + pad, bounds.top() + pad),
                "Preview",
                TextStyle {
                    size: tokens.text.lg,
                    weight: FontWeight::SemiBold,
                    color: tokens.fg.primary,
                },
            );
            renderer.draw_text(
                FPoint::new(bounds.left() + pad, bounds.top() + pad + 34.0),
                "No item selected",
                TextStyle {
                    size: tokens.text.md,
                    weight: FontWeight::Regular,
                    color: tokens.fg.secondary,
                },
            );
            renderer.pop_clip();
            return;
        };

        let mut y = bounds.top() + pad;
        let icon_size = 32.0;
        if let Some(bitmap) = self.icon_cache.get(&cache_key_for(entry)) {
            renderer.draw_bitmap(
                FRect::new(bounds.left() + pad, y, icon_size, icon_size),
                bitmap,
            );
        } else {
            renderer.fill_rect(
                FRect::new(bounds.left() + pad, y, icon_size, icon_size),
                tokens.accent.fill,
            );
        }
        let title_x = bounds.left() + pad + icon_size + tokens.space.md;
        renderer.draw_text(
            FPoint::new(title_x, y + 1.0),
            &entry.name,
            TextStyle {
                size: tokens.text.lg,
                weight: FontWeight::SemiBold,
                color: tokens.fg.primary,
            },
        );
        renderer.draw_text(
            FPoint::new(title_x, y + tokens.text.lg + 6.0),
            &entry.display_kind,
            TextStyle {
                size: tokens.text.sm,
                weight: FontWeight::Regular,
                color: tokens.fg.secondary,
            },
        );
        y += icon_size + pad;

        renderer.fill_rect(
            FRect::new(bounds.left() + pad, y, bounds.size.width - pad * 2.0, 1.0),
            tokens.border.subtle,
        );
        y += pad;

        let path = tab.current_dir.join(&entry.name);
        let path_text = path.to_string_lossy().into_owned();
        let size_text = if matches!(entry.kind, EntryKind::Directory) {
            "Folder".to_string()
        } else {
            format!("{} ({} bytes)", entry.display_size, entry.size)
        };
        let magic_text = if entry.display_magic.is_empty() {
            "Unknown".to_string()
        } else {
            entry.display_magic.clone()
        };
        let rows = [
            ("Where", path_text.as_str()),
            ("Kind", entry.display_kind.as_str()),
            ("Size", size_text.as_str()),
            ("Modified", entry.display_mtime.as_str()),
            ("Magic", magic_text.as_str()),
        ];
        for (label, value) in rows {
            renderer.draw_text(
                FPoint::new(bounds.left() + pad, y),
                label,
                TextStyle {
                    size: tokens.text.xs,
                    weight: FontWeight::Medium,
                    color: tokens.fg.secondary,
                },
            );
            y += tokens.text.xs + 5.0;
            renderer.draw_text(
                FPoint::new(bounds.left() + pad, y),
                value,
                TextStyle {
                    size: tokens.text.sm,
                    weight: FontWeight::Regular,
                    color: tokens.fg.primary,
                },
            );
            y += tokens.text.sm + tokens.space.md;
        }

        renderer.pop_clip();
    }

    fn paint_properties(
        &self,
        tokens: &Tokens,
        viewport: feraille_render::Size,
        renderer: &mut dyn Renderer,
    ) {
        let Some(idx) = self.properties_target else {
            return;
        };
        let tab = &self.tabs[self.active];
        let Some(entry) = tab.entries.get(idx) else {
            return;
        };

        // Backdrop dim
        renderer.fill_rect(
            FRect::new(0.0, 0.0, viewport.width, viewport.height),
            feraille_design::Color::rgba(0, 0, 0, 90),
        );

        // Panel rect
        let panel_w = 480.0;
        let panel_h = 380.0;
        let panel_x = ((viewport.width - panel_w) / 2.0).round();
        let panel_y = ((viewport.height - panel_h) / 2.0).round();
        let panel = FRect::new(panel_x, panel_y, panel_w, panel_h);
        renderer.fill_rect(panel, tokens.bg.layer1);
        renderer.stroke_rect(panel, 1.0, tokens.border.default);

        renderer.push_clip(panel);

        let pad = tokens.space.xl;
        let mut y = panel.top() + pad;

        // Title row: large icon (if cached) + name on the right.
        let icon_size = 32.0;
        let icon_x = panel.left() + pad;
        if let Some(bitmap) = self.icon_cache.get(&cache_key_for(entry)) {
            renderer.draw_bitmap(FRect::new(icon_x, y, icon_size, icon_size), bitmap);
        } else {
            renderer.fill_rect(
                FRect::new(icon_x, y, icon_size, icon_size),
                tokens.accent.fill,
            );
        }
        let name_x = icon_x + icon_size + tokens.space.md;
        renderer.draw_text(
            FPoint::new(name_x, y + 2.0),
            &entry.name,
            TextStyle {
                size: tokens.text.lg,
                weight: FontWeight::SemiBold,
                color: tokens.fg.primary,
            },
        );
        renderer.draw_text(
            FPoint::new(name_x, y + tokens.text.lg + 6.0),
            &entry.display_kind,
            TextStyle {
                size: tokens.text.sm,
                weight: FontWeight::Regular,
                color: tokens.fg.secondary,
            },
        );
        y += icon_size + pad;

        // Divider
        renderer.fill_rect(
            FRect::new(panel.left() + pad, y, panel.size.width - pad * 2.0, 1.0),
            tokens.border.subtle,
        );
        y += pad - 4.0;

        // Key-value rows
        let path = tab.current_dir.join(&entry.name);
        let path_str = path.to_string_lossy().into_owned();
        let size_text = if matches!(entry.kind, EntryKind::Directory) {
            String::from("—")
        } else if entry.size >= 1024 {
            format!("{} ({} bytes)", entry.display_size, entry.size)
        } else {
            format!("{} bytes", entry.size)
        };
        let magic_text = if entry.display_magic.is_empty() {
            String::from("—")
        } else {
            entry.display_magic.clone()
        };
        let mtime_iso = format_iso_date(entry.mtime_unix);

        let rows: [(&str, &str); 5] = [
            ("Where", path_str.as_str()),
            ("Kind", entry.display_kind.as_str()),
            ("Size", size_text.as_str()),
            ("Modified", mtime_iso.as_str()),
            ("Magic", magic_text.as_str()),
        ];
        for (label, value) in rows {
            renderer.draw_text(
                FPoint::new(panel.left() + pad, y),
                label,
                TextStyle {
                    size: tokens.text.sm,
                    weight: FontWeight::Medium,
                    color: tokens.fg.secondary,
                },
            );
            renderer.draw_text(
                FPoint::new(panel.left() + pad + 90.0, y),
                value,
                TextStyle {
                    size: tokens.text.md,
                    weight: FontWeight::Regular,
                    color: tokens.fg.primary,
                },
            );
            y += tokens.text.md * 2.0;
        }

        renderer.pop_clip();

        // Footer hint
        renderer.draw_text(
            FPoint::new(panel.left() + pad, panel.bottom() - tokens.space.xl),
            "Esc to close · Cmd+I again to toggle",
            TextStyle {
                size: tokens.text.xs,
                weight: FontWeight::Regular,
                color: tokens.fg.disabled,
            },
        );
    }

    /// Resolve a path → NodeId via the FS (allocating an ID if new).
    pub fn id_for_path(&self, path: &Path) -> NodeId {
        self.fs.id_for_path(path)
    }

    fn new() -> Self {
        let fs = Arc::new(NativeFs::new());
        let home = home_dir();

        // Seed the tree with Finder-style sections: Recents (top of ant
        // trail) / Favorites (pinned) / Locations (iCloud, Home, root,
        // Trash) / Volumes (other /Volumes mounts).
        let tree = FileTree::new();
        // Default Favorites mirror Finder's out-of-the-box defaults.
        let pinned_paths: Vec<PathBuf> = ["Applications", "Desktop", "Documents", "Downloads"]
            .iter()
            .map(|sub| home.join(sub))
            .filter(|p| p.is_dir())
            .collect();

        let mut breadcrumb = BreadcrumbBar::new();
        breadcrumb.set_path(&home);

        let initial_tab = Tab {
            current_dir: home.clone(),
            all_entries: Vec::new(),
            entries: Vec::new(),
            filter_text: String::new(),
            selection: Selection::new(),
            list_scroll: 0.0,
            error: None,
            history: Vec::new(),
            history_index: 0,
        };

        let mut a = Self {
            window: None,
            sb_context: None,
            surface: None,
            renderer: None,
            fs,
            tabs: vec![initial_tab],
            active: 0,
            list: VirtualizedList::new(),
            scrollbar: Scrollbar::new(),
            splitter: Splitter::new(SIDEBAR_MIN, SIDEBAR_MAX),
            tabstrip: TabStrip::new(),
            breadcrumb,
            tree,
            focused_pane: FocusedPane::List,
            splitter_x: SIDEBAR_DEFAULT,
            show_hidden: false,
            ant_trail: AntTrail::new(),
            pinned_paths,
            properties_target: None,
            dialog: None,
            search: None,
            preview_visible: false,
            icon_cache: HashMap::new(),
            magic_cache: HashMap::new(),
            event_proxy: None,
            magic_generation: 0,
            drag_watch: None,
            pointer_dips: None,
            modifiers: ModifiersState::empty(),
            tokens: Tokens::for_theme(detect_theme()),
            width: 1,
            height: 1,
            scale_factor: 1.0,
        };
        a.rebuild_tree_sections();
        a.navigate(home);
        a
    }

    /// Rebuild the tree's Recents/Favorites/Locations/Volumes sections
    /// from current state (`ant_trail` for Recents, `pinned_paths` for
    /// Favorites, fixed Mac roots for Locations, `/Volumes/*` for
    /// Volumes). Cheap (~5 path lookups + a few `is_dir` checks); call
    /// after navigate / pin / volume change.
    fn rebuild_tree_sections(&mut self) {
        let home = home_dir();
        let mut sections: Vec<(Section, Vec<(feraille_core::NodeId, String)>)> = Vec::new();

        // 1. Recents (no header) — top folders by ant-trail visits.
        let mut recent_entries = Vec::new();
        let mut recent_labels = Vec::new();
        for id in self.ant_trail.most_visited(5) {
            if let Some(path) = self.fs.path_for(id) {
                if !path.is_dir() {
                    continue;
                }
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                recent_entries.push(id);
                recent_labels.push((id, name));
            }
        }
        if !recent_entries.is_empty() {
            sections.push((
                Section::new(SectionKind::Recents, None, recent_entries),
                recent_labels,
            ));
        }

        // 2. Favorites — pinned paths.
        if !self.pinned_paths.is_empty() {
            let mut entries = Vec::new();
            let mut labels = Vec::new();
            for path in &self.pinned_paths {
                let id = self.fs.id_for_path(path);
                let label = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_else(|| path.to_str().unwrap_or(""))
                    .to_string();
                entries.push(id);
                labels.push((id, label));
            }
            sections.push((
                Section::new(SectionKind::Favorites, Some("FAVORITES"), entries),
                labels,
            ));
        }

        // 3. Locations — iCloud Drive (if present), Home, boot volume, Trash.
        let mut entries = Vec::new();
        let mut labels = Vec::new();
        let icloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
        if icloud.is_dir() {
            let id = self.fs.id_for_path(&icloud);
            entries.push(id);
            labels.push((id, "iCloud Drive".to_string()));
        }
        let home_id = self.fs.id_for_path(&home);
        let home_label = home
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("Home")
            .to_string();
        entries.push(home_id);
        labels.push((home_id, home_label));
        let root = PathBuf::from("/");
        let root_id = self.fs.id_for_path(&root);
        entries.push(root_id);
        // TODO iter-5.x: fetch real volume name via
        // NSURL.resourceValuesForKeys:[NSURLVolumeNameKey]. For now,
        // assume the conventional macOS boot volume label.
        labels.push((root_id, "Macintosh HD".to_string()));
        let trash = home.join(".Trash");
        if trash.is_dir() {
            let id = self.fs.id_for_path(&trash);
            entries.push(id);
            labels.push((id, "Trash".to_string()));
        }
        sections.push((
            Section::new(SectionKind::Locations, Some("LOCATIONS"), entries),
            labels,
        ));

        // 4. Volumes — non-boot mounts under /Volumes.
        let volumes: Vec<(String, PathBuf)> = list_volumes()
            .into_iter()
            .filter(|(label, _)| label != "Macintosh HD")
            .collect();
        if !volumes.is_empty() {
            let mut entries = Vec::new();
            let mut labels = Vec::new();
            for (label, path) in volumes {
                let id = self.fs.id_for_path(&path);
                entries.push(id);
                labels.push((id, label));
            }
            sections.push((
                Section::new(SectionKind::Volumes, Some("VOLUMES"), entries),
                labels,
            ));
        }

        self.tree.set_sections(sections);
    }

    fn viewport_size_dips(&self) -> (f32, f32) {
        (
            (self.width as f32) / self.scale_factor,
            (self.height as f32) / self.scale_factor,
        )
    }

    fn body_top(&self) -> f32 {
        TABSTRIP_H
    }

    fn tabstrip_rect(&self) -> FRect {
        let (w, _) = self.viewport_size_dips();
        FRect::new(0.0, 0.0, w, TABSTRIP_H)
    }

    fn tree_rect(&self) -> FRect {
        let (_, h) = self.viewport_size_dips();
        FRect::new(
            0.0,
            self.body_top(),
            self.splitter_x,
            (h - self.body_top() - STATUS_H).max(0.0),
        )
    }

    fn breadcrumb_rect(&self) -> FRect {
        let (w, _) = self.viewport_size_dips();
        FRect::new(
            self.splitter_x,
            self.body_top(),
            (w - self.splitter_x).max(0.0),
            BREADCRUMB_H,
        )
    }

    fn list_pane_rect(&self) -> FRect {
        let (w, h) = self.viewport_size_dips();
        let preview_w = if self.preview_visible {
            PREVIEW_W.min((w - self.splitter_x).max(0.0) * 0.42)
        } else {
            0.0
        };
        FRect::new(
            self.splitter_x,
            self.body_top() + BREADCRUMB_H,
            (w - self.splitter_x - preview_w).max(0.0),
            (h - self.body_top() - BREADCRUMB_H - STATUS_H).max(0.0),
        )
    }

    fn preview_rect(&self) -> FRect {
        if !self.preview_visible {
            return FRect::new(0.0, 0.0, 0.0, 0.0);
        }
        let (w, h) = self.viewport_size_dips();
        let available = (w - self.splitter_x).max(0.0);
        let preview_w = PREVIEW_W.min(available * 0.42);
        FRect::new(
            w - preview_w,
            self.body_top() + BREADCRUMB_H,
            preview_w,
            (h - self.body_top() - BREADCRUMB_H - STATUS_H).max(0.0),
        )
    }

    fn header_rect(&self) -> FRect {
        let pane = self.list_pane_rect();
        FRect::new(
            pane.left(),
            pane.top(),
            pane.size.width,
            self.list.header_height(),
        )
    }

    fn list_inner_rect(&self) -> FRect {
        let pane = self.list_pane_rect();
        let header_h = self.list.header_height();
        FRect::new(
            pane.left(),
            pane.top() + header_h,
            (pane.size.width - SCROLLBAR_W).max(0.0),
            (pane.size.height - header_h).max(0.0),
        )
    }

    fn scrollbar_rect(&self) -> FRect {
        let pane = self.list_pane_rect();
        let header_h = self.list.header_height();
        FRect::new(
            pane.right() - SCROLLBAR_W,
            pane.top() + header_h,
            SCROLLBAR_W,
            (pane.size.height - header_h).max(0.0),
        )
    }

    fn splitter_container(&self) -> FRect {
        let (_, h) = self.viewport_size_dips();
        FRect::new(
            0.0,
            self.body_top(),
            self.viewport_size_dips().0,
            (h - self.body_top() - STATUS_H).max(0.0),
        )
    }

    fn list_content_height(&self) -> f32 {
        self.tabs[self.active].entries.len() as f32 * self.list.row_height
    }

    fn rebuild_visible_entries(&mut self, cursor_name: Option<String>, preserve_scroll: bool) {
        let key = self.list.sort;
        let scroll = self.list.scroll_offset;
        let tab = &mut self.tabs[self.active];
        tab.entries = filter_entries(&tab.all_entries, &tab.filter_text);
        sort_entries(&mut tab.entries, key);
        tab.selection = Selection::new();
        if let Some(name) = cursor_name {
            if let Some(idx) = tab.entries.iter().position(|e| e.name == name) {
                tab.selection.set_cursor(idx);
            } else if !tab.entries.is_empty() {
                tab.selection.set_cursor(0);
            }
        } else if !tab.entries.is_empty() {
            tab.selection.set_cursor(0);
        }
        self.list.scroll_offset = if preserve_scroll { scroll } else { 0.0 };
    }

    pub fn navigate(&mut self, path: PathBuf) {
        // Truncate forward stack and push, unless we're already on the path.
        {
            let tab = &mut self.tabs[self.active];
            if tab.history_index + 1 < tab.history.len() {
                tab.history.truncate(tab.history_index + 1);
            }
            if tab.history.last() != Some(&path) {
                tab.history.push(path.clone());
                tab.history_index = tab.history.len().saturating_sub(1);
            }
        }
        self.goto_path(path);
    }

    /// Move the active tab's `history_index` backward and re-enumerate.
    pub fn navigate_back(&mut self) {
        let path = {
            let tab = &mut self.tabs[self.active];
            if tab.history_index == 0 {
                return;
            }
            tab.history_index -= 1;
            tab.history[tab.history_index].clone()
        };
        self.goto_path(path);
    }

    /// Move the active tab's `history_index` forward and re-enumerate.
    pub fn navigate_forward(&mut self) {
        let path = {
            let tab = &mut self.tabs[self.active];
            if tab.history_index + 1 >= tab.history.len() {
                return;
            }
            tab.history_index += 1;
            tab.history[tab.history_index].clone()
        };
        self.goto_path(path);
    }

    /// Internal: re-enumerate `path` and update view state. Does NOT
    /// touch the history vector; callers manage that.
    fn goto_path(&mut self, path: PathBuf) {
        let id = self.fs.id_for_path(&path);
        self.ant_trail.record(id);
        // Rebuild sections so Recents reflects the latest visit.
        self.rebuild_tree_sections();
        let mut handle = self.fs.enumerate(id);
        filter_hidden(&mut handle.initial, self.show_hidden);
        let tab = &mut self.tabs[self.active];
        tab.all_entries = handle.initial;
        tab.error = handle.error;
        tab.filter_text.clear();
        tab.current_dir = path.clone();
        tab.list_scroll = 0.0;
        self.rebuild_visible_entries(None, false);
        self.breadcrumb.set_path(&path);
        self.reveal_in_tree(&path);
        self.sync_window_title();
        self.prefetch_icons();
        self.start_magic_prefetch();
    }

    /// Walk the active tab's entries, fetch+cache any extensions we
    /// haven't seen before. Synchronous on the navigate path; ~1ms per
    /// new extension on a warm Launch Services cache. Iter-5 will move
    /// to a worker.
    fn prefetch_icons(&mut self) {
        let icon_size_px = (16.0 * self.scale_factor).round().max(16.0) as u32;
        let cur_dir = self.tabs[self.active].current_dir.clone();
        let to_fetch: Vec<(String, PathBuf)> = self.tabs[self.active]
            .entries
            .iter()
            .filter_map(|e| {
                let key = cache_key_for(e);
                if self.icon_cache.contains_key(&key) {
                    None
                } else {
                    Some((key, cur_dir.join(&e.name)))
                }
            })
            .collect();
        for (key, path) in to_fetch {
            if self.icon_cache.contains_key(&key) {
                continue;
            }
            if let Some((rgba, w, h)) = fetch_icon_rgba(&path, icon_size_px) {
                self.icon_cache.insert(key, Bitmap::new(w, h, rgba));
            }
        }
    }

    fn cursor_entry_name(&self) -> Option<String> {
        let tab = &self.tabs[self.active];
        tab.selection
            .cursor()
            .and_then(|i| tab.entries.get(i))
            .map(|e| e.name.clone())
    }

    fn start_magic_prefetch(&mut self) {
        const MAGIC_PREFETCH_CAP: usize = 200;

        let cur_dir = self.tabs[self.active].current_dir.clone();
        let cursor_name = self.cursor_entry_name();
        let scroll = self.list.scroll_offset;
        let mut changed = false;

        for entry in self.tabs[self.active].all_entries.iter_mut() {
            if !matches!(entry.kind, EntryKind::File) || !entry.display_magic.is_empty() {
                continue;
            }
            let key = (cur_dir.join(&entry.name), entry.mtime_unix);
            if let Some(cached) = self.magic_cache.get(&key) {
                entry.display_magic = cached.clone();
                changed = true;
            }
        }

        if changed {
            self.rebuild_visible_entries(cursor_name, true);
            self.list.scroll_offset = scroll;
        }

        let Some(proxy) = self.event_proxy.clone() else {
            return;
        };

        self.magic_generation = self.magic_generation.wrapping_add(1);
        let generation = self.magic_generation;
        let candidates: Vec<(String, i64, PathBuf)> = self.tabs[self.active]
            .all_entries
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::File))
            .filter(|entry| entry.display_magic.is_empty())
            .filter_map(|entry| {
                let path = cur_dir.join(&entry.name);
                if self.magic_cache.contains_key(&(path.clone(), entry.mtime_unix)) {
                    None
                } else {
                    Some((entry.name.clone(), entry.mtime_unix, path))
                }
            })
            .take(MAGIC_PREFETCH_CAP)
            .collect();

        if candidates.is_empty() {
            return;
        }

        std::thread::spawn(move || {
            let results = candidates
                .into_iter()
                .map(|(name, mtime_unix, path)| MagicResult {
                    name,
                    mtime_unix,
                    label: detect_magic(&path).unwrap_or_default().to_string(),
                })
                .collect();
            let _ = proxy.send_event(AppEvent::MagicBatch {
                generation,
                dir: cur_dir,
                results,
            });
        });
    }

    fn sync_window_title(&self) {
        let Some(w) = &self.window else { return };
        let path = &self.tabs[self.active].current_dir;
        let label = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| path.to_str().unwrap_or("Feraille"));
        w.set_title(&format!("{} \u{2014} Feraille", label));
    }

    /// Re-enumerate the active tab without resetting scroll, preserving
    /// the cursor on the same entry name when possible. Used by F5 and
    /// after side-effects (Trash, future copy/move).
    #[allow(clippy::collapsible_else_if)]
    pub fn refresh_active_tab(&mut self) {
        let cursor_name = {
            let t = &self.tabs[self.active];
            t.selection
                .cursor()
                .and_then(|i| t.entries.get(i))
                .map(|e| e.name.clone())
        };
        let scroll = self.list.scroll_offset;
        let path = self.tabs[self.active].current_dir.clone();
        let id = self.fs.id_for_path(&path);
        let mut handle = self.fs.enumerate(id);
        filter_hidden(&mut handle.initial, self.show_hidden);
        let tab = &mut self.tabs[self.active];
        tab.all_entries = handle.initial;
        self.rebuild_visible_entries(cursor_name, true);
        self.list.scroll_offset = scroll;
        // Tree might have a stale view of the current folder's contents.
        // Mark unloaded so a future expand re-enumerates.
        self.tree.invalidate(id);
        self.start_magic_prefetch();
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.refresh_active_tab();
    }

    pub fn cycle_sort(&mut self, column: feraille_controls::ColumnId) {
        self.list.toggle_sort(column);
        self.rebuild_visible_entries(None, false);
    }

    pub fn delete_at_cursor_to_trash(&mut self) {
        let (cur_dir, name) = {
            let t = &self.tabs[self.active];
            let Some(idx) = t.selection.cursor() else {
                return;
            };
            let Some(entry) = t.entries.get(idx) else {
                return;
            };
            (t.current_dir.clone(), entry.name.clone())
        };
        let target = cur_dir.join(&name);
        match move_to_trash(&target) {
            Ok(_) => self.refresh_active_tab(),
            Err(e) => {
                // Iter-4 will surface this in a Toast / ErrorState; for now,
                // log to stderr so it's visible during dev runs.
                eprintln!(
                    "move_to_trash({}) failed: {e} — file remains on disk",
                    target.display()
                );
            }
        }
    }

    /// Walk the tree from the appropriate root down to `path`, expanding
    /// each ancestor. Children are only re-enumerated for ancestors that
    /// haven't been loaded yet — this is the perf fix that keeps tree
    /// re-reveals (e.g. clicking a folder a second time) instant. Auto-
    /// scrolls the tree to keep the target visible.
    pub fn reveal_in_tree(&mut self, path: &Path) {
        let home = home_dir();
        let mut current = if path.starts_with(&home) {
            home
        } else if path.starts_with("/Volumes") {
            let mut comps = path.components();
            let mut acc = PathBuf::new();
            for _ in 0..3 {
                if let Some(c) = comps.next() {
                    acc.push(c.as_os_str());
                } else {
                    break;
                }
            }
            acc
        } else {
            self.tree.select(self.fs.id_for_path(path));
            return;
        };
        loop {
            let id = self.fs.id_for_path(&current);
            if current != path {
                if self.tree.is_loaded(id) {
                    // Cached — just mark expanded; don't touch the FS.
                    self.tree.ensure_expanded(id);
                } else {
                    let mut handle = self.fs.enumerate(id);
                    filter_hidden(&mut handle.initial, self.show_hidden);
                    self.tree.populate_children(id, &handle.initial);
                }
            }
            if current == path {
                self.tree.select(id);
                let viewport_h = self.tree_rect().size.height;
                self.tree.ensure_visible(id, viewport_h);
                break;
            }
            let rel = match path.strip_prefix(&current) {
                Ok(r) => r,
                Err(_) => {
                    self.tree.select(id);
                    break;
                }
            };
            let next = match rel.components().next() {
                Some(c) => c,
                None => {
                    self.tree.select(id);
                    break;
                }
            };
            current.push(next.as_os_str());
            if !current.is_dir() {
                break;
            }
        }
    }

    fn open_at_cursor(&mut self) {
        let (cur_dir, kind, name) = {
            let t = &self.tabs[self.active];
            let Some(idx) = t.selection.cursor() else {
                return;
            };
            let Some(entry) = t.entries.get(idx) else {
                return;
            };
            (t.current_dir.clone(), entry.kind, entry.name.clone())
        };
        let path = cur_dir.join(&name);
        match kind {
            EntryKind::Directory => self.navigate(path),
            EntryKind::File | EntryKind::Symlink => {
                if let Err(e) = open_with_default(&path) {
                    eprintln!("open_with_default({}) failed: {e}", path.display());
                }
            }
        }
    }

    fn navigate_parent(&mut self) {
        let parent = self.tabs[self.active]
            .current_dir
            .parent()
            .map(Path::to_path_buf);
        if let Some(p) = parent {
            self.navigate(p);
        }
    }

    fn switch_tab(&mut self, new: usize) {
        if new == self.active || new >= self.tabs.len() {
            return;
        }
        // Save list scroll to outgoing tab.
        self.tabs[self.active].list_scroll = self.list.scroll_offset;
        self.active = new;
        self.list.scroll_offset = self.tabs[new].list_scroll;
        let path = self.tabs[new].current_dir.clone();
        self.breadcrumb.set_path(&path);
        let id = self.fs.id_for_path(&path);
        self.tree.select(id);
        self.sync_window_title();
    }

    fn new_tab(&mut self) {
        let path = self.tabs[self.active].current_dir.clone();
        self.tabs[self.active].list_scroll = self.list.scroll_offset;
        let new_index = self.tabs.len();
        self.tabs.push(Tab {
            current_dir: path.clone(),
            all_entries: Vec::new(),
            entries: Vec::new(),
            filter_text: String::new(),
            selection: Selection::new(),
            list_scroll: 0.0,
            error: None,
            history: Vec::new(),
            history_index: 0,
        });
        self.active = new_index;
        self.list.scroll_offset = 0.0;
        self.navigate(path);
    }

    fn close_tab(&mut self, idx: usize) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.tabs.remove(idx);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if idx < self.active {
            self.active -= 1;
        }
        self.list.scroll_offset = self.tabs[self.active].list_scroll;
        let path = self.tabs[self.active].current_dir.clone();
        self.breadcrumb.set_path(&path);
        let id = self.fs.id_for_path(&path);
        self.tree.select(id);
    }

    fn handle_tree_event(&mut self, ev: TreeEvent) {
        match ev {
            TreeEvent::Activate(id) => {
                if let Some(path) = self.fs.path_for(id) {
                    self.navigate(path);
                }
            }
            TreeEvent::ExpandRequested(id) => {
                let mut handle = self.fs.enumerate(id);
                filter_hidden(&mut handle.initial, self.show_hidden);
                self.tree.populate_children(id, &handle.initial);
            }
        }
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Pure paint into a renderer. No window/surface dependency. Used by
    /// both the GUI present path and the headless screenshot path.
    pub fn paint_to(&mut self, renderer: &mut dyn Renderer) {
        let tabstrip_rect = self.tabstrip_rect();
        let tree_rect = self.tree_rect();
        let breadcrumb_rect = self.breadcrumb_rect();
        let header_rect = self.header_rect();
        let list_inner = self.list_inner_rect();
        let scrollbar_rect = self.scrollbar_rect();
        let preview_rect = self.preview_rect();
        let splitter_container = self.splitter_container();
        let content_h = self.list_content_height();
        let status_text = format_status(&self.tabs[self.active]);
        let tab_infos: Vec<TabInfo> = self
            .tabs
            .iter()
            .map(|t| TabInfo { label: t.label() })
            .collect();
        let active = self.active;
        let splitter_x = self.splitter_x;
        let viewport = renderer.viewport();
        let tokens = &self.tokens;

        // Window bg
        renderer.fill_rect(
            FRect::new(0.0, 0.0, viewport.width, viewport.height),
            tokens.bg.base,
        );

        // Tabstrip — topmost element (the OS title bar sits above us).
        self.tabstrip
            .paint(tabstrip_rect, &tab_infos, active, tokens, renderer);

        // Tree pane — paint with ant-trail heat overlay + cached folder icon.
        let trail = &self.ant_trail;
        let dir_icon = self.icon_cache.get("DIR");
        self.tree
            .paint(tree_rect, tokens, renderer, |id| trail.heat(id), dir_icon);

        // Breadcrumb
        self.breadcrumb.paint(breadcrumb_rect, tokens, renderer);

        // Column header (above list, below breadcrumb)
        self.list.paint_header(header_rect, tokens, renderer);

        // List + scrollbar
        let tab = &self.tabs[self.active];
        let icon_cache = &self.icon_cache;
        self.list.paint(
            list_inner,
            &tab.entries,
            &tab.selection,
            |entry| icon_cache.get(&cache_key_for(entry)),
            tokens,
            renderer,
        );
        self.scrollbar.paint(
            scrollbar_rect,
            content_h,
            list_inner.size.height,
            self.list.scroll_offset,
            tokens,
            renderer,
        );
        // If enumeration failed (TCC permission denied / not found / I/O),
        // overlay a centered explanation panel so the user understands
        // why the list is empty.
        if let Some(err) = self.tabs[self.active].error.clone() {
            paint_empty_state(list_inner, &err, &self.tabs[self.active].current_dir, tokens, renderer);
        }
        if self.preview_visible {
            self.paint_preview_pane(preview_rect, tokens, renderer);
        }

        // Splitter
        self.splitter
            .paint(splitter_x, splitter_container, tokens, renderer);

        // Properties panel — overlay over everything else when open.
        if self.properties_target.is_some() {
            self.paint_properties(tokens, viewport, renderer);
        }
        // Search/filter overlay.
        if self.search.is_some() {
            self.paint_search(tokens, viewport, renderer);
        }
        // Modal text dialog (rename / new-folder) above everything.
        if self.dialog.is_some() {
            self.paint_dialog(tokens, viewport, renderer);
        }

        // Status
        let status = FRect::new(0.0, viewport.height - STATUS_H, viewport.width, STATUS_H);
        renderer.fill_rect(status, tokens.bg.layer2);
        renderer.fill_rect(
            FRect::new(0.0, viewport.height - STATUS_H, viewport.width, 1.0),
            tokens.border.subtle,
        );
        renderer.draw_text(
            FPoint::new(
                tokens.space.md,
                viewport.height - STATUS_H + (STATUS_H - tokens.text.xs) / 2.0 - 1.0,
            ),
            &status_text,
            TextStyle {
                size: tokens.text.xs,
                weight: FontWeight::Regular,
                color: tokens.fg.secondary,
            },
        );
    }

    fn render(&mut self) {
        // Take the renderer + surface out of self so we can borrow self mutably
        // during paint without aliasing.
        let Some(mut renderer) = self.renderer.take() else {
            return;
        };
        let Some(mut surface) = self.surface.take() else {
            self.renderer = Some(renderer);
            return;
        };
        let Some(w_nz) = NonZeroU32::new(self.width) else {
            self.renderer = Some(renderer);
            self.surface = Some(surface);
            return;
        };
        let Some(h_nz) = NonZeroU32::new(self.height) else {
            self.renderer = Some(renderer);
            self.surface = Some(surface);
            return;
        };
        if surface.resize(w_nz, h_nz).is_err() {
            self.renderer = Some(renderer);
            self.surface = Some(surface);
            return;
        }
        self.paint_to(&mut renderer);
        let pixels = renderer.pixels();
        if let Ok(mut buffer) = surface.buffer_mut() {
            for (dst, src) in buffer.iter_mut().zip(pixels.iter()) {
                *dst = *src & 0x00FF_FFFF;
            }
            let _ = buffer.present();
        }
        self.renderer = Some(renderer);
        self.surface = Some(surface);
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("Feraille")
            .with_inner_size(LogicalSize::new(1180.0, 760.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                eprintln!("create_window: {e}");
                event_loop.exit();
                return;
            }
        };
        let context = match softbuffer::Context::new(window.clone()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Context: {e}");
                event_loop.exit();
                return;
            }
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Surface: {e}");
                event_loop.exit();
                return;
            }
        };
        // Apply native chrome (transparent titlebar + traffic-light handling
        // on macOS; no-op elsewhere). Returned value is the leading-edge
        // inset to reserve in the tabstrip.
        self.tabstrip.inset_left = feraille_shell_mac::apply_native_chrome(&window);

        let scale = window.scale_factor() as f32;
        let size = window.inner_size();
        self.width = size.width.max(1);
        self.height = size.height.max(1);
        self.scale_factor = scale;
        let font_bytes = match load_default_font() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("font: {e:#}");
                event_loop.exit();
                return;
            }
        };
        self.window = Some(window);
        self.sb_context = Some(context);
        self.surface = Some(surface);
        self.renderer = Some(SoftRenderer::new(
            self.width,
            self.height,
            scale,
            font_bytes,
        ));
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::MagicBatch {
                generation,
                dir,
                results,
            } => {
                if generation != self.magic_generation
                    || self.tabs[self.active].current_dir != dir
                {
                    return;
                }

                let cursor_name = self.cursor_entry_name();
                let scroll = self.list.scroll_offset;
                let tab = &mut self.tabs[self.active];
                let mut changed = false;
                for result in results {
                    let key = (dir.join(&result.name), result.mtime_unix);
                    self.magic_cache.insert(key, result.label.clone());
                    if let Some(entry) = tab
                        .all_entries
                        .iter_mut()
                        .find(|entry| {
                            entry.name == result.name && entry.mtime_unix == result.mtime_unix
                        })
                    {
                        if entry.display_magic != result.label {
                            entry.display_magic = result.label;
                            changed = true;
                        }
                    }
                }

                if changed {
                    self.rebuild_visible_entries(cursor_name, true);
                    self.list.scroll_offset = scroll;
                    self.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(PhysicalSize { width, height }) => {
                self.width = width.max(1);
                self.height = height.max(1);
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(self.width, self.height);
                }
                self.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale_factor = scale_factor as f32;
                if let Some(r) = self.renderer.as_mut() {
                    r.set_scale_factor(self.scale_factor);
                }
                self.request_redraw();
            }
            WindowEvent::CursorMoved {
                position: PhysicalPosition { x, y },
                ..
            } => {
                let p = FPoint::new(
                    (x as f32) / self.scale_factor,
                    (y as f32) / self.scale_factor,
                );
                self.pointer_dips = Some(p);
                let mut redraw = false;

                // Drag-out: promote the watch into a system drag once
                // the cursor has moved past the threshold and the delay
                // has elapsed. Once kicked, the OS owns the drag visual
                // until the user drops or cancels.
                if let Some(watch) = self.drag_watch {
                    let dx = p.x - watch.start.x;
                    let dy = p.y - watch.start.y;
                    let dist = (dx * dx + dy * dy).sqrt();
                    if dist > DRAG_THRESHOLD_DIPS
                        && watch.when.elapsed().as_millis() > DRAG_DELAY_MS
                    {
                        self.drag_watch = None;
                        if let Some(entry) = self.tabs[self.active].entries.get(watch.row).cloned()
                        {
                            let path = self.tabs[self.active].current_dir.join(&entry.name);
                            if let Some(window) = &self.window {
                                let _ = feraille_shell_mac::begin_drag(window, &[path.as_path()]);
                            }
                        }
                    }
                }

                if self.scrollbar.is_dragging() {
                    if let Some(off) = self.scrollbar.scroll_offset_for_drag(
                        self.scrollbar_rect(),
                        p.y,
                        self.list_content_height(),
                        self.list_inner_rect().size.height,
                    ) {
                        self.list.scroll_offset = off;
                        redraw = true;
                    }
                } else if self.splitter.is_dragging() {
                    if let Some(pos) = self.splitter.position_for_drag(p) {
                        self.splitter_x = pos;
                        redraw = true;
                    }
                } else {
                    if self.tabstrip.update_hover(
                        self.tabstrip_rect(),
                        &self
                            .tabs
                            .iter()
                            .map(|t| TabInfo { label: t.label() })
                            .collect::<Vec<_>>(),
                        Some(p),
                    ) {
                        redraw = true;
                    }
                    if self
                        .breadcrumb
                        .update_hover(self.breadcrumb_rect(), Some(p))
                    {
                        redraw = true;
                    }
                    if self.tree.update_hover(self.tree_rect(), Some(p)) {
                        redraw = true;
                    }
                    let count = self.tabs[self.active].entries.len();
                    if self
                        .list
                        .update_hover(self.list_inner_rect(), Some(p), count)
                    {
                        redraw = true;
                    }
                    if self.list.update_header_hover(self.header_rect(), Some(p)) {
                        redraw = true;
                    }
                }
                if redraw {
                    self.request_redraw();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer_dips = None;
                self.breadcrumb.update_hover(self.breadcrumb_rect(), None);
                self.tree.update_hover(self.tree_rect(), None);
                let count = self.tabs[self.active].entries.len();
                self.list.update_hover(self.list_inner_rect(), None, count);
                self.list.update_header_hover(self.header_rect(), None);
                self.request_redraw();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(p) = self.pointer_dips else { return };

                // Properties panel — when open, any click closes it.
                // (Iter-3.10 can refine to "click-outside-only" if useful.)
                if self.properties_target.is_some() {
                    let _ = p;
                    self.close_properties();
                    self.request_redraw();
                    return;
                }

                // Splitter (highest priority — narrow but layered above).
                if self
                    .splitter
                    .begin_drag_at(self.splitter_x, self.splitter_container(), p)
                {
                    self.request_redraw();
                    return;
                }
                // Column header — toggle sort.
                if let Some(id) = self.list.header_column_at(self.header_rect(), p) {
                    self.cycle_sort(id);
                    self.request_redraw();
                    return;
                }
                // Scrollbar thumb.
                let inner = self.list_inner_rect();
                if self.scrollbar.begin_drag_at(
                    self.scrollbar_rect(),
                    p,
                    self.list_content_height(),
                    inner.size.height,
                    self.list.scroll_offset,
                ) {
                    self.request_redraw();
                    return;
                }
                // Tabstrip.
                let tab_infos: Vec<TabInfo> = self
                    .tabs
                    .iter()
                    .map(|t| TabInfo { label: t.label() })
                    .collect();
                if let Some(ev) = self.tabstrip.click(self.tabstrip_rect(), &tab_infos, p) {
                    match ev {
                        TabStripEvent::Activate(i) => self.switch_tab(i),
                        TabStripEvent::Close(i) => self.close_tab(i),
                        TabStripEvent::New => self.new_tab(),
                    }
                    self.request_redraw();
                    return;
                }
                // Breadcrumb.
                if let Some(BreadcrumbEvent::Navigate(path)) =
                    self.breadcrumb.click(self.breadcrumb_rect(), p)
                {
                    self.navigate(path);
                    self.request_redraw();
                    return;
                }
                // Tree — `Some(events)` even if empty signals "I handled
                // this click; redraw" (e.g. fold of cached expanded folder
                // mutates state but emits no event). `None` = missed.
                if let Some(tree_events) = self.tree.click(self.tree_rect(), p) {
                    self.set_focused_pane(FocusedPane::Tree);
                    for ev in tree_events {
                        self.handle_tree_event(ev);
                    }
                    self.request_redraw();
                    return;
                }
                // List click.
                if let Some(idx) =
                    self.list
                        .index_at(inner, p, self.tabs[self.active].entries.len())
                {
                    self.set_focused_pane(FocusedPane::List);
                    self.tabs[self.active].selection.set_cursor(idx);
                    // Arm drag-out: a small motion + delay after mouse-down
                    // on a row promotes to a system drag.
                    self.drag_watch = Some(DragWatch {
                        start: p,
                        when: std::time::Instant::now(),
                        row: idx,
                    });
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let Some(p) = self.pointer_dips else { return };
                if self.tree_rect().contains(p) {
                    self.show_tree_context_menu_at(p);
                } else {
                    self.show_context_menu_at(p);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.scrollbar.is_dragging() {
                    self.scrollbar.end_drag();
                    self.request_redraw();
                }
                if self.splitter.is_dragging() {
                    self.splitter.end_drag();
                    self.request_redraw();
                }
                self.drag_watch = None;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // PixelDelta arrives from trackpad / high-precision wheels in
                // logical pixels — pass through. LineDelta is coarse "lines"
                // from a notched wheel; multiply by ~2 rows. Sign: positive
                // delta from winit means scroll forward / content-up; we
                // invert because `scroll_offset` is "DIPs below origin",
                // increasing as content moves up.
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * 56.0,
                    MouseScrollDelta::PixelDelta(p) => -(p.y as f32),
                };
                // Route to whichever pane the pointer is over. macOS Finder
                // does the same — scrolling over the sidebar scrolls the
                // sidebar, not the file pane. If we don't know where the
                // pointer is (e.g. the trackpad fired with no prior
                // CursorMoved), fall back to the file list.
                let target = match self.pointer_dips {
                    Some(p) if self.tree_rect().contains(p) => ScrollTarget::Tree,
                    Some(p) if self.list_pane_rect().contains(p) => ScrollTarget::List,
                    Some(_) => ScrollTarget::None,
                    None => ScrollTarget::List,
                };
                match target {
                    ScrollTarget::Tree => {
                        let viewport_h = self.tree_rect().size.height;
                        self.tree.scroll_by(dy, viewport_h);
                        self.request_redraw();
                    }
                    ScrollTarget::List => {
                        let count = self.tabs[self.active].entries.len();
                        let viewport_h = self.list_inner_rect().size.height;
                        self.list.scroll_by(dy, count, viewport_h);
                        self.request_redraw();
                    }
                    ScrollTarget::None => {}
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        text,
                        ..
                    },
                ..
            } => {
                // Modal text dialog — route all input there, swallow others.
                if let Some(d) = self.dialog.as_mut() {
                    if let Key::Named(named) = &logical_key {
                        if let Some(tk) = map_named_to_textinput(*named) {
                            match d.input.handle_key(tk) {
                                Some(TextInputEvent::Submit(_)) => self.submit_dialog(),
                                Some(TextInputEvent::Cancel) => self.close_dialog(),
                                None => {}
                            }
                            self.request_redraw();
                            return;
                        }
                    }
                    if let Some(t) = text.as_deref() {
                        d.input.handle_text(t);
                        self.request_redraw();
                    }
                    return;
                }

                // Search/filter dialog — update visible rows live as text changes.
                if self.search.is_some() {
                    let mut close = false;
                    let mut new_value: Option<String> = None;
                    if let Some(input) = self.search.as_mut() {
                        if let Key::Named(named) = &logical_key {
                            if let Some(tk) = map_named_to_textinput(*named) {
                                match input.handle_key(tk) {
                                    Some(TextInputEvent::Submit(s)) => {
                                        new_value = Some(s);
                                        close = true;
                                    }
                                    Some(TextInputEvent::Cancel) => close = true,
                                    None => new_value = Some(input.value()),
                                }
                            }
                        } else if let Some(t) = text.as_deref() {
                            input.handle_text(t);
                            new_value = Some(input.value());
                        }
                    }
                    if let Some(value) = new_value {
                        self.set_filter_text(value);
                    }
                    if close {
                        self.search = None;
                    }
                    self.request_redraw();
                    return;
                }

                // If the breadcrumb is in edit mode, route keyboard there.
                if self.breadcrumb.is_editing() {
                    if let Key::Named(named) = &logical_key {
                        if let Some(tk) = map_named_to_textinput(*named) {
                            if let Some(BreadcrumbEvent::Navigate(path)) =
                                self.breadcrumb.handle_key(tk)
                            {
                                self.navigate(path);
                            }
                            self.request_redraw();
                            return;
                        }
                    }
                    if let Some(t) = text.as_deref() {
                        self.breadcrumb.handle_text(t);
                        self.request_redraw();
                    }
                    return;
                }

                // Ctrl+L / Cmd+L → enter breadcrumb edit mode.
                if let Some(t) = text.as_deref() {
                    let mod_held = self.modifiers.control_key() || self.modifiers.super_key();
                    if (t == "l" || t == "L") && mod_held {
                        let path = self.tabs[self.active].current_dir.clone();
                        self.breadcrumb.enter_edit_mode(&path);
                        self.request_redraw();
                        return;
                    }
                    if (t == "f" || t == "F") && mod_held {
                        self.open_search();
                        self.request_redraw();
                        return;
                    }
                    if (t == "p" || t == "P") && mod_held {
                        self.toggle_preview();
                        self.request_redraw();
                        return;
                    }
                }

                // Hidden-files toggle:
                //  - Ctrl+H (Linux/Windows convention)
                //  - Cmd+Shift+. (macOS convention used by Finder)
                if let Some(t) = text.as_deref() {
                    let ctrl_h = (t == "h" || t == "H") && self.modifiers.control_key();
                    let cmd_shift_dot =
                        t == "." && self.modifiers.super_key() && self.modifiers.shift_key();
                    if ctrl_h || cmd_shift_dot {
                        self.toggle_hidden();
                        self.request_redraw();
                        return;
                    }
                    // Cmd+I (macOS Finder) / Ctrl+I → file properties panel.
                    let mod_held = self.modifiers.super_key() || self.modifiers.control_key();
                    if (t == "i" || t == "I") && mod_held {
                        self.toggle_properties();
                        self.request_redraw();
                        return;
                    }
                    // Cmd+[ / Cmd+] → back / forward (Finder convention).
                    if mod_held && t == "[" {
                        self.navigate_back();
                        self.request_redraw();
                        return;
                    }
                    if mod_held && t == "]" {
                        self.navigate_forward();
                        self.request_redraw();
                        return;
                    }
                    // Cmd+Shift+C / Ctrl+Shift+C → copy cursor path.
                    if mod_held && self.modifiers.shift_key() && (t == "C" || t == "c") {
                        self.copy_cursor_path();
                        return;
                    }
                    // Cmd+Opt+R / Ctrl+Alt+R → reveal in Finder.
                    if mod_held
                        && self.modifiers.alt_key()
                        && (t == "r" || t == "R" || t.is_empty())
                    {
                        self.reveal_cursor_in_finder();
                        return;
                    }
                    // Cmd+Shift+N / Ctrl+Shift+N → new folder dialog.
                    if mod_held && self.modifiers.shift_key() && (t == "N" || t == "n") {
                        self.open_new_folder();
                        self.request_redraw();
                        return;
                    }
                }

                // F6 cycles focus between Tree and List regardless of
                // which pane currently owns focus.
                if matches!(logical_key, Key::Named(NamedKey::F6)) {
                    self.cycle_focus();
                    self.request_redraw();
                    return;
                }

                // Tree-pane keyboard routing: when the tree owns focus,
                // arrow keys / Home / End / Enter / type-ahead drive the
                // tree, not the list.
                if matches!(self.focused_pane, FocusedPane::Tree) {
                    let tree_h = self.tree_rect().size.height;
                    let mut redraw = false;
                    let mut tree_event: Option<TreeEvent> = None;
                    match &logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.set_focused_pane(FocusedPane::List);
                            redraw = true;
                        }
                        Key::Named(NamedKey::ArrowDown) => {
                            redraw |= self.tree.move_cursor(1, tree_h);
                        }
                        Key::Named(NamedKey::ArrowUp) => {
                            redraw |= self.tree.move_cursor(-1, tree_h);
                        }
                        Key::Named(NamedKey::Home) => {
                            redraw |= self.tree.move_to_first(tree_h);
                        }
                        Key::Named(NamedKey::End) => {
                            redraw |= self.tree.move_to_last(tree_h);
                        }
                        Key::Named(NamedKey::ArrowLeft) => {
                            redraw |= self.tree.collapse_or_parent(tree_h);
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            tree_event = self.tree.expand_or_first_child(tree_h);
                            redraw = true;
                        }
                        Key::Named(NamedKey::Enter) => {
                            tree_event = self.tree.activate_selected();
                            redraw = true;
                        }
                        _ => {
                            // Type-ahead: a single printable character with
                            // no Cmd/Ctrl/Alt held.
                            let mods = self.modifiers;
                            if !mods.super_key() && !mods.control_key() && !mods.alt_key() {
                                if let Some(t) = text.as_deref() {
                                    if let Some(ch) = t.chars().next() {
                                        if !ch.is_control() {
                                            redraw |= self.tree.type_ahead_push(ch, tree_h);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(ev) = tree_event {
                        self.handle_tree_event(ev);
                    }
                    if redraw {
                        self.request_redraw();
                    }
                    return;
                }

                let count = self.tabs[self.active].entries.len();
                let viewport_h = self.list_inner_rect().size.height;
                let page = (viewport_h / self.list.row_height) as i64;
                let sel = &mut self.tabs[self.active].selection;
                match logical_key {
                    Key::Named(NamedKey::Escape) => {
                        if self.properties_target.is_some() {
                            self.close_properties();
                        } else {
                            event_loop.exit();
                        }
                    }
                    // Alt+Left / Alt+Right → back / forward (alternative
                    // to Cmd+[/]).
                    Key::Named(NamedKey::ArrowLeft) if self.modifiers.alt_key() => {
                        self.navigate_back();
                    }
                    Key::Named(NamedKey::ArrowRight) if self.modifiers.alt_key() => {
                        self.navigate_forward();
                    }
                    Key::Named(NamedKey::ArrowDown) => sel.move_cursor(1, count),
                    Key::Named(NamedKey::ArrowUp) => sel.move_cursor(-1, count),
                    Key::Named(NamedKey::PageDown) => sel.move_cursor(page, count),
                    Key::Named(NamedKey::PageUp) => sel.move_cursor(-page, count),
                    Key::Named(NamedKey::Home) => sel.move_cursor(-(count as i64), count),
                    Key::Named(NamedKey::End) => sel.move_cursor(count as i64, count),
                    Key::Named(NamedKey::Enter) => self.open_at_cursor(),
                    Key::Named(NamedKey::Backspace) => self.navigate_parent(),
                    Key::Named(NamedKey::F5) => self.refresh_active_tab(),
                    Key::Named(NamedKey::Delete) => self.delete_at_cursor_to_trash(),
                    Key::Named(NamedKey::F2) => self.open_rename(),
                    _ => {}
                }
                if let Some(idx) = self.tabs[self.active].selection.cursor() {
                    self.list.ensure_visible(idx, viewport_h);
                }
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }
}

fn map_named_to_textinput(named: NamedKey) -> Option<TextInputKey> {
    Some(match named {
        NamedKey::Backspace => TextInputKey::Backspace,
        NamedKey::Delete => TextInputKey::Delete,
        NamedKey::ArrowLeft => TextInputKey::ArrowLeft,
        NamedKey::ArrowRight => TextInputKey::ArrowRight,
        NamedKey::Home => TextInputKey::Home,
        NamedKey::End => TextInputKey::End,
        NamedKey::Enter => TextInputKey::Enter,
        NamedKey::Escape => TextInputKey::Escape,
        _ => return None,
    })
}

#[derive(Clone, Copy)]
enum ScrollTarget {
    Tree,
    List,
    None,
}

fn format_iso_date(unix: i64) -> String {
    // Reuse the same approach as feraille-fs-native's humanize: derive
    // Y/M/D from days-since-epoch via Howard Hinnant's algorithm.
    let secs_in_day = unix.rem_euclid(86_400);
    let h = (secs_in_day / 3600) as u32;
    let m = ((secs_in_day % 3600) / 60) as u32;
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02} UTC")
}

fn cache_key_for(entry: &FileEntry) -> String {
    match entry.kind {
        EntryKind::Directory => "DIR".to_string(),
        EntryKind::Symlink => "SYMLINK".to_string(),
        EntryKind::File => match entry.name.rsplit_once('.') {
            Some((_, ext)) if !ext.is_empty() => format!(".{}", ext.to_lowercase()),
            _ => "FILE".to_string(),
        },
    }
}

fn filter_hidden(entries: &mut Vec<FileEntry>, show_hidden: bool) {
    if !show_hidden {
        entries.retain(|e| !e.name.starts_with('.'));
    }
}

fn filter_entries(entries: &[FileEntry], filter_text: &str) -> Vec<FileEntry> {
    let query = filter_text.trim().to_lowercase();
    if query.is_empty() {
        return entries.to_vec();
    }
    entries
        .iter()
        .filter(|e| {
            e.name.to_lowercase().contains(&query)
                || e.display_kind.to_lowercase().contains(&query)
                || e.display_magic.to_lowercase().contains(&query)
        })
        .cloned()
        .collect()
}

/// Centered message in the file pane when `enumerate` failed. Most
/// commonly TCC permission-denied for ~/Documents, ~/Desktop, or
/// ~/Downloads when a sandboxed launcher hasn't been granted access.
fn paint_empty_state(
    rect: FRect,
    error: &EnumerationError,
    path: &Path,
    tokens: &Tokens,
    painter: &mut dyn Renderer,
) {
    if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
        return;
    }
    let (heading, body) = match error {
        EnumerationError::PermissionDenied => (
            "macOS denied access to this folder",
            format!(
                "Grant access in System Settings \u{2192} Privacy & Security \u{2192} Files and Folders, \
or run Feraille from outside a sandboxed launcher.\n\nPath: {}",
                path.display()
            ),
        ),
        EnumerationError::NotFound => (
            "This folder is gone",
            format!("It may have been moved or deleted.\n\nPath: {}", path.display()),
        ),
        EnumerationError::Other(msg) => (
            "Couldn't read this folder",
            format!("{msg}\n\nPath: {}", path.display()),
        ),
    };

    let head_style = TextStyle {
        size: tokens.text.lg,
        weight: FontWeight::SemiBold,
        color: tokens.fg.primary,
    };
    let body_style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::Regular,
        color: tokens.fg.secondary,
    };
    // Wrap body text by hand on each '\n'; further visual wrapping is
    // unnecessary at typical pane widths.
    let body_lines: Vec<&str> = body.split('\n').collect();
    let line_h = tokens.text.sm + 6.0;
    let block_h = tokens.text.lg + 12.0 + body_lines.len() as f32 * line_h;
    let center_y = rect.top() + (rect.size.height - block_h).max(0.0) / 2.0;

    let head_w = painter.measure_text(heading, head_style).width;
    let head_x = rect.left() + (rect.size.width - head_w).max(0.0) / 2.0;
    painter.draw_text(FPoint::new(head_x, center_y), heading, head_style);

    let mut y = center_y + tokens.text.lg + 12.0;
    for line in &body_lines {
        let w = painter.measure_text(line, body_style).width;
        let x = rect.left() + (rect.size.width - w).max(0.0) / 2.0;
        painter.draw_text(FPoint::new(x, y), line, body_style);
        y += line_h;
    }
}

fn detect_theme() -> Theme {
    if std::env::var_os("FERAILLE_THEME").as_deref() == Some(std::ffi::OsStr::new("dark")) {
        Theme::Dark
    } else {
        Theme::Light
    }
}

fn format_status(tab: &Tab) -> String {
    let count = tab.entries.len();
    let filter_suffix = if tab.filter_text.trim().is_empty() {
        String::new()
    } else {
        format!("    filter: {}", tab.filter_text.trim())
    };
    match tab.selection.cursor() {
        Some(i) if i < count => format!(
            "{}    {} of {}{}    \u{2191}/\u{2193} navigate · Enter open · Backspace up · Esc quit",
            tab.entries[i].name,
            i + 1,
            count,
            filter_suffix
        ),
        _ => format!(
            "{} items{}    \u{2191}/\u{2193} navigate · Enter open · Backspace up · Esc quit",
            count, filter_suffix
        ),
    }
}

#[cfg(target_os = "macos")]
fn load_default_font() -> Result<Vec<u8>> {
    let path = "/System/Library/Fonts/Supplemental/Arial.ttf";
    std::fs::read(path).with_context(|| format!("read {path}"))
}

#[cfg(target_os = "windows")]
fn load_default_font() -> Result<Vec<u8>> {
    let path = "C:\\Windows\\Fonts\\segoeui.ttf";
    std::fs::read(path).with_context(|| format!("read {path}"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn load_default_font() -> Result<Vec<u8>> {
    anyhow::bail!("no default font path on this OS")
}
