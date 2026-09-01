# Lazy Metadata And Node Identity

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

Ferail keeps expensive metadata and filesystem identity below the render
path. Paint reads cached display data; actions and workers resolve paths at
explicit boundaries.

## What is built

The implementation has:

- A process-wide `NodeStore` in `ProcessState`.
- Stable `NodeId` values for filesystem paths and core virtual nodes.
- Lexical path-key normalization for mechanical spelling differences such as
  trailing separators, duplicate separators, and `.` segments.
- Guarded path resolution through `path_for_action` /
  `path_snapshot_for_job`.
- `FileEntry` rows with preformatted display strings and cached metadata fields
  (`display_size`, `display_mtime`, `display_magic`, `display_description`,
  quarantine details).
- A SQLite metadata DB for durable derived data: magic labels/descriptions,
  quarantine facts, hashes, folder usage, favorites, and folder-size cache.
- Process-owned caches for icons, thumbnails, preview data, inline text
  previews, task state, Ant Trail heat, recents, and mounted volumes.
- Background prefetch for magic/quarantine data after enumeration completes.

The remaining work is not "add lazy metadata" anymore; it is finishing stable
identity across rename/move/mount churn and tightening cancellation/invalidation
around every worker.

## Data Model

`NodeStore` is platform-neutral and lives in `ferail-core`.

- `NodeId` is the UI-facing identity token.
- `NodeKind::Path(PathBuf)` represents real filesystem nodes.
- `NodeKind::Virtual(VirtualNode)` represents roots like Computer and Volumes.
- `path_index` maps normalized path keys to ids.
- Each `Node` stores parent, display name, and heat.

The path key is deliberately lexical only. It folds harmless spelling variants,
but it does not case-fold, collapse `..`, resolve symlinks, or query the
filesystem. Those operations are boundary work because their meaning depends on
the volume and live filesystem state.

## Render Contract

Controls receive ids and cached display data. They may:

- Render names, sizes, dates, icons, heat, badges, and cached descriptions.
- Emit intents such as open, reveal, rename, preview, trash, or copy.
- Read in-memory caches owned by `ProcessState`.

Controls must not:

- Resolve a `NodeId` to a path during paint.
- Read files, xattrs, metadata, thumbnails, aliases, symlinks, or directories
  during paint/hover/scroll.
- Start unbounded work from a visible-row loop.
- Re-enumerate a folder just to update one row.

## Scheduling Points

Lazy work is scheduled from semantic events:

- Navigation committed.
- Folder expanded or enumerated.
- Viewport snapshot changed.
- Selection changed.
- Preview opened.
- File operation completed.
- Idle prefetch started.

Workers receive path snapshots or compact seeds. Results are applied on the
foreground executor only after checking generation, row bounds, or current
selection as appropriate.

## Current Worker Flows

- Directory enumeration streams `FileEntry` rows and `NodeId -> PathBuf` map
  updates from a worker thread.
- Magic/quarantine prefetch snapshots visible rows, reads the metadata DB first,
  falls back to bounded filesystem reads, writes through to SQLite, then applies
  one foreground batch.
- Folder-size, search, duplicate finding, file operations, thumbnails, and
  previews all register background tasks and keep I/O off the render path.
- Preview/text providers drop stale results by request id or current selection;
  full cooperative cancellation is still uneven and tracked in TODO.

## Remaining Work

Tracked in [TODO.md](../../TODO.md):

- Finish stable NodeStore identity for rename, move, mount changes, watcher
  events, Ant Trail, selection, and metadata cache keys.
- Add consistent cancellation tokens for previews, thumbnails, disk usage,
  search, copy/move, duplicate finding, and any remaining stale-result-only
  flows.
- Add slow-path tests for stale workers, cloud placeholders, permission errors,
  and partial failures.
- Audit render paths for accidental filesystem calls or path resolution.
