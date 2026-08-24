# Ferail 0.6.7 — Windows integration update

This is a **Windows-only release**. It publishes the portable Windows x64 ZIP
and its matching symbols archive; macOS and Linux remain on 0.6.5.

## Highlights

- Added the real Windows Shell context menu behind “More options from
  Windows…”, Shift+right-click, and Shift+F10. Third-party handlers run in a
  disposable broker process: a crashing or stalling extension cannot take
  Ferail down, and once the menu is visible it stays open with no timeout.
- Added “What's Locking This?” on files and folders and “What's Blocking
  Eject?” on removable volumes: a dialog names every process holding the item
  open (via the Windows Restart Manager) and can close them — politely first,
  forced only if they refuse. A failed USB eject now names its blockers too.
- Files can now be dragged out of Ferail into Explorer and other Windows
  applications, as a native Shell file drag with the usual copy/move/link
  modifier behavior.
- Shift-clicking “Copy File List” now includes subfolder contents recursively;
  a plain click still copies just the visible rows.
- The Delete Immediately confirmation opens with its Delete button focused, so
  Enter confirms right away.
- Fixed Software Update downloading the debug-symbols archive instead of the
  app: the updater now skips symbols bundles, and the symbols archive was
  renamed (`…-x64-symbols.zip`) so already-shipped builds pick the right
  download as well.

## Packaging and diagnostics

The release contains:

- `Ferail-0.6.7-win-x64.zip` — unsigned portable application and CLI;
- `Ferail-0.6.7-x64-symbols.zip` — matching PDBs and identity manifest.

Windows SmartScreen may warn because the build is not Authenticode-signed. The
symbols archive is for diagnosing crash dumps and is not needed to run Ferail.

The full technical list is in [CHANGELOG.md](CHANGELOG.md#067--2026-08-25-windows-only-release).
