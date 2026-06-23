//! Settings — Phase 3 of the next-level plan adopts gpui-component's
//! [`gpui_component::setting::Settings`] primitive. The library ships
//! a hierarchical Settings (pages → groups → items → fields) with
//! a sidebar, **built-in search**, optional reset, and the same field
//! types we used to hand-roll (switch / dropdown / number-input /
//! custom render). The brief's "search at the top of the sidebar"
//! ask comes for free.
//!
//! External surface preserved so [`crate::main`], [`crate::shell`],
//! and [`crate::screenshot`] don't have to change:
//! - [`SettingsView`] entity (`SettingsView::new(SettingsCategory)`)
//! - [`SettingsCategory`] (the `--settings <page>` argument decodes
//!   to one of its variants via [`category_from_arg`])
//! - [`ThemePref`] (Light / Dark / System)
//! - [`open_settings_window`] (Cmd+, second-window opener)
//!
//! Internally everything below is now thin glue around the primitive.

use crate::text::TextScale as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{Axis, *};
use gpui_component::{
    ActiveTheme, Icon, Root, Theme, ThemeMode,
    color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState},
    setting::{SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage, Settings},
};

use feraille_core::commands::{Category, all_commands};

use crate::app_state::{self, AppState};

// =============================================================================
// Categories — external API
// =============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsCategory {
    Appearance,
    Files,
    SearchDupes,
    Layout,
    Plugins,
    Shortcuts,
    Diagnostics,
    About,
}

impl SettingsCategory {
    pub const ALL: &'static [SettingsCategory] = &[
        SettingsCategory::Appearance,
        SettingsCategory::Files,
        SettingsCategory::SearchDupes,
        SettingsCategory::Layout,
        SettingsCategory::Plugins,
        SettingsCategory::Shortcuts,
        SettingsCategory::Diagnostics,
        SettingsCategory::About,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SettingsCategory::Appearance => "Appearance",
            SettingsCategory::Files => "Files",
            SettingsCategory::SearchDupes => "Search & Duplicates",
            SettingsCategory::Plugins => "Plugins",
            SettingsCategory::Layout => "Layout",
            SettingsCategory::Shortcuts => "Keyboard Shortcuts",
            SettingsCategory::Diagnostics => "Diagnostics",
            SettingsCategory::About => "About",
        }
    }

    fn page_index(self) -> usize {
        match self {
            SettingsCategory::Appearance => 0,
            SettingsCategory::Files => 1,
            SettingsCategory::SearchDupes => 2,
            SettingsCategory::Layout => 3,
            SettingsCategory::Plugins => 4,
            SettingsCategory::Shortcuts => 5,
            SettingsCategory::Diagnostics => 6,
            SettingsCategory::About => 7,
        }
    }
}

pub fn category_from_arg(arg: Option<&str>) -> SettingsCategory {
    match arg.unwrap_or("appearance") {
        "files" => SettingsCategory::Files,
        "search" | "duplicates" | "dupes" => SettingsCategory::SearchDupes,
        "layout" => SettingsCategory::Layout,
        "plugins" | "plugin" => SettingsCategory::Plugins,
        "shortcuts" | "keyboard" | "keys" => SettingsCategory::Shortcuts,
        "diagnostics" | "diag" | "health" | "doctor" => SettingsCategory::Diagnostics,
        "about" => SettingsCategory::About,
        _ => SettingsCategory::Appearance,
    }
}

// =============================================================================
// Theme preference — external API
// =============================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemePref {
    Light,
    Dark,
    System,
}

impl ThemePref {
    fn as_str(self) -> &'static str {
        match self {
            ThemePref::Light => "light",
            ThemePref::Dark => "dark",
            ThemePref::System => "system",
        }
    }

    fn label(self) -> &'static str {
        match self {
            ThemePref::Light => "Light",
            ThemePref::Dark => "Dark",
            ThemePref::System => "System",
        }
    }

    fn resolve(self) -> ThemeMode {
        match self {
            ThemePref::Light => ThemeMode::Light,
            ThemePref::Dark => ThemeMode::Dark,
            ThemePref::System => {
                if crate::platform_shell::system_is_dark() {
                    ThemeMode::Dark
                } else {
                    ThemeMode::Light
                }
            }
        }
    }

    fn load() -> Self {
        match app_state::load().theme_pref.as_deref() {
            Some("light") => ThemePref::Light,
            Some("dark") => ThemePref::Dark,
            _ => ThemePref::System,
        }
    }
}

fn persist_theme_pref(value: &str) {
    let existing = app_state::load();
    app_state::save(&AppState {
        theme_pref: Some(value.to_string()),
        ..existing
    });
}

fn persist_show_hidden(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        show_hidden: Some(value),
        ..existing
    });
}

fn persist_show_thumbnails(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        show_thumbnails: Some(value),
        ..existing
    });
}

pub(crate) fn persist_view_mode(value: &str) {
    let existing = app_state::load();
    app_state::save(&AppState {
        view_mode: Some(value.to_string()),
        ..existing
    });
}

pub(crate) fn persist_icon_size(value: u32) {
    let existing = app_state::load();
    app_state::save(&AppState {
        icon_size: Some(crate::grid::clamp_icon_size(value)),
        ..existing
    });
}

fn persist_ui_scale(value: f32) {
    let existing = app_state::load();
    app_state::save(&AppState {
        ui_scale: Some(value.clamp(0.6, 2.0)),
        ..existing
    });
}

fn persist_search_engine(value: &str) {
    let existing = app_state::load();
    app_state::save(&AppState {
        search_engine: Some(value.to_string()),
        ..existing
    });
}

fn persist_search_match_path(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        search_match_path: Some(value),
        ..existing
    });
}

fn persist_search_include_hidden(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        search_include_hidden: Some(value),
        ..existing
    });
}

fn persist_dupe_presentation(value: &str) {
    let existing = app_state::load();
    app_state::save(&AppState {
        dupe_presentation: Some(value.to_string()),
        ..existing
    });
}

fn persist_dupe_min_size_mb(value: u64) {
    let existing = app_state::load();
    app_state::save(&AppState {
        dupe_min_size_mb: Some(value.min(4096)),
        ..existing
    });
}

fn persist_dupe_skip_cloud(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        dupe_skip_cloud: Some(value),
        ..existing
    });
}

fn persist_dupe_include_packages(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        dupe_include_packages: Some(value),
        ..existing
    });
}

fn persist_dupe_paranoid(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        dupe_paranoid: Some(value),
        ..existing
    });
}

// =============================================================================
// SettingsView entity — external API
// =============================================================================

pub struct SettingsView {
    category: SettingsCategory,
    /// Cached count of dotfiles in `$HOME`, used by the Files page
    /// description. Captured once at construction so the
    /// `build_pages()` call inside `Render::render` doesn't keep
    /// re-reading the home directory on every paint — re-renders
    /// happen on every search-input keystroke and every page-nav
    /// click, which would otherwise pile up sync I/O on the UI
    /// thread. `None` when `$HOME` is unset (sandbox / CI).
    home_hidden_count: Option<usize>,
    /// The Appearance page's selection-color picker. A `ColorPicker` is
    /// a stateful entity (focus / popup / HSL sliders), so the view owns
    /// one and re-renders a stateless `ColorPicker` element over it each
    /// frame. A subscription set up in [`SettingsView::new`] persists +
    /// pushes each change into the live `SelectionAccent` global.
    selection_picker: Entity<ColorPickerState>,
    /// The Appearance page's Ant Trail base-color picker. Same owned-
    /// state pattern as `selection_picker`; its subscription updates the
    /// live `AntTrailColor` global and persists each change, so the file
    /// list and grid recolor at once.
    ant_trail_picker: Entity<ColorPickerState>,
    /// The Diagnostics page's health report, computed once when the settings
    /// window opens (same one-time-I/O-in-`new` pattern as
    /// `home_hidden_count`). Reopening Settings re-runs the checks. `Rc` so the
    /// per-frame page-render closures can share it cheaply.
    diagnostics: std::rc::Rc<crate::diagnostics::Report>,
}

impl SettingsView {
    pub fn new(initial: SettingsCategory, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Seed the picker from the persisted color (works even on the
        // standalone settings-window boot path, where no Shell has
        // seeded the live global yet), falling back to the live accent.
        let initial_color = app_state::load()
            .selection_color
            .as_deref()
            .and_then(crate::selection_colors::parse_hex)
            .unwrap_or_else(|| crate::selection_colors::accent(cx));
        let selection_picker =
            cx.new(|cx| ColorPickerState::new(window, cx).default_value(initial_color));

        // Push every change straight into the live global (open windows
        // repaint at once, like the thumbnail toggle) and persist it.
        cx.subscribe(
            &selection_picker,
            |_this, _picker, event: &ColorPickerEvent, cx| {
                let ColorPickerEvent::Change(color) = event;
                cx.set_global(crate::selection_colors::SelectionAccent(*color));
                persist_selection_color(*color);
            },
        )
        .detach();

        // Ant Trail picker — same shape. Seed from the persisted hex,
        // falling back to the original warm orange so an untouched
        // profile shows the stock tint in the swatch.
        let initial_trail = app_state::load()
            .ant_trail_color
            .as_deref()
            .and_then(crate::selection_colors::parse_hex)
            .unwrap_or_else(crate::ant_trail::default_base);
        let ant_trail_picker =
            cx.new(|cx| ColorPickerState::new(window, cx).default_value(initial_trail));
        cx.subscribe(
            &ant_trail_picker,
            |_this, _picker, event: &ColorPickerEvent, cx| {
                let ColorPickerEvent::Change(color) = event;
                cx.set_global(crate::ant_trail::AntTrailColor(*color));
                persist_ant_trail_color(*color);
            },
        )
        .detach();

        Self {
            category: initial,
            home_hidden_count: count_home_hidden_items(),
            selection_picker,
            ant_trail_picker,
            diagnostics: std::rc::Rc::new(crate::diagnostics::run_checks()),
        }
    }
}

fn persist_selection_color(color: Option<Hsla>) {
    let existing = app_state::load();
    app_state::save(&AppState {
        selection_color: color.map(crate::selection_colors::to_hex),
        ..existing
    });
}

fn persist_ant_trail_color(color: Option<Hsla>) {
    let existing = app_state::load();
    app_state::save(&AppState {
        ant_trail_color: color.map(crate::selection_colors::to_hex),
        ..existing
    });
}

fn persist_ant_trail_enabled(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        ant_trail_enabled: Some(value),
        ..existing
    });
}

fn persist_exclude_favorites_from_tracking(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        exclude_favorites_from_tracking: Some(value),
        ..existing
    });
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Settings::new("feraille-settings")
            .pages(build_pages(
                self.home_hidden_count,
                &self.selection_picker,
                &self.ant_trail_picker,
                &self.diagnostics,
            ))
            .default_selected_index(SelectIndex {
                page_ix: self.category.page_index(),
                group_ix: None,
            })
    }
}

/// Open a second native window hosting the SettingsView. Same shape
/// as the prior implementation — Cmd+, in Shell calls this; the menu-
/// bar `Settings…` item routes through the app-level OpenSettings
/// handler in main.rs which also calls this.
pub fn open_settings_window(cx: &mut App) {
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(820.0), px(580.0)), cx)),
        // Give the window a proper "Settings" title (it had none). A plain OS
        // titlebar suits this dialog — the brand/custom titlebar is for the
        // main browser window.
        titlebar: Some(gpui::TitlebarOptions {
            title: Some(SharedString::from("Settings")),
            ..Default::default()
        }),
        ..Default::default()
    };
    cx.spawn(async move |cx| {
        cx.open_window(opts, |window, cx| {
            let view = cx.new(|cx| SettingsView::new(SettingsCategory::Appearance, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("failed to open settings window");
    })
    .detach();
}

// =============================================================================
// Page builders
// =============================================================================

/// A dropdown setting laid out the way the stock `SettingItem` can't: the
/// label and the control share one line (label left, dropdown right) while
/// the description spans the **full width** below them — instead of being
/// squeezed into the narrow left column the horizontal layout gives it.
///
/// The control is a `small`, width-capped dropdown so its text matches our
/// density (the stock field renders at the page's default size, which is
/// too large here) and a long option label can't spill past the panel
/// edge and clip.
///
/// `get` returns the current stored value; `persist` writes the picked
/// value. Both are plain `fn` pointers (the getters/setters capture
/// nothing — they read/write `app_state`). Most dropdowns only persist;
/// when a pick also has to apply live (recompute a global), use
/// [`dropdown_setting_with`], which hands the setter `&mut App`.
fn dropdown_setting(
    title: &'static str,
    description: &'static str,
    options: &'static [(&'static str, &'static str)],
    // Option values rendered greyed and unselectable (e.g. a provider this
    // build can't honour). Empty for an unrestricted dropdown.
    disabled: &'static [&'static str],
    get: fn() -> String,
    persist: fn(&str),
) -> SettingItem {
    // Persist, then repaint so the button reflects the pick. Live settings
    // skip this wrapper and pass their own `on_pick` to recompute a global.
    dropdown_setting_with(title, description, options, disabled, get, move |value, cx| {
        persist(value);
        cx.refresh_windows();
    })
}

/// The shared dropdown rendering behind [`dropdown_setting`], parameterised by
/// what a pick does. `on_pick` runs with `&mut App`, so a live setting can
/// recompute a global and repaint — not just persist. `Copy` so every menu
/// item can capture its own copy.
fn dropdown_setting_with<F: Fn(&str, &mut App) + Copy + 'static>(
    title: &'static str,
    description: &'static str,
    options: &'static [(&'static str, &'static str)],
    disabled: &'static [&'static str],
    get: fn() -> String,
    on_pick: F,
) -> SettingItem {
    SettingItem::render(move |_options, _window, cx| {
        use gpui_component::{
            ActiveTheme as _, Sizable as _,
            button::Button,
            menu::{DropdownMenu as _, PopupMenuItem},
        };
        let current = get();
        let current_label = options
            .iter()
            .find(|(value, _)| *value == current.as_str())
            .map(|(_, label)| *label)
            .unwrap_or("");
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;

        gpui_component::v_flex()
            .w_full()
            .gap_1()
            .child(
                gpui_component::h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .gap_3()
                    .child(div().flex_shrink_0().text_scale_sm().text_color(fg).child(title))
                    .child(
                        Button::new(SharedString::from(format!("dd-{title}")))
                            .label(current_label)
                            .dropdown_caret(true)
                            .outline()
                            .small()
                            .max_w(px(260.0))
                            .dropdown_menu_with_anchor(
                                gpui::Anchor::TopRight,
                                move |menu, _window, _cx| {
                                    options.iter().fold(menu, |menu, opt| {
                                        let (value, label) = *opt;
                                        let checked = value == current.as_str();
                                        // A disabled item is greyed and its
                                        // click handler is dropped by the menu
                                        // (see PopupMenuItem render), so it
                                        // can't be selected.
                                        menu.item(
                                            PopupMenuItem::new(label)
                                                .checked(checked)
                                                .disabled(disabled.contains(&value))
                                                .on_click(move |_, _window: &mut Window, cx: &mut App| {
                                                    // `on_pick` persists and repaints
                                                    // (and, for a live setting, also
                                                    // recomputes its global). The
                                                    // refresh inside must hit every
                                                    // window — this fires in the popup,
                                                    // so a window-local refresh would
                                                    // repaint the popup, not the page
                                                    // behind it.
                                                    on_pick(value, cx);
                                                }),
                                        )
                                    })
                                },
                            ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .text_scale_sm()
                    .text_color(muted)
                    .child(description),
            )
    })
}

/// A boolean setting laid out like [`dropdown_setting`]: the title and the
/// switch share the top line, and the **description spans the full width**
/// below them — rather than being squeezed into the stock `SettingItem`'s
/// narrow left column next to the control. `value` reads the current state;
/// `set_value` persists a toggle.
fn switch_setting(
    title: &'static str,
    description: impl Into<SharedString>,
    value: impl Fn(&App) -> bool + 'static,
    set_value: impl Fn(bool, &mut App) + 'static,
) -> SettingItem {
    let description = description.into();
    // The SettingItem render closure is `Fn` (re-invoked each frame), so the
    // setter (moved into the switch's `on_click`) must be shareable: `Rc` it
    // and hand each render a clone.
    let set_value = std::rc::Rc::new(set_value);
    SettingItem::render(move |_options, _window, cx| {
        use gpui_component::{ActiveTheme as _, Sizable as _, switch::Switch};

        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let checked = value(cx);
        let set_value = set_value.clone();
        let description = description.clone();

        gpui_component::v_flex()
            .w_full()
            .gap_1()
            .child(
                gpui_component::h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .gap_3()
                    .child(div().flex_shrink_0().text_scale_sm().text_color(fg).child(title))
                    .child(
                        Switch::new(SharedString::from(format!("sw-{title}")))
                            .checked(checked)
                            .small()
                            .on_click(move |checked: &bool, _window: &mut Window, cx: &mut App| {
                                set_value(*checked, cx);
                                // The Switch is controlled by `checked`, re-read
                                // from app state on the next render — so we must
                                // request one or the toggle never visibly moves.
                                // refresh_windows() is the same call the theme
                                // tiles use; it repaints every window. (macOS
                                // repaints eagerly per event; Windows does not.)
                                cx.refresh_windows();
                            }),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .text_scale_sm()
                    .text_color(muted)
                    .child(description),
            )
    })
}

fn build_pages(
    home_hidden_count: Option<usize>,
    selection_picker: &Entity<ColorPickerState>,
    ant_trail_picker: &Entity<ColorPickerState>,
    diagnostics: &std::rc::Rc<crate::diagnostics::Report>,
) -> Vec<SettingPage> {
    vec![
        appearance_page(selection_picker.clone(), ant_trail_picker.clone()),
        files_page(home_hidden_count),
        search_dupes_page(),
        layout_page(),
        plugins_page(),
        shortcuts_page(),
        diagnostics_page(diagnostics.clone()),
        about_page(),
    ]
}

/// The Diagnostics page: the health-check report grouped by area, the recent
/// activity trail, and a "Copy report" button. The report is computed once in
/// [`SettingsView::new`]; this only renders it. `feraille --doctor` prints the
/// same report from a terminal.
fn diagnostics_page(report: std::rc::Rc<crate::diagnostics::Report>) -> SettingPage {
    use crate::diagnostics::Status;

    let mut page =
        SettingPage::new("Diagnostics").icon(Icon::empty().path("icons/activity.svg"));

    // Summary header.
    {
        let report = report.clone();
        page = page.group(SettingGroup::new().item(SettingItem::render(move |_o, _w, cx| {
            let (ok, warn, fail) = report.tally();
            let fg = cx.theme().foreground;
            let muted = cx.theme().muted_foreground;
            gpui_component::v_flex()
                .w_full()
                .gap_1()
                .child(div().text_scale_sm().text_color(fg).child(format!(
                    "Feraille v{} · {}/{} · {ok} OK, {warn} WARN, {fail} FAIL",
                    report.app_version, report.os, report.arch
                )))
                .child(div().w_full().text_scale_xs().text_color(muted).child(
                    "Health check of the app's storage and environment. \
                     Run `feraille --doctor` for the same report from a terminal.",
                ))
        })));
    }

    // One group per check group, one row per check.
    for (gi, group) in report.groups.iter().enumerate() {
        let mut sg = SettingGroup::new().title(group.title);
        for ci in 0..group.checks.len() {
            let report = report.clone();
            sg = sg.item(SettingItem::render(move |_o, _w, cx| {
                let check = &report.groups[gi].checks[ci];
                let (tag_color, tag) = match check.status {
                    Status::Ok => (gpui::rgb(0x16a34a), "OK"),
                    Status::Warn => (gpui::rgb(0xd97706), "WARN"),
                    Status::Fail => (gpui::rgb(0xdc2626), "FAIL"),
                };
                let fg = cx.theme().foreground;
                let muted = cx.theme().muted_foreground;
                gpui_component::v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        gpui_component::h_flex()
                            .w_full()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .w(px(38.0))
                                    .text_scale_xs()
                                    .text_color(tag_color)
                                    .child(tag),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_scale_sm()
                                    .text_color(fg)
                                    .child(SharedString::from(check.name.clone())),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_scale_xs()
                            .text_color(muted)
                            .child(SharedString::from(check.detail.clone())),
                    )
            }));
        }
        page = page.group(sg);
    }

    // Recent activity trail (last ~20 events).
    page = page.group(
        SettingGroup::new()
            .title("Activity trail")
            .item(SettingItem::render(move |_o, _w, cx| {
                let muted = cx.theme().muted_foreground;
                let lines = crate::trail::render_lines();
                let body = if lines.is_empty() {
                    "No activity recorded yet.".to_string()
                } else {
                    let start = lines.len().saturating_sub(20);
                    lines[start..].join("\n")
                };
                div()
                    .w_full()
                    .text_scale_xs()
                    .text_color(muted)
                    .child(SharedString::from(body))
            })),
    );

    // Copy-report action.
    page = page.group(SettingGroup::new().item(SettingItem::render(move |_o, _w, _cx| {
        use gpui_component::{Sizable as _, button::Button};
        let report = report.clone();
        gpui_component::h_flex().w_full().gap_2().child(
            Button::new("diag-copy")
                .label("Copy report")
                .outline()
                .small()
                .on_click(move |_, _w, _cx| {
                    let mut text = crate::diagnostics::render_text(&report);
                    let trail = crate::trail::render_lines();
                    if !trail.is_empty() {
                        text.push_str("\n[Activity trail]\n");
                        for l in &trail {
                            text.push_str(l);
                            text.push('\n');
                        }
                    }
                    crate::platform_shell::copy_to_clipboard(&text);
                }),
        )
        .child(
            Button::new("diag-report")
                .label("Create report bundle\u{2026}")
                .outline()
                .small()
                .on_click(|_, window, _cx| crate::report::open_reporter(window)),
        )
    })));

    page
}

fn search_dupes_page() -> SettingPage {
    SettingPage::new("Search & Duplicates")
        .icon(Icon::empty().path("icons/search.svg"))
        // ---- Search engine ----
        .group(
            SettingGroup::new()
                .title("Search")
                .item(dropdown_setting(
                    "Search engine",
                    "Automatic uses Spotlight's live index when available \u{2014} instant, \
                     content-aware, near-zero CPU \u{2014} and falls back to the built-in \
                     recursive walker where Spotlight is disabled or blind (some external / \
                     network volumes). Force one if you prefer.",
                    &[
                        ("auto", "Automatic (recommended)"),
                        ("spotlight", "Spotlight"),
                        ("walker", "Built-in walker"),
                    ],
                    &[],
                    || {
                        app_state::load()
                            .search_engine
                            .unwrap_or_else(|| "auto".into())
                    },
                    persist_search_engine,
                ))
                .item(switch_setting(
                    "Match full path",
                    "Match the relative path, not just the file name.",
                    |_cx: &App| app_state::load().search_match_path.unwrap_or(false),
                    |val: bool, _cx: &mut App| persist_search_match_path(val),
                ))
                .item(switch_setting(
                    "Include hidden files",
                    "Search dot-files and otherwise-hidden items too.",
                    |_cx: &App| {
                        let s = app_state::load();
                        s.search_include_hidden.or(s.show_hidden).unwrap_or(false)
                    },
                    |val: bool, _cx: &mut App| persist_search_include_hidden(val),
                )),
        )
        // ---- Duplicate finder ----
        .group(
            SettingGroup::new()
                .title("Duplicate finder")
                .item(dropdown_setting(
                    "Results view",
                    "How duplicate groups are shown. Grouped rows reuse the file list \
                     (selection, sort, preview, context menu); the dedicated panel offers \
                     group-level actions like keep-newest.",
                    &[
                        ("grouped", "Grouped rows in a tab"),
                        ("panel", "Dedicated panel"),
                    ],
                    &[],
                    || {
                        app_state::load()
                            .dupe_presentation
                            .unwrap_or_else(|| "grouped".into())
                    },
                    persist_dupe_presentation,
                ))
                .item(dropdown_setting(
                    "Ignore small files",
                    "Skip files below this size \u{2014} the big wins are large files.",
                    &[
                        ("0", "Compare all files"),
                        ("1", "Skip under 1 MB"),
                        ("10", "Skip under 10 MB"),
                        ("100", "Skip under 100 MB"),
                    ],
                    &[],
                    || app_state::load().dupe_min_size_mb.unwrap_or(0).to_string(),
                    |v| persist_dupe_min_size_mb(v.parse().unwrap_or(0)),
                ))
                .item(switch_setting(
                    "Skip cloud placeholders",
                    "Don't download undownloaded iCloud files just to hash them.",
                    |_cx: &App| app_state::load().dupe_skip_cloud.unwrap_or(true),
                    |val: bool, _cx: &mut App| persist_dupe_skip_cloud(val),
                ))
                .item(switch_setting(
                    "Compare inside app bundles",
                    "Descend into .app / .bundle packages and compare their inner files. \
                     Off keeps packages opaque.",
                    |_cx: &App| app_state::load().dupe_include_packages.unwrap_or(false),
                    |val: bool, _cx: &mut App| persist_dupe_include_packages(val),
                ))
                .item(switch_setting(
                    "Byte-for-byte verify",
                    "Confirm each match byte-for-byte after hashing. Removes any \
                     hash-collision doubt at the cost of re-reading confirmed groups.",
                    |_cx: &App| app_state::load().dupe_paranoid.unwrap_or(false),
                    |val: bool, _cx: &mut App| persist_dupe_paranoid(val),
                )),
        )
}

fn appearance_page(
    selection_picker: Entity<ColorPickerState>,
    ant_trail_picker: Entity<ColorPickerState>,
) -> SettingPage {
    SettingPage::new("Appearance")
        .icon(Icon::empty().path("icons/palette.svg"))
        .group(
            SettingGroup::new().title("Theme").item(
                // Vertical layout so the three fixed-width tiles drop
                // below the title rather than competing with it for
                // horizontal space — previous default-horizontal layout
                // clipped the System tile on the right edge.
                SettingItem::new(
                    "Theme",
                    SettingField::render(|_options, _window, _cx| {
                        theme_tile_strip().into_any_element()
                    }),
                )
                .layout(Axis::Vertical)
                .description("Match the system, or pick a side."),
            ),
        )
        .group(
            SettingGroup::new().title("Selection").item(
                // The picker is a stateful entity owned by `SettingsView`;
                // here we render a fresh stateless `ColorPicker` over it
                // each frame. Changes flow through the entity's
                // `ColorPickerEvent::Change` subscription (set up in
                // `SettingsView::new`), which updates the live global and
                // persists — so the file list and grid recolor at once.
                SettingItem::new(
                    "Selection color",
                    SettingField::render(move |_options, _window, _cx| {
                        ColorPicker::new(&selection_picker).into_any_element()
                    }),
                )
                .description(
                    "The highlight behind selected files in the list and grid. \
                     Clear it to follow the theme's blue.",
                ),
            ),
        )
        .group(
            SettingGroup::new()
                .title("Ant Trail")
                // Master switch. Persists *and* updates the live global so the
                // tint appears/vanishes in open windows without a relaunch.
                .item(switch_setting(
                    "Show Ant Trail",
                    "Tint your most-visited folders so the ones you open most stand out. \
                     Off hides the tint entirely \u{2014} your visit history still feeds \
                     Recents.",
                    |cx: &App| crate::ant_trail::enabled(cx),
                    |val: bool, cx: &mut App| {
                        persist_ant_trail_enabled(val);
                        cx.set_global(crate::ant_trail::AntTrailEnabled(val));
                    },
                ))
                .item(
                    // Same owned-picker pattern as Selection above; the
                    // entity's `ColorPickerEvent::Change` subscription
                    // (set up in `SettingsView::new`) updates the live
                    // `AntTrailColor` global and persists.
                    SettingItem::new(
                        "Ant Trail color",
                        SettingField::render(move |_options, _window, _cx| {
                            ColorPicker::new(&ant_trail_picker).into_any_element()
                        }),
                    )
                    .description(
                        "The tint behind your most-visited folders in the list and grid. \
                         Brightness still tracks visit frequency. Clear it for the stock orange.",
                    ),
                )
                // Persists *and* updates the live policy global so the change
                // takes effect on the next favorite click without a relaunch.
                .item(switch_setting(
                    "Don't track favorites",
                    "When on, opening a folder from your Favorites doesn't count toward \
                     its Ant Trail heat or add it to Recents \u{2014} so deliberate \
                     shortcuts don't crowd out folders you actually browse to. Reaching \
                     the same folder any other way still counts.",
                    |_cx: &App| {
                        app_state::load()
                            .exclude_favorites_from_tracking
                            .unwrap_or(true)
                    },
                    |val: bool, cx: &mut App| {
                        persist_exclude_favorites_from_tracking(val);
                        cx.set_global(crate::ant_trail::ExcludeFavoritesFromTracking(val));
                    },
                )),
        )
}

fn files_page(home_hidden_count: Option<usize>) -> SettingPage {
    // Description prefers the live count; the explanatory fallback
    // covers sandbox/CI runs where $HOME can't be read.
    // Copy intentionally implies "next launch" — Show Hidden writes
    // app_state but doesn't push the change into already-open Shell
    // windows. A shared-observer rewire is on the Phase 10 audit
    // list; in the meantime the wording matches reality.
    let description = match home_hidden_count {
        Some(n) if n > 0 => format!(
            "Reveal items that start with a dot \u{2014} {} in your home folder. Takes effect on next launch.",
            n
        ),
        _ => "Reveal items that start with a dot, like .config and .ssh. Takes effect on next launch.".to_string(),
    };
    let page = SettingPage::new("Files")
        .icon(Icon::empty().path("icons/folder.svg"))
        .group(
            SettingGroup::new()
                .title("Visibility")
                .item(switch_setting(
                    "Show hidden files",
                    description,
                    |_cx: &App| app_state::load().show_hidden.unwrap_or(false),
                    |val: bool, _cx: &mut App| persist_show_hidden(val),
                ))
                // Persists *and* updates the live process global, so flipping
                // it repaints open windows at once (no relaunch).
                .item(switch_setting(
                    "Show thumbnails",
                    "Preview photos, videos, and PDFs as their actual content in the \
                     file list. Off shows generic type icons.",
                    |cx: &App| crate::thumbnails::show_thumbnails(cx),
                    |val: bool, cx: &mut App| {
                        persist_show_thumbnails(val);
                        cx.set_global(crate::thumbnails::ShowThumbnails(val));
                    },
                )),
        );
    // Sidebar special-folder root — only meaningful where OneDrive's
    // Known-Folder-Move can split a folder between local and cloud, i.e.
    // Windows. Omitted elsewhere.
    #[cfg(target_os = "windows")]
    let page =
        page.group(SettingGroup::new().title("Locations").item(locations_mode_setting()));
    page
}

/// The sidebar Locations root dropdown (Windows / OneDrive). Picks which copy
/// of a moved folder the sidebar points at; applies live via the
/// `ResolvedLocations` global so open windows' sidebars update without a
/// relaunch. See [`crate::special_folders`].
#[cfg(target_os = "windows")]
fn locations_mode_setting() -> SettingItem {
    dropdown_setting_with(
        "Special folders",
        "When OneDrive moves your Desktop, Documents, or Pictures into the cloud it often \
         leaves a local copy behind, so \u{201C}where is my Documents?\u{201D} has two answers. \
         Automatic follows Windows (cloud where it moved them, local otherwise). Local prefers \
         your %USERPROFILE% copy; OneDrive prefers the OneDrive copy \u{2014} each falls back to \
         the other when its copy doesn\u{2019}t exist, so a shortcut never opens to nothing.",
        &[
            ("auto", "Automatic (recommended)"),
            ("local", "Local profile"),
            ("onedrive", "OneDrive"),
        ],
        &[],
        || {
            app_state::load()
                .special_folder_mode
                .unwrap_or_else(|| "auto".into())
        },
        |value: &str, cx: &mut App| {
            crate::special_folders::persist_and_apply(
                feraille_fs_native::paths::SpecialFolderMode::from_str(value),
                cx,
            );
        },
    )
}

fn layout_page() -> SettingPage {
    SettingPage::new("Layout")
        .icon(Icon::empty().path("icons/settings-2.svg"))
        .group(SettingGroup::new().title("Interface").item(dropdown_setting(
            "UI scale",
            "Overall interface zoom. Restart the app or open a new window for the change to apply.",
            &[
                ("0.85", "Small (85%)"),
                ("1.00", "Default (100%)"),
                ("1.15", "Medium (115%)"),
                ("1.30", "Large (130%)"),
            ],
            &[],
            || format!("{:.2}", app_state::load().ui_scale.unwrap_or(1.0)),
            |v| persist_ui_scale(v.parse().unwrap_or(1.0)),
        )))
}

fn persist_video_backend(value: &str) {
    let existing = app_state::load();
    app_state::save(&AppState {
        video_backend: Some(value.to_string()),
        ..existing
    });
}

fn persist_vlc_app_path(value: &str) {
    let existing = app_state::load();
    let v = value.trim();
    app_state::save(&AppState {
        vlc_app_path: (!v.is_empty()).then(|| v.to_string()),
        ..existing
    });
}

fn plugins_page() -> SettingPage {
    // The VLC provider is only compiled in with the `vlc` feature. In a stock
    // build, grey the "VLC" option out (unselectable) and say why in the
    // description, so it's discoverable but can't be picked into a no-op.
    let (player_desc, player_disabled): (&'static str, &'static [&'static str]) =
        if cfg!(feature = "vlc") {
            (
                "The built-in player uses the platform's native media frameworks \
                 (AVFoundation on macOS, Media Foundation on Windows). VLC plays virtually \
                 any container/codec and applies colour adjustments to the video itself. \
                 VLC must be installed; a change takes effect on the next viewer window.",
                &[],
            )
        } else {
            (
                "The built-in player uses the platform's native media frameworks. \
                 VLC plays virtually any container/codec, but this build was compiled \
                 without the `vlc` feature, so VLC is unavailable \u{2014} rebuild with \
                 `cargo run --bin feraille-gpui --features vlc` (with VLC installed) \
                 to enable it.",
                &["vlc"],
            )
        };
    SettingPage::new("Plugins")
        .icon(Icon::empty().path("icons/settings.svg"))
        .group(
            SettingGroup::new()
                .title("Video player")
                .item(dropdown_setting(
                    "Player",
                    player_desc,
                    &[("builtin", "Built-in"), ("vlc", "VLC")],
                    player_disabled,
                    || {
                        app_state::load()
                            .video_backend
                            .unwrap_or_else(|| "builtin".into())
                    },
                    persist_video_backend,
                ))
                .item(
                    SettingItem::new(
                        "VLC location",
                        SettingField::input(
                            |_cx: &App| {
                                SharedString::from(
                                    app_state::load().vlc_app_path.unwrap_or_else(|| {
                                        crate::viewer::backend_native::default_vlc_path().into()
                                    }),
                                )
                            },
                            |val: SharedString, _cx: &mut App| persist_vlc_app_path(val.as_ref()),
                        ),
                    )
                    .description(
                        "Where libvlc is loaded from — a VLC.app bundle on macOS, the VLC \
                         install folder on Windows/Linux (e.g. C:\\Program Files\\VideoLAN\\VLC).",
                    ),
                ),
        )
}

fn shortcuts_page() -> SettingPage {
    // Each catalogue command becomes its own SettingItem so the
    // primitive's built-in search filters across them. Category goes
    // into the description so typing "view" finds the View-category
    // commands too. Catalogue order preserved.
    let mut groups_by_cat: Vec<(Category, Vec<SettingItem>)> = Vec::new();
    for spec in all_commands() {
        if spec.shortcuts.is_empty() {
            continue;
        }
        let chord = spec
            .shortcuts
            .first()
            .map(crate::keyboard_help::format_shortcut)
            .unwrap_or_default();
        let title = SharedString::from(spec.title);
        let cat_name = SharedString::from(category_name(spec.category));
        let chord_for_render = chord.clone();
        let item = SettingItem::new(
            title,
            SettingField::render(move |_options, _window, cx| {
                let theme = cx.theme();
                gpui_component::h_flex()
                    .justify_end()
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded(theme.radius)
                            .bg(theme.muted.opacity(0.6))
                            .border_1()
                            .border_color(theme.border)
                            .text_scale_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(chord_for_render.clone())),
                    )
                    .into_any_element()
            }),
        )
        .description(format!("{cat_name} \u{00B7} {chord}"));
        if let Some((_, items)) = groups_by_cat.iter_mut().find(|(c, _)| *c == spec.category) {
            items.push(item);
        } else {
            groups_by_cat.push((spec.category, vec![item]));
        }
    }

    let mut page =
        SettingPage::new("Keyboard Shortcuts").icon(Icon::empty().path("icons/keyboard.svg"));
    for (cat, items) in groups_by_cat {
        let title = category_name(cat);
        let mut group = SettingGroup::new().title(title);
        for item in items {
            group = group.item(item);
        }
        page = page.group(group);
    }
    page
}

fn about_page() -> SettingPage {
    SettingPage::new("About")
        .icon(Icon::empty().path("icons/info.svg"))
        .group(
            SettingGroup::new().item(SettingItem::render(|_options, _window, cx| {
                let theme = cx.theme();
                gpui_component::v_flex()
                    .gap_2()
                    .py_4()
                    .items_center()
                    .child(
                        div()
                            .text_scale_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child("Feraille"),
                    )
                    .child(div().text_scale_xs().text_color(theme.muted_foreground).child(
                        SharedString::from(concat!("Version ", env!("CARGO_PKG_VERSION"))),
                    ))
                    .child(
                        div()
                            .mt_2()
                            .text_scale_sm()
                            .text_color(theme.foreground)
                            .child("The macOS port of Ferail — a Finder-class file explorer."),
                    )
                    .child(
                        div()
                            .text_scale_xs()
                            .text_color(theme.muted_foreground)
                            .child("Built for speed, predictability, and a calm UI."),
                    )
                    .into_any_element()
            })),
        )
}

fn category_name(c: Category) -> &'static str {
    match c {
        Category::App => "App",
        Category::File => "File",
        Category::Edit => "Edit",
        Category::View => "View",
        Category::Go => "Go",
        Category::Selection => "Selection",
        Category::Window => "Window",
        Category::Help => "Help",
        Category::Context => "Context",
    }
}

// =============================================================================
// Theme tile strip — visual reinforcement of the Theme dropdown
// =============================================================================

fn theme_tile_strip() -> impl IntoElement {
    gpui_component::h_flex().gap_3().children([
        theme_tile(ThemePref::Light),
        theme_tile(ThemePref::Dark),
        theme_tile(ThemePref::System),
    ])
}

fn theme_tile(pref: ThemePref) -> impl IntoElement {
    let id = ElementId::Name(format!("theme-tile-{}", pref.as_str()).into());
    div()
        .id(id)
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            persist_theme_pref(pref.as_str());
            // Pass `Some(window)` so this window repaints with the
            // new palette immediately; `Theme::change` only refreshes
            // when given a Window. `cx.refresh_windows()` propagates
            // the same change to any other open window (e.g. the
            // main Shell while Settings is open in a second window).
            let resolved = pref.resolve();
            Theme::change(resolved, Some(window), cx);
            // Match native chrome across all windows to the new theme.
            crate::platform_shell::set_app_appearance(matches!(
                resolved,
                gpui_component::ThemeMode::Dark
            ));
            cx.refresh_windows();
        })
        .child(theme_tile_body(pref))
}

fn theme_tile_body(pref: ThemePref) -> impl IntoElement {
    use gpui::rgb;
    // Hard-coded mock palette per tile — each tile shows the OTHER
    // theme so the user sees the consequence of clicking. System
    // tile is split-rendered Light/Dark.
    let (bg, panel, accent, fg) = match pref {
        ThemePref::Light | ThemePref::System => {
            (rgb(0xFAFAFA), rgb(0xF0F0F0), rgb(0x2A63D9), rgb(0x1A1A1A))
        }
        ThemePref::Dark => (rgb(0x1B1B1B), rgb(0x252525), rgb(0x2457CA), rgb(0xF5F5F5)),
    };
    let is_system = matches!(pref, ThemePref::System);
    let dark_bg = rgb(0x1B1B1B);
    let dark_panel = rgb(0x252525);
    let dark_fg = rgb(0xF5F5F5);

    gpui_component::v_flex()
        .relative()
        .gap_2()
        .items_center()
        .child(
            gpui_component::v_flex()
                .w(px(160.0))
                .h(px(96.0))
                .rounded(px(6.0))
                .overflow_hidden()
                .bg(bg)
                .child(
                    gpui_component::h_flex()
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
                    gpui_component::h_flex()
                        .flex_1()
                        .child(div().w(px(46.0)).h_full().bg(panel))
                        .child(
                            gpui_component::v_flex()
                                .flex_1()
                                .gap_1()
                                .pt_2()
                                .px_2()
                                .child(div().w(px(60.0)).h(px(4.0)).rounded_full().bg(fg))
                                .child(div().h(px(10.0)).w_full().rounded(px(2.0)).bg(accent))
                                .child(div().w(px(72.0)).h(px(4.0)).rounded_full().bg(fg)),
                        ),
                )
                .when(is_system, |this| {
                    this.child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .w(px(80.0))
                            .h(px(96.0))
                            .overflow_hidden()
                            .bg(dark_bg)
                            .child(
                                gpui_component::h_flex()
                                    .w_full()
                                    .h(px(14.0))
                                    .px_2()
                                    .gap_1()
                                    .items_center()
                                    .bg(dark_panel)
                                    .child(div().size(px(6.0)).rounded_full().bg(rgb(0xFF6057)))
                                    .child(div().size(px(6.0)).rounded_full().bg(rgb(0xFFBD2E)))
                                    .child(div().size(px(6.0)).rounded_full().bg(rgb(0x28C940))),
                            )
                            .child(
                                gpui_component::h_flex()
                                    .flex_1()
                                    .child(div().w(px(36.0)).h_full().bg(dark_panel))
                                    .child(
                                        gpui_component::v_flex()
                                            .flex_1()
                                            .gap_1()
                                            .pt_2()
                                            .px_2()
                                            .child(
                                                div()
                                                    .w(px(36.0))
                                                    .h(px(4.0))
                                                    .rounded_full()
                                                    .bg(dark_fg),
                                            )
                                            .child(
                                                div()
                                                    .h(px(10.0))
                                                    .w_full()
                                                    .rounded(px(2.0))
                                                    .bg(rgb(0x2457CA)),
                                            )
                                            .child(
                                                div()
                                                    .w(px(28.0))
                                                    .h(px(4.0))
                                                    .rounded_full()
                                                    .bg(dark_fg),
                                            ),
                                    ),
                            ),
                    )
                }),
        )
        .child(
            gpui_component::h_flex()
                .items_center()
                .gap_1()
                // Strengthened active state — accent ring + check
                // badge — applied as a wrapper around the label.
                .child(active_state_decoration(pref))
                .child(div().text_scale_xs().child(pref.label())),
        )
}

/// Renders the selected-state badge for a theme tile. Lives next to
/// the label so the strong cue is on the textual identifier rather
/// than the artwork — matches macOS Settings convention where the
/// chosen pill is the one with a check beside it.
fn active_state_decoration(pref: ThemePref) -> impl IntoElement {
    // We can't read the current pref without &App here, so render an
    // empty 12px slot. The tile click flow handles state via persist
    // + Theme::change; the active state cue lives in the parent
    // SettingsView's re-render path (which calls back through
    // ThemePref::load).
    //
    // For Phase 3 we resolve the active state at tile *build* time
    // by reading app_state once (fast: file read of a short text).
    let active = ThemePref::load() == pref;
    use gpui::rgb;
    if active {
        div()
            .w(px(14.0))
            .h(px(14.0))
            .rounded_full()
            .bg(rgb(0x2A63D9))
            .flex()
            .items_center()
            .justify_center()
            .child(
                gpui::svg()
                    .path("icons/circle-check.svg")
                    .w(px(10.0))
                    .h(px(10.0))
                    .text_color(rgb(0xFFFFFF)),
            )
            .into_any_element()
    } else {
        div().w(px(14.0)).h(px(14.0)).into_any_element()
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Count hidden entries in the home folder, using the same platform
/// semantics as the file list (`entry_is_hidden`: dot-prefix plus
/// UF_HIDDEN on macOS / FILE_ATTRIBUTE_HIDDEN on Windows). Synchronous
/// because it runs exactly once per Files-page build; future revisions
/// can move this onto a background task with live invalidation.
fn count_home_hidden_items() -> Option<usize> {
    let home = feraille_fs_native::home_dir();
    let n = std::fs::read_dir(home)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            e.metadata()
                .map(|m| feraille_fs_native::entry_is_hidden(&name, &m))
                .unwrap_or_else(|_| name.starts_with('.'))
        })
        .count();
    Some(n)
}
