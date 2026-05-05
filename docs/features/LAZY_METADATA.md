# Lazy Metadata And Node Identity

Ferail's lazy-text docs point to one of the most important long-term
architecture shifts: stable identity below the UI, cached display metadata above
I/O, and paint that only reads.

## Status

Partial.

Feraille currently has:

- `NodeId` in core and tree logic.
- `FileEntry` with cached display strings for list rows.
- Cached magic labels and icons.

It does not yet have a global NodeStore that owns every node, parent chain,
display name, path mapping, and lazy metadata update.

## Target Model

One NodeStore:

- Assigns stable ids.
- Stores parent/child relationships.
- Stores cached display name, kind, icon key, path, flags, and lazy metadata.
- Emits update events when metadata becomes ready.

Controls:

- Receive ids and cached display data.
- Emit intents.
- Never resolve paths during paint.

App/coordinator:

- Resolves ids at action boundaries.
- Schedules lazy work on semantic events.
- Applies worker results.

## Semantic Events That Can Schedule Work

- Navigation committed.
- Folder expanded.
- Row enters viewport.
- Selection changed.
- Preview opened.
- Idle prefetch.

## Not Allowed

- Scheduling lazy metadata from paint.
- Formatting file paths or metadata in visible-row loops.
- Resolving aliases/symlinks during hover.
- Re-enumerating whole folders to update one row.

## Todo

- Introduce NodeStore without breaking current FileEntry slices.
- Move tree/list/breadcrumb/status toward NodeId-native state.
- Add minimal invalidation for single-row metadata updates.
- Add worker/event tests for display metadata.
