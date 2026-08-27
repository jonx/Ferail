# Ferail 0.7.2 — Millions of files, on every platform

This is the first Ferail release for macOS, Windows and Linux together since
0.6.5. It brings the large-volume work and the shared features accumulated in
the Windows-first releases to every supported download.

## Faster large trees

- Flat View remains uncapped and responsive across millions of files, with its
  compact scan-local path arena, symbolic Select All and viewport-only detail
  work.
- macOS enumerates APFS directory metadata in native `getattrlistbulk` batches
  with bounded parallel readers. Version 0.7.2 reuses each worker's native
  buffer, extends the bulk path to package and folder-size rollups, and removes
  avoidable per-file iCloud/extension allocations.
- Disk Usage releases its scan index when the surface closes, reports skipped
  folders honestly, and offers Full Disk Access guidance only when protected
  macOS folders actually limit coverage.

## New shared tools since 0.6.5

- Find Similar Images reuses the duplicate-finder surface with adjustable
  structure/detail criteria, best-copy selection, thumbnails and full-size
  keyboard navigation. Perceptual hashes and thumbnails remain memory-only.
- Generate SHA-256 for a file and compare it with a whitespace-trimmed digest
  from the clipboard without persisting either value.
- Preview CP437/ANSI and Kodi NFO files, verify SFV and common checksum lists,
  generate SFV/SHA256SUMS atomically, and jump directly to a problematic file.
- The image/video viewer gains the accumulated navigation, opacity and
  always-on-top controls; tool results can open their location in a new tab.

## Windows Fast NTFS preview

Fast NTFS keeps the unelevated Ferail GUI separate from a temporary read-only
administrator helper. One explicit UAC approval serves subsequent scans in the
same Ferail session; live MFT/index/traversal phases are visible, and failures
discard partial rows before Portable fallback. A real 2.3-million-record volume
reached subtree delivery in about five seconds after elevation.

Fast NTFS remains a preview while the adversarial VHDX matrix and Authenticode
qualification stay open. Normal launch, browsing, Flat View and Portable Disk
Usage do not elevate.

## Downloads

- macOS: signed, notarized and stapled DMG.
- Windows x64: portable ZIP plus a separate matching symbols archive.
- Linux: Ubuntu 22.04-compatible `.deb` packages for amd64 and arm64.

## SHA-256

- `Ferail-0.7.2.dmg` — `1ece0107e0a6d388610b8d46583641a7eae3b097eae4b233866ad2e15d1b25eb`
- `Ferail-0.7.2-win-x64.zip` — `8dbb5f972d9e1e7ed71bce8a36c35864fc4b80411dcc11251e2b151403d3eac5`
- `Ferail-0.7.2-x64-symbols.zip` — `b5fa7f16b9cfbe6b36b99e2afb4142346185e753f9ef22c70ee511669afc4adb`
- `ferail_0.7.2-1_amd64.deb` — `e510371166de0516d69b77452626be4372ae3bd796b333847438c83d183e8c46`
- `ferail_0.7.2-1_arm64.deb` — `be91d9275120c05148f30986d51d5fb9be0a25c9bf64e77701c1dd3819f3ff5b`

The full technical history is in [CHANGELOG.md](CHANGELOG.md).
