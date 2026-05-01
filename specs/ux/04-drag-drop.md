# Drag and Drop

The hardest UX surface in any file explorer. Get this wrong and users distrust the app for tasks where it matters most.

## Two distinct integrations

1. **Within-app DnD.** Drag from file pane → tree node, file pane → file pane (different tab), tree → tree, etc. Pure UI logic; uses internal data types.
2. **OS shell DnD.** Drag from file pane → another app (Explorer, Outlook, VS Code). Drag *from* another app → Feraille. This is `IDataObject` / `IDropSource` / `IDropTarget` (Windows) or `NSPasteboard` (macOS dev). Lives in [`feraille-shell-win32::drag_drop`](../../crates/feraille-shell-win32) — kept compatible with the existing Ferail implementation we'll port.

Both must feel like one feature to the user.

## Drag start (source)

A drag begins when:
- Pointer was pressed on a row,
- Pointer moved ≥ 4 DIPs while held,
- Time held ≥ 100 ms (avoids accidental drags on quick clicks).

If the pressed row was **not** in the current selection, selection is replaced with `Single(row)` first, *then* the drag starts. Payload is the post-replacement selection. (Matches Explorer; surprising otherwise.)

## Drag visuals (source)

- Cursor switches to system arrow + drag glyph immediately.
- 50 ms after drag-start (avoids flash on fast drags), a **drag preview** appears under the cursor: a stack representation of the dragged items.
  - 1 item: row-height card with icon + name, 80% opacity.
  - 2–9 items: same card with a small badge (e.g. "3").
  - ≥ 10 items: card with badge "10+".
- The preview tracks the cursor with no offset (cursor at top-left of card).
- The original rows in the source list dim to 50% opacity.

The preview is rendered by Feraille on Windows (we don't use the system's `IDragSourceHelper` because we want token-driven styling). On macOS dev, we use `NSDraggingSource`'s default preview.

## Drag-over (target)

While a drag is in progress and the cursor is over a potential target:

- **List row that's a folder:** highlight the row with `accent.subtle`, draw a 1-DIP `border.focus` outline at row bounds. Effect: drop *into* that folder.
- **List row that's a file:** no highlight (not a target). Pointer remains over the row but it's not a drop zone.
- **Empty space in file pane:** highlight the entire pane with a 2-DIP `border.focus` inset. Effect: drop into the **current folder**.
- **Tree node:** same as list-row-folder.
- **Tab (in TabStrip):** activate that tab after a 600 ms hover, then handle as drop into its current folder.

### Auto-expand and auto-scroll

- Hovering over a collapsed tree node ≥ 600 ms during drag → expand it.
- Pointer within 24 DIPs of the top/bottom edge of a scrollable surface during drag → auto-scroll at a rate proportional to proximity (saturating at 16 DIPs from the edge).

## Drop indicator

When a drop would *insert* into an ordered position (rare in a file explorer but used in tab reordering), show a 2-DIP `accent.fill` line at the insertion gap. For folder drops, prefer the row highlight (described above).

## Effect resolution

The resolved effect (Move / Copy / Link / None) depends on:

1. The source's allowed effects (`DROPEFFECT_*` mask on Windows).
2. The target's accepted effects.
3. The held modifier keys (see [03-keyboard.md](03-keyboard.md)).
4. **Same-volume vs different-volume** for the default (no-modifier) case: same → Move, different → Copy. Match Explorer.

The cursor reflects the *resolved* effect at all times. If during a drag the user adds Ctrl, the cursor updates within one frame.

## Drop

On pointer-up over a valid target:

1. Compute final effect from current modifier keys.
2. Hand off to [`feraille-shell-win32::IFileOperation`](../../crates/feraille-shell-win32) on Windows, or to `feraille-fs-native` for the macOS dev mode.
3. Show progress in the StatusBar (operation progress slot, see controls spec).
4. On completion: refresh affected folders (the source folder and the target folder) via shell change notifications (Windows) or filesystem watcher (dev mode).

If the drop target is invalid (e.g. dropping a folder onto its own descendant), the cursor shows the system's "no-drop" glyph. Drop is a no-op.

## Cancel

- Esc during drag: cancel.
- Right-click during drag: cancel (Windows convention).
- Pointer leaves *all* drop-eligible regions and is released over none: cancel.

## Edge cases worth specifying

| Case | Behavior |
|---|---|
| Drop a folder onto itself | no-op (cursor: no-drop) |
| Drop a folder onto its descendant | no-op (cursor: no-drop) |
| Drop a file onto itself (same name, same folder) | no-op |
| Drop with a name collision | shell's standard "Replace / Skip / Compare" dialog (we don't reimplement) |
| Drag out to Recycle Bin | resolved as Delete; confirmation per the user's Recycle Bin settings |
| Drag a file from a network share to local | Copy with progress; show network throughput in StatusBar |
| Drag during a search-results view | drop target is the *real* parent of the dragged row, not the result list |

## Within-app fast paths

When the drag *source* and *target* are both Feraille panes in the same window:

- We can skip the round-trip through `IDataObject` HGLOBAL packing and just pass `Vec<NodeId>` directly.
- This is an optimization, not a correctness requirement. The shell-DnD path must still work end-to-end (so external apps see us as a normal drag source).

## Performance

- Drag-preview rendering: ≤ 1 ms per frame, 60 fps minimum during drag (because the system polls fast).
- Effect resolution on modifier change: ≤ 8 ms (one frame).
- Auto-scroll: capped at 32 rows/sec to avoid runaway during long drags.
