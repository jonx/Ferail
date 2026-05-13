//! File-manager shell — main window content during Phases 4+.
//!
//! Phase 4.a: holds `current_dir`, renders a clickable breadcrumb at
//! the top of the main pane, sidebar entries are still placeholder.
//! Phase 4.b will wire the sidebar to real Locations/Volumes. Phase
//! 4.c brings the virtualized file list.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use feraille_core::EntryKind;
use feraille_fs_native::{home_dir, list_volumes, open_with_default, NativeFs, VolumeInfo};
use gpui::*;
use gpui_component::{
    ActiveTheme, Sizable, h_flex,
    sidebar::{Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem},
    table::{DataTable, TableEvent, TableState},
    v_flex,
};

use crate::file_list::FileListDelegate;

actions!(shell, [NavigateParent]);

/// Key-context name for the Shell's outer container — same convention
/// gpui-component uses (e.g. `Root` / `Input`). Only one context-bound
/// keystroke today (Backspace → NavigateParent); the full keymap
/// system arrives in Phase 5.
const SHELL_CONTEXT: &str = "Shell";

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "backspace",
        NavigateParent,
        Some(SHELL_CONTEXT),
    )]);
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
        let mut delegate = FileListDelegate::new(fs.clone());
        let _ = delegate.load(start.clone());
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .col_selectable(false)
                .col_movable(false)
        });
        // Bridge double-click events into our own activate handler.
        // `cx.subscribe_in` keeps a window reference so we can route
        // navigate() calls back through the foreground executor.
        cx.subscribe_in(
            &table,
            window,
            |this, _table, event: &TableEvent, _window, cx| {
                if let TableEvent::DoubleClickedRow(row_ix) = event {
                    this.activate_row(*row_ix, cx);
                }
            },
        )
        .detach();
        let focus_handle = cx.focus_handle();
        // Grab focus on first paint so the Backspace keybind works
        // immediately without the user having to click into the
        // shell.
        focus_handle.focus(window, cx);
        Self {
            current_dir: start,
            volumes: list_volumes(),
            fs,
            table,
            focus_handle,
        }
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

    /// Navigate to `path`: update `current_dir`, re-enumerate the
    /// directory via the FS backend, refresh the Table, request a
    /// re-render.
    pub fn navigate(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.current_dir = path.clone();
        let table = self.table.clone();
        table.update(cx, |state, cx| {
            state.delegate_mut().load(path);
            state.refresh(cx);
        });
        cx.notify();
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

        h_flex()
            .key_context(SHELL_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_navigate_parent))
            .size_full()
            .bg(cx.theme().background)
            .child(sidebar)
            .child(
                v_flex()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .child(breadcrumb)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .child(
                                DataTable::new(&self.table)
                                    .bordered(false)
                                    .stripe(true)
                                    .small(),
                            ),
                    ),
            )
    }
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
