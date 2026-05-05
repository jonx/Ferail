# Duplicate Finder

Ferail's duplicate finder remains a future Feraille feature. It is valuable,
but it is I/O-heavy and therefore belongs behind the worker/task/progress
architecture.

## Status

Todo.

## Target Pipeline

1. Group by size.
2. For size collisions, compute a partial hash.
3. For partial-hash collisions, compute full hash.
4. Group duplicates by full hash.
5. Present groups in a dedicated view.

## Rules

- Hashing never runs on the UI thread.
- Results stream incrementally.
- Scans are cancellable.
- Status progress is visible.
- The app remains navigable during scans.

## Mac Notes

- Avoid downloading cloud placeholders unless the user explicitly scans them.
- Be careful with packages and bundles.
- File identity and hard links matter; duplicate bytes and duplicate file IDs
  are different concepts.

## Todo

- Worker pipeline.
- Metadata DB schema.
- Duplicate view UI.
- Safe delete/move actions.
- Tests for cancellation and stale results.
