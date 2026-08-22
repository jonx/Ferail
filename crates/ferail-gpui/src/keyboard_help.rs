//! Cmd+/ keyboard-shortcuts overlay.
//!
//! Harvest Stage 9.b. A modal listing every entry in
//! `ferail_core::commands::all_commands()` grouped by category,
//! filterable by a top text-input. The state (visible flag + filter
//! string) lives on `Shell`; this module is a pure render helper.

use crate::text::TextScale as _;
use ferail_core::commands::{Category, CommandSpec, Shortcut, all_commands};
use gpui::*;
use gpui_component::{
    ActiveTheme, Sizable, h_flex,
    input::{Escape, Input},
    v_flex,
};
// gpui-component's `Kbd` renders macOS ⌘ glyphs; only the macOS badge uses it.
#[cfg(target_os = "macos")]
use gpui_component::kbd::Kbd;

use crate::shell::Shell;

/// Map a catalogue `CommandId` to a dispatchable `gpui::Action`.
/// Used by the shortcuts-help overlay (which doubles as our Cmd+K
/// command palette) so clicking a row fires the corresponding
/// action. Returns the boxed action when we know how to dispatch it;
/// `None` for commands that don't have a Shell-level handler yet
/// (e.g. tag colours / open-with slots — context-only).
fn action_for_command(id: ferail_core::commands::CommandId) -> Option<Box<dyn gpui::Action>> {
    use crate::shell::*;
    Some(match id.0 {
        "file.new_tab" => Box::new(NewTab) as Box<dyn gpui::Action>,
        "file.close_tab" => Box::new(CloseTab),
        "file.new_folder" => Box::new(NewFolder),
        "file.get_info" => Box::new(GetInfo),
        "file.move_to_trash" => Box::new(MoveToTrash),
        "file.copy_path" => Box::new(CopyPath),
        "file.copy_file_list" => Box::new(CopyFileList),
        "file.reveal_in_finder" => Box::new(RevealInFinder),
        "file.refresh" => Box::new(Refresh),
        "file.open" | "selection.activate" => Box::new(OpenSelected),
        "file.duplicate" => Box::new(Duplicate),
        "file.make_alias" => Box::new(MakeAlias),
        "file.compress" => Box::new(Compress),
        "file.extract" => Box::new(Extract),
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
        "view.find_duplicates" => Box::new(FindDuplicates),
        "view.find_similar_images" => Box::new(FindSimilarImages),
        "view.open_viewer" => Box::new(OpenViewer),
        "view.sort_name" => Box::new(SortByName),
        "view.sort_size" => Box::new(SortBySize),
        "view.sort_kind" => Box::new(SortByKind),
        "view.sort_modified" => Box::new(SortByModified),
        "file.copy" => Box::new(CopyFiles),
        "file.paste" => Box::new(PasteFiles),
        "file.move_paste" => Box::new(MovePasteFiles),
        "file.delete_immediately" => Box::new(DeleteImmediately),
        "file.empty_trash" => Box::new(EmptyTrash),
        "file.reopen_closed_tab" => Box::new(ReopenClosedTab),
        "window.close_window" => Box::new(CloseWindow),
        "app.settings" => Box::new(OpenSettings),
        "go.back" => Box::new(NavigateBack),
        "go.forward" => Box::new(NavigateForward),
        "go.parent" => Box::new(NavigateParent),
        "go.home" => Box::new(GoHome),
        "go.go_to_folder" => Box::new(GoToFolder),
        "go.clear_recents" => Box::new(ClearRecents),
        "help.shortcuts" => Box::new(ShortcutsHelp),
        "selection.cursor_up" => Box::new(CursorUp),
        "selection.cursor_down" => Box::new(CursorDown),
        "selection.cursor_first" => Box::new(CursorFirst),
        "selection.cursor_last" => Box::new(CursorLast),
        "selection.page_up" => Box::new(PageUp),
        "selection.page_down" => Box::new(PageDown),
        "window.next_tab" => Box::new(NextTab),
        "window.prev_tab" => Box::new(PrevTab),
        "window.bring_all_to_front" => Box::new(crate::boot::BringAllToFront),
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
        // Match the translated title (what the user sees) and the English
        // catalogue title (so muscle memory keeps working in any language).
        let title_match = spec.title.to_lowercase().contains(&lower)
            || crate::i18n::tr_static(spec.title)
                .to_lowercase()
                .contains(&lower);
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
pub fn palette_top_command(filter: &str) -> Option<ferail_core::commands::CommandId> {
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
                .text_scale_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(foreground)
                .child(tr!("Keyboard Shortcuts")),
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
            cx.listener(|this, _, window, cx| {
                this.close_shortcuts_help(window, cx);
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
            // Esc dismisses the palette. The focused filter Input
            // propagates the `Escape` action up its ancestor chain
            // (see gpui-component InputState::escape → cx.propagate);
            // we sit on that chain and consume it before the shell's
            // ClearFilter handler can act.
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                this.close_shortcuts_help(window, cx);
                cx.stop_propagation();
            }))
            // Swallow scroll while the modal is up. The overlay is a
            // sibling subtree of the file list, painted on top; GPUI
            // still dispatches the wheel to the list's scroll hitbox
            // underneath unless we consume it here. The inner
            // overflow-scroll fires first (top-most), so the command
            // list still scrolls; this only stops the leak-through.
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(backdrop)
            .child(card)
            .into_any_element(),
    )
}

#[allow(clippy::too_many_arguments)]
fn section(
    cat: Category,
    specs: Vec<&CommandSpec>,
    top: Option<ferail_core::commands::CommandId>,
    foreground: gpui::Hsla,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    cx: &mut Context<Shell>,
) -> Div {
    let title = match cat {
        Category::App => tr!("App"),
        Category::File => tr!("File"),
        Category::Edit => tr!("Edit"),
        Category::View => tr!("View"),
        Category::Go => tr!("Go"),
        Category::Selection => tr!("Selection"),
        Category::Window => tr!("Window"),
        Category::Help => tr!("Help"),
        Category::Context => tr!("Context"),
    };
    let rows: Vec<AnyElement> = specs
        .into_iter()
        .map(|spec| row(spec, top == Some(spec.id), foreground, muted, accent, cx))
        .collect();
    v_flex()
        .gap_1()
        .child(
            div()
                .text_scale_xs()
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
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    cx: &mut Context<Shell>,
) -> AnyElement {
    // The trailing key-cap badge. On macOS this is gpui-component's `Kbd`
    // (native ⌘ glyphs, matching Finder's menu-bar shortcuts); on
    // Windows/Linux `Kbd` would still draw ⌘, so we render `Ctrl+…` text in a
    // matching bordered box instead. See `shortcut_badge`.
    let badge: Option<AnyElement> = spec
        .shortcuts
        .first()
        .and_then(|s| shortcut_badge(s, muted));
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
    // Inert commands (no Shell-level handler yet — tag colours,
    // open-with slots, etc.) read in muted grey so it's obvious they
    // can't be invoked from the palette; live ones keep full contrast.
    let title_color = if dispatchable { foreground } else { muted };
    let mut title = h_flex().flex_1().items_center().gap_2().child(
        div()
            .text_scale_sm()
            .text_color(title_color)
            .child(crate::i18n::tr_static(spec.title)),
    );
    if !dispatchable {
        // A trailing tag spells out *why* the row is dim, so it doesn't
        // just look like a styling glitch.
        title = title.child(
            div()
                .text_scale_xs()
                .text_color(muted)
                .child(tr!("\u{2014} unavailable here")),
        );
    }
    row = row.child(title);
    if let Some(b) = badge {
        row = row.child(div().flex_shrink_0().child(b));
    }
    if dispatchable {
        // Clickable + hover-tinted when the command maps to a known
        // action handler.
        row.cursor_pointer()
            .hover(move |this| this.bg(accent))
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(action) = action_for_command(id) {
                    this.close_shortcuts_help(window, cx);
                    window.dispatch_action(action, cx);
                }
            }))
            .into_any_element()
    } else {
        // Dimmed and non-interactive: no pointer cursor, no hover tint.
        row.opacity(0.5).cursor_default().into_any_element()
    }
}

/// The trailing key-cap badge for a command row.
///
/// macOS hands the chord to gpui-component's `Kbd`, which renders native ⌘/⇧
/// glyphs. `Kbd` only speaks macOS glyphs, so on Windows/Linux we instead draw
/// `format_shortcut`'s `Ctrl+…` text in a bordered box that matches the row.
#[cfg(target_os = "macos")]
fn shortcut_badge(s: &Shortcut, _muted: gpui::Hsla) -> Option<AnyElement> {
    let kb_str = keystroke_string(s)?;
    gpui::Keystroke::parse(&kb_str)
        .ok()
        .map(|ks| Kbd::new(ks).into_any_element())
}

#[cfg(not(target_os = "macos"))]
fn shortcut_badge(s: &Shortcut, muted: gpui::Hsla) -> Option<AnyElement> {
    Some(
        div()
            .px(px(5.0))
            .py(px(1.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(muted)
            .text_scale_xs()
            .child(SharedString::from(format_shortcut(s)))
            .into_any_element(),
    )
}

/// Mirror of `keymap::translate_shortcut` — produces the same
/// `cmd-shift-x` style chord string the keymap installer uses, so
/// `Keystroke::parse` accepts it. Returns `None` for unsupported
/// keys (e.g. the catalogue's `+` alternate, gpui's parser treats
/// `-` as a separator). Only the macOS badge path needs it.
#[cfg(target_os = "macos")]
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

/// Render a `Shortcut` as a human-readable chord.
///
/// macOS uses the native modifier glyphs with no separators, exactly as Finder
/// shows them in its menus: `\u{2318}\u{21E7}H` (Cmd+Shift+H). Windows and Linux
/// use conventional `Ctrl+Shift+H` text — the catalogue's `primary` modifier
/// binds to **Ctrl** there (gpui maps `cmd-` to Ctrl off macOS), and the Apple
/// `\u{2318}`/`\u{2325}` glyphs would be both wrong and unreadable on those
/// platforms. The key label (arrows, `\u{232B}`, …) is shared.
pub fn format_shortcut(s: &Shortcut) -> String {
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

    #[cfg(target_os = "macos")]
    {
        let mut out = String::new();
        if s.alt {
            out.push('\u{2325}'); // ⌥
        }
        if s.shift {
            out.push('\u{21E7}'); // ⇧
        }
        if s.primary {
            out.push('\u{2318}'); // ⌘
        }
        out.push_str(&key_label);
        out
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Ctrl is the Windows/Linux stand-in for macOS ⌘ on these chords.
        let mut out = String::new();
        if s.primary {
            out.push_str("Ctrl+");
        }
        if s.shift {
            out.push_str("Shift+");
        }
        if s.alt {
            out.push_str("Alt+");
        }
        out.push_str(&key_label);
        out
    }
}
