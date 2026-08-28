# Ferail 0.7.4 — Private screenshots, flexible navigation and Windows setup

This release turns Private Mode into the intended screenshot-safe projection,
adds a configurable sidebar and discoverable editable path bar, improves
content-based audio recognition, and publishes a real Windows installer beside
the portable package.

## Private screenshots and diagnostics

- Private Mode keeps the prepared Ferail UI visible while replacing names,
  paths, metadata and content with stable session aliases or neutral
  placeholders. The title-bar shield is the sole indicator and toggle;
  `Cmd/Ctrl+Shift+K` toggles it and Escape exits it.
- Screenshot capture enables privacy by default. Ferail-owned interaction is
  frozen while private data is projected, but close and Quit remain usable.
- On Windows, a UI freeze now asks a pristine out-of-process broker to write a
  same-stem minidump with all thread contexts beside the text report.
- The new public bug-report guide explains which crash/hang files to attach,
  which context is useful, and how to capture screenshots without exposing a
  personal library.

## Navigation and sidebar

- Click the empty tail of the breadcrumb or its edit icon to type or paste a
  path; the existing `Cmd/Ctrl+L` shortcut remains available.
- `Cmd/Ctrl+Shift+B` cycles the sidebar through normal, compact and icons-only
  widths while preserving the user's normal width.
- Sidebar sections can be collapsed and reordered persistently, including
  sections which exist only on Windows or Linux, and their default order can
  be restored.
- Windows no longer renders Finder-only tag controls or schedules their dead
  background work.

## Media correctness

- Known audio extensions remain the fast parser hint. Renamed or extensionless
  audio is still recognized from bounded content signatures.
- MPEG/AAC fallback detection requires several coherent frames, so Get Info no
  longer invents MP3 duration and bitrate for an executable containing a
  random sync word.
- Get Info, the rich Description column and embedded cover thumbnails use the
  same policy without adding a whole-list file scan.

## Windows installer and Fast NTFS

- Windows downloads now include both the portable ZIP and an Inno Setup
  installer. Installed copies prefer the setup update path and offer Install
  and Restart; portable copies continue to receive ZIP updates.
- The installer includes the exact sibling `ferail-ntfs-helper.exe`, preserving
  the narrow elevation boundary used by the portable build. Ferail itself is
  not elevated.
- The helper's standalone diagnostic is documented. From an elevated
  PowerShell, `ferail-ntfs-helper.exe --diagnose <path>` emits aggregate MFT
  geometry, phases, rates, counts and timings without names or the requested
  path.

## Downloads

- macOS: signed, notarized and stapled DMG.
- Windows x64: per-user setup EXE or portable ZIP, plus matching PDB symbols.
- Linux: Ubuntu 22.04-compatible `.deb` packages for amd64 and arm64.

Windows binaries remain unsigned, so SmartScreen may show its standard
unknown-publisher warning.

## SHA-256

- `Ferail-0.7.4.dmg` — `694cbd3253e9cf65e0bfab2e784c25c2d477d614560a22d7ab7456f3f8b63e71`
- `Ferail-0.7.4-win-x64-setup.exe` — `8bb2386705638018241ae953f5a4c86a1bb423d7f052a0c0ede164ea83d46c4e`
- `Ferail-0.7.4-win-x64.zip` — `6a359396d1511b9bcde85dd20dbf5aaab1e5f0dc76a73abec6508074b5d0bd81`
- `Ferail-0.7.4-x64-symbols.zip` — `844376944c7a17af2241ad02015b19a566575692c55fa33a31ebd432fbcb15d4`
- `ferail_0.7.4-1_amd64.deb` — `0ae91352e1b67ef9a2611428d025e4fb246c4847ce3cfd7c3f5fb6097dc206cb`
- `ferail_0.7.4-1_arm64.deb` — `4f53d4d311dbc2411a28b1ba2804f2fe9a747a8b0de579fa2405adf7a7302274`

The full technical history is in [CHANGELOG.md](CHANGELOG.md), and reporting
instructions are in [docs/REPORTING_BUGS.md](docs/REPORTING_BUGS.md).
