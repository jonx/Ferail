# Feraille UX Overview

## What This App Is

Feraille is a native-feeling macOS file explorer with the speed ambition of the
Windows predecessor `../Ferail`.

It should feel closer to Finder, Zed, and a power-user file pane than to a
marketing app. Dense, direct, predictable, and fast.

## Product Promise

The user should be able to open a huge folder, scroll, filter, inspect, preview,
drag, and navigate away without the UI ever feeling stuck.

Performance is not an implementation detail here. It is the product.

## Mental Model

Users bring expectations from:

1. **Finder:** `Cmd+Shift+.`, `Cmd+I`, reveal, Trash, volumes, sidebar locations,
   drag/drop, native menus.
2. **Windows Ferail/Explorer:** speed at scale, dense list, direct keyboard
   manipulation, context menu confidence.
3. **Zed/IDE file panes:** fast fuzzy movement, tabs, compact chrome, keyboard
   immediacy.

When these conflict, Feraille should prefer Mac conventions for platform
actions and Ferail conventions for speed and density.

## Primary Tasks

1. Navigate to a folder.
2. Find an item in the current folder.
3. Inspect metadata and preview content.
4. Manipulate files safely.
5. Open items in other apps.

## Non-Goals For v1

- Workflow automation or scripting.
- Dual-pane commander mode.
- Cloud-sync management.
- Windows shell-extension parity inside the Mac app.
- Anything that requires blocking UI interaction while I/O completes.

## Hard UX Opinion

Every interaction has a fastest possible honest response. Use that as the
design baseline. If an animation, modal, confirmation, metadata query, or
visual flourish makes the median action slower, it is wrong.

## Sub-Specs

- [01-navigation.md](01-navigation.md)
- [02-selection.md](02-selection.md)
- [03-keyboard.md](03-keyboard.md)
- [04-drag-drop.md](04-drag-drop.md)
- [05-performance.md](05-performance.md)
- [06-error-and-empty-states.md](06-error-and-empty-states.md)
