//! Cmd+/ keyboard-shortcuts overlay.
//!
//! Harvest Stage 9.b. A modal listing every entry in
//! `feraille_core::commands::all_commands()` grouped by category,
//! filterable by a top text-input. The state (visible flag + filter
//! string) lives on `Shell`; this module is a pure render helper.

use feraille_core::commands::{all_commands, Category, CommandSpec, Shortcut};
use gpui::*;
use gpui_component::{
    h_flex,
    input::Input,
    kbd::Kbd,
    v_flex, ActiveTheme, Sizable,
};

use crate::shell::Shell;

pub fn render(shell: &Shell, cx: &mut Context<Shell>) -> Option<AnyElement> {
    let filter = shell.shortcuts_help_filter.as_ref()?.clone();
    let bg = cx.theme().background;
    let border = cx.theme().border;
    let foreground = cx.theme().foreground;
    let muted = cx.theme().muted_foreground;
    let input = shell.shortcuts_help_input.clone();

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

    let body_sections: Vec<Div> = groups
        .into_iter()
        .map(|(cat, list)| section(cat, list, foreground, muted))
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

fn section(
    cat: Category,
    specs: Vec<&CommandSpec>,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
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
    let rows: Vec<Div> = specs
        .into_iter()
        .map(|spec| row(spec, foreground, muted))
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

fn row(spec: &CommandSpec, foreground: gpui::Hsla, _muted: gpui::Hsla) -> Div {
    // Convert the catalogue's first shortcut to a gpui Keystroke via
    // the same chord-string DSL keymap.rs uses, then hand it to
    // gpui-component's `Kbd` for boxed-glyph styling matching
    // Finder's menu-bar shortcuts.
    let kbd: Option<Kbd> = spec.shortcuts.first().and_then(|s| {
        let kb_str = keystroke_string(s)?;
        gpui::Keystroke::parse(&kb_str).ok().map(Kbd::new)
    });
    let mut row = h_flex()
        .w_full()
        .items_center()
        .py_1()
        .gap_2()
        .child(
            div()
                .flex_1()
                .text_sm()
                .text_color(foreground)
                .child(SharedString::from(spec.title)),
        );
    if let Some(k) = kbd {
        row = row.child(div().flex_shrink_0().child(k));
    }
    row
}

/// Mirror of `keymap::translate_shortcut` — produces the same
/// `cmd-shift-x` style chord string the keymap installer uses, so
/// `Keystroke::parse` accepts it. Returns `None` for unsupported
/// keys (e.g. the catalogue's `+` alternate, gpui's parser treats
/// `-` as a separator).
fn keystroke_string(s: &Shortcut) -> Option<String> {
    let key = match s.key {
        "Up" | "Down" | "Left" | "Right" | "Home" | "End" | "PageUp" | "PageDown"
        | "Escape" | "Enter" | "Tab" | "Space" | "Backspace" | "Delete"
        | "F1" | "F2" | "F3" | "F4" | "F5" | "F6" | "F7" | "F8" | "F9"
        | "F10" | "F11" | "F12" => s.key.to_ascii_lowercase(),
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
