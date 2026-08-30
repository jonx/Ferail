# Built-in Text Editor

A deliberately small "open, fix, save, close" editor for text files, not an
IDE. One standalone window per file over gpui-component's `Editor` widget,
which contributes multi-line editing, undo/redo, find, line numbers, and
tree-sitter syntax highlighting; Ferail contributes the file I/O discipline
around it.

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
edits: the last writer wins, exactly like TextEdit. Text zoom does ship
(toolbar, Cmd+= / Cmd+- / Cmd+0), scaling the document font only.

## Follow-ups if wanted

- A footer strip (size · encoding · line-ending · cursor position).
- Cmd+S on a refused/failed state could offer Save As.
- Watcher-driven "file changed on disk" banner, once the shared watcher
  items under Responsiveness land.
