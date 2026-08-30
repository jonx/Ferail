# Sidebar Layout and Density

This document records the implementation contract behind the sidebar polish
work. The design keeps one sidebar and one navigation model; density,
disclosure and ordering only alter presentation.

## Density

`Cmd/Ctrl+Shift+B`, the View-menu command and the title-bar sidebar control
cycle through:

1. normal: user-resizable, restoring the exact persisted width;
2. compact: fixed 176 DIP with labels;
3. icons: fixed 48 DIP;
4. normal again.

Compact and icon modes never overwrite the user's normal width. The two mode
bits are persisted independently and reconciled on load so icon mode wins if a
damaged state file claims both.

## Sections

`sidebar_layout.rs` owns stable IDs for Locations, Windows, Linux, Favorites,
Recents, Browse and Volumes. Persisted order is reconciled on every load:
unknown and duplicate IDs are discarded, and sections introduced by a newer
Ferail are appended. Conditional sections keep their position while absent,
which makes the same state portable across machines and WSL availability.

Locations, Windows, Linux, Browse and Volumes use the shared disclosure model.
Favorites and Recents retain their established persistence mechanisms and
header-specific actions. All section headers are drag sources; insertion gaps
are drop targets. View ▸ Reset Sidebar Order restores the canonical order
without expanding or deleting anything.

The model contains no file paths and performs no filesystem work during
rendering or drag/drop.

## Acceptance

- Cycle density repeatedly after resizing normal mode; normal must return to
  the exact prior width after restart.
- Collapse every section, restart, and confirm disclosure state.
- Reorder across conditional sections, disable/re-enable WSL or detach a
  volume, and confirm the hidden section returns at its stored position.
- Drop at the beginning, between every pair and at the end; no section may be
  duplicated or lost.
- Reset order and confirm collapse states remain unchanged.
- Verify icon-only mode has no visible insertion-gap artifacts.
- Repeat on macOS, Windows and Linux with keyboard command/menu parity.

