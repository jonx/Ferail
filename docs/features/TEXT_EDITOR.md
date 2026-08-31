# Built-in Text Editor

A deliberately small "open, fix, save, close" editor for text files, not an
IDE. One standalone window per file over gpui-component's `Editor` widget,
which contributes multi-line editing, undo/redo, find and replace, line
numbers, and tree-sitter syntax highlighting; Ferail contributes the file I/O
discipline around it, and the toolbar that makes the widget's own commands
findable.

← Back to [feature notes](README.md) · Source:
`crates/ferail-gpui/src/text_editor.rs`

## What ships

- **Entry points.** Row context menu **Edit** (single file, never a folder),
  the `file.edit` command (Cmd+E, command palette, and the File menu's
  "Edit File" [mac]). The pre-existing system-editor escape hatch ("Edit in
  TextEdit / Notepad / Text Editor") stays directly beneath the new entry.
- **The window.** Spiral-cascaded standalone window (same
  `window_cascade` scheme as Get Info), titled `Edit: <name>`, with a
  `•` prefix while the buffer is dirty. Cmd+S saves; Esc and Cmd+W close
  through the unsaved-changes guard; the OS close button goes through
  `on_window_should_close` and honours the same guard. The prompt offers
  **Save / Don't Save / Cancel**. The location button returns to the exact
  Ferail tab that opened the editor and reselects the source file; if that tab
  is gone, it falls back to an ordinary reveal in Ferail. Command-icon
  tooltips include their Cmd/Ctrl shortcuts, and every editor window appears
  in the app's Window menu.
- **Find and replace** are the widget's own, on its own shortcuts: Cmd+F /
  Ctrl+F finds, Cmd+Shift+F / Ctrl+H opens the replace field. The panel
  brings previous/next, a `3/17` match counter, a case toggle, *Replace* and
  *Replace All*; the toolbar has a button for each. **Esc closes the panel
  before it closes the window**: the interceptor in `open()` checks
  `search_session().open` first, so it behaves the same whether focus sits in
  the query field or the text.
- **Reload from Disk** re-reads the file, keeping the caret where it was
  (clamped, in case the file shrank). With unsaved edits it asks first, since
  the swap is not undoable.
- **Wrap Lines** and **Line Numbers** are checkable entries under the
  toolbar's `⋯` menu. Per window, not persisted; a new window gets the
  widget's defaults, both on.
- **A footer strip** reports `Line 42, Column 7 · 1.104 lines · UTF-8 (BOM) ·
  CRLF`. The caret moves without an edit and `InputEvent` only fires on
  change, so the view observes the editor entity to repaint. Line and column
  are coordinates, not counts, so they are not digit-grouped; the line total
  is. That total is the rope's line count, so a file ending in a newline
  reports one line more than it has text lines, matching the gutter. No file
  size: it would be stale after the first keystroke, and re-reading it on the
  UI thread is not allowed.
- **Language pick is extension-only** (no content sniffing on the UI
  thread); unknown extensions fall back to plain text inside the widget.
  `syntax_extra`'s vendored highlight queries apply here exactly as in the
  preview pane.
- **Refusal states, never surprises.** The editor refuses (with a message
  and a one-click system-editor fallback): files over 2 MiB, files with
  100K+ lines, files that are not valid UTF-8, and anything containing NUL
  bytes. A refused file is never touched.

## I/O discipline

- **Read**: whole file on the background executor, size-guarded by
  `fs::metadata` first. UTF-8 BOM and CRLF line endings are detected,
  normalized away for the widget, and **restored verbatim on save** (a
  mixed-endings file is normalized to its dominant CRLF shape).
- **Write**: the serialized text goes to a unique hidden sibling first
  (`.name.ferail-edit-<pid>-<seq>.tmp`), so the bytes are durably on disk
  before the original is touched; then the original is rewritten **in
  place**, same inode, so Finder tags, permissions, ACLs, and the creation
  date survive, which a rename-over would silently drop, and the sibling
  is removed. If the in-place write fails midway, the sibling stays behind
  and the error toast names it as the recovery copy.
- **After a save** the containing directory's listing refreshes through
  `Shell::reload_tabs_matching_paths` (size/mtime rows go stale otherwise).
- Edits made while a save is in flight keep the buffer dirty (the saved
  snapshot is compared against the live text on completion).

## Prime directive notes

`read_for_edit` / `write_for_edit` both carry
`path_guard::assert_off_ui_thread`. The constructor and render path touch
no filesystem state; the load lands through the window-handle re-entry
pattern (`confirm_fanout`'s shape). In **Private Mode** the stage blanks
fail-closed (the file's text is user content) and the window caption stays
"Private" as its title; the headless screenshot harness demonstrated both
states (`--text-editor <path>`, with and without `--unsafe-real-data`).

## Deliberate non-features (v1)

No tabs, no LSP, no Save As, no encoding conversion (non-UTF-8 files are
refused rather than transcoded), no file watching for concurrent external
edits: the last writer wins, exactly like TextEdit, and Reload from Disk is
the manual way back. No "go to line", the one basic command the widget lacks.
Text zoom does ship (toolbar, Cmd+= / Cmd+- / Cmd+0), scaling the document
font only.

## Follow-ups if wanted

- Go to line. In French the label must read *Atteindre la ligne…*: *aller à
  la ligne* means inserting a line break.
- Cmd+S on a refused/failed state could offer Save As.
- Watcher-driven "file changed on disk" banner, once the shared watcher
  items under Responsiveness land.
- Remembering Wrap Lines / Line Numbers across windows.
