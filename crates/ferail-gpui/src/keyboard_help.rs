//! Cmd+/ keyboard-shortcuts overlay.
//!
//! Harvest Stage 9.b. A modal listing every entry in
//! `ferail_core::commands::all_commands()` grouped by category,
//! filterable by a top text-input. The state (visible flag + filter
//! string) lives on `Shell`; this module is a pure render helper.

use ferail_core::commands::{Category, Shortcut, all_commands};
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable as _,
    command::{Command, CommandGroup, CommandItem},
    v_flex,
};

use crate::shell::Shell;
use crate::text::TextScale as _;

/// Map a catalogue `CommandId` to a dispatchable `gpui::Action`.
/// Used by the shortcuts-help overlay (which doubles as our Cmd+K
/// command palette) so clicking a row fires the corresponding
/// action. Returns the boxed action when we know how to dispatch it;
/// `None` for commands that don't have a Shell-level handler yet
/// (e.g. tag colours / open-with slots: context-only).
fn action_for_command(id: ferail_core::commands::CommandId) -> Option<Box<dyn gpui::Action>> {
    use crate::shell::*;
    Some(match id.0 {
        "file.new_tab" => Box::new(NewTab) as Box<dyn gpui::Action>,
        "file.close_tab" => Box::new(CloseTab),
        "file.new_folder" => Box::new(NewFolder),
        "file.get_info" => Box::new(GetInfo),
        "file.move_to_trash" => Box::new(MoveToTrash),
        "file.copy_path" => Box::new(CopyPath),
        "file.generate_sha256" => Box::new(GenerateSha256),
        "file.verify_checksums" => Box::new(VerifyChecksums),
        "file.create_checksum_file" => Box::new(CreateChecksumFile),
        "file.copy_file_list" => Box::new(CopyFileList),
        "file.reveal_in_finder" => Box::new(RevealInFinder),
        "file.refresh" => Box::new(Refresh),
        "file.edit" => Box::new(EditFile),
        "file.edit_image" => Box::new(EditImage),
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
        "view.toggle_flat" => Box::new(ToggleFlatView),
        "view.zoom_in" => Box::new(ZoomIn),
        "view.zoom_out" => Box::new(ZoomOut),
        "view.zoom_reset" => Box::new(ZoomReset),
        "view.disk_usage" => Box::new(OpenDiskUsage),
        "view.performance_hud" => Box::new(TogglePerformanceHud),
        "view.find_duplicates" => Box::new(FindDuplicates),
        "view.find_similar_images" => Box::new(FindSimilarImages),
        "view.open_viewer" => Box::new(OpenViewer),
        "view.sort_name" => Box::new(SortByName),
        "view.sort_size" => Box::new(SortBySize),
        "view.sort_kind" => Box::new(SortByKind),
        "view.sort_modified" => Box::new(SortByModified),
        "view.sort_ant_trail" => Box::new(SortByAntTrail),
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

fn command_groups() -> Vec<CommandGroup> {
    let mut groups: Vec<(Category, Vec<CommandItem>)> = Vec::new();
    for spec in all_commands() {
        let translated = crate::i18n::tr_static(spec.title);
        let shortcuts = spec
            .shortcuts
            .iter()
            .map(format_shortcut)
            .map(SharedString::from)
            .collect::<Vec<_>>();
        let mut keywords = vec![SharedString::from(spec.title)];
        keywords.extend(shortcuts);
        let mut item = CommandItem::new().label(translated).keywords(keywords);
        if let Some(action) = action_for_command(spec.id) {
            // Command resolves the live keybinding for this Action, displays
            // it, and dispatches it on click or Enter.
            item = item.action(action);
        } else {
            item = item.disabled(true);
        }
        if let Some((_, list)) = groups.iter_mut().find(|(c, _)| *c == spec.category) {
            list.push(item);
        } else {
            groups.push((spec.category, vec![item]));
        }
    }
    groups
        .into_iter()
        .map(|(category, items)| {
            CommandGroup::new()
                .label(category_title(category))
                .items(items)
        })
        .collect()
}

pub fn render(shell: &Shell, cx: &mut Context<Shell>) -> Option<AnyElement> {
    shell.shortcuts_help_filter.as_ref()?;
    let bg = cx.theme().background;
    let border = cx.theme().border;
    let foreground = cx.theme().foreground;
    let command_state = shell.shortcuts_help_command.clone();
    let weak_confirm = cx.weak_entity();
    let weak_cancel = cx.weak_entity();
    let mut palette = Command::new(&command_state)
        .placeholder(tr!("Search commands…"))
        .max_h(px(460.0))
        .bordered(false)
        .header(move |_state, _window, _cx| {
            div()
                .pb_2()
                .text_scale_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(foreground)
                .child(tr!("Keyboard Shortcuts"))
        })
        .empty(|_state, _window, cx| {
            div()
                .w_full()
                .py_6()
                .text_center()
                .text_color(cx.theme().muted_foreground)
                .child(tr!("No matching command"))
        })
        .on_confirm(move |_index, window, cx| {
            let _ = weak_confirm.update(cx, |shell, cx| {
                shell.close_shortcuts_help(window, cx);
            });
        })
        .on_cancel(move |window, cx| {
            let _ = weak_cancel.update(cx, |shell, cx| {
                shell.close_shortcuts_help(window, cx);
            });
        });
    for group in command_groups() {
        palette = palette.group(group);
    }

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
        .p_5()
        .bg(bg)
        .rounded(px(12.0))
        .border_1()
        .border_color(border)
        .shadow_lg()
        .child(palette)
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
            // The overlay is a sibling subtree of the file list, painted on
            // top, but painting on top is not hit-testing on top: without
            // this the rows underneath stay hovered, so they show their
            // tooltips over the palette and still claim the scroll wheel.
            // `occlude` marks every hitbox behind this one unhovered and
            // non-scrolling; the palette's own hitboxes are in front of it.
            .occlude()
            .child(backdrop)
            .child(card)
            .into_any_element(),
    )
}

fn category_title(category: Category) -> SharedString {
    let title = match category {
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
    title.to_string().into()
}

/// Render a `Shortcut` as a human-readable chord.
///
/// macOS uses the native modifier glyphs with no separators, exactly as Finder
/// shows them in its menus: `\u{2318}\u{21E7}H` (Cmd+Shift+H). Windows and Linux
/// use conventional `Ctrl+Shift+H` text: the catalogue's `primary` modifier
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
