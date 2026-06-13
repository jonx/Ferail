//! Cmd+/ keyboard-shortcuts overlay.
//!
//! Harvest Stage 9.b. A modal listing every entry in
//! `feraille_core::commands::all_commands()` grouped by category,
//! filterable by a top text-input. The state (visible flag + filter
//! string) lives on `Shell`; this module is a pure render helper.

use feraille_core::commands::{Category, CommandSpec, Shortcut, all_commands};
use gpui::*;
use gpui_component::{ActiveTheme, Sizable, h_flex, input::Input, kbd::Kbd, v_flex};

use crate::shell::Shell;

/// Map a catalogue `CommandId` to a dispatchable `gpui::Action`.
/// Used by the shortcuts-help overlay (which doubles as our Cmd+K
/// command palette) so clicking a row fires the corresponding
/// action. Returns the boxed action when we know how to dispatch it;
/// `None` for commands that don't have a Shell-level handler yet
/// (e.g. tag colours / open-with slots — context-only).
fn action_for_command(id: feraille_core::commands::CommandId) -> Option<Box<dyn gpui::Action>> {
    use crate::shell::*;
    Some(match id.0 {
        "file.new_tab" => Box::new(NewTab) as Box<dyn gpui::Action>,
        "file.close_tab" => Box::new(CloseTab),
        "file.new_folder" => Box::new(NewFolder),
        "file.get_info" => Box::new(GetInfo),
        "file.move_to_trash" => Box::new(MoveToTrash),
        "file.copy_path" => Box::new(CopyPath),
        "file.reveal_in_finder" => Box::new(RevealInFinder),
        "file.refresh" => Box::new(Refresh),
        "file.open" | "selection.activate" => Box::new(OpenSelected),
        "file.duplicate" => Box::new(Duplicate),
        "file.make_alias" => Box::new(MakeAlias),
        "file.compress" => Box::new(Compress),
        "file.rename" | "selection.start_rename" => Box::new(RenameSelected),
        "file.quick_look" => Box::new(QuickLook),
        "file.open_in_new_tab" => Box::new(OpenInNewTab),
        "view.search" => Box::new(FocusFilter),
        "view.edit_breadcrumb" => Box::new(EditBreadcrumb),
        "view.toggle_preview" => Box::new(TogglePreview),
        "view.toggle_hidden" => Box::new(ToggleHidden),
        "view.zoom_in" => Box::new(ZoomIn),
        "view.zoom_out" => Box::new(ZoomOut),
        "view.zoom_reset" => Box::new(ZoomReset),
        "view.disk_usage" => Box::new(OpenDiskUsage),
        "view.open_viewer" => Box::new(OpenViewer),
        "view.sort_name" => Box::new(SortByName),
        "view.sort_size" => Box::new(SortBySize),
        "view.sort_kind" => Box::new(SortByKind),
        "view.sort_modified" => Box::new(SortByModified),
        "file.copy" => Box::new(CopyFiles),
        "file.paste" => Box::new(PasteFiles),
        "file.move_paste" => Box::new(MovePasteFiles),
        "file.empty_trash" => Box::new(EmptyTrash),
        "file.reopen_closed_tab" => Box::new(ReopenClosedTab),
        "window.close_window" => Box::new(CloseWindow),
        "app.settings" => Box::new(OpenSettings),
        "go.back" => Box::new(NavigateBack),
        "go.forward" => Box::new(NavigateForward),
        "go.parent" => Box::new(NavigateParent),
        "go.home" => Box::new(GoHome),
        "help.shortcuts" => Box::new(ShortcutsHelp),
        "selection.cursor_up" => Box::new(CursorUp),
        "selection.cursor_down" => Box::new(CursorDown),
        "selection.cursor_first" => Box::new(CursorFirst),
        "selection.cursor_last" => Box::new(CursorLast),
        "selection.page_up" => Box::new(PageUp),
        "selection.page_down" => Box::new(PageDown),
        "window.next_tab" => Box::new(NextTab),
        "window.prev_tab" => Box::new(PrevTab),
        _ => return None,
    })
}

/// Commands matching `filter`, grouped by category in display order
/// (categories in first-encounter order; rows in catalogue order).
/// Shared by the render and the "top match" the palette runs on Enter
/// so the highlight and Enter target always agree with what's shown.
fn filtered_groups(filter: &str) -> Vec<(Category, Vec<&'static CommandSpec>)> {
    let lower = filter.to_lowercase();
    let mut groups: Vec<(Category, Vec<&CommandSpec>)> = Vec::new();
    for spec in all_commands() {
        let title_match = spec.title.to_lowercase().contains(&lower);
        let shortcut_match = spec
            .shortcuts
            .iter()
            .any(|s| format_shortcut(s).to_lowercase().contains(&lower));
        if !filter.is_empty() && !title_match && !shortcut_match {
            continue;
        }
        if let Some((_, list)) = groups.iter_mut().find(|(c, _)| *c == spec.category) {
            list.push(spec);
        } else {
            groups.push((spec.category, vec![spec]));
        }
    }
    groups
}

/// The first dispatchable command in display order — what the palette
/// runs on Enter and highlights as the default pick.
pub fn palette_top_command(filter: &str) -> Option<feraille_core::commands::CommandId> {
    filtered_groups(filter)
        .into_iter()
        .flat_map(|(_, list)| list)
        .find(|spec| action_for_command(spec.id).is_some())
        .map(|spec| spec.id)
}

/// The boxed action for the palette's current top match, if any.
pub fn palette_top_action(filter: &str) -> Option<Box<dyn gpui::Action>> {
    palette_top_command(filter).and_then(action_for_command)
}

pub fn render(shell: &Shell, cx: &mut Context<Shell>) -> Option<AnyElement> {
    let filter = shell.shortcuts_help_filter.as_ref()?.clone();
    let bg = cx.theme().background;
    let border = cx.theme().border;
    let foreground = cx.theme().foreground;
    let muted = cx.theme().muted_foreground;
    let accent = cx.theme().secondary;
    let input = shell.shortcuts_help_input.clone();

    // The default pick: highlighted, and run by Enter (see the
    // shortcuts-help input's PressEnter subscription in shell.rs).
    let top = palette_top_command(&filter);

    let body_sections: Vec<Div> = filtered_groups(&filter)
        .into_iter()
        .map(|(cat, list)| section(cat, list, top, foreground, muted, accent, cx))
        .collect();

    let header = v_flex()
        .gap_2()
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(foreground)
                .child("Keyboard Shortcuts"),
        )
        .child(Input::new(&input).small());

    let backdrop = div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .bg(rgba(0x00000080))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.close_shortcuts_help(cx);
            }),
        );

    let card = v_flex()
        .w(px(560.0))
        .max_h(px(560.0))
        .gap_3()
        .p_5()
        .bg(bg)
        .rounded(px(12.0))
        .border_1()
        .border_color(border)
        .shadow_lg()
        .child(header)
        .child(div().h_px().bg(border))
        .child(
            div()
                .id("shortcuts-help-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(v_flex().gap_4().children(body_sections)),
        )
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

    Some(
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .flex()
            .items_center()
            .justify_center()
            .child(backdrop)
            .child(card)
            .into_any_element(),
    )
}

#[allow(clippy::too_many_arguments)]
fn section(
    cat: Category,
    specs: Vec<&CommandSpec>,
    top: Option<feraille_core::commands::CommandId>,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    cx: &mut Context<Shell>,
) -> Div {
    let title = match cat {
        Category::App => "App",
        Category::File => "File",
        Category::Edit => "Edit",
        Category::View => "View",
        Category::Go => "Go",
        Category::Selection => "Selection",
        Category::Window => "Window",
        Category::Help => "Help",
        Category::Context => "Context",
    };
    let rows: Vec<AnyElement> = specs
        .into_iter()
        .map(|spec| row(spec, top == Some(spec.id), foreground, muted, accent, cx))
        .collect();
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(title),
        )
        .children(rows)
}

fn row(
    spec: &CommandSpec,
    is_top: bool,
    foreground: gpui::Hsla,
    _muted: gpui::Hsla,
    accent: gpui::Hsla,
    cx: &mut Context<Shell>,
) -> AnyElement {
    // Convert the catalogue's first shortcut to a gpui Keystroke via
    // the same chord-string DSL keymap.rs uses, then hand it to
    // gpui-component's `Kbd` for boxed-glyph styling matching
    // Finder's menu-bar shortcuts.
    let kbd: Option<Kbd> = spec.shortcuts.first().and_then(|s| {
        let kb_str = keystroke_string(s)?;
        gpui::Keystroke::parse(&kb_str).ok().map(Kbd::new)
    });
    let id = spec.id;
    let dispatchable = action_for_command(id).is_some();
    let row_id = SharedString::from(format!("cmd-{}", id.0));
    let mut row = h_flex()
        .id(ElementId::Name(row_id))
        .w_full()
        .items_center()
        .py_1()
        .px_2()
        .gap_2()
        .rounded(px(4.0));
    // The default pick (run by Enter) sits pre-highlighted.
    if is_top {
        row = row.bg(accent);
    }
    row = row.child(
            div()
                .flex_1()
                .text_sm()
                .text_color(foreground)
                .child(SharedString::from(spec.title)),
        );
    if let Some(k) = kbd {
        row = row.child(div().flex_shrink_0().child(k));
    }
    if dispatchable {
        // Clickable + hover-tinted when the command maps to a known
        // action handler. Commands missing a dispatch (e.g. tag
        // colours / open-with slots — context-only) render but stay
        // inert.
        row.cursor_pointer()
            .hover(move |this| this.bg(accent))
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(action) = action_for_command(id) {
                    this.close_shortcuts_help(cx);
                    window.dispatch_action(action, cx);
                }
            }))
            .into_any_element()
    } else {
        row.into_any_element()
    }
}

/// Mirror of `keymap::translate_shortcut` — produces the same
/// `cmd-shift-x` style chord string the keymap installer uses, so
/// `Keystroke::parse` accepts it. Returns `None` for unsupported
/// keys (e.g. the catalogue's `+` alternate, gpui's parser treats
/// `-` as a separator).
fn keystroke_string(s: &Shortcut) -> Option<String> {
    let key = match s.key {
        "Up" | "Down" | "Left" | "Right" | "Home" | "End" | "PageUp" | "PageDown" | "Escape"
        | "Enter" | "Tab" | "Space" | "Backspace" | "Delete" | "F1" | "F2" | "F3" | "F4" | "F5"
        | "F6" | "F7" | "F8" | "F9" | "F10" | "F11" | "F12" => s.key.to_ascii_lowercase(),
        "+" => return None,
        k if k.chars().count() == 1 => k.to_ascii_lowercase(),
        _ => return None,
    };
    let mut parts: Vec<&str> = Vec::with_capacity(4);
    if s.primary {
        parts.push("cmd");
    }
    if s.shift {
        parts.push("shift");
    }
    if s.alt {
        parts.push("alt");
    }
    parts.push(&key);
    Some(parts.join("-"))
}

/// Render a `Shortcut` as a human-readable chord like
/// `\u{2318}\u{21E7}H` (Cmd+Shift+H).
pub fn format_shortcut(s: &Shortcut) -> String {
    let mut out = String::new();
    if s.alt {
        out.push('\u{2325}');
    }
    if s.shift {
        out.push('\u{21E7}');
    }
    if s.primary {
        out.push('\u{2318}');
    }
    let key_label = match s.key {
        "Up" => "\u{2191}".to_string(),
        "Down" => "\u{2193}".to_string(),
        "Left" => "\u{2190}".to_string(),
        "Right" => "\u{2192}".to_string(),
        "Backspace" => "\u{232B}".to_string(),
        "Enter" => "\u{21B5}".to_string(),
        "Escape" => "\u{238B}".to_string(),
        "Space" => "Space".to_string(),
        k => k.to_uppercase(),
    };
    out.push_str(&key_label);
    out
}
