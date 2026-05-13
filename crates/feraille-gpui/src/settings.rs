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
    ActiveTheme, Root, Selectable, Sizable, Theme, ThemeMode,
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
    /// Count of dotfiles in the user's home directory, computed once
    /// at view construction so the Files page can show a live
    /// consequence preview ("Would reveal N items …"). `None` if the
    /// home directory couldn't be read (sandbox, CI, etc.).
    pub home_hidden_count: Option<usize>,
}

impl SettingsView {
    pub fn new(initial: SettingsCategory) -> Self {
        Self {
            category: initial,
            show_hidden: false,
            sidebar_width: SidebarWidthSnap::Medium,
            home_hidden_count: count_home_hidden_items(),
        }
    }
}

/// Count entries in `$HOME` whose name starts with `.`. Synchronous
/// because it runs exactly once at view construction; future revisions
/// will move this onto a background task with live invalidation.
fn count_home_hidden_items() -> Option<usize> {
    let home = std::env::var_os("HOME")?;
    let n = std::fs::read_dir(home)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with('.')
        })
        .count();
    Some(n)
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

    /// Two-tile theme picker (Light, Dark). Each tile is a mini-
    /// window mock — the user sees the consequence of the choice
    /// before committing, per the design brief's "show consequence,
    /// don't describe it" principle.
    fn theme_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = cx.theme().mode;
        h_flex()
            .gap_3()
            .child(preview_tile(
                "tile-light",
                "Light",
                PreviewKind::Light,
                current == ThemeMode::Light,
                cx,
                |window, cx| Theme::change(ThemeMode::Light, Some(window), cx),
            ))
            .child(preview_tile(
                "tile-dark",
                "Dark",
                PreviewKind::Dark,
                current == ThemeMode::Dark,
                cx,
                |window, cx| Theme::change(ThemeMode::Dark, Some(window), cx),
            ))
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
        let mut rows = vec![settings_row(
            "Show hidden files and folders",
            Some("Display items that start with a dot, like .config and .ssh."),
            toggle,
            cx,
        )];
        // Live consequence preview per the design brief: tell the
        // user what flipping this would actually change.
        if let Some(n) = self.home_hidden_count {
            let phrase = if self.show_hidden {
                format!("Currently revealing {} hidden items in your home folder.", n)
            } else {
                format!("Would reveal {} hidden items in your home folder.", n)
            };
            rows.push(
                h_flex().w_full().px_4().py_2().child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(phrase),
                ),
            );
        }
        settings_card(rows, cx)
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

// =============================================================================
// PreviewTile — mini-window theme swatches
// =============================================================================

#[derive(Clone, Copy)]
pub enum PreviewKind {
    Light,
    Dark,
}

/// Build a clickable preview tile: a stylized mini-window rendered in
/// the target theme's palette, with a label below. Selected tiles get
/// an accent border ring; unselected ones get `border.subtle`.
fn preview_tile(
    id: &'static str,
    label: &'static str,
    kind: PreviewKind,
    selected: bool,
    cx: &mut Context<SettingsView>,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    // Mock palette for the inner artwork — hardcoded so the tile
    // shows the OTHER theme even when the app is in the current one.
    let (bg, panel, accent, fg) = match kind {
        PreviewKind::Light => (
            rgb(0xFAFAFA),
            rgb(0xF0F0F0),
            rgb(0x2A63D9),
            rgb(0x1A1A1A),
        ),
        PreviewKind::Dark => (
            rgb(0x1B1B1B),
            rgb(0x252525),
            rgb(0x2457CA),
            rgb(0xF5F5F5),
        ),
    };

    let border_color = if selected {
        cx.theme().primary
    } else {
        cx.theme().border
    };
    let border_w = if selected { px(2.0) } else { px(1.0) };

    // Inner artwork: titlebar with three traffic lights, sidebar
    // slab, three "rows" with the middle one selection-highlighted.
    let artwork = v_flex()
        .w(px(160.0))
        .h(px(96.0))
        .rounded(px(6.0))
        .overflow_hidden()
        .bg(bg)
        .child(
            h_flex()
                .w_full()
                .h(px(14.0))
                .items_center()
                .gap_1()
                .px_2()
                .bg(panel)
                .child(div().size(px(6.0)).rounded_full().bg(rgb(0xFF6057)))
                .child(div().size(px(6.0)).rounded_full().bg(rgb(0xFFBD2E)))
                .child(div().size(px(6.0)).rounded_full().bg(rgb(0x28C940))),
        )
        .child(
            h_flex()
                .flex_1()
                .child(div().w(px(46.0)).h_full().bg(panel))
                .child(
                    v_flex()
                        .flex_1()
                        .gap_1()
                        .pt_2()
                        .px_2()
                        .child(div().w(px(60.0)).h(px(4.0)).rounded_full().bg(fg))
                        .child(
                            div()
                                .h(px(10.0))
                                .w_full()
                                .rounded(px(2.0))
                                .bg(accent),
                        )
                        .child(div().w(px(72.0)).h(px(4.0)).rounded_full().bg(fg)),
                ),
        );

    div()
        .id(ElementId::Name(id.into()))
        .flex()
        .flex_col()
        .gap_2()
        .items_center()
        .p_1p5()
        .rounded(px(8.0))
        .border(border_w)
        .border_color(border_color)
        .cursor_pointer()
        .child(artwork)
        .child(
            div()
                .text_xs()
                .font_weight(if selected {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(cx.theme().foreground)
                .child(label),
        )
        .on_click(cx.listener(move |_, _, window, cx| on_click(window, cx)))
}

/// Open a second native window hosting the SettingsView. Used by
/// the Cmd+, action and by future menu-bar entries. Idempotent in
/// the practical sense: each call opens a new window, so spamming
/// the shortcut just stacks them — fine for now, deduplication when
/// the workspace-with-multiple-windows pattern lands.
pub fn open_settings_window(cx: &mut App) {
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(820.0), px(560.0)), cx)),
        ..Default::default()
    };
    cx.spawn(async move |cx| {
        cx.open_window(opts, |window, cx| {
            let view = cx.new(|_| SettingsView::new(SettingsCategory::Appearance));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("failed to open settings window");
    })
    .detach();
}

pub fn category_from_arg(arg: Option<&str>) -> SettingsCategory {
    match arg.unwrap_or("appearance") {
        "files" => SettingsCategory::Files,
        "layout" => SettingsCategory::Layout,
        "about" => SettingsCategory::About,
        _ => SettingsCategory::Appearance,
    }
}
