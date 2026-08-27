//! Command-catalogue-driven keymap installation.
//!
//! Single source of truth for which shortcut fires what command:
//! [`ferail_core::commands`]. At app start we walk
//! the catalogue, translate each `Shortcut` into the gpui
//! `KeyBinding` string DSL, and bind it to the matching gpui
//! `Action` type. Commands without a matching action (because the
//! feature hasn't been ported yet) get a one-time log warning and
//! are skipped.
//!
//! When a new command's shortcut needs to change, the edit lands in
//! `ferail-core::commands` and propagates everywhere (this
//! keymap, the native AppKit menu bar, the future
//! Keyboard-Shortcuts dialog).

use ferail_core::commands::{CommandId, Shortcut, all_commands};
use gpui::{App, KeyBinding};

use crate::entry_info::{ENTRY_INFO_CONTEXT, EntryInfoDismiss};
use crate::private_mode::TogglePrivateMode;
#[cfg(windows)]
use crate::shell::ShowWindowsContextMenu;
use crate::shell::{
    self, ClearFilter, CloseTab, CloseToolResult, CloseWindow, CopyFiles, CopyPath, CursorDown,
    CursorDownExtend, CursorFirst, CursorFirstExtend, CursorLast, CursorLastExtend, CursorUp,
    CursorUpExtend, CutFiles, DeleteImmediately, EditBreadcrumb, EmptyTrash, FindDuplicates,
    FindSimilarImages, FocusFilter, GetInfo, GoHome, GoToFolder, GridDown, GridDownExtend,
    GridLeft, GridLeftExtend, GridRight, GridRightExtend, GridUp, GridUpExtend, MovePasteFiles,
    MoveToTrash, NavigateBack, NavigateForward, NavigateParent, NewFolder, NewTab, NextTab,
    OpenDiskUsage, OpenInNewTab, OpenSelected, OpenSettings, OpenViewer, PageDown, PageDownExtend,
    PageUp, PageUpExtend, PasteFiles, PopOutDiskUsage, PrevTab, QuickLook, Refresh, RenameSelected,
    ReopenClosedTab, RevealInFinder, SelectAll, ShortcutsHelp, ToggleFlatView, ToggleHidden,
    TogglePreview, ZoomIn, ZoomOut, ZoomReset,
};
use crate::viewer::window::{
    VIEWER_CONTEXT, ViewerActualSize, ViewerDelete, ViewerDismiss, ViewerLeft, ViewerNext,
    ViewerPrev, ViewerRight, ViewerRotateCcw, ViewerRotateCw, ViewerToggleAdjust,
    ViewerToggleFullscreen, ViewerTogglePlay, ViewerZoomIn, ViewerZoomOut, ViewerZoomReset,
};

/// Install keybindings for every command in `ferail_core::commands`
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
    #[cfg(windows)]
    cx.bind_keys([KeyBinding::new(
        "shift-f10",
        ShowWindowsContextMenu,
        Some(shell::SHELL_CONTEXT),
    )]);
    // Undo (Cmd+Z) — not in the catalogue today because the action
    // is a UI-layer Shell helper (replays UndoOp inverse from the
    // Shell::undo_stack), not a catalogued command in ferail-core.
    cx.bind_keys([KeyBinding::new(
        "secondary-z",
        crate::shell::UndoLastAction,
        Some(shell::SHELL_CONTEXT),
    )]);

    // Disk Usage treemap keys — DiskUsage context only, so they never
    // shadow the file list. Enter zooms into the selected folder,
    // Backspace zooms out, Escape clears the selection; Cmd+C / Cmd+I /
    // Cmd+Backspace mirror the file-list verbs on the treemap
    // selection.
    let du_ctx = Some(crate::disk_usage::DISK_USAGE_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("enter", crate::disk_usage::DuZoomIn, du_ctx),
        KeyBinding::new("backspace", crate::disk_usage::DuZoomOut, du_ctx),
        KeyBinding::new("escape", crate::disk_usage::DuClearSelection, du_ctx),
        KeyBinding::new("secondary-c", crate::disk_usage::DuCopyFiles, du_ctx),
        KeyBinding::new("secondary-i", crate::disk_usage::DuGetInfo, du_ctx),
        KeyBinding::new("secondary-backspace", crate::disk_usage::DuTrash, du_ctx),
    ]);

    // Icon-grid 2-D navigation. Bound in the FerailGrid context,
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

/// Translate a `ferail_core::commands::Shortcut` to gpui's
/// keybinding-string DSL. Returns `None` when the key name doesn't
/// translate (e.g. a future "Media-Play" entry we don't bind).
fn translate_shortcut(s: &Shortcut) -> Option<String> {
    let key = translate_key(s.key)?;
    let mut parts: Vec<&str> = Vec::with_capacity(4);
    if s.primary {
        // The catalogue's `primary` is Cmd on macOS, Ctrl on Windows/Linux.
        // Use gpui's portable `secondary` token: it resolves to the platform
        // key (Cmd) on macOS and to Control everywhere else. Plain `cmd` does
        // NOT do this — gpui maps `cmd` to the *platform* modifier, which on
        // Windows is the Windows logo key, so `cmd-shift-p` would demand
        // Win+Shift+P instead of Ctrl+Shift+P (and Win+P is OS-reserved).
        parts.push("secondary");
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
fn install_binding(cx: &mut App, id: CommandId, kb_str: &str) -> bool {
    let ctx = Some(shell::SHELL_CONTEXT);
    macro_rules! bind {
        ($action:expr, $context:expr) => {{
            cx.bind_keys([KeyBinding::new(kb_str, $action, $context)]);
            true
        }};
    }
    let recognized = match id.0 {
        // -- App --------------------------------------------------
        "app.settings" => bind!(OpenSettings, None),

        // -- File -------------------------------------------------
        "file.new_tab" => bind!(NewTab, ctx),
        "file.close_tab" => bind!(CloseTab, ctx),
        "file.reopen_closed_tab" => bind!(ReopenClosedTab, ctx),
        "file.new_folder" => bind!(NewFolder, ctx),
        "file.move_to_trash" => bind!(MoveToTrash, ctx),
        // No catalogue shortcut (the Shortcut DSL lacks Delete);
        // install_extras binds shift-delete / secondary-alt-backspace.
        "file.delete_immediately" => true,
        // No catalogue shortcut (the Shortcut DSL lacks Delete);
        // install_extras binds Finder's cmd-shift-backspace chord.
        "file.empty_trash" => true,
        "file.copy_path" => bind!(CopyPath, ctx),
        "file.copy" => bind!(CopyFiles, ctx),
        "file.cut" => bind!(CutFiles, ctx),
        "file.paste" => bind!(PasteFiles, ctx),
        "file.move_paste" => bind!(MovePasteFiles, ctx),
        "file.reveal_in_finder" => bind!(RevealInFinder, ctx),
        "file.refresh" => bind!(Refresh, ctx),

        // -- View -------------------------------------------------
        "view.search" => bind!(FocusFilter, ctx),
        "view.toggle_hidden" => bind!(ToggleHidden, ctx),
        "view.toggle_private" => bind!(TogglePrivateMode, None),
        "view.edit_breadcrumb" => bind!(EditBreadcrumb, ctx),
        "view.toggle_preview" => bind!(TogglePreview, ctx),
        "view.toggle_flat" => bind!(ToggleFlatView, ctx),
        "view.open_viewer" => bind!(OpenViewer, ctx),
        // Sort commands have no shortcut today; the arms exist so the
        // catalogue→palette path (and any future binding) recognizes
        // them instead of falling through to the unknown-id warning.
        "view.sort_name" => true,
        "view.sort_size" => true,
        "view.sort_kind" => true,
        "view.sort_modified" => true,
        "view.sort_ant_trail" => true,
        "view.zoom_in" => bind!(ZoomIn, ctx),
        "view.zoom_out" => bind!(ZoomOut, ctx),
        "view.zoom_reset" => bind!(ZoomReset, ctx),

        // -- File: Get Info --------------------------------------
        "file.get_info" => bind!(GetInfo, ctx),

        // -- Selection cursor nav --------------------------------
        "selection.cursor_up" => bind!(CursorUp, ctx),
        "selection.cursor_down" => bind!(CursorDown, ctx),
        "selection.cursor_first" => bind!(CursorFirst, ctx),
        "selection.cursor_last" => bind!(CursorLast, ctx),
        "selection.page_up" => bind!(PageUp, ctx),
        "selection.page_down" => bind!(PageDown, ctx),

        // -- Help -------------------------------------------------
        "help.shortcuts" => {
            // The shortcuts-help overlay doubles as our command
            // palette today (searchable list of every catalogued
            // command + its chord). Bind both Cmd+/ (the catalogue's
            // declared shortcut) and Cmd+K (the de-facto command-
            // palette key in modern apps) to the same action.
            cx.bind_keys([
                KeyBinding::new(kb_str, ShortcutsHelp, ctx),
                KeyBinding::new("secondary-k", ShortcutsHelp, ctx),
            ]);
            true
        }

        // -- Disk Usage -------------------------------------------
        "view.disk_usage" => bind!(OpenDiskUsage, ctx),
        "view.close_results" => bind!(CloseToolResult, ctx),
        "disk_usage.open_in_window" => {
            bind!(PopOutDiskUsage, ctx)
        }
        "view.find_duplicates" => bind!(FindDuplicates, ctx),
        "view.find_similar_images" => {
            bind!(FindSimilarImages, ctx)
        }

        // -- Go ---------------------------------------------------
        "go.back" => bind!(NavigateBack, ctx),
        "go.forward" => bind!(NavigateForward, ctx),
        "go.parent" => bind!(NavigateParent, ctx),
        "go.home" => bind!(GoHome, ctx),
        // No context: Cmd+G has to reach the App-level fallback in
        // `boot` when no Shell window is open (the process stays
        // resident at zero windows), the same way Cmd+N does.
        "go.go_to_folder" => bind!(GoToFolder, None),

        // -- Selection --------------------------------------------
        "selection.activate" => bind!(OpenSelected, ctx),
        "selection.rename" | "selection.start_rename" => {
            bind!(RenameSelected, ctx)
        }
        "selection.collapse_or_parent" => {
            bind!(NavigateParent, ctx)
        }
        "selection.expand_or_first_child" => {
            bind!(OpenSelected, ctx)
        }
        "selection.dismiss" => bind!(ClearFilter, ctx),

        // -- Window: tab cycling ---------------------------------
        "window.next_tab" => bind!(NextTab, ctx),
        "window.prev_tab" => bind!(PrevTab, ctx),
        "window.close_window" => bind!(CloseWindow, ctx),

        // -- File: open in new tab -------------------------------
        "file.open_in_new_tab" => bind!(OpenInNewTab, ctx),

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
        | "help.github" => true,

        _ => false,
    };
    if !recognized {
        crate::log_warn!(
            90,
            "keymap: unknown command id '{}'; binding '{}' skipped",
            id.0,
            kb_str
        );
    }
    recognized
}

/// Tab-cycling and filter-escape bindings live outside the
/// `ferail_core::commands` catalogue today (they were added in
/// the GPUI shell during Phase 5.5). Install them alongside the
/// catalogue-driven bindings so a single `keymap::install` call
/// covers every shortcut the shell handles.
pub(crate) fn install_extras(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-t", NewTab, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new("secondary-w", CloseTab, Some(shell::SHELL_CONTEXT)),
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
            "secondary-shift-backspace",
            EmptyTrash,
            Some(shell::SHELL_CONTEXT),
        ),
        // Permanent delete of the selection (no trash). Finder's
        // Option+Cmd+Delete [mac]; Shift+Delete is the Windows/Linux
        // convention. Like Empty Trash, the Delete key keeps these out of
        // the Shortcut catalogue, so they're bound here.
        #[cfg(target_os = "macos")]
        KeyBinding::new(
            "secondary-alt-backspace",
            DeleteImmediately,
            Some(shell::SHELL_CONTEXT),
        ),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new(
            "shift-delete",
            DeleteImmediately,
            Some(shell::SHELL_CONTEXT),
        ),
        // Favorites toggle on the currently-selected folder
        // (docs/features/FAVORITES.md). Cmd+D mirrors Finder's
        // "Add to Sidebar" muscle memory and avoids the Cmd+T
        // collision with NewTab.
        KeyBinding::new(
            "secondary-d",
            crate::shell::ToggleFavoriteForTarget,
            Some(shell::SHELL_CONTEXT),
        ),
        // Keyboard reorder of the focused favorite (§4.4).
        KeyBinding::new(
            "secondary-alt-up",
            crate::shell::MoveFavoriteUp,
            Some(shell::SHELL_CONTEXT),
        ),
        KeyBinding::new(
            "secondary-alt-down",
            crate::shell::MoveFavoriteDown,
            Some(shell::SHELL_CONTEXT),
        ),
        // Arrow-key focus + Enter/Delete within the focused Favorites
        // section (§11.4). Bound in FAVORITES_CONTEXT — more specific
        // than SHELL_CONTEXT — so they only fire while the section is
        // focused, never stealing the file list's arrow navigation.
        KeyBinding::new(
            "up",
            crate::shell::FocusFavoriteUp,
            Some(crate::favorites_section::FAVORITES_CONTEXT),
        ),
        KeyBinding::new(
            "down",
            crate::shell::FocusFavoriteDown,
            Some(crate::favorites_section::FAVORITES_CONTEXT),
        ),
        KeyBinding::new(
            "enter",
            crate::shell::ActivateFavorite,
            Some(crate::favorites_section::FAVORITES_CONTEXT),
        ),
        KeyBinding::new(
            "backspace",
            crate::shell::DeleteFavorite,
            Some(crate::favorites_section::FAVORITES_CONTEXT),
        ),
        KeyBinding::new(
            "delete",
            crate::shell::DeleteFavorite,
            Some(crate::favorites_section::FAVORITES_CONTEXT),
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
        KeyBinding::new("secondary-a", SelectAll, Some(shell::SHELL_CONTEXT)),
        // Spec §2.5: Cmd+Up/Down → jump to first/last row (plain nav).
        KeyBinding::new("secondary-up", CursorFirst, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new("secondary-down", CursorLast, Some(shell::SHELL_CONTEXT)),
        // Shift-extend variants for arrow, Home/End, PgUp/PgDn,
        // and Cmd+Shift for first/last extend.
        KeyBinding::new("shift-up", CursorUpExtend, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new("shift-down", CursorDownExtend, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new(
            "secondary-shift-up",
            CursorFirstExtend,
            Some(shell::SHELL_CONTEXT),
        ),
        KeyBinding::new(
            "secondary-shift-down",
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
        // Left/Right are video-aware (frame-step a clip, navigate a
        // still); Up/Down are always entry navigation so a video is
        // still reachable from the keyboard. See ViewerWindow::on_left.
        KeyBinding::new("left", ViewerLeft, Some(VIEWER_CONTEXT)),
        KeyBinding::new("right", ViewerRight, Some(VIEWER_CONTEXT)),
        KeyBinding::new("up", ViewerPrev, Some(VIEWER_CONTEXT)),
        KeyBinding::new("down", ViewerNext, Some(VIEWER_CONTEXT)),
        // Ctrl+Left/Right always navigate entries — the horizontal twin of
        // Up/Down, so on a video (where plain Left/Right frame-step) you can
        // still flip to the previous/next clip without reaching for the
        // arrows' vertical pair. (On macOS these may be claimed by Mission
        // Control's "move a space" shortcut; Up/Down remain as a fallback.)
        KeyBinding::new("ctrl-left", ViewerPrev, Some(VIEWER_CONTEXT)),
        KeyBinding::new("ctrl-right", ViewerNext, Some(VIEWER_CONTEXT)),
        KeyBinding::new("escape", ViewerDismiss, Some(VIEWER_CONTEXT)),
        KeyBinding::new("escape", EntryInfoDismiss, Some(ENTRY_INFO_CONTEXT)),
        KeyBinding::new("space", ViewerTogglePlay, Some(VIEWER_CONTEXT)),
        KeyBinding::new("secondary-=", ViewerZoomIn, Some(VIEWER_CONTEXT)),
        KeyBinding::new("secondary--", ViewerZoomOut, Some(VIEWER_CONTEXT)),
        KeyBinding::new("secondary-0", ViewerZoomReset, Some(VIEWER_CONTEXT)),
        KeyBinding::new("secondary-1", ViewerActualSize, Some(VIEWER_CONTEXT)),
        KeyBinding::new(
            "secondary-ctrl-f",
            ViewerToggleFullscreen,
            Some(VIEWER_CONTEXT),
        ),
        // View-only per-item rotation (docs/features/VIEWER.md): R turns
        // clockwise, Shift-R counter-clockwise.
        KeyBinding::new("r", ViewerRotateCw, Some(VIEWER_CONTEXT)),
        KeyBinding::new("shift-r", ViewerRotateCcw, Some(VIEWER_CONTEXT)),
        // Colour adjustments popup (brightness / contrast / colour). Also
        // reachable by right-click on the stage or the toolbar button.
        KeyBinding::new("e", ViewerToggleAdjust, Some(VIEWER_CONTEXT)),
        // Move the current file to the Trash and advance. Cmd+Backspace is
        // the canonical Finder binding. We also bind the bare delete keys so
        // pressing Delete on its own works (safe here — the viewer has no
        // text inputs): on macOS the ⌫ key above Return emits `backspace`,
        // while `delete` is forward-delete (⌦ / fn+⌫), so bind both to cover
        // every keyboard — matching the favorites `DeleteFavorite` binding.
        KeyBinding::new("secondary-backspace", ViewerDelete, Some(VIEWER_CONTEXT)),
        KeyBinding::new("backspace", ViewerDelete, Some(VIEWER_CONTEXT)),
        KeyBinding::new("delete", ViewerDelete, Some(VIEWER_CONTEXT)),
        // -- Keyboard-layout alternates for the digit-based zoom keys --
        // This build's gpui has no platform key-equivalents mapper, so
        // bindings match the *character* a key produces, with no
        // US-layout remapping (which is what lets ⌘0 work in native apps
        // on a French keyboard). On AZERTY — and any layout where the
        // number row needs Shift — pressing ⌘0 / ⌘1 physically emits
        // `cmd-shift-0` / `cmd-shift-1`, so the catalogue's `cmd-0` never
        // fires. Bind the shifted forms as alternates. Harmless on US
        // layouts (no one presses ⌘⇧0); the catalogue keeps the canonical
        // `cmd-0` so the menu/settings still read "⌘0". `=`/`+` get the
        // same treatment for zoom-in; `-` is unshifted on these layouts
        // so it already works.
        KeyBinding::new("secondary-shift-0", ZoomReset, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new("secondary-shift-=", ZoomIn, Some(shell::SHELL_CONTEXT)),
        KeyBinding::new("secondary-shift-0", ViewerZoomReset, Some(VIEWER_CONTEXT)),
        KeyBinding::new("secondary-shift-1", ViewerActualSize, Some(VIEWER_CONTEXT)),
        KeyBinding::new("secondary-shift-=", ViewerZoomIn, Some(VIEWER_CONTEXT)),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    /// Exercises the same dispatcher used at startup. Adding a shortcut to the
    /// bundled catalogue without adding its GPUI/external/deferred route now
    /// fails on every target instead of becoming a customer log warning.
    #[gpui::test]
    fn every_bundled_shortcut_has_a_recognized_route(cx: &mut TestAppContext) {
        let mut unresolved = Vec::new();
        cx.update(|app| {
            for spec in all_commands() {
                for shortcut in spec.shortcuts {
                    let Some(binding) = translate_shortcut(shortcut) else {
                        // `+` is an explicitly documented alternate for `=`;
                        // GPUI's DSL uses plus as its chord separator.
                        assert_eq!(shortcut.key, "+");
                        continue;
                    };
                    if !install_binding(app, spec.id, &binding) {
                        unresolved.push((spec.id.0, binding));
                    }
                }
            }
        });
        assert!(unresolved.is_empty(), "unresolved bindings: {unresolved:?}");
    }
}
