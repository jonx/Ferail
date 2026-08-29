# Ferail 0.7.5 — Direct editing and smoother large folders

This release makes everyday file work more direct and keeps Ferail responsive
when folders, searches and background results become very large.

## Edit and navigate where you are

- Rename files and folders directly in list or icon view with F2 or Rename.
  Ferail selects a file's stem without swallowing its extension, validates the
  new name as you type, accepts with Enter or a click elsewhere, and cancels
  with Escape.
- The path bar, Go to Folder and filter are compact single-line controls with
  useful completion. Windows paths stay readable and never expose the internal
  `\\?\` prefix.
- Search tokens such as `kind:` offer their values in place, while Escape and
  the clear control make it quick to start over.

## Large folders remain interactive

- Ordinary folders, recursive searches, duplicate results and folder-size
  updates now use finite queues and time-sliced interface updates. Ferail keeps
  every result, but background work waits instead of consuming unlimited
  memory or monopolising the window.
- Hidden tabs continue collecting results without causing invisible redraws;
  returning to one refreshes it once and warms only the visible rows.
- Rapid preview selection keeps the active request and the newest request,
  cancels obsolete work cooperatively, and leaves the final selection in
  control.
- Rectangle selection is restored in list view. Drag from empty space across
  rows and use Shift/Cmd/Ctrl to extend the current selection without scanning
  the complete directory.

## Clearer and safer presentation

- Suspicious control characters, unusual whitespace, bidirectional marks and
  homoglyphs remain visibly warned even when a narrow Name column elides the
  dangerous part of a filename.
- Back, Forward and Parent navigation reveal the intended item after streamed
  loading and sorting instead of following a temporary row number.
- Narrow sidebars, Settings rows, search controls and the Disk Usage header now
  adapt more cleanly as the window is resized.
- Ferail's public privacy policy documents local processing, diagnostics,
  update checks and how to remove saved application data.

## Windows reliability

- Fast NTFS verifies that its administrator helper exactly matches this build
  before asking Windows to elevate it. A missing, stale or replaced helper now
  falls back to Portable Disk Usage instead of running an unexpected program.
- The Windows rendering stack and GPUI integration have been updated, including
  the native drag image path and current input/window behavior.
- Headless captures no longer inherit the Fast NTFS preference, so automated
  screenshots cannot display an elevation prompt or wait on a helper.

## Platform polish

- macOS warms native folder artwork only for visible rows, restoring consistent
  folder icons without sweeping a huge directory.
- Icons-only sidebars use less space, stay centred in narrow windows and expose
  every location name in a tooltip.

## Downloads

- macOS: signed, notarized and stapled DMG.
- Windows x64: per-user setup EXE or portable ZIP, plus matching PDB symbols.
- Linux: Ubuntu 22.04-compatible `.deb` packages for amd64 and arm64.

Windows binaries remain unsigned, so SmartScreen may show its standard
unknown-publisher warning.

The full technical history is in [CHANGELOG.md](CHANGELOG.md), and reporting
instructions are in [docs/REPORTING_BUGS.md](docs/REPORTING_BUGS.md).
