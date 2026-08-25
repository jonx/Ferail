# Ferail 0.6.8 — Windows drag responsiveness update

This is a **Windows-only release**. It publishes the portable Windows x64 ZIP
and its matching symbols archive; macOS and Linux remain on 0.6.5.

## Highlights

- Dragging files now stays responsive in large folders. Ferail reuses one
  selection snapshot, paces edge autoscroll by elapsed time, avoids redundant
  viewport warming and temporary cache-key allocations, and no longer drives
  full application renders at the mouse report rate during Windows OLE
  `DragOver` callbacks.
- A drag that leaves Ferail now has exactly one icon stack. From the first
  handoff onward, the native Windows Shell image remains visible even if the
  pointer comes back over Ferail; the internal typed payload is restored
  invisibly so Ferail folders still accept the drop.
- Rubber-band selection in grid view visits only intersected cells and applies
  incremental selection changes, then synchronizes the list once when the
  gesture ends.
- Path icon and thumbnail cache lookups no longer allocate formatted keys on
  hot render paths, and favorites/sidebar lookups avoid repeated model clones
  or filesystem-lock acquisition when cached identity is available.

## Packaging and diagnostics

The release contains:

- `Ferail-0.6.8-win-x64.zip` — unsigned portable application and CLI;
- `Ferail-0.6.8-x64-symbols.zip` — matching PDBs and identity manifest.

Windows SmartScreen may warn because the build is not Authenticode-signed. The
symbols archive is for diagnosing crash dumps and is not needed to run Ferail.

The full technical list is in [CHANGELOG.md](CHANGELOG.md#068--2026-08-25-windows-only-release).
