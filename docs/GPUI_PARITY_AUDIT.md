# GPUI Migration — Parity Audit

Honest comparison between the old soft-rendered `Feraille` binary and
the new `feraille-gpui` binary, as of the close of Phase 5.

Decision: **do not cut over yet.** Phase 6's "delete the old crates"
commit is premature — the new shell is feature-functional but missing
substantial old-app features. The cutover plan needs to grow a
"restore parity" phase between 5 and 6.

## Done on the new stack (Phase 1–5)

- Native macOS window chrome (real traffic lights)
- Light/Dark theme + live swap
- Sidebar nav with Locations (Home, Apps, Desktop, Documents,
  Downloads, Movies, Music, Pictures) + Volumes from `/Volumes`
- Toolbar with Back / Forward / Show Hidden
- Clickable breadcrumb path
- Virtualized file list (gpui-component `Table`), Name / Size /
  Kind / Modified
- Selection (arrow-key + click), double-click open, Backspace = parent
- Preview pane with metadata of selected row
- File-system watcher (live updates on external changes)
- Settings window (Cmd+,) — Appearance / Files / Layout / About,
  with PreviewTile theme picker, hidden-files toggle with live count,
  Narrow/Medium/Wide sidebar stops
- Context menu on file rows — Open, Reveal in Finder, Copy Path,
  Move to Trash
- OS drag-out from file rows to Finder / other apps
- Persistent state — last directory + show-hidden across launches
- Native macOS menu bar — Feraille / File / Edit / Go / View
- Headless `--screenshot` CLI

## Gap — features the OLD app has, the new shell does NOT

Listed honestly, sorted by user-visible importance.

### Critical (blocks cutover)

- **Tabstrip / multiple in-window tabs.** Old app: Cmd+T new tab,
  Cmd+W close tab, click to switch, per-tab history + filter + scroll.
  New: single-pane only.
- **Search / live filter (Cmd+F).** Old app: filter current folder
  by name / kind / magic. New: no search.
- **Inline rename + new-folder dialog.** Old app: F2 rename in-row,
  Cmd+Shift+N opens new-folder dialog. New: missing.
- **Real macOS file icons.** Old app: `NSWorkspace iconForFile:`
  cached by kind/extension. New: emoji glyphs (📁 📄).

### High value

- **Disk Usage window (Cmd+Shift+D).** Treemap + Top-N + cancel-able
  scanner. Significant feature, separate window.
- **Get Info / properties pane (Cmd+I).** Detail metadata view.
- **Magic byte sniffing.** Pre-computed display_kind strings (PNG,
  Mach-O, etc.). Today's old-app pipeline.
- **Quarantine badges.** Mark-of-the-Web overlay dot from
  `com.apple.quarantine` xattr.
- **Quick Look thumbnails + Space-bar preview.**
- **Ant Trail heat coloring.** Folder usage frequency persistence
  via `feraille-meta`.

### Medium value

- **Status bar.** Item count, shared progress strip, task popover.
- **Breadcrumb edit mode (Cmd+L).** Type a path directly.
- **UI zoom (Cmd+/Cmd-).** User scale on tokens.
- **System theme follow ("Auto").** Old app supports it; new only
  Light/Dark.
- **Home navigation shortcut (Cmd+Shift+H).**
- **Right-click on sidebar entries** (Pin/Unpin/Open in new tab).
- **Drag-into-app** (drop a file from Finder onto a folder row).
- **Persistent metadata DB integration** (Ant Trail, magic cache,
  quarantine cache, layout state via `feraille-meta`).

### Deferred by design (won't block cutover)

- **Windows shell-context-menu** — N/A on macOS.
- **WSL integration** — N/A on macOS.

## What this means for Phase 6

The 8-week plan budgeted Phase 6 at 3–5 days. That was conditional on
Phase 4 producing actual feature parity. It did not — Phase 4 produced
the file-manager *skeleton* and Phase 5 added the *integration glue*,
but the old app's full *feature set* hasn't been ported.

Honest path forward, in order of risk:

1. **Stay parallel-build.** Keep both binaries shipping. Cutover only
   when the new app has at least the "critical" items above.
2. **Plan a Phase 5.5 / Phase 6-prep iteration** focused on parity.
   Roughly 1–2 weeks of part-time work:
   - Tabstrip
   - Search field
   - Inline rename + new-folder dialog
   - Real macOS icons (NSWorkspace bridge via feraille-shell-mac)
   - Cmd+I properties / get-info
3. **High-value items** become Phase 6.5: Disk Usage, Magic sniffing,
   Quarantine, Quick Look thumbs. These are bigger and can flow in
   one-at-a-time post-cutover IF the cutover is gated on critical only.
4. **Medium-value** rolls in as polish iters in the months after.

## Recommended decision

Do not delete `feraille-app` / `feraille-render` / `feraille-controls`
in the next commit. They are the safety net while parity work
continues. The new app is a strong demonstration of the new stack
working end-to-end — but it is not yet a drop-in replacement.

The migration is on track. The plan's timeline just under-budgeted
parity work, which is normal — better to discover this at week three
than at the cutover-PR review.
