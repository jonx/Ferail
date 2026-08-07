use gpui::actions;

actions!(
    shell,
    [
        NavigateParent,
        NavigateBack,
        NavigateForward,
        OpenSelected,
        Refresh,
        /// Trigger the macOS "Show Desktop" reveal (the Dock's private
        /// `CoreDockSendNotification`). The toolbar button and menu item
        /// only appear when `platform_shell::show_desktop_available()` is
        /// true, so on platforms / OS versions without it this action is
        /// never reachable and its handler silently no-ops.
        ShowDesktop,
        ToggleHidden,
        OpenSettings,
        CopyPath,
        /// Copy the *entire* list of items currently shown in the active
        /// tab — folder contents, duplicate-finder groups, or search
        /// results — as newline-joined full paths, regardless of
        /// selection. The list-export complement to `CopyPath` (which
        /// copies just the selection). All three views populate the same
        /// table delegate, so one handler serves them all.
        CopyFileList,
        /// Folder-only file-list context action — open a terminal at the
        /// right-clicked directory. Resolves its target the same way
        /// `CopyPath` does (`action_entries_visible_order`, consuming
        /// `context_row`); the sidebar/tree equivalent is
        /// `OpenTerminalAtContext`, which reads `context_target`.
        OpenTerminalHere,
        MoveToTrash,
        /// Permanently delete the selected items without trashing first
        /// (Shift+Delete [win/linux], Option+Cmd+Delete [mac, Finder's
        /// chord]), after a counted confirmation. No undo — like a targeted
        /// Empty Trash. On a permission denial it offers an elevated retry
        /// (docs/features/FILE_OPS.md).
        DeleteImmediately,
        /// Cmd+Shift+Delete — permanently delete the contents of every
        /// reachable trash, after a counted confirmation dialog. The
        /// one file operation with no undo (docs/features/FILE_OPS.md).
        EmptyTrash,
        /// Cmd+C — selection's file URLs onto the general pasteboard
        /// (cross-app: Finder pastes what we copy). FILE_OPS.md.
        CopyFiles,
        /// Cmd+X — like Copy, but marks the items so the next plain
        /// Paste moves them (and clears the mark). FILE_OPS.md.
        CutFiles,
        /// Cmd+V — paste the pasteboard's file URLs into the current
        /// folder; a move if those items were Cut, else a copy.
        PasteFiles,
        /// Cmd+Option+V — Finder's "Move Items Here".
        MovePasteFiles,
        RevealInFinder,
        FocusFilter,
        ClearFilter,
        NewFolder,
        RenameSelected,
        /// Pattern-rule rename over the whole multi-selection —
        /// find/replace (literal or regex with $1..$9), case
        /// transforms, and a {name}/{ext}/{n}/{date} template, with a
        /// live before→after preview (docs/features/BULK_RENAME.md).
        /// With fewer than two resolved targets it degrades to the
        /// single-rename prompt (one) or a no-op (none).
        BulkRenameSelected,
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
        /// File-list context action — open the viewer starting at the
        /// right-clicked file and immediately begin the slideshow
        /// (docs/features/VIEWER.md). Same playlist as `OpenViewer` but
        /// anchored to the context row and auto-playing.
        SlideshowFromHere,
        GoHome,
        /// Cmd+G — open the "Go to Folder" prompt: a modal path box
        /// pre-filled with the current folder and selected, so a paste
        /// replaces it. Committing opens the path in a new tab (a new
        /// window when the app is running with none open). Cmd+L's
        /// breadcrumb edit is the in-place twin — it retargets the
        /// *current* tab instead.
        GoToFolder,
        EditBreadcrumb,
        ShortcutsHelp,
        OpenDiskUsage,
        /// Close the active tab's tool result surface and return to normal
        /// directory browsing. Applies to Search, Duplicate Finder, and
        /// docked Disk Usage.
        CloseToolResult,
        /// Pop the active docked Disk Usage surface into a standalone
        /// window. No-op for other result types.
        PopOutDiskUsage,
        /// Move the docked archive workbench into its own window, so it can sit
        /// beside Finder (or another Ferail window) for drag-and-drop.
        PopOutArchive,
        /// Find duplicate files under the active tab's directory and show
        /// them grouped in the tab (docs/features/DUPLICATES.md).
        FindDuplicates,
        /// Toolbar Sort menu (docs/features — toolbar density). Each
        /// sets the file-table sort column; re-selecting the active
        /// column flips its direction. Dispatched from the sort
        /// dropdown and available to the command palette.
        SortByName,
        SortBySize,
        SortByKind,
        SortByModified,
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
        // Icon-grid 2D navigation. Left/Right step the lead by one;
        // Up/Down step by a full row (columns-per-row, computed at
        // dispatch from the pane width). Bound only in the
        // `FerailGrid` key context so they override the table's
        // 1-D Cursor* bindings when the grid is focused.
        GridLeft,
        GridRight,
        GridUp,
        GridDown,
        GridLeftExtend,
        GridRightExtend,
        GridUpExtend,
        GridDownExtend,
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
        /// Extract the selected archive(s) **here** — into the current folder.
        /// Reads each archive's table of contents off-thread to choose a smart
        /// destination: extract in place when the archive has one root folder,
        /// wrap in a folder named after the archive otherwise.
        Extract,
        /// Extract the selected archive(s) into a folder chosen from a native
        /// picker (same smart in-place/wrap logic, rooted at the choice).
        ExtractTo,
        /// Open the selected archive in the embedded workbench view (browse
        /// contents, cherry-pick extract).
        OpenAsArchive,
        /// Format variants under the "Compress" submenu. The engine's
        /// `create_archive` produces each; the plain `Compress` makes a ZIP.
        /// Open the New Archive dialog over the selection (format, compression
        /// level, and optional password) instead of one-click compressing.
        NewArchive,
        CompressSevenZ,
        CompressTar,
        CompressTarGz,
        CompressTarBz2,
        CompressTarXz,
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
        /// Sidebar volume menu: unmount and eject the right-clicked
        /// volume (`context_target`). Only attached to removable/
        /// ejectable volume rows.
        EjectVolume,
        /// Sidebar/tree "Get Info" — open the Get Info window for the
        /// right-clicked row (`context_target`). Split from the
        /// file-list `GetInfo` because that one resolves through the
        /// row selection, not the right-clicked sidebar path.
        GetInfoAtContext,
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
        /// Up/Down within the focused Favorites section — move keyboard
        /// focus to the previous / next favorite row (§11.4). Sets
        /// `Shell::focused_favorite`, which drives the focus ring and is
        /// the target of `DeleteFavorite` / `ActivateFavorite`.
        FocusFavoriteUp,
        FocusFavoriteDown,
        /// Enter on the focused favorite — navigate to it (§11.4).
        ActivateFavorite,
        /// Delete / Backspace on the focused favorite — remove it (with
        /// undo), the keyboard twin of the context-menu remove (§3.1).
        DeleteFavorite,
        /// Rename the favorite under `favorites_context_path` via the
        /// shared gpui text-prompt modal (§6) — the same surface every
        /// other naming flow uses, so it's consistent and cross-platform
        /// (no native text prompt exists on Windows).
        RenameFavorite,
        /// "Locate…" (§8.2 / §8.3) — open a folder picker and repoint
        /// the favorite under `favorites_context_path` at the chosen
        /// folder, keeping its id / name / sort_index. Reachable from
        /// the normal row context menu and the broken-state dialog.
        LocateFavorite,
        /// Clear the favorite's custom display_name so it tracks the
        /// folder's on-disk basename again (§6 "Reset to Original Name").
        ResetFavoriteName,
        /// Strip a custom icon, falling back to kind+target default (§7).
        ResetFavoriteIcon,
        /// Open the icon-picker window (§7 "Change Icon…") for the
        /// contextual favorite. The picker lists the bundled Lucide
        /// library and writes the chosen glyph back as
        /// `custom_icon = Some(Lucide(name))`. See `favorite_icon_picker`.
        OpenFavoriteIconPicker,
        // Recents sidebar section (docs/features — Recents). The
        // section is a recency view over the Ant Trail visit log.
        /// Header click — flip the Recents section's disclosure
        /// triangle; the collapse state persists in app_state.
        ToggleRecentsSection,
        /// Row context menu — drop `context_target` from Recents (and
        /// forget its visit record, which also clears its heat tint).
        RemoveFromRecents,
        /// Header/row context menu — forget the whole visit log
        /// (clears Recents and the Ant Trail heat).
        ClearRecents,
        // Window docking (docs/features/DOCK.md). Dock the whole window to the
        // left or right screen edge as an auto-hiding drawer that floats over
        // everything and reveals on an edge-slam; `Undock` restores it to a
        // normal window. macOS-only in practice — the other platforms' shell
        // stubs no-op, so these silently do nothing there.
        DockLeft,
        DockRight,
        Undock,
    ]
);
