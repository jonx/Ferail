# Architecture

Feraille keeps the Ferail ambition but changes the platform center of gravity:
macOS is the first-class target, and Windows-specific shell mechanics are
replaced by Mac-native boundaries.

## Dependency Direction

```text
feraille-app
  |-- feraille-controls
  |     |-- feraille-design
  |     `-- feraille-render
  |-- feraille-core
  |-- feraille-fs-native
  |-- feraille-shell-mac
  `-- feraille-shell-win32
```

Rules:

- Controls do not import filesystem or shell crates.
- Renderers do not know about paths, windows, shell APIs, or app state.
- Shell crates do not paint controls.
- App owns orchestration, cancellation, generation ids, and stale-result
  dropping.

## Data Model Today

Feraille currently uses `FileEntry` batches for the active tab and tree nodes
keyed by `NodeId`. `FileEntry` includes preformatted display data:

- `name`
- `kind`
- `size`
- `mtime`
- `display_size`
- `display_mtime`
- `display_kind`
- `display_magic`

This is enough for current slices, but the Ferail docs point to a stronger
destination: stable NodeId-native list/tree models backed by a NodeStore.

## Target Model

One global store:

- Owns NodeId identity.
- Maps NodeId to path, parent, kind, display metadata, and cached shell data.
- Emits updates when lazy data is ready.

Per tab:

- Owns current NodeId/path.
- Owns back/forward history.
- Owns filter, sort, selection, and scroll.

Views:

- Render cached data.
- Emit semantic events only.
- Never resolve paths during paint.

Coordinator/app:

- Accepts intents from controls.
- Schedules workers.
- Applies results if still current.
- Triggers minimal redraw.

## Worker Boundaries

Every worker result needs enough identity to be safely applied:

- Generation/request id.
- Folder path or NodeId.
- Item name or NodeId.
- Mtime/size if stale file results matter.

Current example: magic detection sends `{ generation, dir, results }` back
through winit user events. Results are ignored if the generation changed or the
active tab moved to another folder.

## Mac Replacements For Windows Concepts

| Ferail/Windows | Feraille/macOS |
|---|---|
| HWND/D2D/GDI coordinate conversion | winit window plus renderer-owned scale conversion |
| IContextMenu and shell extensions | NSMenu, Finder actions, Services where possible |
| IDataObject / IDropSource / IDropTarget | NSPasteboard and AppKit dragging |
| IFileOperation | NSFileManager/NSWorkspace plus worker-managed file operations |
| Recycle Bin | NSWorkspace trash API |
| WSL roots | Not applicable; consider network volumes/SSHFS/iCloud later |
| Shell namespace "This PC" | Finder-style Locations, Favorites, `/Volumes`, iCloud |
| Direct2D renderer | Current soft renderer; future GPU/Vello or platform renderer |

## Geometry And Rendering

- Layout uses DIPs.
- Renderer owns scale conversion.
- Controls receive resolved bounds from the app layout, not window-global math.
- Paint reads from state and caches only.

The old Ferail DPI warnings remain conceptually useful, but the Mac renderer
must keep the conversion hidden from controls.

## Status And Progress

Long-running jobs should report into a status/task model:

- Determinate progress for copy, move, hash, scan.
- Indeterminate pulse for enumeration, preview, metadata warming.
- Multiple tasks aggregate into one status-bar presentation.

This comes from Ferail's status-bar progress doc, but the implementation should
be renderer/control-native rather than Win32-control based.
