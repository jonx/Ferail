//! Settings panel — Phase 3 of the GPUI migration.
//!
//! IA mirrors what shipped on the soft renderer: left sidebar with
//! four categories (Appearance / Files / Layout / About), right
//! content area with one card per page. Each row has the brief's
//! anatomy: title (bold), description (muted, optional), control
//! slot (right-aligned). PreviewTile polish for theme + live count
//! for hidden files lands in 3.C.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Selectable, Sizable, Theme, ThemeMode,
    button::{Button, ButtonGroup},
    h_flex,
    sidebar::{Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem},
    switch::Switch,
    v_flex,
};

// =============================================================================
// Category enum
// =============================================================================

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

// =============================================================================
// Sidebar width snap stops (Layout page)
// =============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SidebarWidthSnap {
    Narrow,
    Medium,
    Wide,
}

impl SidebarWidthSnap {
    pub const ALL: &'static [SidebarWidthSnap] = &[
        SidebarWidthSnap::Narrow,
        SidebarWidthSnap::Medium,
        SidebarWidthSnap::Wide,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SidebarWidthSnap::Narrow => "Narrow",
            SidebarWidthSnap::Medium => "Medium",
            SidebarWidthSnap::Wide => "Wide",
        }
    }
}

// =============================================================================
// SettingsView state
// =============================================================================

pub struct SettingsView {
    pub category: SettingsCategory,
    pub show_hidden: bool,
    pub sidebar_width: SidebarWidthSnap,
}

impl SettingsView {
    pub fn new(initial: SettingsCategory) -> Self {
        Self {
            category: initial,
            show_hidden: false,
            sidebar_width: SidebarWidthSnap::Medium,
        }
    }
}

// =============================================================================
// Row helper — title + optional description + control slot
// =============================================================================

/// Render one row inside a settings card. Title sits above an optional
/// description; the control is right-aligned and vertically centred.
fn settings_row(
    title: &'static str,
    description: Option<&'static str>,
    control: AnyElement,
    cx: &mut App,
) -> Div {
    let text_block = v_flex()
        .gap_1()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().foreground)
                .child(title),
        )
        .when_some(description, |this, d| {
            this.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(d),
            )
        });

    h_flex()
        .w_full()
        .items_center()
        .gap_4()
        .py_3()
        .px_4()
        .child(text_block)
        .child(control)
}

/// Wrap a list of rows in the canonical card surface.
fn settings_card(rows: Vec<Div>, cx: &mut App) -> Div {
    let mut card = v_flex()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary);
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            card = card.child(
                div()
                    .h(px(1.0))
                    .w_full()
                    .bg(cx.theme().border),
            );
        }
        card = card.child(row);
    }
    card
}

// =============================================================================
// Render implementations per page
// =============================================================================

impl SettingsView {
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

    /// Theme picker — two-button segmented control. Each click flips
    /// the global theme via `Theme::change`, so the rest of the
    /// window updates immediately.
    fn theme_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = cx.theme().mode;
        ButtonGroup::new("theme-picker")
            .small()
            .outline()
            .compact()
            .child(
                Button::new("theme-light")
                    .label("Light")
                    .selected(current == ThemeMode::Light),
            )
            .child(
                Button::new("theme-dark")
                    .label("Dark")
                    .selected(current == ThemeMode::Dark),
            )
            .on_click(cx.listener(|_, clicks: &Vec<usize>, window, cx| {
                let mode = match clicks.first().copied() {
                    Some(0) => ThemeMode::Light,
                    Some(1) => ThemeMode::Dark,
                    _ => return,
                };
                Theme::change(mode, Some(window), cx);
            }))
            .into_any_element()
    }

    fn hidden_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        Switch::new("hidden-toggle")
            .checked(self.show_hidden)
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.show_hidden = *checked;
                cx.notify();
            }))
            .into_any_element()
    }

    /// Sidebar width — Narrow / Medium / Wide segmented control.
    fn sidebar_width_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.sidebar_width;
        ButtonGroup::new("sidebar-width-picker")
            .small()
            .outline()
            .compact()
            .child(
                Button::new("width-narrow")
                    .label("Narrow")
                    .selected(current == SidebarWidthSnap::Narrow),
            )
            .child(
                Button::new("width-medium")
                    .label("Medium")
                    .selected(current == SidebarWidthSnap::Medium),
            )
            .child(
                Button::new("width-wide")
                    .label("Wide")
                    .selected(current == SidebarWidthSnap::Wide),
            )
            .on_click(cx.listener(|this, clicks: &Vec<usize>, _, cx| {
                let snap = match clicks.first().copied() {
                    Some(0) => SidebarWidthSnap::Narrow,
                    Some(1) => SidebarWidthSnap::Medium,
                    Some(2) => SidebarWidthSnap::Wide,
                    _ => return,
                };
                this.sidebar_width = snap;
                cx.notify();
            }))
            .into_any_element()
    }

    fn appearance_card(&self, cx: &mut Context<Self>) -> Div {
        let theme = self.theme_picker(cx);
        settings_card(
            vec![settings_row(
                "Theme",
                Some("Match the system, or pick a side."),
                theme,
                cx,
            )],
            cx,
        )
    }

    fn files_card(&self, cx: &mut Context<Self>) -> Div {
        let toggle = self.hidden_toggle(cx);
        settings_card(
            vec![settings_row(
                "Show hidden files and folders",
                Some("Display items that start with a dot, like .config and .ssh."),
                toggle,
                cx,
            )],
            cx,
        )
    }

    fn layout_card(&self, cx: &mut Context<Self>) -> Div {
        let picker = self.sidebar_width_picker(cx);
        settings_card(
            vec![settings_row(
                "Sidebar width",
                Some("How wide the navigation panel appears."),
                picker,
                cx,
            )],
            cx,
        )
    }

    fn about_card(&self, cx: &mut Context<Self>) -> Div {
        let inner = v_flex()
            .gap_2()
            .px_4()
            .py_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child("Feraille"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(concat!("Version ", env!("CARGO_PKG_VERSION"))),
            )
            .child(
                div()
                    .mt_2()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child("The macOS port of Ferail — a Finder-class file explorer."),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Built for speed, predictability, and a calm UI."),
            );
        v_flex()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(inner)
    }

    fn page(&self, cx: &mut Context<Self>) -> Div {
        let card = match self.category {
            SettingsCategory::Appearance => self.appearance_card(cx),
            SettingsCategory::Files => self.files_card(cx),
            SettingsCategory::Layout => self.layout_card(cx),
            SettingsCategory::About => self.about_card(cx),
        };
        v_flex()
            .gap_4()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(self.category.title()),
            )
            .child(card)
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
        let page = self.page(cx);
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
                    .child(v_flex().h_full().flex_1().min_w_0().p_6().child(page)),
            )
            .child(footer)
    }
}

// =============================================================================
// CLI helper
// =============================================================================

pub fn category_from_arg(arg: Option<&str>) -> SettingsCategory {
    match arg.unwrap_or("appearance") {
        "files" => SettingsCategory::Files,
        "layout" => SettingsCategory::Layout,
        "about" => SettingsCategory::About,
        _ => SettingsCategory::Appearance,
    }
}
