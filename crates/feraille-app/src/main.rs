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
    scrollbar::Scrollbar, splitter::Splitter, text_input::TextInputKey,
};
use feraille_controls::{
    BreadcrumbBar, BreadcrumbEvent, FileTree, Selection, TabInfo, TabStrip, TabStripEvent,
    TreeEvent, VirtualizedList,
};
use feraille_core::{EntryKind, FileEntry, FsBackend, NodeId};
use feraille_design::{FontWeight, Theme, Tokens};
use feraille_fs_native::{home_dir, list_volumes, NativeFs};

mod screenshot;
use feraille_render::{Point as FPoint, Rect as FRect, Renderer, SoftRenderer, TextStyle};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

const TABSTRIP_H: f32 = 32.0;
const BREADCRUMB_H: f32 = 32.0;
const STATUS_H: f32 = 24.0;
const SCROLLBAR_W: f32 = 10.0;
const SIDEBAR_DEFAULT: f32 = 220.0;
const SIDEBAR_MIN: f32 = 160.0;
const SIDEBAR_MAX: f32 = 480.0;

fn main() -> Result<()> {
    let args = screenshot::parse_args();
    if args.screenshot.is_some() {
        return screenshot::run(args);
    }
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new();
    event_loop.run_app(&mut app)?;
    Ok(())
}

pub struct Tab {
    pub current_dir: PathBuf,
    pub entries: Vec<FileEntry>,
    pub selection: Selection,
    pub list_scroll: f32,
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
    pub splitter_x: f32,
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
            entries: Vec::new(),
            selection: Selection::new(),
            list_scroll: 0.0,
        });
        self.active = new_index;
        self.list.scroll_offset = 0.0;
        self.navigate(path);
    }

    pub fn switch_to_tab(&mut self, idx: usize) {
        self.switch_tab(idx);
    }

    pub fn set_splitter(&mut self, x: f32) {
        self.splitter_x = x.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
    }

    pub fn set_scroll(&mut self, y: f32) {
        let viewport_h = self.list_inner_rect().size.height;
        let count = self.tabs[self.active].entries.len();
        self.list.scroll_by(
            y - self.list.scroll_offset,
            count,
            viewport_h,
        );
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

    /// Resolve a path → NodeId via the FS (allocating an ID if new).
    pub fn id_for_path(&self, path: &Path) -> NodeId {
        self.fs.id_for_path(path)
    }

    fn new() -> Self {
        let fs = Arc::new(NativeFs::new());
        let home = home_dir();

        // Seed the tree with Home + /Volumes mounts as roots.
        let mut tree = FileTree::new();
        let mut roots: Vec<(feraille_core::NodeId, String)> = Vec::new();
        let home_id = fs.id_for_path(&home);
        let home_label = home
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("Home")
            .to_string();
        roots.push((home_id, home_label));
        for (label, path) in list_volumes() {
            roots.push((fs.id_for_path(&path), label));
        }
        tree.set_roots(roots);
        tree.select(home_id);

        let mut breadcrumb = BreadcrumbBar::new();
        breadcrumb.set_path(&home);

        let initial_tab = Tab {
            current_dir: home.clone(),
            entries: Vec::new(),
            selection: Selection::new(),
            list_scroll: 0.0,
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
            splitter_x: SIDEBAR_DEFAULT,
            pointer_dips: None,
            modifiers: ModifiersState::empty(),
            tokens: Tokens::for_theme(detect_theme()),
            width: 1,
            height: 1,
            scale_factor: 1.0,
        };
        a.navigate(home);
        a
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
        FRect::new(0.0, self.body_top(), self.splitter_x, (h - self.body_top() - STATUS_H).max(0.0))
    }

    fn breadcrumb_rect(&self) -> FRect {
        let (w, _) = self.viewport_size_dips();
        FRect::new(self.splitter_x, self.body_top(), (w - self.splitter_x).max(0.0), BREADCRUMB_H)
    }

    fn list_pane_rect(&self) -> FRect {
        let (w, h) = self.viewport_size_dips();
        FRect::new(
            self.splitter_x,
            self.body_top() + BREADCRUMB_H,
            (w - self.splitter_x).max(0.0),
            (h - self.body_top() - BREADCRUMB_H - STATUS_H).max(0.0),
        )
    }

    fn list_inner_rect(&self) -> FRect {
        let pane = self.list_pane_rect();
        FRect::new(pane.left(), pane.top(), (pane.size.width - SCROLLBAR_W).max(0.0), pane.size.height)
    }

    fn scrollbar_rect(&self) -> FRect {
        let pane = self.list_pane_rect();
        FRect::new(pane.right() - SCROLLBAR_W, pane.top(), SCROLLBAR_W, pane.size.height)
    }

    fn splitter_container(&self) -> FRect {
        let (_, h) = self.viewport_size_dips();
        FRect::new(0.0, self.body_top(), self.viewport_size_dips().0, (h - self.body_top() - STATUS_H).max(0.0))
    }

    fn list_content_height(&self) -> f32 {
        self.tabs[self.active].entries.len() as f32 * self.list.row_height
    }

    pub fn navigate(&mut self, path: PathBuf) {
        let id = self.fs.id_for_path(&path);
        let handle = self.fs.enumerate(id);
        let tab = &mut self.tabs[self.active];
        tab.entries = handle.initial;
        tab.current_dir = path.clone();
        tab.selection = Selection::new();
        if !tab.entries.is_empty() {
            tab.selection.set_cursor(0);
        }
        tab.list_scroll = 0.0;
        self.list.scroll_offset = 0.0;
        self.breadcrumb.set_path(&path);
        self.reveal_in_tree(&path);
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
                    let handle = self.fs.enumerate(id);
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
        let (cur_dir, cursor_idx, kind, name) = {
            let t = &self.tabs[self.active];
            let Some(idx) = t.selection.cursor() else { return };
            let Some(entry) = t.entries.get(idx) else { return };
            (t.current_dir.clone(), idx, entry.kind, entry.name.clone())
        };
        let _ = cursor_idx;
        if matches!(kind, EntryKind::Directory) {
            self.navigate(cur_dir.join(name));
        }
    }

    fn navigate_parent(&mut self) {
        let parent = self.tabs[self.active].current_dir.parent().map(Path::to_path_buf);
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
    }

    fn new_tab(&mut self) {
        let path = self.tabs[self.active].current_dir.clone();
        self.tabs[self.active].list_scroll = self.list.scroll_offset;
        let new_index = self.tabs.len();
        self.tabs.push(Tab {
            current_dir: path.clone(),
            entries: Vec::new(),
            selection: Selection::new(),
            list_scroll: 0.0,
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
                let handle = self.fs.enumerate(id);
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
        let list_inner = self.list_inner_rect();
        let scrollbar_rect = self.scrollbar_rect();
        let splitter_container = self.splitter_container();
        let content_h = self.list_content_height();
        let status_text = format_status(&self.tabs[self.active]);
        let tab_infos: Vec<TabInfo> =
            self.tabs.iter().map(|t| TabInfo { label: t.label() }).collect();
        let active = self.active;
        let splitter_x = self.splitter_x;
        let viewport = renderer.viewport();
        let tokens = &self.tokens;

        // Window bg
        renderer.fill_rect(FRect::new(0.0, 0.0, viewport.width, viewport.height), tokens.bg.base);

        // Tabstrip — topmost element (the OS title bar sits above us).
        self.tabstrip.paint(tabstrip_rect, &tab_infos, active, tokens, renderer);

        // Tree pane (replaces sidebar)
        self.tree.paint(tree_rect, tokens, renderer);

        // Breadcrumb
        self.breadcrumb.paint(breadcrumb_rect, tokens, renderer);

        // List + scrollbar
        let tab = &self.tabs[self.active];
        self.list.paint(list_inner, &tab.entries, &tab.selection, tokens, renderer);
        self.scrollbar.paint(
            scrollbar_rect,
            content_h,
            list_inner.size.height,
            self.list.scroll_offset,
            tokens,
            renderer,
        );

        // Splitter
        self.splitter.paint(splitter_x, splitter_container, tokens, renderer);

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
        let Some(mut renderer) = self.renderer.take() else { return };
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

impl ApplicationHandler for App {
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
        self.renderer = Some(SoftRenderer::new(self.width, self.height, scale, font_bytes));
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
            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } => {
                let p = FPoint::new((x as f32) / self.scale_factor, (y as f32) / self.scale_factor);
                self.pointer_dips = Some(p);
                let mut redraw = false;
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
                    if self.tabstrip.update_hover(self.tabstrip_rect(),
                        &self.tabs.iter().map(|t| TabInfo { label: t.label() }).collect::<Vec<_>>(),
                        Some(p))
                    {
                        redraw = true;
                    }
                    if self.breadcrumb.update_hover(self.breadcrumb_rect(), Some(p)) {
                        redraw = true;
                    }
                    if self.tree.update_hover(self.tree_rect(), Some(p)) {
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
                self.request_redraw();
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let Some(p) = self.pointer_dips else { return };

                // Splitter (highest priority — narrow but layered above).
                if self.splitter.begin_drag_at(self.splitter_x, self.splitter_container(), p) {
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
                let tab_infos: Vec<TabInfo> =
                    self.tabs.iter().map(|t| TabInfo { label: t.label() }).collect();
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
                // Tree — may emit two events (expand + activate).
                let tree_events = self.tree.click(self.tree_rect(), p);
                if !tree_events.is_empty() {
                    for ev in tree_events {
                        self.handle_tree_event(ev);
                    }
                    self.request_redraw();
                    return;
                }
                // List click.
                if let Some(idx) = self.list.index_at(inner, p, self.tabs[self.active].entries.len()) {
                    self.tabs[self.active].selection.set_cursor(idx);
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.scrollbar.is_dragging() {
                    self.scrollbar.end_drag();
                    self.request_redraw();
                }
                if self.splitter.is_dragging() {
                    self.splitter.end_drag();
                    self.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * 56.0,
                    MouseScrollDelta::PixelDelta(p) => -(p.y as f32),
                };
                let count = self.tabs[self.active].entries.len();
                let viewport_h = self.list_inner_rect().size.height;
                self.list.scroll_by(dy, count, viewport_h);
                self.request_redraw();
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
                    let mod_held =
                        self.modifiers.control_key() || self.modifiers.super_key();
                    if (t == "l" || t == "L") && mod_held {
                        let path = self.tabs[self.active].current_dir.clone();
                        self.breadcrumb.enter_edit_mode(&path);
                        self.request_redraw();
                        return;
                    }
                }

                let count = self.tabs[self.active].entries.len();
                let viewport_h = self.list_inner_rect().size.height;
                let page = (viewport_h / self.list.row_height) as i64;
                let sel = &mut self.tabs[self.active].selection;
                match logical_key {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Named(NamedKey::ArrowDown) => sel.move_cursor(1, count),
                    Key::Named(NamedKey::ArrowUp) => sel.move_cursor(-1, count),
                    Key::Named(NamedKey::PageDown) => sel.move_cursor(page, count),
                    Key::Named(NamedKey::PageUp) => sel.move_cursor(-page, count),
                    Key::Named(NamedKey::Home) => sel.move_cursor(-(count as i64), count),
                    Key::Named(NamedKey::End) => sel.move_cursor(count as i64, count),
                    Key::Named(NamedKey::Enter) => self.open_at_cursor(),
                    Key::Named(NamedKey::Backspace) => self.navigate_parent(),
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

fn detect_theme() -> Theme {
    if std::env::var_os("FERAILLE_THEME").as_deref() == Some(std::ffi::OsStr::new("dark")) {
        Theme::Dark
    } else {
        Theme::Light
    }
}

fn format_status(tab: &Tab) -> String {
    let count = tab.entries.len();
    match tab.selection.cursor() {
        Some(i) if i < count => format!(
            "{}    {} of {}    \u{2191}/\u{2193} navigate · Enter open · Backspace up · Esc quit",
            tab.entries[i].name,
            i + 1,
            count
        ),
        _ => format!(
            "{} items    \u{2191}/\u{2193} navigate · Enter open · Backspace up · Esc quit",
            count
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
