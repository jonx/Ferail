# Ferail 0.6.6 — Windows reliability update

This is a **Windows-only release**. It publishes the portable Windows x64 ZIP
and its matching symbols archive; macOS and Linux remain on 0.6.5.

## Highlights

- Fixed the shutdown `InputState` / “leaked handles” crash seen after opening
  Get Info or other secondary windows. Production packages no longer include
  GPUI's developer-only leak assertion, and development shutdown now releases
  retained window callbacks deterministically.
- Isolated third-party preview handlers in disposable helper processes. A
  crashing or hung PDF/Office shell extension can no longer terminate Ferail;
  it is killed, quarantined for the session, and replaced by a safe fallback.
- Added native first-page PDF thumbnails through `Windows.Data.Pdf`, with one
  five-second deadline across open, parse, render, and stream read.
- Fixed blank icons and thumbnails in `C:\Windows\Fonts`, corrected upside-down
  font preview cards, and restored Explorer's shortcut-arrow overlay on `.lnk`
  files.
- Bounded thumbnail and preview scheduling. Rapid scrolling or selection keeps
  one active request and only the newest pending viewport instead of building
  an obsolete work queue.
- Improved crash evidence with useful, path-free breadcrumbs, native minidumps,
  matching line-table PDBs, and correct compact Windows backtraces.
- Made the portable package self-contained with the static MSVC runtime and a
  dependency gate that rejects accidental non-system DLL requirements.
- Added image metadata to Get Info: dimensions, camera, lens, date, exposure,
  orientation, and privacy-preserving GPS presence (never coordinates).
- Added the real Windows Shell context menu behind “More options from
  Windows…”, Shift+right-click, and Shift+F10. Third-party handlers remain in
  a disposable broker; Properties uses Windows' dedicated property-sheet API
  because its context-menu handler returns asynchronously.

## Packaging and diagnostics

The release contains:

- `Ferail-0.6.6-win-x64.zip` — unsigned portable application and CLI;
- `Ferail-0.6.6-x64-symbols.zip` — matching PDBs and identity manifest.

Windows SmartScreen may warn because the build is not Authenticode-signed. The
symbols archive is for diagnosing crash dumps and is not needed to run Ferail.

The full technical list is in [CHANGELOG.md](CHANGELOG.md#066--2026-08-24-windows-only-release).
