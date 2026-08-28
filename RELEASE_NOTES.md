# Ferail 0.7.3 — Private screenshots and responsive disk maps

This release protects screenshots by default and keeps Disk Usage responsive
while portable scans stream millions of files. The performance fixes are in
shared code, so macOS, Windows and Linux receive the update together.

## Private Mode

- The new shield button or `Cmd/Ctrl+Shift+K` replaces every Ferail-owned
  window with an opaque, synthetic presentation containing no real names,
  paths, thumbnails, metadata or content.
- While Private Mode is active, normal commands and interaction are disabled.
  Escape, the shield, or the shortcut restores the session; window close and
  app quit remain available.
- Viewer transparency is forced off, video is torn down, and the native window
  title is replaced. The mode is session-only and never persisted.
- Ferail's screenshot harness now enters Private Mode automatically. Its
  explicit `--unsafe-real-data` override is reserved for repo-owned fixtures.

## Disk Usage at million-file scale

- Depth-limited treemaps use incrementally maintained subtree totals rather
  than repeatedly walking every hidden descendant on the UI thread.
- Broad directories use a linear iterative squarifier instead of recursively
  copying the remaining sibling list.
- The largest-files panel keeps only its 50 candidates instead of allocating
  one temporary row per file, and presentation refreshes back off adaptively
  on very large trees.
- Scan completion no longer rewrites every node. Privacy-safe timing
  breadcrumbs identify any remaining slow layout or Top-N pass in a hang
  report without recording the scanned path.

## Smaller polish

- Large counts now use localized grouping consistently across tool headers,
  progress, notifications, checksum results and viewer positions.
- Windows clipboard paths no longer expose the internal `\\?\` prefix.
- Ferail's version is visible beside the toolbar wordmark, making screenshots
  and tester reports easier to identify.
- The shipped Fast NTFS helper documents its elevated standalone diagnostic:
  `ferail-ntfs-helper.exe --diagnose <path>`. Its report contains aggregate
  phases, rates, counts and timings, never file names or the requested path.

## Downloads

- macOS: signed, notarized and stapled DMG.
- Windows x64: portable ZIP plus a separate matching symbols archive.
- Linux: Ubuntu 22.04-compatible `.deb` packages for amd64 and arm64.

The full technical history is in [CHANGELOG.md](CHANGELOG.md).
