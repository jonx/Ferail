//! Settings: Phase 3 of the next-level plan adopts gpui-component's
//! setting primitive. The library ships
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
    ActiveTheme, AxisExt as _, Icon, Root, Theme, ThemeMode,
    color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState},
};

use gpui_component::setting::{
    SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};

use ferail_core::commands::{Category, all_commands};
use ferail_core::msgid;

use crate::app_state::{self, AppState};

const SETTINGS_SWITCH_LANE: f32 = 36.0;
const SETTINGS_DROPDOWN_LANE: f32 = 260.0;
const SETTINGS_CONTROL_GAP: f32 = 12.0;
const SETTINGS_TOP_ROW_HEIGHT: f32 = 28.0;

// =============================================================================
// Categories: external API
// =============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsCategory {
    Appearance,
    Files,
    Performance,
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
        SettingsCategory::Performance,
        SettingsCategory::SearchDupes,
        SettingsCategory::Layout,
        SettingsCategory::Plugins,
        SettingsCategory::Shortcuts,
        SettingsCategory::Diagnostics,
        SettingsCategory::About,
    ];

    /// The page's English title as a `msgid!` literal: translate it for
    /// display with `crate::i18n::tr_static`.
    pub fn title(self) -> &'static str {
        match self {
            SettingsCategory::Appearance => msgid!("Appearance"),
            SettingsCategory::Files => msgid!("Files"),
            SettingsCategory::Performance => msgid!("Performance"),
            SettingsCategory::SearchDupes => msgid!("Search & Duplicates"),
            SettingsCategory::Plugins => msgid!("Plugins"),
            SettingsCategory::Layout => msgid!("Layout"),
            SettingsCategory::Shortcuts => msgid!("Keyboard Shortcuts"),
            SettingsCategory::Diagnostics => msgid!("Diagnostics"),
            SettingsCategory::About => msgid!("About"),
        }
    }

    fn page_index(self) -> usize {
        match self {
            SettingsCategory::Appearance => 0,
            SettingsCategory::Files => 1,
            SettingsCategory::Performance => 2,
            SettingsCategory::SearchDupes => 3,
            SettingsCategory::Layout => 4,
            SettingsCategory::Plugins => 5,
            SettingsCategory::Shortcuts => 6,
            SettingsCategory::Diagnostics => 7,
            SettingsCategory::About => 8,
        }
    }
}

pub fn category_from_arg(arg: Option<&str>) -> SettingsCategory {
    match arg.unwrap_or("appearance") {
        "files" => SettingsCategory::Files,
        "performance" | "perf" => SettingsCategory::Performance,
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
// Theme preference: external API
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

    fn label(self) -> SharedString {
        match self {
            ThemePref::Light => tr!("Light"),
            ThemePref::Dark => tr!("Dark"),
            ThemePref::System => tr!("System"),
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

fn persist_folder_sizing(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        folder_sizing: Some(value),
        ..existing
    });
}

fn persist_file_detail_scan(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        file_detail_scan: Some(value),
        ..existing
    });
}

#[cfg(target_os = "windows")]
fn disk_usage_engine_preference() -> String {
    app_state::load()
        .disk_usage_engine
        .unwrap_or_else(|| "portable".to_owned())
}

#[cfg(target_os = "windows")]
fn persist_disk_usage_engine(value: &str) {
    let existing = app_state::load();
    app_state::save(&AppState {
        disk_usage_engine: Some(value.to_owned()),
        ..existing
    });
}

fn persist_update_check(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        update_check: Some(value),
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

fn persist_cell_gap(value: f32) {
    let existing = app_state::load();
    app_state::save(&AppState {
        cell_gap: Some(crate::grid::clamp_cell_gap(value)),
        ..existing
    });
}

fn persist_thumb_fit(value: crate::grid::ThumbFit) {
    let existing = app_state::load();
    app_state::save(&AppState {
        thumb_fit: Some(value.as_str().to_string()),
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

fn persist_redact_diagnostics(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        redact_diagnostics: Some(value),
        ..existing
    });
}

// =============================================================================
// SettingsView entity: external API
// =============================================================================

pub struct SettingsView {
    category: SettingsCategory,
    /// Cached count of dotfiles in `$HOME`, used by the Files page
    /// description. Captured once at construction so the
    /// `build_pages()` call inside `Render::render` doesn't keep
    /// re-reading the home directory on every paint: re-renders
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
    /// The Diagnostics page's health report. `None` while the checks run
    /// on the background executor (kicked off by `new`; the page shows
    /// "Running health checks…" meanwhile). The checks open the metadata
    /// DB and write a disk probe: real I/O that must not run on the UI
    /// thread when Cmd+, opens the window (Prime Directive). Reopening
    /// Settings re-runs them. `Rc` so the per-frame page-render closures
    /// can share the report cheaply.
    diagnostics: Option<std::rc::Rc<crate::diagnostics::Report>>,
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

        // Ant Trail picker, same shape. Seed from the persisted hex,
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

        // Diagnostics checks (metadata-DB open, config reads, a
        // Full-Disk-Access probe write) and the home hidden-item count
        // (a full `read_dir($HOME)` + per-entry stat) are real I/O:
        // computed off-thread and applied when they land, so opening
        // Settings never blocks on them. The pages render placeholders
        // until then.
        cx.spawn(async move |this, cx| {
            let (report, hidden_count) = cx
                .background_executor()
                .spawn(async move { (crate::diagnostics::run_checks(), count_home_hidden_items()) })
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                this.diagnostics = Some(std::rc::Rc::new(report));
                this.home_hidden_count = hidden_count;
                cx.notify();
            });
        })
        .detach();

        Self {
            category: initial,
            home_hidden_count: None,
            selection_picker,
            ant_trail_picker,
            diagnostics: None,
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

fn persist_recents_enabled(value: bool) {
    let existing = app_state::load();
    app_state::save(&AppState {
        recents_enabled: Some(value),
        ..existing
    });
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title(&tr!("Settings"));
        let content = Settings::new("ferail-settings")
            // Long translated section names need more than the component's
            // compact default. The current component also supplies a real
            // splitter, so this is a starting width rather than a hard wall.
            .sidebar_width(px(300.0))
            .sidebar_size_range(px(240.0)..px(420.0))
            .pages(build_pages(
                self.home_hidden_count,
                &self.selection_picker,
                &self.ant_trail_picker,
                self.diagnostics.clone(),
            ))
            .default_selected_index(SelectIndex {
                page_ix: self.category.page_index(),
                group_ix: None,
            })
            .into_any_element();
        crate::private_mode::protect(content, cx)
    }
}

/// Open a second native window hosting the SettingsView. Same shape
/// as the prior implementation: Cmd+, in Shell calls this; the menu-
/// bar `Settings…` item routes through the app-level OpenSettings
/// handler in main.rs which also calls this.
pub fn open_settings_window(cx: &mut App) {
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(820.0), px(580.0)), cx)),
        // Give the window a proper "Settings" title (it had none). A plain OS
        // titlebar suits this dialog: the brand/custom titlebar is for the
        // main browser window.
        titlebar: Some(gpui::TitlebarOptions {
            title: Some(tr!("Settings")),
            ..Default::default()
        }),
        ..crate::base_window_options()
    };
    cx.spawn(async move |cx| {
        // A failed `open_window` (display reconfiguration, resource pressure)
        // must not take the app down: log and leave the existing windows be.
        match cx.open_window(opts, |window, cx| {
            crate::boot::install_dev_window_callback_cleanup(window, cx);
            let view = cx.new(|cx| SettingsView::new(SettingsCategory::Appearance, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        }) {
            Ok(handle) => {
                cx.update(|cx| {
                    crate::process_state::process_state(cx)
                        .register_aux_window(handle.into(), tr!("Settings").to_string());
                    crate::boot::refresh_window_menu(cx);
                });
            }
            Err(e) => crate::log_warn!(90, "could not open settings window: {e}"),
        }
    })
    .detach();
}

// =============================================================================
// Page builders
// =============================================================================

/// A dropdown setting laid out the way the stock `SettingItem` can't: the
/// label and the control share one line (label left, dropdown right) while
/// the description spans the **full width** below them, instead of being
/// squeezed into the narrow left column the horizontal layout gives it.
///
/// The control is a `small`, width-capped dropdown so its text matches our
/// density (the stock field renders at the page's default size, which is
/// too large here) and a long option label can't spill past the panel
/// edge and clip.
///
/// `get` returns the current stored value; `persist` writes the picked
/// value. Both are plain `fn` pointers (the getters/setters capture
/// nothing: they read/write `app_state`). Most dropdowns only persist;
/// when a pick also has to apply live (recompute a global), use
/// [`dropdown_setting_with`], which hands the setter `&mut App`.
fn dropdown_setting(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    options: &'static [(&'static str, &'static str)],
    // Option values rendered greyed and unselectable (e.g. a provider this
    // build can't honour). Empty for an unrestricted dropdown.
    disabled: &'static [&'static str],
    get: fn() -> String,
    persist: fn(&str),
) -> SettingItem {
    // Persist, then repaint so the button reflects the pick. Live settings
    // skip this wrapper and pass their own `on_pick` to recompute a global.
    dropdown_setting_with(
        title,
        description,
        options,
        disabled,
        get,
        move |value, cx| {
            persist(value);
            cx.refresh_windows();
        },
    )
}

/// The shared dropdown rendering behind [`dropdown_setting`], parameterised by
/// what a pick does. `on_pick` runs with `&mut App`, so a live setting can
/// recompute a global and repaint, not just persist. `Copy` so every menu
/// item can capture its own copy.
///
/// `options` is a static `(value, label)` table whose labels are `msgid!`
/// literals; they are translated here, at render time, so the menu follows a
/// language switch without rebuilding the page.
fn dropdown_setting_with<F: Fn(&str, &mut App) + Copy + 'static>(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    options: &'static [(&'static str, &'static str)],
    disabled: &'static [&'static str],
    get: fn() -> String,
    on_pick: F,
) -> SettingItem {
    dropdown_setting_dyn(
        title,
        description,
        move |_cx| {
            options
                .iter()
                .map(|(value, label)| DropdownOption {
                    value: (*value).to_owned(),
                    label: crate::i18n::tr_static(label),
                    disabled: disabled.contains(value),
                })
                .collect()
        },
        move |_cx| get(),
        on_pick,
    )
}

/// One entry of a [`dropdown_setting_dyn`] menu.
struct DropdownOption {
    value: String,
    label: SharedString,
    /// Greyed and unselectable (e.g. a provider this build can't honour).
    disabled: bool,
}

/// The dropdown row every `dropdown_setting*` helper renders: title on the
/// left, the current value as a small outline button with a caret on the
/// right, and the description spanning the full width below. `options` and
/// `get` run on every render, so the menu can reflect live state (installed
/// language packs, for instance): keep them cheap and I/O-free.
fn dropdown_setting_dyn(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    options: impl Fn(&App) -> Vec<DropdownOption> + 'static,
    get: impl Fn(&App) -> String + 'static,
    on_pick: impl Fn(&str, &mut App) + 'static,
) -> SettingItem {
    let title = title.into();
    let description = description.into();
    let keyword_title = title.clone();
    let keyword_description = description.clone();
    let on_pick = std::rc::Rc::new(on_pick);
    SettingItem::render(move |render_options, _window, cx| {
        use gpui_component::{
            ActiveTheme as _, Sizable as _,
            button::Button,
            menu::{DropdownMenu as _, PopupMenuItem},
        };
        let current = get(cx);
        let options = std::rc::Rc::new(options(cx));
        let current_label = options
            .iter()
            .find(|o| o.value == current)
            .map(|o| o.label.clone())
            .unwrap_or_default();
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let on_pick = on_pick.clone();
        let title = title.clone();
        let description = description.clone();

        let control = Button::new(SharedString::from(format!("dd-{title}")))
            .label(current_label)
            .dropdown_caret(true)
            .outline()
            .small()
            .max_w(px(SETTINGS_DROPDOWN_LANE))
            .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _window, _cx| {
                options.iter().fold(menu, |menu, opt| {
                    let checked = opt.value == current;
                    let value = opt.value.clone();
                    let on_pick = on_pick.clone();
                    menu.item(
                        PopupMenuItem::new(opt.label.clone())
                            .checked(checked)
                            .disabled(opt.disabled)
                            .on_click(move |_, _window: &mut Window, cx: &mut App| {
                                on_pick(&value, cx);
                            }),
                    )
                })
            });

        if render_options.layout().is_vertical() {
            gpui_component::v_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .child(div().text_scale_sm().text_color(fg).child(title))
                .child(
                    div()
                        .w_full()
                        .text_scale_sm()
                        .text_color(muted)
                        .child(description),
                )
                .child(div().w_full().flex().justify_start().child(control))
                .into_any_element()
        } else {
            gpui_component::v_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .child(
                    gpui_component::h_flex()
                        .w_full()
                        .min_h(px(SETTINGS_TOP_ROW_HEIGHT))
                        .items_center()
                        .gap(px(SETTINGS_CONTROL_GAP))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_scale_sm()
                                .text_color(fg)
                                .child(title),
                        )
                        .child(div().flex_shrink_0().child(control)),
                )
                .child(
                    div()
                        .w_full()
                        .text_scale_sm()
                        .text_color(muted)
                        .child(description),
                )
                .into_any_element()
        }
    })
    .keywords([keyword_title, keyword_description])
}

/// A boolean setting laid out like [`dropdown_setting`]: the title and the
/// switch share the top line, and the **description spans the full width**
/// below them, rather than being squeezed into the stock `SettingItem`'s
/// narrow left column next to the control. `value` reads the current state;
/// `set_value` persists a toggle.
fn switch_setting(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    value: impl Fn(&App) -> bool + 'static,
    set_value: impl Fn(bool, &mut App) + 'static,
) -> SettingItem {
    let title = title.into();
    let description = description.into();
    let keyword_title = title.clone();
    let keyword_description = description.clone();
    // The SettingItem render closure is `Fn` (re-invoked each frame), so the
    // setter (moved into the switch's `on_click`) must be shareable: `Rc` it
    // and hand each render a clone.
    let set_value = std::rc::Rc::new(set_value);
    SettingItem::render(move |render_options, _window, cx| {
        use gpui_component::{ActiveTheme as _, Sizable as _, switch::Switch};

        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let checked = value(cx);
        let set_value = set_value.clone();
        let title = title.clone();
        let description = description.clone();

        let control = Switch::new(SharedString::from(format!("sw-{title}")))
            .checked(checked)
            .small()
            .on_click(move |checked: &bool, _window: &mut Window, cx: &mut App| {
                set_value(*checked, cx);
                cx.refresh_windows();
            });

        gpui_component::v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .when(render_options.layout().is_vertical(), |this| this.gap_2())
            .child(
                gpui_component::h_flex()
                    .w_full()
                    .min_h(px(SETTINGS_TOP_ROW_HEIGHT))
                    .items_center()
                    .gap(px(SETTINGS_CONTROL_GAP))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_scale_sm()
                            .text_color(fg)
                            .child(title),
                    )
                    .child(
                        div()
                            .w(px(SETTINGS_SWITCH_LANE))
                            .flex()
                            .justify_end()
                            .child(control),
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
    .keywords([keyword_title, keyword_description])
}

fn build_pages(
    home_hidden_count: Option<usize>,
    selection_picker: &Entity<ColorPickerState>,
    ant_trail_picker: &Entity<ColorPickerState>,
    diagnostics: Option<std::rc::Rc<crate::diagnostics::Report>>,
) -> Vec<SettingPage> {
    vec![
        appearance_page(selection_picker.clone(), ant_trail_picker.clone()),
        files_page(home_hidden_count),
        performance_page(),
        search_dupes_page(),
        layout_page(),
        plugins_page(),
        shortcuts_page(),
        diagnostics_page(diagnostics),
        about_page(),
    ]
}

/// The Diagnostics page: the health-check report grouped by area, the recent
/// activity trail, and a "Copy report" button. The report is computed on the
/// background executor (kicked off by [`SettingsView::new`]); `None` here
/// means it hasn't landed yet and the page shows a placeholder. This only
/// renders it. `ferail --doctor` prints the same report from a terminal.
fn diagnostics_page(report: Option<std::rc::Rc<crate::diagnostics::Report>>) -> SettingPage {
    use crate::diagnostics::Status;

    let Some(report) = report else {
        return SettingPage::new(tr!("Diagnostics"))
            .icon(Icon::empty().path("icons/activity.svg"))
            .group(
                SettingGroup::new().item(SettingItem::render(move |_o, _w, cx| {
                    div()
                        .text_scale_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr!("Running health checks\u{2026}"))
                })),
            );
    };

    let mut page =
        SettingPage::new(tr!("Diagnostics")).icon(Icon::empty().path("icons/activity.svg"));

    // Summary header.
    {
        let report = report.clone();
        page = page.group(
            SettingGroup::new().item(SettingItem::render(move |_o, _w, cx| {
                let (ok, warn, fail) = report.tally();
                let fg = cx.theme().foreground;
                let muted = cx.theme().muted_foreground;
                gpui_component::v_flex()
                    .w_full()
                    .gap_1()
                    .child(div().text_scale_sm().text_color(fg).child(tr!(
                        "Ferail v{version} · {os}/{arch} · {ok} OK, {warn} WARN, {fail} FAIL",
                        version = report.app_version,
                        os = report.os,
                        arch = report.arch,
                        ok = ok,
                        warn = warn,
                        fail = fail
                    )))
                    .child(
                        div()
                            .w_full()
                            .text_scale_xs()
                            .text_color(muted)
                            .child(tr!("Health check of the app's storage and environment. \
                     Run `ferail --doctor` for the same report from a terminal.")),
                    )
            })),
        );
    }

    // Privacy: the redaction toggle that makes "share your logs with us" safe.
    // Placed up front (right under the summary) so it's the first thing a user
    // sees before reading or sharing the report.
    page = page.group(
        SettingGroup::new()
            .title(tr!("Privacy"))
            .item(switch_setting(
                tr!("Redact file names & paths"),
                tr!(
                    "When on (the default), the report below, the bundle you can save, and the \
             activity trail all replace every file and folder name with \u{201c}\u{2026}\u{201d}. \
             We see only the shape of what you did (how deep a folder was, what file \
             type), never the names. So you can share a report with us and we learn \
             nothing about your files. Turn it off only if a maintainer asks for real paths \
             to reproduce a bug."
                ),
                |_cx: &App| {
                    app_state::load()
                        .redact_diagnostics
                        .unwrap_or(app_state::DEFAULT_REDACT_DIAGNOSTICS)
                },
                |val: bool, _cx: &mut App| {
                    persist_redact_diagnostics(val);
                    crate::redact::set_enabled(val);
                },
            )),
    );

    // Bug reports: the folders a bug report draws on, then the packaging
    // actions. Right after Privacy so "where do I find the crash files"
    // has a one-scroll answer. `config_dir` is pure env-var derivation
    // (no disk I/O), so building these rows here is render-safe; the
    // Open buttons create the folder off-thread before opening it.
    let mut bug_reports = SettingGroup::new().title(tr!("Bug reports"));
    if let Some(config) = app_state::config_dir() {
        bug_reports = bug_reports
            .item(report_folder_item(
                "bug-folder-reports",
                tr!("Crash reports folder"),
                tr!(
                    "Crash reports, freeze reports, native minidumps, and saved report \
                     bundles are written here. A crash in a terminal prints a short summary \
                     and puts the full detail in this folder. Attach the newest files when \
                     reporting a bug."
                ),
                config.join("reports"),
            ))
            .item(report_folder_item(
                "bug-folder-config",
                tr!("Settings folder"),
                tr!(
                    "The app's configuration lives here: the settings file, the metadata \
                     database (tags, visit history), and your language packs."
                ),
                config,
            ));
    }

    // Copy-report / bundle actions, both honor the redaction toggle and scrub
    // the account name out of the app-owned diagnostics paths.
    let report_for_copy = report.clone();
    page = page.group(bug_reports.item(SettingItem::render(move |_o, _w, _cx| {
        use gpui_component::{Sizable as _, button::Button};
        let report = report_for_copy.clone();
        gpui_component::h_flex()
            .w_full()
            .gap_2()
            .child(
                Button::new("diag-copy")
                    .label(tr!("Copy report"))
                    .outline()
                    .small()
                    .on_click(move |_, _w, _cx| {
                        let mut text = crate::report::redact_username(
                            &crate::diagnostics::render_text(&report),
                        );
                        let trail = crate::trail::render_lines_sanitized();
                        if !trail.is_empty() {
                            text.push_str("\n[Activity trail]\n");
                            for l in &trail {
                                text.push_str(&crate::report::redact_username(l));
                                text.push('\n');
                            }
                        }
                        crate::platform_shell::copy_to_clipboard(&text);
                    }),
            )
            .child(
                Button::new("diag-report")
                    .label(tr!("Create report bundle\u{2026}"))
                    .outline()
                    .small()
                    .on_click(|_, window, _cx| crate::report::open_reporter(window)),
            )
    })));

    // One group per check group, one row per check.
    for (gi, group) in report.groups.iter().enumerate() {
        let mut sg = SettingGroup::new().title(crate::i18n::tr_static(group.title));
        for ci in 0..group.checks.len() {
            let report = report.clone();
            sg = sg.item(SettingItem::render(move |_o, _w, cx| {
                use gpui_component::{Sizable as _, button::Button};
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
                            .child(div().flex_1().text_scale_sm().text_color(fg).child(
                                SharedString::from(crate::private_mode::present_label(&check.name)),
                            ))
                            // Jump to the location this check is about:
                            // reveal it (selected in its parent) in a
                            // Ferail file window. Local UI action: the
                            // target never enters a shared report, so it
                            // coexists with the redaction toggle.
                            .children(check.path.clone().map(|path| {
                                Button::new(SharedString::from(format!("diag-reveal-{gi}-{ci}")))
                                    .label(tr!("Reveal"))
                                    .outline()
                                    .xsmall()
                                    .on_click(move |_, _w, cx| {
                                        crate::shell::reveal_path_in_app(cx, path.clone());
                                    })
                            })),
                    )
                    .child(div().w_full().text_scale_xs().text_color(muted).child(
                        SharedString::from(crate::private_mode::present_label(&check.detail)),
                    ))
            }));
        }
        page = page.group(sg);
    }

    // Recent activity trail (last ~20 events): rendered through the *same*
    // redaction the bundle uses, so the user sees exactly what would be shared.
    page = page.group(
        SettingGroup::new()
            .title(tr!("Activity trail"))
            .item(SettingItem::render(move |_o, _w, cx| {
                let muted = cx.theme().muted_foreground;
                let lines = crate::trail::render_lines_sanitized();
                let body = if lines.is_empty() {
                    tr!("No activity recorded yet.")
                } else {
                    let start = lines.len().saturating_sub(20);
                    SharedString::from(lines[start..].join("\n"))
                };
                let caption = if crate::redact::enabled() {
                    tr!(
                        "Privacy protection is on: paths and file names are replaced before this activity is attached to a report."
                    )
                } else {
                    tr!("Privacy protection is off: a shared report would include the paths shown here.")
                };
                gpui_component::v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .w_full()
                            .text_scale_sm()
                            .text_color(muted)
                            .child(tr!(
                                "Preview of the latest actions included with a diagnostic report."
                            )),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_h(px(72.0))
                            .p_3()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().muted.opacity(0.22))
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_scale_xs()
                            .text_color(cx.theme().foreground)
                            .child(body),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_scale_xs()
                            .text_color(muted)
                            .child(caption),
                    )
            })),
    );

    page
}

/// One row of the Diagnostics page's Bug-reports group: a folder that
/// matters when filing an issue: title, an Open button, the folder's
/// path (account name scrubbed, as the language group shows it), and a
/// description of what lives inside. The folder may not exist yet (no
/// crash ever happened), so the click creates it on the background
/// executor first, then opens it in a Ferail tab on the main thread
/// (Prime Directive: no disk I/O in a click handler).
fn report_folder_item(
    id: &'static str,
    title: SharedString,
    description: SharedString,
    dir: std::path::PathBuf,
) -> SettingItem {
    let keyword_title = title.clone();
    let keyword_description = description.clone();
    SettingItem::render(move |_o, _w, cx| {
        use gpui_component::{ActiveTheme as _, Sizable as _, button::Button};
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let shown = crate::report::redact_username(&dir.display().to_string());
        let dir = dir.clone();
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
                            .flex_1()
                            .text_scale_sm()
                            .text_color(fg)
                            .child(title.clone()),
                    )
                    .child(
                        Button::new(id)
                            .label(tr!("Open folder"))
                            .outline()
                            .xsmall()
                            .on_click(move |_, _w, cx| {
                                let dir = dir.clone();
                                cx.spawn(async move |cx: &mut AsyncApp| {
                                    let created = cx
                                        .background_executor()
                                        .spawn({
                                            let dir = dir.clone();
                                            async move { std::fs::create_dir_all(&dir) }
                                        })
                                        .await;
                                    cx.update(|cx| match created {
                                        Ok(()) => crate::shell::open_dir_in_app(cx, dir),
                                        Err(e) => {
                                            crate::log_warn!(90, "bug-report folder: {e}")
                                        }
                                    });
                                })
                                .detach();
                            }),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .text_scale_xs()
                    .text_color(muted)
                    .child(SharedString::from(shown)),
            )
            .child(
                div()
                    .w_full()
                    .text_scale_sm()
                    .text_color(muted)
                    .child(description.clone()),
            )
    })
    .keywords([keyword_title, keyword_description])
}

fn search_dupes_page() -> SettingPage {
    SettingPage::new(tr!("Search & Duplicates"))
        .icon(Icon::empty().path("icons/search.svg"))
        // ---- Search engine ----
        .group(
            SettingGroup::new()
                .title(tr!("Search"))
                .item(dropdown_setting(
                    tr!("Search engine"),
                    tr!(
                        "Automatic uses Spotlight's live index when available (instant, \
                     content-aware, near-zero CPU) and falls back to the built-in \
                     recursive walker where Spotlight is disabled or blind (some external / \
                     network volumes). Force one if you prefer."
                    ),
                    &[
                        ("auto", msgid!("Automatic (recommended)")),
                        ("spotlight", msgid!("Spotlight")),
                        ("walker", msgid!("Built-in walker")),
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
                    tr!("Match full path"),
                    tr!("Match the relative path, not just the file name."),
                    |_cx: &App| app_state::load().search_match_path.unwrap_or(false),
                    |val: bool, _cx: &mut App| persist_search_match_path(val),
                ))
                .item(switch_setting(
                    tr!("Include hidden files"),
                    tr!("Search dot-files and otherwise-hidden items too."),
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
                .title(tr!("Duplicate finder"))
                .item(dropdown_setting(
                    tr!("Results view"),
                    tr!(
                        "How duplicate groups are shown. Grouped rows reuse the file list \
                     (selection, sort, preview, context menu); the dedicated panel offers \
                     group-level actions like keep-newest."
                    ),
                    &[
                        ("grouped", msgid!("Grouped rows in a tab")),
                        ("panel", msgid!("Dedicated panel")),
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
                    tr!("Ignore small files"),
                    tr!("Skip files below this size: the big wins are large files."),
                    &[
                        ("0", msgid!("Compare all files")),
                        ("1", msgid!("Skip under 1 MB")),
                        ("10", msgid!("Skip under 10 MB")),
                        ("100", msgid!("Skip under 100 MB")),
                    ],
                    &[],
                    || app_state::load().dupe_min_size_mb.unwrap_or(0).to_string(),
                    |v| persist_dupe_min_size_mb(v.parse().unwrap_or(0)),
                ))
                .item(switch_setting(
                    tr!("Skip cloud placeholders"),
                    tr!("Don't download undownloaded iCloud files just to hash them."),
                    |_cx: &App| app_state::load().dupe_skip_cloud.unwrap_or(true),
                    |val: bool, _cx: &mut App| persist_dupe_skip_cloud(val),
                ))
                .item(switch_setting(
                    tr!("Compare inside app bundles"),
                    tr!(
                        "Descend into .app / .bundle packages and compare their inner files. \
                     Off keeps packages opaque."
                    ),
                    |_cx: &App| app_state::load().dupe_include_packages.unwrap_or(false),
                    |val: bool, _cx: &mut App| persist_dupe_include_packages(val),
                ))
                .item(switch_setting(
                    tr!("Byte-for-byte verify"),
                    tr!(
                        "Confirm each match byte-for-byte after hashing. Removes any \
                     hash-collision doubt at the cost of re-reading confirmed groups."
                    ),
                    |_cx: &App| app_state::load().dupe_paranoid.unwrap_or(false),
                    |val: bool, _cx: &mut App| persist_dupe_paranoid(val),
                )),
        )
}

/// Appearance › Language: pick the UI language, and the import / export /
/// new-language tools behind it (docs/features/LOCALIZATION.md). No LLM is
/// called from inside the app: the user creates a template file, hands it to
/// a translator or an AI chat together with the embedded instructions, and
/// imports the result.
fn language_group() -> SettingGroup {
    use crate::i18n::{self, ENGLISH, SYSTEM};
    SettingGroup::new()
        .title(tr!("Language"))
        .item(dropdown_setting_dyn(
            tr!("Language"),
            tr!("Follow the system language, or pick an installed language pack. \
             Strings a pack doesn't cover stay in English."),
            |cx| {
                let langs = i18n::languages(cx);
                let mut out = Vec::with_capacity(langs.packs.len() + 2);
                let system_label = match langs.system_locale.as_deref() {
                    Some(loc) => tr!("System ({loc})", loc = loc),
                    None => tr!("System"),
                };
                out.push(DropdownOption { value: SYSTEM.to_owned(), label: system_label, disabled: false });
                out.push(DropdownOption { value: ENGLISH.to_owned(), label: tr!("English"), disabled: false });
                for p in &langs.packs {
                    out.push(DropdownOption { value: p.code.clone(), label: p.label().into(), disabled: false });
                }
                out
            },
            |cx| i18n::languages(cx).selection.clone(),
            i18n::set_selection,
        ))
        .item(
            SettingItem::render(|_o, _w, cx| {
                use gpui_component::{
                    ActiveTheme as _, Sizable as _,
                    button::Button,
                    menu::{DropdownMenu as _, PopupMenuItem},
                };
                let langs = i18n::languages(cx);
                let muted = cx.theme().muted_foreground;
                let installed: std::collections::BTreeSet<String> =
                    langs.packs.iter().map(|p| p.code.clone()).collect();
                let folder = i18n::user_dir()
                    .map(|d| crate::report::redact_username(&d.display().to_string()))
                    .unwrap_or_default();
                let summary = if langs.packs.is_empty() {
                    tr!("No language packs installed.")
                } else {
                    let list: Vec<String> = langs
                        .packs
                        .iter()
                        .map(|p| {
                            let origin = match p.origin {
                                i18n::Origin::Bundled => tr!("built in"),
                                i18n::Origin::User => tr!("your file"),
                            };
                            tr!(
                                "{name} - {translated} of {total} strings, {origin}",
                                name = p.name,
                                translated = ferail_core::counts::format_count(p.translated as u64),
                                total = ferail_core::counts::format_count(p.total as u64),
                                origin = origin
                            )
                            .to_string()
                        })
                        .collect();
                    tr!("Installed: {list}.", list = list.join("; "))
                };
                gpui_component::v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        gpui_component::h_flex()
                            .w_full()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Button::new("lang-new")
                                    .label(tr!("New language\u{2026}"))
                                    .dropdown_caret(true)
                                    .outline()
                                    .small()
                                    .dropdown_menu_with_anchor(gpui::Anchor::TopLeft, move |menu, _window, _cx| {
                                        // ~35 languages: taller than the settings window.
                                        // Scrollable with a capped height so the tail
                                        // (Polish … Vietnamese) is reachable instead of
                                        // clipped at the window edge.
                                        let menu = menu.scrollable(true).max_h(px(360.));
                                        i18n::PRESET_LANGUAGES.iter().fold(menu, |menu, (code, name, english)| {
                                            let (code, name, english) = (*code, *name, *english);
                                            menu.item(
                                                PopupMenuItem::new(SharedString::from(format!("{name} - {english}")))
                                                    .disabled(installed.contains(code))
                                                    .on_click(move |_, window: &mut Window, cx: &mut App| {
                                                        i18n::create_template(code, name, english, window, cx);
                                                    }),
                                            )
                                        })
                                    }),
                            )
                            .child(
                                Button::new("lang-import")
                                    .label(tr!("Import\u{2026}"))
                                    .outline()
                                    .small()
                                    .on_click(|_, window, cx| i18n::import_file(window, cx)),
                            )
                            .child(
                                Button::new("lang-export")
                                    .label(tr!("Export\u{2026}"))
                                    .outline()
                                    .small()
                                    .on_click(|_, window, cx| i18n::export_current(window, cx)),
                            )
                            .child(
                                Button::new("lang-folder")
                                    .label(tr!("Show folder"))
                                    .outline()
                                    .small()
                                    .on_click(|_, _window, cx| i18n::reveal_folder(cx)),
                            )
                            .child(
                                Button::new("lang-reload")
                                    .label(tr!("Reload"))
                                    .outline()
                                    .small()
                                    .on_click(|_, _window, cx| i18n::reload(cx)),
                            )
                            .child(
                                Button::new("lang-instructions")
                                    .label(tr!("Copy instructions"))
                                    .outline()
                                    .small()
                                    .on_click(|_, _window, cx| i18n::copy_instructions(cx)),
                            ),
                    )
                    .child(
                        div().w_full().text_scale_sm().text_color(muted).child(tr!(
                            "To add a language: New language\u{2026} writes a template into {folder}. \
                             Give that file to a translator or an AI assistant (Claude, ChatGPT, \u{2026}) \
                             (the instructions are inside it), then Import\u{2026} the result. \
                             Export\u{2026} saves the current language the same way, for translating the \
                             missing strings or sharing it. {summary}",
                            folder = folder,
                            summary = summary
                        )),
                    )
            })
            .keywords(["language", "translation", "locale", "import", "export"]),
        )
}

fn appearance_page(
    selection_picker: Entity<ColorPickerState>,
    ant_trail_picker: Entity<ColorPickerState>,
) -> SettingPage {
    SettingPage::new(tr!("Appearance"))
        .icon(Icon::empty().path("icons/palette.svg"))
        .group(language_group())
        .group(
            SettingGroup::new().title(tr!("Theme")).item(
                // Vertical layout so the three fixed-width tiles drop
                // below the title rather than competing with it for
                // horizontal space: previous default-horizontal layout
                // clipped the System tile on the right edge.
                SettingItem::new(
                    tr!("Theme"),
                    SettingField::render(|_options, _window, _cx| {
                        theme_tile_strip().into_any_element()
                    }),
                )
                .layout(Axis::Vertical)
                .description(tr!("Match the system, or pick a side.")),
            ),
        )
        .group(
            SettingGroup::new()
                .title(tr!("Selection"))
                .item(
                    // The picker is a stateful entity owned by `SettingsView`;
                    // here we render a fresh stateless `ColorPicker` over it
                    // each frame. Changes flow through the entity's
                    // `ColorPickerEvent::Change` subscription (set up in
                    // `SettingsView::new`), which updates the live global and
                    // persists, so the file list and grid recolor at once.
                    SettingItem::new(
                        tr!("Selection color"),
                        SettingField::render(move |_options, _window, _cx| {
                            ColorPicker::new(&selection_picker).into_any_element()
                        }),
                    )
                    .layout(Axis::Vertical)
                    .description(tr!(
                        "The highlight behind selected files in the list and grid. \
                     Clear it to follow the theme's blue."
                    )),
                )
                .item(dropdown_setting_with(
                    tr!("Icon spacing"),
                    tr!(
                        "Gap between the selection highlights in icon view. Wider spacing \
                 lets the boxes breathe; None packs them edge-to-edge."
                    ),
                    &[
                        ("0", msgid!("None")),
                        ("2", msgid!("Tight")),
                        ("4", msgid!("Default")),
                        ("8", msgid!("Comfortable")),
                        ("12", msgid!("Spacious")),
                    ],
                    &[],
                    || {
                        format!(
                            "{:.0}",
                            crate::grid::clamp_cell_gap(
                                app_state::load()
                                    .cell_gap
                                    .unwrap_or(crate::grid::DEFAULT_CELL_GAP),
                            )
                        )
                    },
                    |value, cx| {
                        let g = value
                            .parse::<f32>()
                            .unwrap_or(crate::grid::DEFAULT_CELL_GAP);
                        persist_cell_gap(g);
                        cx.set_global(crate::grid::CellGap(crate::grid::clamp_cell_gap(g)));
                        cx.refresh_windows();
                    },
                ))
                // Photos are rarely square and the icon slot always is, so
                // something has to give. Sits next to Icon spacing because
                // both answer "how does icon view lay a cell out".
                .item(dropdown_setting_with(
                    tr!("Icon fit"),
                    tr!(
                        "How a photo or preview fills its square in icon view. Best fit shows \
                 the whole image with bars beside it; Fill frame crops the edges so the \
                 image fills the icon completely."
                    ),
                    &[
                        ("best", msgid!("Best fit")),
                        ("fill", msgid!("Fill frame")),
                        ("width", msgid!("Fit width")),
                        ("height", msgid!("Fit height")),
                        ("stretch", msgid!("Stretch")),
                    ],
                    &[],
                    || {
                        crate::grid::ThumbFit::from_str(
                            app_state::load().thumb_fit.as_deref().unwrap_or("best"),
                        )
                        .as_str()
                        .to_string()
                    },
                    |value, cx| {
                        let fit = crate::grid::ThumbFit::from_str(value);
                        persist_thumb_fit(fit);
                        cx.set_global(crate::grid::ThumbFitMode(fit));
                        cx.refresh_windows();
                    },
                )),
        )
        .group(
            SettingGroup::new()
                .title(tr!("Ant Trail"))
                // Master switch. Persists *and* updates the live global so the
                // tint appears/vanishes in open windows without a relaunch.
                .item(switch_setting(
                    tr!("Show Ant Trail"),
                    tr!(
                        "Tint your most-visited folders so the ones you open most stand out. \
                     Off hides the tint entirely, your visit history still feeds \
                     Recents."
                    ),
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
                        tr!("Ant Trail color"),
                        SettingField::render(move |_options, _window, _cx| {
                            ColorPicker::new(&ant_trail_picker).into_any_element()
                        }),
                    )
                    .layout(Axis::Vertical)
                    .description(tr!(
                        "The tint behind your most-visited folders in the list and grid. \
                         Brightness still tracks visit frequency. Clear it for the stock orange."
                    )),
                )
                // Persists *and* updates the live policy global so the change
                // takes effect on the next favorite click without a relaunch.
                .item(switch_setting(
                    tr!("Don't track favorites"),
                    tr!(
                        "When on, opening a folder from your Favorites doesn't count toward \
                     its Ant Trail heat or add it to Recents, so deliberate \
                     shortcuts don't crowd out folders you actually browse to. Reaching \
                     the same folder any other way still counts."
                    ),
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
        .group(
            SettingGroup::new()
                .title(tr!("Recents"))
                // Master switch. Persists *and* updates the live global so the
                // sidebar section appears/vanishes without a relaunch. Use
                // "Clear Recents\u{2026}" (Go menu / \u{2318}K) to wipe the list.
                .item(switch_setting(
                    tr!("Show Recents"),
                    tr!(
                        "List the folders you've opened recently in the sidebar, most \
                     recent first. Off hides the section and stops adding to it \
                     your Ant Trail heat is unaffected."
                    ),
                    |cx: &App| crate::recents_section::recents_enabled(cx),
                    |val: bool, cx: &mut App| {
                        persist_recents_enabled(val);
                        cx.set_global(crate::recents_section::RecentsEnabled(val));
                    },
                )),
        )
}

/// Performance-tuning toggles for the background work that costs the
/// most on a slow disk / low-powered Mac. Both settings are live: they
/// persist and update a process global so open windows react without a
/// relaunch.
fn performance_page() -> SettingPage {
    let page = SettingPage::new(tr!("Performance"))
        .icon(Icon::empty().path("icons/cpu.svg"))
        .group(
            SettingGroup::new()
                .title(tr!("Background work"))
                // Quick Look previews vs. generic type icons. Moved here
                // from Files because rendering real previews is one of the
                // per-folder background costs (see `warm_*_viewport`).
                .item(switch_setting(
                    tr!("Show file previews"),
                    tr!(
                        "Draw photos, videos, and PDFs as their actual content in the file \
                     list and grid. Off uses generic type icons, lighter, since \
                     Quick Look never runs."
                    ),
                    |cx: &App| crate::thumbnails::show_thumbnails(cx),
                    |val: bool, cx: &mut App| {
                        persist_show_thumbnails(val);
                        cx.set_global(crate::thumbnails::ShowThumbnails(val));
                    },
                ))
                // The heaviest routine the app runs on a slow disk: a
                // recursive walk per directory row, re-checked on every
                // window activation. Off leaves folder rows with a dash in
                // the Size column.
                .item(switch_setting(
                    tr!("Calculate folder sizes"),
                    tr!(
                        "Recursively total each folder so the Size column shows how big it is. \
                     This walks the whole subtree in the background: the biggest \
                     disk cost on large folders. Off shows a dash for folder sizes."
                    ),
                    |cx: &App| crate::folder_sizes::folder_sizing_enabled(cx),
                    |val: bool, cx: &mut App| {
                        persist_folder_sizing(val);
                        cx.set_global(crate::folder_sizes::FolderSizingEnabled(val));
                    },
                ))
                // Magic-byte sniffing (Format column) + Finder-tag xattr
                // reads (tag dots): the two remaining per-row disk costs on
                // every folder load. Bundled into one switch.
                .item(switch_setting(
                    tr!("Detect file types and tags"),
                    tr!(
                        "Read each file's contents to name its type in the Format column and \
                     read its Finder tags for the colour dots. Both are per-file disk \
                     reads on every folder. Off falls back to types from the file \
                     extension and hides tag dots."
                    ),
                    |cx: &App| crate::prefetch::file_detail_scan_enabled(cx),
                    |val: bool, cx: &mut App| {
                        persist_file_detail_scan(val);
                        cx.set_global(crate::prefetch::FileDetailScan(val));
                    },
                )),
        );

    #[cfg(target_os = "macos")]
    let page = page.group(
        SettingGroup::new()
            .title(tr!("Disk Usage access"))
            .item(full_disk_access_setting()),
    );

    #[cfg(target_os = "windows")]
    let page = page.group(
        SettingGroup::new()
            .title(tr!("Disk Usage"))
            .item(dropdown_setting(
                tr!("Disk Usage engine"),
                tr!(
                    "Portable works everywhere without elevation. Fast NTFS reads local NTFS metadata through an administrator helper started on first use and reused until Ferail exits; if it cannot finish safely, Ferail discards its partial result and retries with Portable."
                ),
                &[
                    ("portable", msgid!("Portable")),
                    ("fast-ntfs", msgid!("Fast NTFS (administrator)")),
                ],
                &[],
                disk_usage_engine_preference,
                persist_disk_usage_engine,
            )),
    );

    page
}

#[cfg(target_os = "windows")]
fn persist_show_linux_locations(value: bool, cx: &mut App) {
    let existing = app_state::load();
    app_state::save(&AppState {
        show_linux_locations: Some(value),
        ..existing
    });
    crate::platform_locations::set_enabled(value, cx);
}

#[cfg(target_os = "macos")]
fn full_disk_access_setting() -> SettingItem {
    let title = tr!("Full Disk Access");
    let description = tr!(
        "Optional. Lets Disk Usage include folders protected by macOS. Fast directory reading works without it; Ferail asks only when a scan encounters protected folders."
    );
    let keyword_title = title.clone();
    let keyword_description = description.clone();
    SettingItem::render(move |_options, _window, cx| {
        use gpui_component::{ActiveTheme as _, Sizable as _, button::Button};
        let foreground = cx.theme().foreground;
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
                            .flex_1()
                            .min_w_0()
                            .text_scale_sm()
                            .text_color(foreground)
                            .child(title.clone()),
                    )
                    .child(
                        Button::new("settings-full-disk-access")
                            .label(tr!("Open System Settings"))
                            .outline()
                            .small()
                            .on_click(|_, window, cx| {
                                use gpui_component::{WindowExt as _, notification::Notification};
                                if let Some(path) = crate::platform_shell::app_bundle_path() {
                                    cx.write_to_clipboard(ClipboardItem::new_string(path));
                                    window.push_notification(
                                        Notification::info(tr!(
                                            "Ferail's path is copied. Add it in Full Disk Access, then relaunch Ferail."
                                        ))
                                        .autohide(false),
                                        cx,
                                    );
                                }
                                cx.background_spawn(async move {
                                    crate::platform_shell::open_url(
                                        crate::shell::FULL_DISK_ACCESS_SETTINGS_URL,
                                    );
                                })
                                .detach();
                            }),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .text_scale_sm()
                    .text_color(muted)
                    .child(description.clone()),
            )
    })
    .keywords([keyword_title, keyword_description])
}

fn files_page(home_hidden_count: Option<usize>) -> SettingPage {
    // Description prefers the live count; the explanatory fallback
    // covers sandbox/CI runs where $HOME can't be read.
    // Copy intentionally implies "next launch": Show Hidden writes
    // app_state but doesn't push the change into already-open Shell
    // windows. A shared-observer rewire is on the Phase 10 audit
    // list; in the meantime the wording matches reality.
    let description = match home_hidden_count {
        Some(n) if n > 0 => trn!(
            "Reveal items that start with a dot: {n} in your home folder. Takes effect on next launch.",
            "Reveal items that start with a dot: {n} in your home folder. Takes effect on next launch.",
            n
        ),
        _ => tr!(
            "Reveal items that start with a dot, like .config and .ssh. Takes effect on next launch."
        ),
    };
    let page = SettingPage::new(tr!("Files"))
        .icon(Icon::empty().path("icons/folder.svg"))
        .group(
            SettingGroup::new()
                .title(tr!("Visibility"))
                .item(switch_setting(
                    tr!("Show hidden files"),
                    description,
                    |_cx: &App| app_state::load().show_hidden.unwrap_or(false),
                    |val: bool, _cx: &mut App| persist_show_hidden(val),
                )),
        );
    // Sidebar special-folder root, only meaningful where OneDrive's
    // Known-Folder-Move can split a folder between local and cloud, i.e.
    // Windows. Omitted elsewhere.
    #[cfg(target_os = "windows")]
    let page = page.group(
        SettingGroup::new()
            .title(tr!("Locations"))
            .item(locations_mode_setting())
            .item(switch_setting(
                tr!("Show Linux (WSL)"),
                tr!(
                    "Show installed Windows Subsystem for Linux distributions in the sidebar. Disabled by default; when disabled Ferail does not discover or start WSL distributions."
                ),
                |_cx: &App| {
                    app_state::load()
                        .show_linux_locations
                        .unwrap_or(false)
                },
                persist_show_linux_locations,
            )),
    );
    page.group(terminal_group())
}

/// The Files-page Terminal group: which terminal the "Open Terminal Here"
/// context command launches, its launch arguments, and standard vs.
/// administrator mode. Resolved at use by
/// [`crate::feature_settings::TerminalConfig`].
fn terminal_group() -> SettingGroup {
    SettingGroup::new()
        .title(tr!("Terminal"))
        .item(
            SettingItem::new(
                tr!("Terminal application"),
                SettingField::input(
                    |_cx: &App| {
                        let raw = app_state::load().terminal_path.unwrap_or_default();
                        SharedString::from(crate::private_mode::present_label(&raw))
                    },
                    |val: SharedString, _cx: &mut App| persist_terminal_path(val.as_ref()),
                ),
            )
            .layout(Axis::Vertical)
            .description(tr!(
                "Which terminal \u{201C}Open Terminal Here\u{201D} launches: an app name \
                 or .app bundle on macOS, a program path, or a command on PATH. Blank uses the \
                 platform default: Terminal.app on macOS, Windows Terminal on Windows, and \
                 auto-detection ($TERMINAL, then common emulators) on Linux."
            )),
        )
        .item(
            SettingItem::new(
                tr!("Arguments"),
                SettingField::input(
                    |_cx: &App| {
                        let raw = app_state::load().terminal_args.unwrap_or_default();
                        SharedString::from(crate::private_mode::present_label(&raw))
                    },
                    |val: SharedString, _cx: &mut App| persist_terminal_args(val.as_ref()),
                ),
            )
            .layout(Axis::Vertical)
            // `{dir}` here is the terminal's own placeholder, shown to the
            // reader verbatim (no `tr!` arguments, so nothing is filled in).
            .description(tr!(
                "Extra launch arguments. {dir} expands to the folder; double quotes group a \
                 value with spaces (e.g. --working-directory \"{dir}\"). Without {dir} the \
                 terminal starts in the folder via its working directory. Blank uses the \
                 terminal's defaults."
            )),
        )
        .item(dropdown_setting(
            tr!("Launch mode"),
            tr!(
                "Standard opens the terminal normally. Administrator opens it with elevated \
             rights: a UAC prompt on Windows; on macOS and Linux the window opens \
             into a root shell, with sudo asking for your password inside the terminal."
            ),
            &[
                ("standard", msgid!("Standard")),
                ("admin", msgid!("Administrator")),
            ],
            &[],
            || {
                app_state::load()
                    .terminal_mode
                    .unwrap_or_else(|| "standard".into())
            },
            persist_terminal_mode,
        ))
}

fn persist_terminal_path(value: &str) {
    let existing = app_state::load();
    let v = value.trim();
    app_state::save(&AppState {
        terminal_path: (!v.is_empty()).then(|| v.to_string()),
        ..existing
    });
}

fn persist_terminal_args(value: &str) {
    let existing = app_state::load();
    let v = value.trim();
    app_state::save(&AppState {
        terminal_args: (!v.is_empty()).then(|| v.to_string()),
        ..existing
    });
}

fn persist_terminal_mode(value: &str) {
    let existing = app_state::load();
    app_state::save(&AppState {
        terminal_mode: Some(value.to_string()),
        ..existing
    });
}

/// The sidebar Locations root dropdown (Windows / OneDrive). Picks which copy
/// of a moved folder the sidebar points at; applies live via the
/// `ResolvedLocations` global so open windows' sidebars update without a
/// relaunch. See [`crate::special_folders`].
#[cfg(target_os = "windows")]
fn locations_mode_setting() -> SettingItem {
    dropdown_setting_with(
        tr!("Special folders"),
        tr!(
            "When OneDrive moves your Desktop, Documents, or Pictures into the cloud it often \
         leaves a local copy behind, so \u{201C}where is my Documents?\u{201D} has two answers. \
         Automatic follows Windows (cloud where it moved them, local otherwise). Local prefers \
         your %USERPROFILE% copy; OneDrive prefers the OneDrive copy, each falls back to \
         the other when its copy doesn\u{2019}t exist, so a shortcut never opens to nothing."
        ),
        &[
            ("auto", msgid!("Automatic (recommended)")),
            ("local", msgid!("Local profile")),
            ("onedrive", msgid!("OneDrive")),
        ],
        &[],
        || {
            app_state::load()
                .special_folder_mode
                .unwrap_or_else(|| "auto".into())
        },
        |value: &str, cx: &mut App| {
            crate::special_folders::persist_and_apply(
                ferail_fs_native::paths::SpecialFolderMode::from_str(value),
                cx,
            );
        },
    )
}

fn layout_page() -> SettingPage {
    SettingPage::new(tr!("Layout"))
        .icon(Icon::empty().path("icons/settings-2.svg"))
        .group(SettingGroup::new().title(tr!("Interface")).item(dropdown_setting(
            tr!("UI scale"),
            tr!("Overall interface zoom. Restart the app or open a new window for the change to apply."),
            &[
                ("0.85", msgid!("Small (85%)")),
                ("1.00", msgid!("Default (100%)")),
                ("1.15", msgid!("Medium (115%)")),
                ("1.30", msgid!("Large (130%)")),
            ],
            &[],
            || format!("{:.2}", app_state::load().ui_scale.unwrap_or(1.0)),
            |v| persist_ui_scale(v.parse().unwrap_or(1.0)),
        )))
        .group(SettingGroup::new().title(tr!("Viewer")).item(dropdown_setting(
            tr!("Default zoom"),
            tr!("How media is sized when the viewer opens, and what zoom reset returns to. \
             \u{201C}Fit to window\u{201D} scales large media down and small media up to fill \
             the window; \u{201C}Fit, never enlarge\u{201D} stops at the media's real size, so \
             small images stay pixel-true; \u{201C}Actual size\u{201D} always shows 1:1. \
             Takes effect on the next viewer window."),
            &[
                ("fit", msgid!("Fit to window")),
                ("fit-down", msgid!("Fit, never enlarge")),
                ("actual", msgid!("Actual size (100%)")),
            ],
            &[],
            || {
                app_state::load()
                    .viewer_default_zoom
                    .unwrap_or_else(|| "fit".into())
            },
            persist_viewer_default_zoom,
        )))
}

fn persist_viewer_default_zoom(value: &str) {
    let existing = app_state::load();
    app_state::save(&AppState {
        viewer_default_zoom: Some(value.to_string()),
        ..existing
    });
}

fn persist_video_backend(value: &str) {
    let existing = app_state::load();
    app_state::save(&AppState {
        video_backend: Some(value.to_string()),
        ..existing
    });
}

fn persist_mpv_path(value: &str) {
    let existing = app_state::load();
    let v = value.trim();
    app_state::save(&AppState {
        mpv_path: (!v.is_empty()).then(|| v.to_string()),
        ..existing
    });
}

fn plugins_page() -> SettingPage {
    // The mpv provider is only compiled in with the `mpv` feature. In a stock
    // build, grey the "mpv" option out (unselectable) and say why in the
    // description, so it's discoverable but can't be picked into a no-op.
    let (player_desc, player_disabled): (SharedString, &'static [&'static str]) =
        if cfg!(feature = "mpv") {
            (
                tr!(
                    "The built-in player uses the platform's native media frameworks \
                 (AVFoundation on macOS, Media Foundation on Windows). mpv plays virtually \
                 any container/codec and applies colour adjustments and a transparent-colour \
                 key to the video itself. libmpv must be installed; a change takes effect on \
                 the next viewer window."
                ),
                &[],
            )
        } else {
            (
                tr!(
                    "The built-in player uses the platform's native media frameworks. \
                 mpv plays virtually any container/codec, but this build was compiled \
                 without the `mpv` feature, so mpv is unavailable: rebuild with \
                 `cargo run --bin ferail-gpui --features mpv` (with libmpv installed) \
                 to enable it."
                ),
                &["mpv"],
            )
        };
    SettingPage::new(tr!("Plugins"))
        .icon(Icon::empty().path("icons/settings.svg"))
        .group(
            SettingGroup::new()
                .title(tr!("Video player"))
                .item(dropdown_setting(
                    tr!("Player"),
                    player_desc,
                    &[("builtin", msgid!("Built-in")), ("mpv", msgid!("mpv"))],
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
                        tr!("mpv library"),
                        SettingField::input(
                            |_cx: &App| {
                                let raw = app_state::load().mpv_path.unwrap_or_else(|| {
                                    crate::viewer::backend_native::default_mpv_path().into()
                                });
                                SharedString::from(crate::private_mode::present_label(&raw))
                            },
                            |val: SharedString, _cx: &mut App| persist_mpv_path(val.as_ref()),
                        ),
                    )
                    .layout(Axis::Vertical)
                    .description(tr!(
                        "Where libmpv is loaded from: the dylib, a directory containing it, \
                         or mpv.app on macOS. Blank uses the platform default (Homebrew)."
                    )),
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
        let title = crate::i18n::tr_static(spec.title);
        let cat_name = crate::i18n::tr_static(category_name(spec.category));
        let chord_for_render = chord.clone();
        let title_for_render = title.clone();
        let cat_for_render = cat_name.clone();
        let item = SettingItem::render(move |_options, _window, cx| {
            let theme = cx.theme();
            gpui_component::v_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .child(
                    gpui_component::h_flex()
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_scale_sm()
                                .text_color(theme.foreground)
                                .child(title_for_render.clone()),
                        )
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
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .text_scale_sm()
                        .text_color(theme.muted_foreground)
                        .child(tr!(
                            "{category} \u{00B7} {chord}",
                            category = cat_for_render,
                            chord = chord_for_render
                        )),
                )
                .into_any_element()
        })
        .keywords([
            title,
            cat_name.clone(),
            SharedString::from(chord.clone()),
            SharedString::from(format!("{cat_name} \u{00B7} {chord}")),
        ]);
        if let Some((_, items)) = groups_by_cat.iter_mut().find(|(c, _)| *c == spec.category) {
            items.push(item);
        } else {
            groups_by_cat.push((spec.category, vec![item]));
        }
    }

    let mut page =
        SettingPage::new(tr!("Keyboard Shortcuts")).icon(Icon::empty().path("icons/keyboard.svg"));
    for (cat, items) in groups_by_cat {
        let title = crate::i18n::tr_static(category_name(cat));
        let mut group = SettingGroup::new().title(title);
        for item in items {
            group = group.item(item);
        }
        page = page.group(group);
    }
    page
}

fn about_page() -> SettingPage {
    SettingPage::new(tr!("About"))
        .icon(Icon::empty().path("icons/info.svg"))
        .group(
            // Updates: the automatic check is opt-in; the menu's manual
            // Check for Updates… works regardless (docs/features/UPDATES.md).
            SettingGroup::new()
                .title(tr!("Updates"))
                .item(switch_setting(
                    tr!("Check for updates automatically"),
                    tr!(
                        "Once a day, ask GitHub whether a newer Ferail release exists, and show a \
                 notification when one does. Off by default: when off, Ferail makes no \
                 network requests on its own. Nothing is ever downloaded or installed without \
                 you choosing to; use Ferail \u{2192} Check for Updates\u{2026} to check by hand \
                 at any time."
                    ),
                    |_cx: &App| {
                        app_state::load()
                            .update_check
                            .unwrap_or(app_state::DEFAULT_UPDATE_CHECK)
                    },
                    |val: bool, cx: &mut App| {
                        persist_update_check(val);
                        if val {
                            // Opting in mid-session: answer immediately rather
                            // than at tomorrow's daily wake.
                            crate::update_check::start_check_background(cx);
                        }
                    },
                )),
        )
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
                            .child("Ferail"),
                    )
                    .child(
                        div()
                            .text_scale_xs()
                            .text_color(theme.muted_foreground)
                            .child(tr!(
                                "Version {version}",
                                version = env!("CARGO_PKG_VERSION")
                            )),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_scale_sm()
                            .text_color(theme.foreground)
                            .child(tr!("Fast, focused file operations at any scale.")),
                    )
                    .child(
                        div()
                            .text_scale_xs()
                            .text_color(theme.muted_foreground)
                            .child(tr!(
                                "Predictable, private, and built for real-world directories."
                            )),
                    )
                    .into_any_element()
            })),
        )
}

/// English name of a command category as a `msgid!` literal: translate it
/// for display with `crate::i18n::tr_static`.
fn category_name(c: Category) -> &'static str {
    match c {
        Category::App => msgid!("App"),
        Category::File => msgid!("File"),
        Category::Edit => msgid!("Edit"),
        Category::View => msgid!("View"),
        Category::Go => msgid!("Go"),
        Category::Selection => msgid!("Selection"),
        Category::Window => msgid!("Window"),
        Category::Help => msgid!("Help"),
        Category::Context => msgid!("Context"),
    }
}

// =============================================================================
// Theme tile strip: visual reinforcement of the Theme dropdown
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
    // Hard-coded mock palette per tile, each tile shows the OTHER
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
                // Strengthened active state: accent ring + check
                // badge: applied as a wrapper around the label.
                .child(active_state_decoration(pref))
                .child(div().text_scale_xs().child(pref.label())),
        )
}

/// Renders the selected-state badge for a theme tile. Lives next to
/// the label so the strong cue is on the textual identifier rather
/// than the artwork: matches macOS Settings convention where the
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
    let home = ferail_fs_native::home_dir();
    let n = std::fs::read_dir(home)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            e.metadata()
                .map(|m| ferail_fs_native::entry_is_hidden(&name, &m))
                .unwrap_or_else(|_| name.starts_with('.'))
        })
        .count();
    Some(n)
}
