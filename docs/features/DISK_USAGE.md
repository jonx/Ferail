# Disk Usage And Treemap

Ferail's disk usage feature becomes a future Feraille mode: an async scanner
feeding a dense visual treemap.

## Status

Todo.

## Target

- Scan a folder tree without blocking navigation.
- Stream partial size totals.
- Let the user keep using the app while scanning.
- Show progress in the status bar.
- Render a treemap for large folders and volumes.
- Let users zoom into subfolders smoothly.

## Mac Notes

- APFS snapshots, hard links, packages, aliases, sparse files, cloud files, and
  external volumes can make "size" tricky.
- Respect package boundaries: apps and bundles may be shown as packages by
  default, with an option to descend.
- Do not force cloud files to download just to count them.
- Use workers and cancellation for every scan.

## Data Pipeline

1. Enqueue root.
2. Enumerate children in batches.
3. Accumulate apparent size and allocated size when available.
4. Emit partial totals.
5. Cache results by folder identity and mtime/change token where possible.
6. Cancel or deprioritize when the user navigates away.

## Todo

- Scanner worker.
- Partial result model.
- Treemap layout.
- Status progress integration.
- CLI screenshot state.
- Tests with symlinks, packages, unreadable folders, and external volumes.
