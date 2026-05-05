# Navigation

Navigation must feel instantaneous even when enumeration is still running.

## Ways In

### Tree

- Click row: navigate and expand if needed.
- Click chevron: expand/collapse without changing folder when possible.
- Tree scroll is independent from list scroll.
- First expand may schedule enumeration; cached expand is instant.

### Breadcrumb

- Click segment: navigate to ancestor.
- `Cmd+L` / `Ctrl+L`: edit full path.
- Enter submits.
- Esc cancels edit mode.
- Future: segment dropdowns and tab completion.

### File List

- Enter on folder: navigate.
- Enter on file: open with default app.
- Backspace: parent folder.
- Double-click should match Enter behavior when implemented.

### Tabs

- Each tab owns current folder, history, scroll, selection, filter.
- New tab opens at active folder.
- Future: tab persistence, reordering, detach, recent-folder new tab.

## History

Per-tab history follows browser semantics:

- Back/forward move through committed folder visits.
- New navigation after Back truncates the forward stack.
- Parent navigation commits normally.

## Special Locations

Current:

- Home.
- Visible home subfolders.
- `/Volumes` mounts.

Target:

- Finder Favorites.
- iCloud Drive.
- Desktop/Documents/Downloads/Pictures/Music/Movies.
- External volumes.
- Network locations.
- Recent folders.

## Nonblocking Requirements

- Navigation commit updates chrome immediately.
- Enumeration, magic, icons, preview, and folder-size work happen after commit.
- Results are ignored if they return for an old folder/generation.
- Slow network/cloud folders show partial/loading/error state, not a frozen app.

## Deliberate Non-Features

- No transition animation between folders.
- No blocking "loading" modal.
- No full folder scan before first paint.
- No automatic per-folder sort persistence until there is a clear user value.
