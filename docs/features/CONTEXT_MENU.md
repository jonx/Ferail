# Context Menu

Ferail used Win32 shell context menus and prewarming to avoid the first
right-click feeling stuck. Feraille needs the same user outcome with Mac-native
building blocks.

## Status

Partial.

Feraille currently has:

- A hardcoded context menu slice.
- Open, Reveal in Finder, Get Info, Copy Path, Move to Trash actions.

## Mac Target

- Native `NSMenu` presentation.
- Finder-equivalent actions where possible.
- Services/share/open-with integration where it can be done without blocking.
- Multi-selection support.
- Background-folder menu support.
- Predictable enable/disable rules for read-only, missing, root, mixed-kind, and
  permission-denied targets.

## Prewarm Rule

Pointer hover or mouse prediction may prepare pure app-side state:

- selected paths/ids,
- target kind,
- likely enabled commands,
- cached labels/icons.

It must not block querying Finder, filesystem metadata, services, or file
contents on the UI path.

## Windows Notes That Do Not Port

- `IContextMenu`, `TrackPopupMenuEx`, PIDLs, shell extensions, and wait-cursor
  suppression are Windows implementation details.
- The lesson that does port: the first context menu must not make the app look
  frozen.

## Todo

- Replace hardcoded menu helper with native NSMenu.
- Add `Open With`.
- Add background menu.
- Add multi-selection.
- Add async-safe service discovery or a conservative static menu.
- Add tests/screenshots for disabled commands.
