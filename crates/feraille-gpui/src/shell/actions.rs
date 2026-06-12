use gpui::actions;

actions!(
    shell,
    [
        NavigateParent,
        NavigateBack,
        NavigateForward,
        OpenSelected,
        Refresh,
        ToggleHidden,
        OpenSettings,
        CopyPath,
        /// Folder-only file-list context action — open a terminal at the
        /// right-clicked directory. Resolves its target the same way
        /// `CopyPath` does (`action_entries_visible_order`, consuming
        /// `context_row`); the sidebar/tree equivalent is
        /// `OpenTerminalAtContext`, which reads `context_target`.
        OpenTerminalHere,
        MoveToTrash,
        RevealInFinder,
        FocusFilter,
        ClearFilter,
        NewFolder,
        RenameSelected,
        NewTab,
        CloseTab,
        /// Cmd+Shift+W — close the entire window regardless of tab
        /// count. Last-tab Cmd+W also closes the window per spec §3.4,
        /// but this is the "I mean *the window*" verb for users with
        /// many tabs open.
        CloseWindow,
        /// Cmd+Shift+T — pop the most-recently-closed tab off
        /// `ProcessState::closed_tabs` and reopen it (spec §3.3
        /// "Reopen closed tab"). Restores directory, history, filter,
        /// and best-effort selection; sort restore is deferred.
        /// No-op when the stack is empty.
        ReopenClosedTab,
        NextTab,
        PrevTab,
        QuickLook,
        /// Cmd+Y — open the viewer window on the current folder's
        /// files (docs/features/VIEWER.md). Space stays Quick Look.
        OpenViewer,
        GoHome,
        EditBreadcrumb,
        ShortcutsHelp,
        OpenDiskUsage,
        CursorUp,
        CursorDown,
        CursorFirst,
        CursorLast,
        PageUp,
        PageDown,
        // Spec §2.5 — Shift-extend variants. The plain `Cursor*` /
        // `Page*` set above move the lead and collapse the selection
        // to just that row; the extend variants move the lead and
        // make the selection the inclusive span from anchor to lead.
        CursorUpExtend,
        CursorDownExtend,
        CursorFirstExtend,
        CursorLastExtend,
        PageUpExtend,
        PageDownExtend,
        /// Cmd+A — selection becomes every row currently in the
        /// (filtered) model. anchor = first visible row, lead = last
        /// visible row. Spec §2.5.
        SelectAll,
        /// Esc on a non-empty selection — clear the selection set,
        /// anchor and lead. Higher-precedence Esc behaviors
        /// (close-shortcuts-overlay, ClearFilter) are still bound
        /// against the filter input's own focus context; this fires
        /// only when the shell pane itself owns focus.
        ClearSelection,
        TogglePreview,
        GetInfo,
        /// Strip the Mark-of-the-Web (and its where-from provenance)
        /// from the selected quarantined files — `com.apple.quarantine`
        /// + `kMDItemWhereFroms` on macOS, the `Zone.Identifier` ADS on
        /// Windows. Worker-side I/O; the rows and the metadata-DB cache
        /// update on completion so the badge can't resurrect from cache.
        ClearQuarantine,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        OpenInNewTab,
        Duplicate,
        MakeAlias,
        Compress,
        // Phase 6 (next-level): right-click context menus on the
        // sidebar / breadcrumb / file-pane background. These four
        // actions all operate on `Shell::context_target` instead
        // of the file-list selection. The right-click event hands
        // the closure in `context_menu(...)` a PathBuf; the
        // closure stashes it on Shell, then dispatches one of the
        // actions below. Handlers `take()` the target so the next
        // keyboard-driven action falls back to the regular row
        // selection.
        RevealContextPath,
        CopyContextPath,
        /// Sidebar/tree/breadcrumb sibling of `OpenTerminalHere` — open a
        /// terminal at `context_target` (the right-clicked folder). Split
        /// from the file-list `OpenTerminalHere` so a dismissed sidebar
        /// menu's stale `context_target` can't hijack a file-list action.
        OpenTerminalAtContext,
        OpenContextInNewTab,
        NewFolderHere,
        // Phase 6 follow-on: Tags + Open-With submenus. Seven tag
        // colours match Finder's canonical Red/Orange/Yellow/Green/
        // Blue/Purple/Gray set; toggle behaviour mirrors
        // `crate::platform_shell::toggle_tag`.
        ToggleTagRed,
        ToggleTagOrange,
        ToggleTagYellow,
        ToggleTagGreen,
        ToggleTagBlue,
        ToggleTagPurple,
        ToggleTagGray,
        // Twelve indexed slots for the Open-With submenu. The menu
        // builder lays out at most this many candidate apps; each
        // handler re-resolves the candidates for the target row at
        // dispatch time and opens slot N. Twelve covers every file
        // kind we've seen in practice (~5 is typical).
        OpenWithSlot0,
        OpenWithSlot1,
        OpenWithSlot2,
        OpenWithSlot3,
        OpenWithSlot4,
        OpenWithSlot5,
        OpenWithSlot6,
        OpenWithSlot7,
        OpenWithSlot8,
        OpenWithSlot9,
        OpenWithSlot10,
        OpenWithSlot11,
        /// Cmd+Z — pop the most recent reversible action off
        /// Shell::undo_stack and replay its inverse. Currently handles
        /// Rename (rename back) and NewFolder (delete the created
        /// folder); Move-to-Trash undo is documented in the deferred
        /// list (needs NSFileManager.trashItemAt to return the trash
        /// URL so we can move the file back).
        UndoLastAction,
        // Favorites (docs/features/FAVORITES.md). The toggle action
        // is the unified Cmd+D / menu-bar / row-context-menu entry
        // point: it adds the target if absent, removes it if present,
        // pulses the row on a dedup attempt (which can't happen via
        // toggle, but matches the spec's verb shape). The target is
        // either:
        //   - `Shell::favorites_context_path` (right-clicked source row,
        //     or right-clicked favorite row), if set
        //   - the file list selection (a folder), or
        //   - the active tab's current_dir as a last fallback.
        ToggleFavoriteForTarget,
        /// Append the active tab's current directory to Favorites.
        /// Backs the section-header `+` button and the menu bar item.
        AddCurrentFolderToFavorites,
        /// Section header click handler — also reachable via menu bar
        /// "Window → Toggle Favorites Section".
        ToggleFavoritesSection,
        // One-shot sorts (§4.5). Each rewrites every `sort_index` in
        // place; subsequent drags continue to work — the order isn't
        // "locked", it's just set.
        SortFavoritesByName,
        SortFavoritesByDateAddedNewest,
        SortFavoritesByDateAddedOldest,
        SortFavoritesByKind,
        /// Cmd+Option+Up — shift the most-recently-focused favorite
        /// up one slot in the section list (§4.4).
        MoveFavoriteUp,
        /// Cmd+Option+Down — shift down one slot.
        MoveFavoriteDown,
        /// Rename the favorite under `favorites_context_path` via a
        /// native NSAlert prompt (§6).
        RenameFavorite,
        /// Clear the favorite's custom display_name so it tracks the
        /// folder's on-disk basename again (§6 "Reset to Original Name").
        ResetFavoriteName,
        /// Strip a custom icon, falling back to kind+target default (§7).
        ResetFavoriteIcon,
        // Curated icon picks (§7 "Change Icon" submenu). Each sets
        // `custom_icon = Some(Lucide(subpath))` on the contextual
        // favorite, where the subpath references an asset under
        // `crates/feraille-gpui/resources/icons/` (e.g. "nav/star",
        // "file/code"). Six pre-curated picks; a full picker UI is a
        // future polish piece.
        SetFavoriteIconStar,
        SetFavoriteIconFolder,
        SetFavoriteIconCode,
        SetFavoriteIconImage,
        SetFavoriteIconMusic,
        SetFavoriteIconArchive,
    ]
);
