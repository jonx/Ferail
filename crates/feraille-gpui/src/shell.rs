//! File-manager shell — main window content during Phases 4+.
//!
//! Phase 4.a: holds `current_dir`, renders a clickable breadcrumb at
//! the top of the main pane, sidebar entries are still placeholder.
//! Phase 4.b will wire the sidebar to real Locations/Volumes. Phase
//! 4.c brings the virtualized file list.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use feraille_core::{EntryKind, EnumerationError};
use feraille_fs_native::{home_dir, list_volumes, open_with_default, NativeFs, VolumeInfo};
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, Sizable, button::Button, h_flex,
    sidebar::{Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem},
    switch::Switch,
    table::{DataTable, TableEvent, TableState},
    v_flex,
};

use crate::file_list::FileListDelegate;
use crate::fs_watcher::{FsWatcher, POLL_INTERVAL};

actions!(
    shell,
    [
        NavigateParent,
        NavigateBack,
        NavigateForward,
        OpenSelected,
        Refresh,
        ToggleHidden,
        OpenSettings,
    ]
);

/// Key-context name for the Shell's outer container — same convention
/// gpui-component uses (e.g. `Root` / `Input`). Only one context-bound
/// keystroke today (Backspace → NavigateParent); the full keymap
/// system arrives in Phase 5.
const SHELL_CONTEXT: &str = "Shell";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        // Per-shell context: only fire when the Shell holds focus.
        KeyBinding::new("backspace", NavigateParent, Some(SHELL_CONTEXT)),
        KeyBinding::new("cmd-[", NavigateBack, Some(SHELL_CONTEXT)),
        KeyBinding::new("cmd-]", NavigateForward, Some(SHELL_CONTEXT)),
        KeyBinding::new("enter", OpenSelected, Some(SHELL_CONTEXT)),
        KeyBinding::new("cmd-r", Refresh, Some(SHELL_CONTEXT)),
        KeyBinding::new("cmd-shift-.", ToggleHidden, Some(SHELL_CONTEXT)),
        // App-wide: Cmd+, is the system convention for Preferences /
        // Settings and should work from anywhere in the app.
        KeyBinding::new("cmd-,", OpenSettings, None),
    ]);
}

pub struct Shell {
    /// Path the file pane is currently showing.
    pub current_dir: PathBuf,
    /// Volumes mounted at /Volumes. Refreshed lazily in 4.b; future
    /// iters will watch for changes via the macOS Disk Arbitration
    /// framework.
    pub volumes: Vec<VolumeInfo>,
    /// Shared FS backend. `Arc` because the file-list delegate also
    /// holds a reference (for path lookups during navigation).
    pub fs: Arc<NativeFs>,
    /// gpui-component's virtualized Table state, parameterised by
    /// our file-list delegate. The Shell talks to the delegate
    /// through `cx.update_entity` calls on this handle.
    pub table: Entity<TableState<FileListDelegate>>,
    /// Focus handle for the Shell's key-context. Keybindings declared
    /// against `SHELL_CONTEXT` only fire when this handle (or one of
    /// its children) holds focus.
    pub focus_handle: FocusHandle,
    /// Row index of the currently-selected entry (or None when the
    /// pane is empty / nothing chosen). Drives the preview pane.
    pub selected: Option<usize>,
    /// When true, dotfiles are shown in the list.
    pub show_hidden: bool,
    /// Navigation history (back-forward stack). The current location
    /// is always `history[history_index]`. `navigate(p)` truncates
    /// forward, pushes `p`, and advances the index.
    pub history: Vec<PathBuf>,
    pub history_index: usize,
    /// `Some(err)` when the last `enumerate` returned an OS error
    /// (most commonly macOS TCC denial on ~/Documents etc.). Drives
    /// an in-pane empty-state instead of a silent blank list.
    pub last_error: Option<EnumerationError>,
    /// Background file-system watcher. `Rc<RefCell<>>` so the
    /// foreground-executor polling task can read it without taking
    /// a mutable borrow of the whole Shell. None if the platform
    /// watcher failed to start (rare — typically only in stripped
    /// CI environments without FSEvents).
    pub watcher: Rc<RefCell<Option<FsWatcher>>>,
}

/// A named filesystem destination shown in the sidebar's Locations
/// section. The user's home directory, Applications, Documents, etc.
struct Location {
    label: &'static str,
    /// `home`-relative subpath (None ⇒ the home directory itself).
    sub: Option<&'static str>,
}

const LOCATIONS: &[Location] = &[
    Location { label: "Home", sub: None },
    Location { label: "Applications", sub: Some("Applications") },
    Location { label: "Desktop", sub: Some("Desktop") },
    Location { label: "Documents", sub: Some("Documents") },
    Location { label: "Downloads", sub: Some("Downloads") },
    Location { label: "Movies", sub: Some("Movies") },
    Location { label: "Music", sub: Some("Music") },
    Location { label: "Pictures", sub: Some("Pictures") },
];

impl Location {
    fn path(&self) -> PathBuf {
        let mut p = home_dir();
        if let Some(sub) = self.sub {
            p.push(sub);
        }
        p
    }
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let fs = Arc::new(NativeFs::new());
        let start = home_dir();
        let show_hidden = false;
        let mut delegate = FileListDelegate::new(fs.clone());
        let last_error = delegate.load(start.clone(), show_hidden);
        let initial_selection = if delegate.entries.is_empty() { None } else { Some(0) };
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .col_selectable(false)
                .col_movable(false)
        });
        // Bridge Table events (selection + double-click) to the
        // Shell's own state so the preview pane sees the live row.
        cx.subscribe_in(
            &table,
            window,
            |this, _table, event: &TableEvent, _window, cx| match event {
                TableEvent::SelectRow(row_ix) => {
                    this.selected = Some(*row_ix);
                    cx.notify();
                }
                TableEvent::DoubleClickedRow(row_ix) => {
                    this.activate_row(*row_ix, cx);
                }
                _ => {}
            },
        )
        .detach();
        let focus_handle = cx.focus_handle();
        // Grab focus on first paint so the Backspace keybind works
        // immediately without the user having to click into the
        // shell.
        focus_handle.focus(window, cx);

        // Spin up the platform file-system watcher and start
        // watching the initial directory. If the watcher itself
        // can't be constructed (very rare; sandbox without
        // FSEvents), we just operate without it — manual Cmd+R
        // still works.
        let watcher = match FsWatcher::new() {
            Ok(mut w) => {
                let _ = w.watch(&start);
                Rc::new(RefCell::new(Some(w)))
            }
            Err(_) => Rc::new(RefCell::new(None)),
        };

        // Foreground-executor polling task. Wakes every POLL_INTERVAL,
        // drains the channel, asks the Shell to reload if anything
        // changed. Stops when this.update returns Err — that means
        // the Shell entity has been dropped.
        let poll_watcher = watcher.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let dirty = poll_watcher
                    .borrow()
                    .as_ref()
                    .map(|w| w.drain_reload_relevant())
                    .unwrap_or(false);
                if dirty {
                    if this
                        .update(cx, |this, cx| {
                            let path = this.current_dir.clone();
                            this.load_path(path, cx);
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        })
        .detach();

        Self {
            current_dir: start.clone(),
            volumes: list_volumes(),
            fs,
            table,
            focus_handle,
            selected: initial_selection,
            show_hidden,
            history: vec![start],
            history_index: 0,
            last_error,
            watcher,
        }
    }

    fn on_navigate_back(
        &mut self,
        _: &NavigateBack,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_back(cx);
    }

    fn on_navigate_forward(
        &mut self,
        _: &NavigateForward,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_forward(cx);
    }

    fn on_open_selected(
        &mut self,
        _: &OpenSelected,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(idx) = self.selected {
            self.activate_row(idx, cx);
        }
    }

    fn on_refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        let path = self.current_dir.clone();
        self.load_path(path, cx);
    }

    fn on_toggle_hidden(
        &mut self,
        _: &ToggleHidden,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_hidden(cx);
    }

    fn on_open_settings(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Spawn a second native window hosting the SettingsView,
        // matching macOS convention where Preferences is its own
        // window not a modal sheet. Independent of the file-manager
        // shell's lifecycle — closing one doesn't close the other.
        let _ = window;
        crate::settings::open_settings_window(cx);
    }

    pub fn navigate_back(&mut self, cx: &mut Context<Self>) {
        if self.history_index > 0 {
            self.history_index -= 1;
            let path = self.history[self.history_index].clone();
            self.load_path(path, cx);
        }
    }

    pub fn navigate_forward(&mut self, cx: &mut Context<Self>) {
        if self.history_index + 1 < self.history.len() {
            self.history_index += 1;
            let path = self.history[self.history_index].clone();
            self.load_path(path, cx);
        }
    }

    /// Inner load: re-enumerate the directory + refresh the table +
    /// re-target the watcher. Does **not** touch history (history
    /// is only mutated by `navigate`).
    fn load_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.current_dir = path.clone();
        let show_hidden = self.show_hidden;
        let table = self.table.clone();
        let mut err: Option<EnumerationError> = None;
        table.update(cx, |state, cx| {
            err = state.delegate_mut().load(path.clone(), show_hidden);
            state.refresh(cx);
        });
        self.last_error = err;
        self.selected = None;
        // Point the watcher at the new directory. Errors (path
        // doesn't exist, watcher saturated) are non-fatal — the
        // user still gets the listing; they just lose live updates.
        if let Some(w) = self.watcher.borrow_mut().as_mut() {
            let _ = w.watch(&path);
        }
        cx.notify();
    }

    pub fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        let path = self.current_dir.clone();
        self.load_path(path, cx);
    }

    fn on_navigate_parent(
        &mut self,
        _: &NavigateParent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_parent(cx);
    }

    /// User activated a row (double-click or Enter). For directories
    /// we navigate into them; for files we hand off to the OS opener.
    pub fn activate_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        let path_and_kind = self.table.read(cx).delegate().entries.get(row_ix).map(|e| {
            (
                self.fs.path_for(e.id).unwrap_or_else(|| {
                    let mut p = self.current_dir.clone();
                    p.push(&e.name);
                    p
                }),
                e.kind,
            )
        });
        let Some((path, kind)) = path_and_kind else {
            return;
        };
        match kind {
            EntryKind::Directory => self.navigate(path, cx),
            EntryKind::File | EntryKind::Symlink => {
                // open_with_default routes through `open(1)` on macOS;
                // failures are logged-and-ignored — the user already
                // gets system-level feedback if the app can't open.
                let _ = open_with_default(&path);
            }
        }
    }

    /// Navigate to the parent of the current directory (Backspace
    /// keybind in 4.c.2). No-op when already at the filesystem root.
    pub fn navigate_parent(&mut self, cx: &mut Context<Self>) {
        if let Some(parent) = self.current_dir.parent() {
            let parent = parent.to_path_buf();
            if parent != self.current_dir {
                self.navigate(parent, cx);
            }
        }
    }

    /// Navigate to `path`: re-enumerate, refresh the Table, push to
    /// history (truncating any forward stack first), reset selection.
    pub fn navigate(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.history.get(self.history_index) != Some(&path) {
            self.history.truncate(self.history_index + 1);
            self.history.push(path.clone());
            self.history_index = self.history.len() - 1;
        }
        self.load_path(path, cx);
    }

    /// Locations menu: Home + Applications + Desktop + Documents +
    /// Downloads + Movies + Music + Pictures. Each entry navigates
    /// on click; the entry whose `path()` matches `current_dir`
    /// gets the active state.
    fn locations_menu(&self, cx: &mut Context<Self>) -> SidebarMenu {
        let current = self.current_dir.clone();
        SidebarMenu::new().children(
            LOCATIONS
                .iter()
                .map(|loc| {
                    let path = loc.path();
                    let active = path == current;
                    let nav_path = path.clone();
                    SidebarMenuItem::new(loc.label)
                        .active(active)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.navigate(nav_path.clone(), cx);
                        }))
                })
                .collect::<Vec<_>>(),
        )
    }

    /// Volumes menu: every mounted volume at /Volumes.
    fn volumes_menu(&self, cx: &mut Context<Self>) -> SidebarMenu {
        let current = self.current_dir.clone();
        SidebarMenu::new().children(
            self.volumes
                .iter()
                .map(|v| {
                    let path = v.path.clone();
                    let active = path == current;
                    let nav_path = path.clone();
                    SidebarMenuItem::new(SharedString::from(v.name.clone()))
                        .active(active)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.navigate(nav_path.clone(), cx);
                        }))
                })
                .collect::<Vec<_>>(),
        )
    }

    /// Either the file Table, or an inline error/empty state when
    /// the directory couldn't be listed (typically macOS TCC denial
    /// on ~/Documents, ~/Desktop, ~/Downloads in a sandboxed runner).
    fn file_pane_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(err) = self.last_error.clone() {
            let (title, body) = error_copy(&err);
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .p_8()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child(title),
                )
                .child(
                    div()
                        .max_w(px(420.0))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(body),
                )
                .into_any_element();
        }
        DataTable::new(&self.table)
            .bordered(false)
            .stripe(true)
            .small()
            .into_any_element()
    }

    /// Toolbar row above the breadcrumb: Back / Forward buttons +
    /// "Show hidden" toggle. Disabled buttons grey out via Button's
    /// own disabled state — no manual styling.
    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let can_back = self.history_index > 0;
        let can_forward = self.history_index + 1 < self.history.len();
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("nav-back")
                    .small()
                    .label("\u{2190}")
                    .disabled(!can_back)
                    .on_click(cx.listener(|this, _, _, cx| this.navigate_back(cx))),
            )
            .child(
                Button::new("nav-forward")
                    .small()
                    .label("\u{2192}")
                    .disabled(!can_forward)
                    .on_click(cx.listener(|this, _, _, cx| this.navigate_forward(cx))),
            )
            .child(div().flex_1())
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Show hidden"),
                    )
                    .child(
                        Switch::new("hidden-toggle-toolbar")
                            .checked(self.show_hidden)
                            .on_click(cx.listener(|this, _checked: &bool, _, cx| {
                                this.toggle_hidden(cx);
                            })),
                    ),
            )
    }

    /// Build the preview pane on the right of the file list. Shows
    /// title / kind / size / modified / full path of the selected
    /// row. Falls back to a neutral empty state when nothing is
    /// selected. Format-specific previews (image, text, PDF) arrive
    /// in a follow-up polish iter.
    fn preview(&self, cx: &mut Context<Self>) -> Div {
        let selected = self
            .selected
            .and_then(|i| self.table.read(cx).delegate().entries.get(i).cloned());

        let header = div()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.theme().muted_foreground)
            .child("Preview");

        let body: AnyElement = match selected {
            None => div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No selection")
                .into_any_element(),
            Some(entry) => {
                let kind_label = match entry.kind {
                    EntryKind::Directory => "Folder",
                    EntryKind::File => "File",
                    EntryKind::Symlink => "Symlink",
                };
                let mut full_path = self.current_dir.clone();
                full_path.push(&entry.name);
                let path_str = full_path.to_string_lossy().into_owned();

                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child(SharedString::from(entry.name.clone())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(entry.display_kind.clone())),
                    )
                    .child(preview_field("Kind", kind_label.to_string(), cx))
                    .child(preview_field("Size", entry.display_size.clone(), cx))
                    .child(preview_field("Modified", entry.display_mtime.clone(), cx))
                    .child(preview_field("Where", path_str, cx))
                    .into_any_element()
            }
        };

        v_flex()
            .w(px(280.0))
            .h_full()
            .min_h_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .p_4()
            .gap_3()
            .child(header)
            .child(body)
    }

    /// Build the breadcrumb row from `current_dir`. Each ancestor is
    /// clickable and navigates the pane to that level. The root `/`
    /// gets its own leading segment.
    fn breadcrumb(&self, cx: &mut Context<Self>) -> Div {
        let segments = path_segments(&self.current_dir);
        let mut row = h_flex()
            .w_full()
            .items_center()
            .gap_1()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border);

        for (i, (label, path)) in segments.iter().enumerate() {
            if i > 0 {
                row = row.child(
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("\u{203A}"), // SINGLE RIGHT-POINTING ANGLE QUOTATION MARK
                );
            }
            let is_last = i + 1 == segments.len();
            let label = label.clone();
            let path = path.clone();
            let style = TextStyle {
                ..Default::default()
            };
            let _ = style; // unused-import shim if we add styled-text later
            let crumb = div()
                .id(ElementId::Name(format!("crumb-{i}").into()))
                .px_2()
                .py_1()
                .rounded(cx.theme().radius)
                .text_sm()
                .text_color(if is_last {
                    cx.theme().foreground
                } else {
                    cx.theme().muted_foreground
                })
                .when(is_last, |this| {
                    this.font_weight(FontWeight::SEMIBOLD)
                })
                .cursor_pointer()
                .hover(|this| this.bg(cx.theme().secondary))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.navigate(path.clone(), cx);
                }));
            row = row.child(crumb);
        }
        row
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locations = self.locations_menu(cx);
        let volumes = self.volumes_menu(cx);
        let has_volumes = !self.volumes.is_empty();
        let breadcrumb = self.breadcrumb(cx);
        let path_str = self.current_dir.to_string_lossy().into_owned();

        let mut sidebar = Sidebar::new("shell-sidebar")
            .w(px(220.0))
            .header(
                SidebarHeader::new().child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child("Feraille"),
                ),
            )
            .child(SidebarGroup::new("Locations").child(locations));
        if has_volumes {
            sidebar = sidebar.child(SidebarGroup::new("Volumes").child(volumes));
        }

        let _ = path_str; // breadcrumb already shows the path

        let toolbar = self.toolbar(cx);

        h_flex()
            .key_context(SHELL_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_navigate_parent))
            .on_action(cx.listener(Self::on_navigate_back))
            .on_action(cx.listener(Self::on_navigate_forward))
            .on_action(cx.listener(Self::on_open_selected))
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_toggle_hidden))
            .on_action(cx.listener(Self::on_open_settings))
            .size_full()
            .bg(cx.theme().background)
            .child(sidebar)
            .child(
                v_flex()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .child(toolbar)
                    .child(breadcrumb)
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .items_stretch()
                            .child(
                                div().flex_1().min_w_0().h_full().child(
                                    self.file_pane_body(cx),
                                ),
                            )
                            .child(self.preview(cx)),
                    ),
            )
    }
}

/// Map an `EnumerationError` to (title, body) copy for the in-pane
/// error state. macOS users hitting `Documents` / `Desktop` /
/// `Downloads` for the first time in a sandboxed launcher will see
/// the TCC permission case; other variants get a generic message.
fn error_copy(err: &EnumerationError) -> (&'static str, String) {
    match err {
        EnumerationError::PermissionDenied => (
            "Access required",
            "Feraille needs permission to read this folder. Grant access in \
             System Settings \u{2192} Privacy & Security \u{2192} Files and Folders."
                .to_string(),
        ),
        EnumerationError::NotFound => (
            "Folder not found",
            "This location may have been moved, renamed, or unmounted.".to_string(),
        ),
        EnumerationError::Other(msg) => (
            "Couldn't open this folder",
            msg.clone(),
        ),
    }
}

/// Two-line field for the preview pane: muted label on top, primary
/// value below. Used for Kind / Size / Modified / Where.
fn preview_field(label: &'static str, value: String, cx: &Context<Shell>) -> Div {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().foreground)
                .child(SharedString::from(value)),
        )
}

/// Split `path` into clickable breadcrumb segments. Each entry is
/// `(visible_label, path_to_navigate_to_on_click)`. The first entry
/// represents the filesystem root.
///
/// Public for the integration test in `tests/path_segments.rs` —
/// keeping it private and using an inline `#[cfg(test)] mod tests`
/// crashes the compiler (gpui's type graph plus the macro recursion
/// from `#[test]` overflows syn's parser).
pub fn path_segments(path: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut accum = PathBuf::from("/");
    out.push(("/".to_string(), accum.clone()));
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::RootDir => {}
            Component::Normal(s) => {
                accum.push(s);
                out.push((s.to_string_lossy().into_owned(), accum.clone()));
            }
            Component::CurDir => {}
            Component::ParentDir => {
                accum.pop();
            }
            Component::Prefix(_) => {}
        }
    }
    out
}

use gpui::prelude::FluentBuilder as _;
