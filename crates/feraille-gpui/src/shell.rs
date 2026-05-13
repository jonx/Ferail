//! The top-level shell view. Phase 1 is empty scaffolding; Phases 3+
//! attach Settings, workspace pane, etc.
//!
//! Lives in its own module so both `main.rs` (GUI) and `screenshot.rs`
//! (headless) construct the same tree.

use gpui::*;
use gpui_component::{
    ActiveTheme, h_flex,
    sidebar::{Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem},
    v_flex,
};

pub struct Shell;

impl Shell {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }

    fn nav_menu(&self) -> SidebarMenu {
        SidebarMenu::new().children([
            SidebarMenuItem::new("Recents").active(true),
            SidebarMenuItem::new("Favorites"),
            SidebarMenuItem::new("Locations"),
            SidebarMenuItem::new("Volumes"),
        ])
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu = self.nav_menu();
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
                    .p_6()
                    .gap_2()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child("Empty shell"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Phase 1 of the GPUI migration. Sidebar + main pane scaffolding only. \
                                Domain wiring lands in Phase 3 (Settings) and Phase 4 (file list).",
                            ),
                    ),
            )
    }
}
