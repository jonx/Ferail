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
//! keymap, the native AppKit menu bar, the future
//! Keyboard-Shortcuts dialog).

use feraille_core::commands::{CommandId, Shortcut, all_commands};
use gpui::{App, KeyBinding};

use crate::shell::{
    self, ClearFilter, CloseTab, CloseWindow, CopyFiles, CopyPath, CursorDown, CursorDownExtend,
    CutFiles,
    CursorFirst, CursorFirstExtend, CursorLast, CursorLastExtend, CursorUp, CursorUpExtend,
    EditBreadcrumb, EmptyTrash, FindDuplicates, FocusFilter, GetInfo, GoHome, GridDown,
    GridDownExtend, GridLeft, GridLeftExtend, GridRight, GridRightExtend, GridUp, GridUpExtend,
    MovePasteFiles, MoveToTrash, NavigateBack,
    NavigateForward, NavigateParent, NewFolder, NewTab, NextTab, OpenDiskUsage, OpenInNewTab,
    OpenSelected, OpenSettings, OpenViewer, PageDown, PageDownExtend, PageUp, PageUpExtend,
    PasteFiles, PrevTab, QuickLook, Refresh, RenameSelected, ReopenClosedTab, RevealInFinder,
    SelectAll, ShortcutsHelp, ToggleHidden, TogglePreview, ZoomIn, ZoomOut, ZoomReset,
};
use crate::entry_info::{ENTRY_INFO_CONTEXT, EntryInfoDismiss};
use crate::viewer::window::{
    VIEWER_CONTEXT, ViewerActualSize, ViewerDismiss, ViewerNext, ViewerPrev, ViewerRotateCcw,
    ViewerRotateCw, ViewerToggleFullscreen, ViewerTogglePlay, ViewerZoomIn, ViewerZoomOut,
    ViewerZoomReset,
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
    // Undo (Cmd+Z) — not in the catalogue today because the action
    // is a UI-layer Shell helper (replays UndoOp inverse from the
    // Shell::undo_stack), not a catalogued command in feraille-core.
    cx.bind_keys([KeyBinding::new(
        "cmd-z",
        crate::shell::UndoLastAction,
        Some(shell::SHELL_CONTEXT),
    )]);

    // Icon-grid 2-D navigation. Bound in the FerailleGrid context,
    // which is more specific than SHELL_CONTEXT, so these win over the
    // table's 1-D Cursor* arrow bindings whenever the grid is focused.
    let grid_ctx = Some(crate::grid::GRID_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("left", GridLeft, grid_ctx),
        KeyBinding::new("right", GridRight, grid_ctx),
        KeyBinding::new("up", GridUp, grid_ctx),
        KeyBinding::new("down", GridDown, grid_ctx),
        KeyBinding::new("shift-left", GridLeftExtend, grid_ctx),
        KeyBinding::new("shift-right", GridRightExtend, grid_ctx),
        KeyBinding::new("shift-up", GridUpExtend, grid_ctx),
        KeyBinding::new("shift-down", GridDownExtend, grid_ctx),
    ]);

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
        "up" | "down" | "left" | "right" | "home" | "end" | "pageup" | "pagedown" | "escape"
        | "enter" | "tab" | "space" | "backspace" | "delete" | "f1" | "f2" | "f3" | "f4" | "f5"
        | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" => Some(lower),
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
        "file.reopen_closed_tab" => {
            cx.bind_keys([KeyBinding::new(kb_str, ReopenClosedTab, ctx)])
        }
        "file.new_folder" => cx.bind_keys([KeyBinding::new(kb_str, NewFolder, ctx)]),
        "file.move_to_trash" => cx.bind_keys([KeyBinding::new(kb_str, MoveToTrash, ctx)]),
        // No catalogue shortcut (the Shortcut DSL lacks Delete);
        // install_extras binds Finder's cmd-shift-backspace chord.
        "file.empty_trash" => {}
        "file.copy_path" => cx.bind_keys([KeyBinding::new(kb_str, CopyPath, ctx)]),
        "file.copy" => cx.bind_keys([KeyBinding::new(kb_str, CopyFiles, ctx)]),
        "file.cut" => cx.bind_keys([KeyBinding::new(kb_str, CutFiles, ctx)]),
        "file.paste" => cx.bind_keys([KeyBinding::new(kb_str, PasteFiles, ctx)]),
        "file.move_paste" => cx.bind_keys([KeyBinding::new(kb_str, MovePasteFiles, ctx)]),
        "file.reveal_in_finder" => cx.bind_keys([KeyBinding::new(kb_str, RevealInFinder, ctx)]),
        "file.refresh" => cx.bind_keys([KeyBinding::new(kb_str, Refresh, ctx)]),

        // -- View -------------------------------------------------
        "view.search" => cx.bind_keys([KeyBinding::new(kb_str, FocusFilter, ctx)]),
        "view.toggle_hidden" => cx.bind_keys([KeyBinding::new(kb_str, ToggleHidden, ctx)]),
        "view.edit_breadcrumb" => cx.bind_keys([KeyBinding::new(kb_str, EditBreadcrumb, ctx)]),
        "view.toggle_preview" => cx.bind_keys([KeyBinding::new(kb_str, TogglePreview, ctx)]),
        "view.open_viewer" => cx.bind_keys([KeyBinding::new(kb_str, OpenViewer, ctx)]),
        // Sort commands have no shortcut today; the arms exist so the
        // catalogue→palette path (and any future binding) recognizes
        // them instead of falling through to the unknown-id warning.
        "view.sort_name" => {}
        "view.sort_size" => {}
        "view.sort_kind" => {}
        "view.sort_modified" => {}
        "view.zoom_in" => cx.bind_keys([KeyBinding::new(kb_str, ZoomIn, ctx)]),
        "view.zoom_out" => cx.bind_keys([KeyBinding::new(kb_str, ZoomOut, ctx)]),
        "view.zoom_reset" => cx.bind_keys([KeyBinding::new(kb_str, ZoomReset, ctx)]),

        // -- File: Get Info --------------------------------------
        "file.get_info" => cx.bind_keys([KeyBinding::new(kb_str, GetInfo, ctx)]),

        // -- Selection cursor nav --------------------------------
        "selection.cursor_up" => cx.bind_keys([KeyBinding::new(kb_str, CursorUp, ctx)]),
        "selection.cursor_down" => cx.bind_keys([KeyBinding::new(kb_str, CursorDown, ctx)]),
        "selection.cursor_first" => cx.bind_keys([KeyBinding::new(kb_str, CursorFirst, ctx)]),
        "selection.cursor_last" => cx.bind_keys([KeyBinding::new(kb_str, CursorLast, ctx)]),
        "selection.page_up" => cx.bind_keys([KeyBinding::new(kb_str, PageUp, ctx)]),
        "selection.page_down" => cx.bind_keys([KeyBinding::new(kb_str, PageDown, ctx)]),

        // -- Help -------------------------------------------------
        "help.shortcuts" => {
            // The shortcuts-help overlay doubles as our command
            // palette today (searchable list of every catalogued
            // command + its chord). Bind both Cmd+/ (the catalogue's
            // declared shortcut) and Cmd+K (the de-facto command-
            // palette key in modern apps) to the same action.
            cx.bind_keys([
                KeyBinding::new(kb_str, ShortcutsHelp, ctx),
                KeyBinding::new("cmd-k", ShortcutsHelp, ctx),
            ])
        }

        // -- Disk Usage -------------------------------------------
        "view.disk_usage" => cx.bind_keys([KeyBinding::new(kb_str, OpenDiskUsage, ctx)]),
        "view.find_duplicates" => cx.bind_keys([KeyBinding::new(kb_str, FindDuplicates, ctx)]),

        // -- Go ---------------------------------------------------
        "go.back" => cx.bind_keys([KeyBinding::new(kb_str, NavigateBack, ctx)]),
        "go.forward" => cx.bind_keys([KeyBinding::new(kb_str, NavigateForward, ctx)]),
        "go.parent" => cx.bind_keys([KeyBinding::new(kb_str, NavigateParent, ctx)]),
        "go.home" => cx.bind_keys([KeyBinding::new(kb_str, GoHome, ctx)]),

        // -- Selection --------------------------------------------
        "selection.activate" => cx.bind_keys([KeyBinding::new(kb_str, OpenSelected, ctx)]),
        "selection.rename" | "selection.start_rename" => {
            cx.bind_keys([KeyBinding::new(kb_str, RenameSelected, ctx)])
        }
        "selection.collapse_or_parent" => {
            cx.bind_keys([KeyBinding::new(kb_str, NavigateParent, ctx)])
        }
        "selection.expand_or_first_child" => {
            cx.bind_keys([KeyBinding::new(kb_str, OpenSelected, ctx)])
        }
        "selection.dismiss" => cx.bind_keys([KeyBinding::new(kb_str, ClearFilter, ctx)]),

        // -- Window: tab cycling ---------------------------------
        "window.next_tab" => cx.bind_keys([KeyBinding::new(kb_str, NextTab, ctx)]),
        "window.prev_tab" => cx.bind_keys([KeyBinding::new(kb_str, PrevTab, ctx)]),
        "window.close_window" => cx.bind_keys([KeyBinding::new(kb_str, CloseWindow, ctx)]),

        // -- File: open in new tab -------------------------------
        "file.open_in_new_tab" => cx.bind_keys([KeyBinding::new(kb_str, OpenInNewTab, ctx)]),

        // -- Tab cycling. These aren't in the canonical catalogue
        //    yet (Stage 5.5.d added them locally); bind from the
        //    same context but use our existing constants so menu
        //    parity later remains a one-line add.
        // (handled below)

        // -- Not yet ported. Each is wired in a later stage.  -----
        // Known-deferred catalogue entries — handlers land in later
        // iters. Silent to keep startup logs clean; PORTING.md
        // tracks status authoritatively. New commands fall through
        // to the _ arm below which DOES log so genuinely-unknown
        // IDs still surface.
        // `window.new_window` is bound at App level (Cmd+N opens a new
        // shell window regardless of focus). main.rs installs the
        // binding directly via `cx.bind_keys` — we leave it out of
        // SHELL_CONTEXT here so a missing-focus user can still hit it.
        "window.new_window"
        | "app.about"
        | "view.cycle_focus"
        | "view.theme_light"
        | "view.theme_dark"
        | "view.theme_system"
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
        | "help.github" => {}

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
        // Phase D `cmd-shift-t` (ReopenClosedTab) goes through the
        // command catalogue (`file.reopen_closed_tab`) so the menu
        // bar and command palette pick it up automatically. No need
        // to repeat the binding here.
        KeyBinding::new("ctrl-tab", NextTab, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", PrevTab, Some(shell::SHELL_CONTEXT)),
        // Stage 8: macOS Quick Look. Space-bar on the selected row
        // pops the QL panel. Not in the catalogue because Quick
        // Look is a macOS-specific affordance; the binding lives
        // here alongside other shell-context extras.
        KeyBinding::new("space", QuickLook, Some(shell::SHELL_CONTEXT)),
        // Finder's Empty Trash chord [mac]. Backspace = the Mac
        // Delete key in gpui's DSL.
        KeyBinding::new(
            "cmd-shift-backspace",
            EmptyTrash,
            Some(shell::SHELL_CONTEXT),
        ),
        // Favorites toggle on the currently-selected folder
        // (docs/features/FAVORITES.md). Cmd+D mirrors Finder's
        // "Add to Sidebar" muscle memory and avoids the Cmd+T
        // collision with NewTab.
        KeyBinding::new(
            "cmd-d",
            crate::shell::ToggleFavoriteForTarget,
            Some(shell::SHELL_CONTEXT),
        ),
        // Keyboard reorder of the focused favorite (§4.4).
        KeyBinding::new(
            "cmd-alt-up",
            crate::shell::MoveFavoriteUp,
            Some(shell::SHELL_CONTEXT),
        ),
        KeyBinding::new(
            "cmd-alt-down",
            crate::shell::MoveFavoriteDown,
            Some(shell::SHELL_CONTEXT),
        ),
        // Spec §2.5 multi-select keyboard:
        //   Cmd+A — select every visible row.
        //   Shift+Up/Down/Home/End/PgUp/PgDn — Shift-extend the lead
        //   keeping the anchor fixed. The non-Shift variants are
        //   already bound through the command catalogue
        //   (`selection.cursor_up` etc.); these augment them.
        // The catalogue stays the source of truth for the plain
        // variants; the extend variants are shell-local for now and
        // can move into the catalogue when other surfaces (menu
        // bar, command palette) need to enumerate them.
        KeyBinding::new("cmd-a", SelectAll, Some(shell::SHELL_CONTEXT)),
        // Spec §2.5: Cmd+Up/Down → jump to first/last row (plain nav).
        KeyBinding::new("cmd-up", CursorFirst, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new("cmd-down", CursorLast, Some(shell::SHELL_CONTEXT)),
        // Shift-extend variants for arrow, Home/End, PgUp/PgDn,
        // and Cmd+Shift for first/last extend.
        KeyBinding::new("shift-up", CursorUpExtend, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new("shift-down", CursorDownExtend, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new(
            "cmd-shift-up",
            CursorFirstExtend,
            Some(shell::SHELL_CONTEXT),
        ),
        KeyBinding::new(
            "cmd-shift-down",
            CursorLastExtend,
            Some(shell::SHELL_CONTEXT),
        ),
        KeyBinding::new("shift-home", CursorFirstExtend, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new("shift-end", CursorLastExtend, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new("shift-pageup", PageUpExtend, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new("shift-pagedown", PageDownExtend, Some(shell::SHELL_CONTEXT)),
        // Viewer window (docs/features/VIEWER.md). Not in the
        // catalogue: these are window-local keys, like a dialog's —
        // the catalogue carries the commands other surfaces (menu
        // bar, palette) must enumerate, and for the viewer that's
        // only `view.open_viewer`. [mac] Cmd-chords + Cmd+Ctrl+F;
        // win-parity remaps to Ctrl / F11 when the Windows shell
        // lands.
        KeyBinding::new("left", ViewerPrev, Some(VIEWER_CONTEXT)),
        KeyBinding::new("up", ViewerPrev, Some(VIEWER_CONTEXT)),
        KeyBinding::new("right", ViewerNext, Some(VIEWER_CONTEXT)),
        KeyBinding::new("down", ViewerNext, Some(VIEWER_CONTEXT)),
        KeyBinding::new("escape", ViewerDismiss, Some(VIEWER_CONTEXT)),
        KeyBinding::new("escape", EntryInfoDismiss, Some(ENTRY_INFO_CONTEXT)),
        KeyBinding::new("space", ViewerTogglePlay, Some(VIEWER_CONTEXT)),
        KeyBinding::new("cmd-=", ViewerZoomIn, Some(VIEWER_CONTEXT)),
        KeyBinding::new("cmd--", ViewerZoomOut, Some(VIEWER_CONTEXT)),
        KeyBinding::new("cmd-0", ViewerZoomReset, Some(VIEWER_CONTEXT)),
        KeyBinding::new("cmd-1", ViewerActualSize, Some(VIEWER_CONTEXT)),
        KeyBinding::new("cmd-ctrl-f", ViewerToggleFullscreen, Some(VIEWER_CONTEXT)),
        // View-only per-item rotation (docs/features/VIEWER.md): R turns
        // clockwise, Shift-R counter-clockwise.
        KeyBinding::new("r", ViewerRotateCw, Some(VIEWER_CONTEXT)),
        KeyBinding::new("shift-r", ViewerRotateCcw, Some(VIEWER_CONTEXT)),
    ]);
}
