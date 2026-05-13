//! Settings panel — Phase 3 of the GPUI migration.
//!
//! Re-implements the design brief on the new stack. The IA mirrors
//! what we shipped on the soft renderer earlier today: a left
//! sidebar with four categories (Appearance / Files / Layout /
//! About), and a right content area per page. The implementation
//! is rebuilt from scratch — the rect-based atoms in
//! `feraille-controls` don't carry over.
//!
//! This module is intentionally small in Phase 3.A: just the
//! scaffold (sidebar + active state + page title + an empty card
//! per page). Real controls land in 3.B; PreviewTile polish in 3.C.

use gpui::*;
use gpui_component::{
    ActiveTheme, h_flex,
    sidebar::{Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem},
    v_flex,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsCategory {
    Appearance,
    Files,
    Layout,
    About,
}

impl SettingsCategory {
    pub const ALL: &'static [SettingsCategory] = &[
        SettingsCategory::Appearance,
        SettingsCategory::Files,
        SettingsCategory::Layout,
        SettingsCategory::About,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SettingsCategory::Appearance => "Appearance",
            SettingsCategory::Files => "Files",
            SettingsCategory::Layout => "Layout",
            SettingsCategory::About => "About",
        }
    }
}

/// State for the Settings panel. Currently just tracks which page
/// is active; 3.B will grow this to hold the theme preference
/// preview, the slider state, etc.
pub struct SettingsView {
    pub category: SettingsCategory,
}

impl SettingsView {
    pub fn new(initial: SettingsCategory) -> Self {
        Self { category: initial }
    }

    fn nav(&self, cx: &mut Context<Self>) -> SidebarMenu {
        let make_item = |label: &'static str, cat: SettingsCategory| -> SidebarMenuItem {
            SidebarMenuItem::new(label)
                .active(self.category == cat)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.category = cat;
                    cx.notify();
                }))
        };
        SidebarMenu::new().children(
            SettingsCategory::ALL
                .iter()
                .map(|cat| make_item(cat.title(), *cat))
                .collect::<Vec<_>>(),
        )
    }

    fn page_body(&self, cx: &mut Context<Self>) -> Div {
        let title = self.category.title();
        v_flex()
            .gap_4()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(title),
            )
            .child(
                // Empty card placeholder. Filled in 3.B per category.
                v_flex()
                    .min_h(px(120.0))
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .p_4()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(match self.category {
                                SettingsCategory::Appearance => {
                                    "Theme picker arrives in Phase 3.B."
                                }
                                SettingsCategory::Files => {
                                    "Show-hidden-files toggle arrives in Phase 3.B."
                                }
                                SettingsCategory::Layout => {
                                    "Narrow / Medium / Wide sidebar stops arrive in Phase 3.B."
                                }
                                SettingsCategory::About => {
                                    "App info arrives in Phase 3.B."
                                }
                            }),
                    ),
            )
    }

    fn footer(&self, cx: &mut Context<Self>) -> Div {
        h_flex()
            .w_full()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Changes save instantly \u{00B7} Press Esc to close"),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = self.nav(cx);
        let page = self.page_body(cx);
        let footer = self.footer(cx);

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        Sidebar::new("settings-sidebar")
                            .w(px(200.0))
                            .header(
                                SidebarHeader::new().child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(cx.theme().foreground)
                                        .child("Settings"),
                                ),
                            )
                            .child(SidebarGroup::new("").child(nav)),
                    )
                    .child(
                        v_flex()
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .p_6()
                            .child(page),
                    ),
            )
            .child(footer)
    }
}

/// Parse a `--settings <page>` value into a category. Defaults to
/// Appearance for unknown / missing values so screenshot scripts
/// don't blow up on typos.
pub fn category_from_arg(arg: Option<&str>) -> SettingsCategory {
    match arg.unwrap_or("appearance") {
        "files" => SettingsCategory::Files,
        "layout" => SettingsCategory::Layout,
        "about" => SettingsCategory::About,
        _ => SettingsCategory::Appearance,
    }
}

