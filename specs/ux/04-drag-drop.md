# Drag And Drop

Drag/drop is both UI logic and shell integration. It must feel native on macOS
and never block the UI while resolving file operations.

## Current Status

Partial:

- Drag-out from a list row exists through `feraille-shell-mac`.
- Full drop target support is Todo.

## Source Drag

A drag starts after:

- Pointer moves beyond the drag threshold.
- Press has lasted long enough to avoid accidental drags.

If the pressed row is not selected, selection should update before the drag
payload is built.

## Target Drag

Targets:

- Folder row in list.
- Empty list space, meaning current folder.
- Tree folder.
- Tab, after hover activation.

Todo:

- Auto-expand tree nodes on hover.
- Auto-scroll near viewport edges.
- Modifier-driven copy/move/link semantics.
- Native cursor feedback.
- Multi-selection payloads.

## Mac Shell Boundary

Use AppKit dragging and `NSPasteboard`. File operations after drop run through a
worker and report progress through status. Dropping large folders must not pin
the UI.

## Nonblocking Rules

- Drag hover computes target from cached geometry/state.
- Drag hover does not stat files or query Finder.
- Drop schedules work; it does not perform large copy/move work inline.
- Progress and errors are reported without modal freezes.
