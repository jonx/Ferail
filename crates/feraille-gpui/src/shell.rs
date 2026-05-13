//! File-manager shell — main window content during Phases 4+.
//!
//! Phase 4.a: holds `current_dir`, renders a clickable breadcrumb at
//! the top of the main pane, sidebar entries are still placeholder.
//! Phase 4.b will wire the sidebar to real Locations/Volumes. Phase
//! 4.c brings the virtualized file list.

use std::path::{Path, PathBuf};

use feraille_fs_native::home_dir;
use gpui::*;
use gpui_component::{
    ActiveTheme, h_flex,
    sidebar::{Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem},
    v_flex,
};

pub struct Shell {
    /// Path the file pane is currently showing. Phase 4.a uses this
    /// only for the breadcrumb; Phase 4.c wires it to a real
    /// `FsBackend::enumerate` call.
    pub current_dir: PathBuf,
}

impl Shell {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            current_dir: home_dir(),
        }
    }

    /// Navigate to `path` and request a re-render.
    pub fn navigate(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.current_dir = path;
        cx.notify();
    }

    fn nav_menu(&self) -> SidebarMenu {
        SidebarMenu::new().children([
            SidebarMenuItem::new("Recents").active(true),
            SidebarMenuItem::new("Favorites"),
            SidebarMenuItem::new("Locations"),
            SidebarMenuItem::new("Volumes"),
        ])
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
        let menu = self.nav_menu();
        let breadcrumb = self.breadcrumb(cx);
        let path_str = self.current_dir.to_string_lossy().into_owned();
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                Sidebar::new("shell-sidebar")
                    .w(px(220.0))
                    .header(
                        SidebarHeader::new().child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .child("Feraille"),
                        ),
                    )
                    .child(SidebarGroup::new("").child(menu)),
            )
            .child(
                v_flex()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .child(breadcrumb)
                    .child(
                        v_flex()
                            .flex_1()
                            .p_6()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Current directory"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(cx.theme().foreground)
                                    .child(path_str),
                            )
                            .child(
                                div()
                                    .mt_4()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "Phase 4.a: breadcrumb only. \
                                        File list (virtualized Table) arrives in 4.c. \
                                        Click any breadcrumb segment to navigate to that ancestor.",
                                    ),
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
