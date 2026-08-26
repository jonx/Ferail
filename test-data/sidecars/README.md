# Sidecar fixture corpus

Deterministic fixtures for `docs/features/SIDECARS.md`. They are intentionally
small enough to inspect, and cover positive, negative, interoperability and
security cases for the implementation and future regressions.

Run `python3 generate.py` from this directory to recreate `generated/`. Use
`python3 generate.py --large 1000000` to additionally create an ignored
million-entry manifest for manual scale tests.

## Layout

- `generated/nfo/`: CP437/ANSI scene art (including the synthetic colored
  Ferail release NFO), UTF-8 art, UTF-16 MsInfo and Kodi's metadata-only,
  URL-only and combined forms.
- `generated/manifests/`: valid SFV, GNU/BSD checksum lists, comments, spaced
  and Unicode names, GNU escaped names, malformed/mixed lists and unsafe paths.
- `generated/payload/`: the deterministic files referenced by valid manifests.
- `generated/negative/`: ordinary French Latin-1 prose and generic XML that
  must not become sidecars.
- `generated/security-root/`: containment fixtures. Tests must never read
  `outside-secret.txt` through an entry in the child manifest.
- `generated/release/`: a ready-to-open manual fixture folder. Its NFO, SFV,
  SHA256SUMS and payloads agree; `problems.sfv` deliberately contains a
  mismatch, a missing file and a traversal attempt for the result UI.

The corpus contains no personal paths or data. Do not add real release NFOs:
they commonly contain handles, site names and other identifying material.

## Expected classifications

| Fixture | Expected |
| --- | --- |
| `nfo/scene-cp437.nfo` | Scene NFO, CP437 |
| `nfo/scene-ansi.nfo` | Scene NFO, inert ANSI layout |
| `nfo/ferail-release-color.nfo` | Scene NFO, ANSI colors + CP437 art |
| `nfo/kodi-*.nfo` | Kodi NFO |
| `nfo/msinfo.nfo` | Microsoft System Information NFO |
| `manifests/release.sfv` | SFV/CRC32 manifest |
| `manifests/SHA256SUMS` | SHA-256 checksum list |
| `manifests/MIXEDSUMS` | Recognized but rejected mixed algorithms |
| `negative/french-latin1.txt` | Ordinary text, never scene art |
| `negative/generic.xml` | Generic XML, never Kodi NFO |
| `security-root/child/unsafe.sfv` | Unsafe entries reported, never opened |
