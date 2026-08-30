# Ferail 0.7.6 - Built-in editors and a Disk Usage you can search

This release adds two small editors so routine fixes no longer leave the file
manager, makes Disk Usage searchable in place, and settles how keyboards behave
in every field that offers suggestions.

## Edit without leaving Ferail

- **Text files.** Right-click a file and choose Edit, or press Cmd+E, to open a
  small dedicated window with undo, find, line numbers and syntax highlighting
  by file type. Saving keeps the file's identity (tags, permissions, creation
  date) and its exact line-ending and BOM shape, and always writes the new text
  durably to disk before touching the original. Files too large or not text are
  politely refused with a one-click hand-off to the system editor.
- **Images.** Choose Edit Image to black out sensitive areas (rectangle or
  brush, always opaque) or annotate them in seven colours and three brush
  sizes, with step-by-step undo. Cmd+S saves an "edited" copy beside the
  original, which is never touched by default; Cmd+Shift+S overwrites it after
  an explicit confirmation. Edits always render at the image's full resolution.
  Covers PNG, JPEG, BMP, TIFF and WebP; GIF is excluded so animations cannot be
  silently flattened.
- Both editors zoom (Cmd+= / Cmd+- / Cmd+0), carry an icon toolbar with
  shortcut tooltips, and offer a location button that returns to the exact tab
  the file came from and reselects it.

## Disk Usage answers questions

- The filter field now filters the treemap you already scanned instead of
  replacing it with a generic search. The full map stays visible with
  non-matches dimmed, and an icon toggle redraws the map and the side list from
  matching files only.
- Filtering never restarts or waits for a scan: a bounded queue holds incoming
  facts while a debounced, cancellable background projection reads a stable
  snapshot, then the same scan resumes.
- Nested folder labels no longer show through their children. The layout
  reserves an exact label strip per container, and both the live view and the
  HTML export clip names to it.
- Clicking a deeply nested file in the largest-files list highlights its
  nearest visible ancestor when the exact tile is below the drawing depth.

## Keyboards behave predictably

- Enter in a path or filter field now uses exactly what the field holds.
  Pasting a folder path that contains subfolders used to append the first
  suggestion; completion is now opt-in with Tab, and Up/Down move the
  highlight.
- Escape unwinds transient interface consistently: it closes suggestions before
  clearing a query, cancels inline edits, leaves docked result surfaces, hides
  the preview pane, and closes secondary windows. Unsaved editor work still
  requires an explicit Save, Discard or Cancel.
- `type:` is accepted as a friendly alias for `kind:`, token suggestions remain
  available after a plain-name term, and choosing one appends the criterion
  instead of replacing the search.
- Every secondary window Ferail opens is listed in the Window menu and removed
  from it the moment it closes.

## Presentation and performance

- Horizontal scrolling returned to the bottom of the list view, and keyboard
  column navigation moves headers and rows in the same frame.
- The command palette now uses the maintained upstream Command control, so
  search, grouping, shortcut hints and arrow-key navigation share one
  implementation.
- The filter autocomplete popover can grow independently of the narrow
  title-bar field, and tab close buttons sit to the left of their labels so the
  pointer stays put while several tabs are closed in a row.
- An opt-in performance HUD (`--performance-hud`, or the command palette)
  reports frame timing and process resources without forcing redraws.
- Open With fills its submenu without rebuilding the context menu, and very
  large text previews are bounded on whole visual lines instead of growing
  layout without limit.

## Downloads

- macOS: signed, notarized and stapled DMG.
- Windows x64: per-user setup EXE or portable ZIP, plus matching PDB symbols.
- Linux: Ubuntu 22.04-compatible `.deb` packages for amd64 and arm64.

Windows binaries remain unsigned, so SmartScreen may show its standard
unknown-publisher warning.

The full technical history is in [CHANGELOG.md](CHANGELOG.md), and reporting
instructions are in [docs/REPORTING_BUGS.md](docs/REPORTING_BUGS.md).
