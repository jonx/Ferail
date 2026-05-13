//! File-manager shell — main window content during Phases 4+.
//!
//! Phase 4.a: holds `current_dir`, renders a clickable breadcrumb at
//! the top of the main pane, sidebar entries are still placeholder.
//! Phase 4.b will wire the sidebar to real Locations/Volumes. Phase
//! 4.c brings the virtualized file list.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use feraille_fs_native::{home_dir, list_volumes, NativeFs, VolumeInfo};
use gpui::*;
use gpui_component::{
    ActiveTheme, Sizable, h_flex,
    sidebar::{Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem},
    table::{DataTable, TableState},
    v_flex,
};

use crate::file_list::FileListDelegate;

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
        Self {
            current_dir: start,
            volumes: list_volumes(),
            fs,
            table,
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
