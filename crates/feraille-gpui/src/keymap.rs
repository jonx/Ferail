//! Command-catalogue-driven keymap installation.
//!
//! Single source of truth for which shortcut fires what command:
//! [`feraille_core::commands`] (65 entries). At app start we walk
//! the catalogue, translate each `Shortcut` into the gpui
//! `KeyBinding` string DSL, and bind it to the matching gpui
//! `Action` type. Commands without a matching action (because the
//! feature hasn't been ported yet) get a one-time log warning and
//! are skipped.
//!
//! When a new command's shortcut needs to change, the edit lands in
//! `feraille-core::commands` and propagates everywhere (this
//! keymap, the native AppKit menu bar in Stage 3.b, the future
//! Keyboard-Shortcuts dialog). The old app's bespoke
//! `keystroke_to_command` matcher in `feraille-app/src/main.rs` is
//! replaced by this single function call.

use feraille_core::commands::{all_commands, CommandId, Shortcut};
use gpui::{App, KeyBinding};

use crate::shell::{
    self, ClearFilter, CloseTab, CopyPath, FocusFilter, MoveToTrash, NavigateBack,
    NavigateForward, NavigateParent, NewFolder, NewTab, NextTab, OpenSelected, OpenSettings,
    PrevTab, QuickLook, Refresh, RenameSelected, RevealInFinder, ToggleHidden,
};

/// Install keybindings for every command in `feraille_core::commands`
/// whose action has a Rust handler in this crate. Run once at app
/// startup, before the first window opens. Idempotent — calling it
/// twice rebinds the same keys.
pub fn install(cx: &mut App) {
    // The non-catalogue ClearFilter is a UI-implementation detail
    // (Esc to clear the filter field) so it's not in the catalogue;
    // bind it directly.
    cx.bind_keys([KeyBinding::new(
        "escape",
        ClearFilter,
        Some(shell::SHELL_CONTEXT),
    )]);

    for spec in all_commands() {
        for shortcut in spec.shortcuts {
            let Some(kb_str) = translate_shortcut(shortcut) else {
                continue;
            };
            install_binding(cx, spec.id, &kb_str);
        }
    }
}

/// Translate a `feraille_core::commands::Shortcut` to gpui's
/// keybinding-string DSL. Returns `None` when the key name doesn't
/// translate (e.g. a future "Media-Play" entry we don't bind).
fn translate_shortcut(s: &Shortcut) -> Option<String> {
    let key = translate_key(s.key)?;
    let mut parts: Vec<&str> = Vec::with_capacity(4);
    if s.primary {
        // On macOS the catalogue's `primary` means Cmd. gpui's
        // platform layer maps "cmd-" to Cmd on macOS / Ctrl on
        // Linux+Windows automatically (same convention as Zed),
        // so this stays portable when we eventually ship on Linux.
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

/// Map the catalogue's key names to gpui's. Most letters / numbers
/// pass through lowercased; named keys (Up, Down, Backspace, F1…)
/// have known gpui names. Returns `None` for an unrecognised key so
/// the caller can skip the binding rather than crash.
fn translate_key(key: &str) -> Option<String> {
    let lower = key.to_ascii_lowercase();
    match lower.as_str() {
        // Named keys gpui's keystroke parser accepts.
        "up" | "down" | "left" | "right" | "home" | "end" | "pageup" | "pagedown"
        | "escape" | "enter" | "tab" | "space" | "backspace" | "delete" | "f1" | "f2"
        | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" => {
            Some(lower)
        }
        // gpui doesn't currently accept `+` directly in keybind
        // strings (it's the chord separator). The catalogue lists
        // `+` as an alternate for zoom-in alongside `=`; binding
        // `=` covers most keyboards.
        "+" => None,
        // Single-character keys: punctuation and letters. The
        // gpui parser accepts these lowercased.
        _ if lower.chars().count() == 1 => Some(lower),
        _ => None,
    }
}

/// For each known command ID, bind its shortcut to the matching
/// gpui Action type. Unknown / not-yet-ported IDs are logged once
/// and skipped so the keymap installer never blocks startup.
fn install_binding(cx: &mut App, id: CommandId, kb_str: &str) {
    let ctx = Some(shell::SHELL_CONTEXT);
    match id.0 {
        // -- App --------------------------------------------------
        "app.settings" => cx.bind_keys([KeyBinding::new(kb_str, OpenSettings, None)]),

        // -- File -------------------------------------------------
        "file.new_tab" => cx.bind_keys([KeyBinding::new(kb_str, NewTab, ctx)]),
        "file.close_tab" => cx.bind_keys([KeyBinding::new(kb_str, CloseTab, ctx)]),
        "file.new_folder" => cx.bind_keys([KeyBinding::new(kb_str, NewFolder, ctx)]),
        "file.move_to_trash" => cx.bind_keys([KeyBinding::new(kb_str, MoveToTrash, ctx)]),
        "file.copy_path" => cx.bind_keys([KeyBinding::new(kb_str, CopyPath, ctx)]),
        "file.reveal_in_finder" => {
            cx.bind_keys([KeyBinding::new(kb_str, RevealInFinder, ctx)])
        }
        "file.refresh" => cx.bind_keys([KeyBinding::new(kb_str, Refresh, ctx)]),

        // -- View -------------------------------------------------
        "view.search" => cx.bind_keys([KeyBinding::new(kb_str, FocusFilter, ctx)]),
        "view.toggle_hidden" => cx.bind_keys([KeyBinding::new(kb_str, ToggleHidden, ctx)]),

        // -- Go ---------------------------------------------------
        "go.back" => cx.bind_keys([KeyBinding::new(kb_str, NavigateBack, ctx)]),
        "go.forward" => cx.bind_keys([KeyBinding::new(kb_str, NavigateForward, ctx)]),
        "go.parent" => cx.bind_keys([KeyBinding::new(kb_str, NavigateParent, ctx)]),

        // -- Selection --------------------------------------------
        "selection.activate" => {
            cx.bind_keys([KeyBinding::new(kb_str, OpenSelected, ctx)])
        }
        "selection.rename" => {
            cx.bind_keys([KeyBinding::new(kb_str, RenameSelected, ctx)])
        }

        // -- Tab cycling. These aren't in the canonical catalogue
        //    yet (Stage 5.5.d added them locally); bind from the
        //    same context but use our existing constants so menu
        //    parity later remains a one-line add.
        // (handled below)

        // -- Not yet ported. Each is wired in a later stage.  -----
        "app.about"
        | "file.get_info"
        | "view.edit_breadcrumb"
        | "view.toggle_preview"
        | "view.cycle_focus"
        | "view.zoom_in"
        | "view.zoom_out"
        | "view.zoom_reset"
        | "view.disk_usage"
        | "view.theme_light"
        | "view.theme_dark"
        | "view.theme_system"
        | "go.home"
        | "disk_usage.refresh"
        | "disk_usage.zoom_out"
        | "disk_usage.toggle_topn"
        | "disk_usage.toggle_packages"
        | "disk_usage.toggle_follow_navigation"
        | "disk_usage.coloring_category"
        | "disk_usage.coloring_age"
        | "disk_usage.coloring_depth"
        | "disk_usage.size_apparent"
        | "disk_usage.size_allocated"
        | "selection.cursor_up"
        | "selection.cursor_down"
        | "selection.cursor_first"
        | "selection.cursor_last"
        | "selection.page_up"
        | "selection.page_down" => {
            crate::log_warn!(
                90,
                "keymap: command '{}' has no handler yet; binding '{}' skipped",
                id.0,
                kb_str
            );
        }

        _ => {
            crate::log_warn!(
                90,
                "keymap: unknown command id '{}'; binding '{}' skipped",
                id.0,
                kb_str
            );
        }
    }
}

/// Tab-cycling and filter-escape bindings live outside the
/// `feraille_core::commands` catalogue today (they were added in
/// the GPUI shell during Phase 5.5). Install them alongside the
/// catalogue-driven bindings so a single `keymap::install` call
/// covers every shortcut the shell handles.
pub(crate) fn install_extras(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-t", NewTab, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new("cmd-w", CloseTab, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new("ctrl-tab", NextTab, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new(
            "ctrl-shift-tab",
            PrevTab,
            Some(shell::SHELL_CONTEXT),
        ),
        // Stage 8: macOS Quick Look. Space-bar on the selected row
        // pops the QL panel. Not in the catalogue because Quick
        // Look is a macOS-specific affordance; the binding lives
        // here alongside other shell-context extras.
        KeyBinding::new("space", QuickLook, Some(shell::SHELL_CONTEXT)),
    ]);
}
