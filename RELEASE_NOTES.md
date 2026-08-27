# Ferail 0.7.1 — Fast NTFS Reliability for Windows

This is a **Windows-only release**. It publishes the portable Windows x64 ZIP
and its matching symbols archive; macOS and Linux remain on 0.6.5.

## Fast NTFS fixes

- Fast NTFS now shows live MFT-reading, index-building and subtree-traversal
  progress instead of appearing frozen. The completed result also reports the
  actual scan duration, measured after UAC so credential-entry time is not
  included.
- Large-volume scans read bounded 8 MiB MFT windows, parse records in parallel
  without per-record buffer copies or unnecessary run-list allocations, and
  build parent adjacency in linear time. On the development machine, a volume
  with about 2.3 million MFT records reached subtree delivery in roughly five
  seconds after elevation instead of several minutes.
- OneDrive Files On-Demand folders containing real MFT children are traversed
  correctly. Ferail never resolves an external reparse target, so ordinary
  leaf junctions remain opaque and cannot escape the selected subtree.
- The elevated helper is reused for subsequent Fast scans in the same Ferail
  process. The first use may show UAC; later scans do not prompt again. The
  helper remains isolated, read-only and authenticated, releases every scan's
  raw-volume state, and exits when Ferail disconnects.
- The helper has a privacy-preserving standalone diagnostic mode for Windows
  qualification. It reports aggregate parser, index and phase timings without
  printing names or the requested path.

A real elevated scan of a OneDrive-backed Pictures folder returned 2,842
files, three directories including the root and 617.1 MiB, matching ordinary
filesystem enumeration. Fast NTFS still needs broader real-world testing, and
Portable remains available at all times.

Fast NTFS currently accelerates **Disk Usage only**. Search and Flat View
continue to use their portable recursive walker in this release.

## Packaging

The release contains:

- `Ferail-0.7.1-win-x64.zip` — unsigned portable GUI, CLI and the dedicated
  `ferail-ntfs-helper.exe`;
- `Ferail-0.7.1-x64-symbols.zip` — matching GUI, CLI and helper PDBs plus the
  build-identity manifest.

Windows SmartScreen may warn because the build is not Authenticode-signed. The
symbols archive is only needed for diagnosing crash dumps.

The full technical list is in
[CHANGELOG.md](CHANGELOG.md#071--2026-08-27-windows-only-release).
