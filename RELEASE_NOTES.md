# Ferail 0.6.9 — Windows Shell reliability update

This is a **Windows-only release**. It publishes the portable Windows x64 ZIP
and its matching symbols archive; macOS and Linux remain on 0.6.5.

## Highlights

- **This PC, Recycle Bin, and connected provider/MTP containers can now be
  browsed safely.** Pathless Windows Shell items remain in a dedicated,
  virtualized browse-only surface and expose the official Windows menu through
  **More…** or Shift+right-click. Normal drive and folder paths return to
  Ferail's full filesystem engine immediately.
- **Explorer transfer semantics are preserved.** Clipboard cut/copy and native
  outbound drag-and-drop now negotiate Copy, Move, and Create Shortcut using
  the same Shell formats and modifier behavior as Explorer.
- **Windows shortcuts and launch failures behave predictably.** Directory
  `.lnk` files navigate inside Ferail; file/application shortcuts retain their
  arguments and working directory. Failed Open, Reveal, and Shell verbs now
  produce actionable errors and refresh the affected folder when appropriate.
- **Get Info is more useful.** It includes an approved, privacy-preserving
  allow-list of Windows Shell metadata and shortcut details. Creation,
  modification, and access dates can be edited in place; directory writes are
  verified after Windows closes the handle so ignored changes are not reported
  as successful.
- **Linux/WSL locations are opt-in and disabled by default.** Ferail does not
  discover or start WSL distributions until enabled in Settings › Files ›
  Locations. Activation is explicit, cancellable, and no longer causes a
  nested GPUI update crash.
- **Shell providers are contained and background work is bounded.** Namespace
  enumeration, property handlers, thumbnails, and icons use process-wide
  budgets or disposable time-bounded workers, preventing one slow or faulty
  provider from blocking Ferail's UI or taking down the process.

## Reliability and diagnostics

- Development builds now retire the complete GPUI window-owned graph in
  dependency order. Closing after preview/filter use or with a popup menu open
  no longer triggers the upstream leaked-handle assertion.
- Virtual Shell locations never fall through to filesystem-only commands aimed
  at the folder previously shown in the tab.
- GPS coordinates and arbitrary Windows property blobs are never retained,
  logged, or persisted.

## Packaging

The release contains:

- `Ferail-0.6.9-win-x64.zip` — unsigned portable application and CLI;
- `Ferail-0.6.9-x64-symbols.zip` — matching PDBs and identity manifest.

Windows SmartScreen may warn because the build is not Authenticode-signed. The
symbols archive is for diagnosing crash dumps and is not needed to run Ferail.

The full technical list is in
[CHANGELOG.md](CHANGELOG.md#069--2026-08-26-windows-only-release).
