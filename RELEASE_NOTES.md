# Ferail 0.7.0 — Fast NTFS Disk Usage for Windows

This is a **Windows-only release**. It publishes the portable Windows x64 ZIP
and its matching symbols archive; macOS and Linux remain on 0.6.5.

## Fast NTFS Disk Usage

- Disk Usage now offers **Fast NTFS (administrator)** for eligible local,
  fixed NTFS volumes. It reads the MFT through a dedicated one-shot helper and
  streams a bounded tree to the normal Disk Usage UI, avoiding a per-file walk.
- Elevation is explicit. Ferail itself remains `asInvoker`; normal startup,
  browsing and Portable scans never show UAC. Denying UAC, a missing helper or
  any validation/protocol failure discards partial Fast state and starts a
  fresh Portable scan automatically.
- The helper authenticates its private pipe peer, independently revalidates
  the volume and selected root, opens the volume read-only, performs one scan
  and exits. Names, requested paths, MFT records and protocol payloads are not
  persisted or logged.
- Exact raw UTF-16 path components are retained for actions. Hard links are
  charged once, reparse-point directories remain leaves, and apparent versus
  allocated size is preserved. Results made while a volume changes are marked
  as a best-effort snapshot.
- Engine eligibility, active/fallback state and the remembered engine
  preference are available in Disk Usage and Settings.

Fast NTFS is new in 0.7.0 and needs more real-world testing. Portable remains
available at all times.

## Other improvements

- Portable Windows Disk Usage now uses bounded native directory batches from
  one handle per folder. NTFS file identities prevent hard-linked names from
  inflating totals without opening every file.
- NFO, SFV and common checksum sidecars receive content-aware previews and
  cancellable, bounded verification. SFV/SHA256SUMS generation is atomic and
  never overwrites an existing manifest.
- Text files can be opened directly in Notepad from their context menu, and
  long Preview/Get Info paths elide cleanly with a full-path tooltip.

## Packaging

The release contains:

- `Ferail-0.7.0-win-x64.zip` — unsigned portable GUI, CLI and the dedicated
  `ferail-ntfs-helper.exe`;
- `Ferail-0.7.0-x64-symbols.zip` — matching GUI, CLI and helper PDBs plus the
  build-identity manifest.

Windows SmartScreen may warn because the build is not Authenticode-signed. The
symbols archive is only needed for diagnosing crash dumps.

The full technical list is in
[CHANGELOG.md](CHANGELOG.md#070--2026-08-27-windows-only-release).
