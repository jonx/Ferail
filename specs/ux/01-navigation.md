# Navigation

## The four ways in

A user reaches a folder by exactly one of these paths. All four must feel instantaneous.

### 1. Tree (left pane)
- Single click on a node: navigate the active tab to that folder.
- Single click on a chevron: toggle expand without navigating.
- Double-click on a node: navigate **and** expand.
- Right-click: context menu (shell on Windows).
- Drag a folder from the file pane onto a tree node: move (or copy with Ctrl).

### 2. Breadcrumb (address bar)
- Click a segment: navigate to that ancestor.
- Click the chevron between segments: dropdown listing siblings of the next segment.
- Click empty rail or press `Ctrl+L` / `F4`: enter edit mode (path text input).
- Edit mode: Enter navigates, Esc cancels and returns to breadcrumb.

### 3. File pane (double-click into a folder)
- Double-click row: open. If folder → navigate; if file → OS handles ("Open with default").
- Enter on a selected folder row: same as double-click.
- Backspace: navigate to parent.

### 4. Tabs
- Each tab has its own independent navigation state (current folder, history, scroll, selection).
- `Ctrl+T` new tab (opens at the active tab's current folder).
- `Ctrl+W` close active tab. If last tab, close the window.
- `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle.
- `Ctrl+1..9` jump to tab N.
- Drag a tab horizontally → reorder. Drag downward off the strip → detach (v2).

## History (per-tab)

Each tab maintains a **stack** of recently visited folders.

- `Alt+Left` / mouse button 4: back.
- `Alt+Right` / mouse button 5: forward.
- `Alt+Up` / `Backspace`: parent.

Going *back* and then navigating somewhere new **truncates** the forward stack. Standard browser semantics — nothing exotic.

## Address-bar edit mode rules

- Tab completion: completes the current segment to the longest common prefix; subsequent Tabs cycle through matches.
- Pasting a path with quotes: strip them.
- Pasting a path with environment variables: expand `%USERPROFILE%`, `%APPDATA%`, etc., before navigating.
- Pasting a network path `\\server\share`: navigate, with a 4-second visible "Connecting…" status if not immediate.
- Pasting an HTTP URL: do **not** navigate; copy to clipboard with a toast "Looks like a URL — copied to clipboard."

## Special locations (Windows)

These appear under "This PC" / "Quick Access":

- Drives (with usage indicator on hover).
- User folders: Desktop, Documents, Downloads, Pictures, Music, Videos.
- Quick Access pinned items (from `%APPDATA%\Microsoft\Windows\Recent\AutomaticDestinations`).
- Network locations.
- WSL distributions (under "Linux").

These are exposed by [feraille-shell-win32](../../crates/feraille-shell-win32) via the shell namespace; the UI doesn't synthesize them.

On macOS dev mode, the tree shows: Home, Desktop, Documents, Downloads, plus `/Volumes/`. Just enough to test the UI.

## What we deliberately *don't* do

- **No animated transition between folders.** A new folder paints instantly. Animation here is exactly the kind of "delight" that makes the tool feel slow.
- **No automatic sort persistence per folder.** Sort is a tab attribute, not a folder attribute. (Explorer persists per-folder; users find it confusing more often than helpful.)
- **No "loading…" placeholder for the file list.** If enumeration is < 50 ms (typical for local SSD), we just paint the result. If > 50 ms, we paint partial results as they stream in (see performance spec).

## Failure modes

- **Folder doesn't exist** (deleted, drive ejected): show ErrorState with "This folder no longer exists. Go to parent" (action), keep the address bar editable. Don't auto-navigate.
- **Permission denied:** show ErrorState with "You don't have access to this folder. Take ownership" (action invokes the shell's standard ownership flow). Keep history intact.
- **Network share unreachable:** ErrorState with retry button. Don't block the UI thread waiting for SMB timeout — the enumeration runs on a worker, and the UI shows a LoadingSpinner until 4 seconds, then switches to ErrorState.
