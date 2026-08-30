# Windows Fast NTFS Disk Usage

Implementation specification for the optional elevated NTFS backend used by
Disk Usage. This is deliberately narrower than a general filesystem indexer:
v1 produces ephemeral Disk Usage snapshots and releases every byte of per-scan
raw-volume state. The narrow elevated process may serve more than one request
during a Ferail session so repeated scans do not repeatedly prompt for UAC.

← Back to [feature notes](README.md) · [Disk Usage](DISK_USAGE.md) ·
[Windows handover](../testing/WINDOWS_HANDOVER.md)

## Status and non-goals

Implemented on Windows as the isolated commit series `d0c0a0b` through
`8dca494`. The neutral parser, fragmented MFT stream, compact index, private
protocol, authenticated elevated helper, Disk Usage adapter, exact UTF-16 path
arena, atomic Portable fallback, settings and UI are present. Automated gates
on a real Windows NTFS machine pass (24 neutral parser/protocol tests, 9 Win32
helper/pipe tests, 26 Disk Usage model tests and 316 GPUI tests; one explicit
network test ignored). The disposable adversarial VHDX recipe lives at
`scripts/testing/fast-ntfs-vhdx.ps1`.

Version 0.7.2 keeps this engine as a Windows preview after real elevated tests
validated ordinary NTFS and OneDrive-backed directory traversal. Before
promoting it beyond preview, retain broader evidence from the packaged helper
and adversarial filesystems. Hyper-V-dependent VHDX and million-entry
memory/performance gates remain hardware qualification, not claims inferred
from unit tests. Authenticode qualification is also still open: the current
portable helper is unsigned, so a same-publisher signature check at launch
cannot yet distinguish it from a replacement in the user-writable package
directory. An interim salted-digest check now stands in its place: see
[Interim helper attestation](#interim-helper-attestation) for what it does and
does not cover.

The 0.7.1 integration is for Disk Usage only. Flat View and Search still use
their portable recursive walker; sharing the fast transport with them requires
a separate adapter that preserves their `FileEntry`, filtering and fallback
semantics. Duplicate finding, ordinary listings and background indexing are
also outside v1. Do not persist an MFT index or requested paths. USN-journal
incremental refresh is a later feature, not part of v1.

## User contract

- Ferail itself remains `asInvoker`. A normal launch never requests elevation.
- Disk Usage offers `Portable` everywhere and `Fast NTFS (administrator)` only
  for a local NTFS volume. FAT, exFAT, ReFS, network paths, shell namespace
  items and WSL always use Portable.
- Ferail may suggest Fast NTFS when a portable volume-sized scan is expected
  to be slow. It never opens UAC at application startup and never elevates
  merely because the user navigated to a directory.
- Choosing Fast NTFS is the explicit consent point. The first Fast scan in a
  Ferail process may show UAC; later scans reuse the authenticated helper until
  Ferail exits. Nothing is installed and the GUI never receives an elevated
  token.
- Denying or cancelling UAC, a missing helper, a protocol error, unsupported
  media or a helper failure leaves the GUI usable and visibly starts Portable.
- The result states which engine produced it and whether concurrent volume
  changes made it a best-effort snapshot.
- Closing, cancelling or refreshing Disk Usage drops that request's raw
  reader, compact index, queued batches and GUI result arena. The idle helper
  remains connected for later requests and exits when Ferail disconnects.

## Interim helper attestation

Until the package is signed, `crates/ferail-ntfs-win32/src/attest.rs` stands in
for the same-publisher signature check. `scripts/package-win.ps1` draws a fresh
32-byte salt per release, hashes the **staged and signed** helper as
`SHA-256(salt ‖ file ‖ salt)`, and rebuilds `Ferail.exe` with both values baked
into its constant data. Before elevating, the launcher opens the helper denying
other writers and deleters, hashes it through that handle, and holds the handle
across `ShellExecuteExW`.

Be precise about what this is worth:

- **A closed check-to-launch window is a real guarantee.** The deny-write hold
  means nothing can substitute the file between the hash and the elevation.
- **Mundane failures fail closed.** A stale helper from an older version, a
  half-extracted ZIP, an interrupted update, or a helper replaced on its own
  all fall back to Portable with a distinct banner.
- **Against a local attacker it buys cost, not immunity.** Anyone who can write
  the helper can usually write `Ferail.exe` and patch the expected digest out.
  The salt means they must reverse the binary instead of searching it for a
  known 32-byte hash, so a scripted swap stops working and a determined one
  does not.

Only Authenticode closes the last point, because the signal is then enforced by
Windows rather than by our own code: UAC names the publisher, so a substituted
helper prompts as an unknown one where the user can see it. Do not describe the
digest check as that boundary in release notes or documentation.

A self-binding variant (folding the parent's own hash into the expected value,
so patching the parent invalidates it) was considered and rejected: Authenticode
signing rewrites the parent after the build, which would break the binding at
exactly the moment signing starts.

Ordering constraints the packaging script must keep:

- The digest is taken **after** signtool has run on the helper. Signing rewrites
  the file, so a digest taken earlier describes bytes that no longer exist.
- The attestation rebuild selects only `ferail-gpui`, so the hashed helper is
  not relinked. The script re-hashes the staged helper afterwards and fails if
  it moved.
- A `-SkipBuild` run cannot bake a digest. The script says so and marks the
  artifact unpublishable.

## Architecture boundary

Use three layers; do not put NTFS parsing or Win32 calls in GPUI rendering code.

1. `ferail-ntfs`: a new pure, non-UI crate. It owns bounded parsing, compact
   records, subtree construction and fixtures. It must compile and run parser
   tests on macOS/Linux from byte-backed fixtures. Evaluate and pin the
   read-only `ntfs` crate only after its license, workspace MSRV, malformed
   input behavior, attribute-list traversal and allocation semantics pass the
   fixture suite. Wrap it behind Ferail-owned types; no third-party parser
   type crosses the crate boundary.
2. A Windows-only helper binary, shipped beside and signed with Ferail. Its
   entry point performs protocol dispatch before logging, databases, GPUI or
   shell-provider initialization. It handles one request at a time, opens that
   volume read-only, writes bounded result frames, releases all request state,
   then waits for another request until the GUI disconnects. Keep a separate
   binary rather than elevating the complete GUI process.
3. The existing GPUI Disk Usage coordinator selects an engine and consumes a
   common event stream. Both engines keep the existing generation,
   cancellation, queue-cap and foreground-drain rules. Render code never knows
   about MFT records.

Introduce an engine seam instead of adding `cfg(windows)` branches throughout
`DiskUsageView`:

```text
DuEngine = Portable | FastNtfs
DuScanRequest = root + descend_packages + scan-local id namespace + cancel
DuScanEvent = Ready + Batch + Progress + Complete | Failed
DiskUsageScanner::scan(request, sink)
```

The exact Rust trait may differ, but `DiskUsageView::start_scan` must only
choose an implementation and consume `DuScanEvent`. Portable behavior and the
CLI remain unchanged unless an engine is explicitly requested.

## Probe and elevation sequence

The unelevated parent performs the cheap eligibility probe off the UI thread:

1. Resolve the requested path with `GetVolumePathNameW`, then obtain its volume
   GUID with `GetVolumeNameForVolumeMountPointW`. Do not infer identity from a
   drive letter: a volume may be mounted below another volume.
2. Use `GetDriveTypeW` and `GetVolumeInformationW` to require a local NTFS
   volume. Reject UNC/network, WSL, removable policy exclusions and every
   unrecognised result without elevation.
3. On first use, create a random session-private named-pipe name. The
   unelevated process owns the server with `FILE_FLAG_FIRST_PIPE_INSTANCE`,
   `PIPE_REJECT_REMOTE_CLIENTS`,
   overlapped I/O and a DACL limited to the invoking user, Administrators and
   SYSTEM. Cap every wait and make it cancellation-aware.
4. Start the exact sibling helper path with `ShellExecuteExW(..., "runas")`.
   The command line contains only protocol version and the random pipe name,
   never the requested filesystem path or an MFT-derived name.
5. After connection, use `GetNamedPipeClientProcessId` and compare it with the
   PID returned by `ShellExecuteExW`. A nonce visible in the command line is
   not authentication. Only after the PID check does the parent send the root
   and volume identity inside the private pipe.
6. For every request, the helper independently resolves the root and confirms
   the same volume GUID, NTFS type and root file identity before opening the
   raw volume. A mismatch is a hard protocol failure.

The helper opens the volume GUID without a trailing slash using `CreateFileW`
with `GENERIC_READ`, `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`,
`OPEN_EXISTING` and no write access. Never lock, dismount or write the volume;
never issue `FSCTL_ALLOW_EXTENDED_DASD_IO`. Elevation is required for direct
volume access. Enable `SeBackupPrivilege` only when present and needed for
otherwise inaccessible path handles; it is not a substitute for validating
the elevated token and it cannot be added by `AdjustTokenPrivileges`.

## Private protocol

Use a versioned binary protocol, not JSON and not process arguments. All
integers are little-endian. Every frame has a fixed header:

```text
magic "FDU1" | protocol u16 | kind u16 | request_id u64 | payload_len u32
```

Reject unknown versions/kinds, a mismatched request id, duplicate terminal
frames, trailing bytes, embedded NULs and payloads above 16 MiB. UTF-16 fields
are opaque code-unit sequences: preserve even unpaired surrogates rather than
silently replacing a legal NTFS name. Batch payloads are independently bounded
so the pipe can never force unbounded allocation.
The parent-to-helper frames are `Start` and `Cancel`; helper-to-parent frames
are `Hello`, `Ready`, `Batch`, `Progress`, `Complete` and `Failed`.

`Start` carries the volume GUID, requested root as length-prefixed UTF-16, the
expected root file identity, sizing mode and package policy. `Ready` is sent
only after all validation and reader setup succeeds. `Batch` carries owned
neutral link rows, not raw NTFS buffers or pointers. `Complete` includes final
counters, skipped/corrupt records and start/end journal observations.

Pipe disconnect is cancellation. The helper checks cancellation between raw
read windows, records, ancestry phases and writes. The parent retains the
process handle, applies finite startup/inactivity/terminal deadlines, and
terminates a helper that ignores disconnect during shutdown. Verify on Windows
whether the elevated child can be placed in a kill-on-close Job; use it when
permitted, but do not make correctness depend on cross-integrity Job access.

## Raw reader and parser

The performance property is one bounded sequential raw-volume reader, not one
Win32 call per file.

1. Validate the NTFS boot sector and call `FSCTL_GET_NTFS_VOLUME_DATA`. Cross-
   check sector size, cluster size, file-record size, MFT start LCN, MFT valid
   length and volume serial. Reject zero, non-power-of-two, overflowing or
   volume-out-of-range geometry.
2. Wrap the volume handle in a small aligned read/seek cache (for example,
   8–32 MiB). Volume handles may behave as non-cached devices; arbitrary
   parser reads must be rounded to valid sector boundaries. Do not load the
   whole MFT into memory.
3. Bootstrap record 0 (`$MFT`) at `MftStartLcn`, validate its `FILE` signature
   and update-sequence-array fixups, then resolve the unnamed `$DATA` stream.
   The MFT is not guaranteed contiguous. Its signed runlist deltas, sparse
   runs, `$ATTRIBUTE_LIST` and extension records must be handled before the
   rest of the MFT is traversed. Reading `MftValidDataLength` bytes starting at
   `MftStartLcn` as one extent is explicitly forbidden.
4. Iterate the MFT stream in file-record-sized units. `FSCTL_GET_NTFS_FILE_RECORD`
   is useful for sampled cross-checks and fixture diagnosis, but a per-record
   IOCTL loop is not the fast engine.
5. For each in-use base record, validate fixups and every offset/length before
   reading it. Follow extension records through a bounded attribute-list
   traversal with cycle detection. Checked arithmetic is mandatory for VCN,
   LCN, run length, byte offset, allocation and buffer calculations.
6. Extract the record number and sequence, flags, link count, standard mtime,
   all meaningful `$FILE_NAME` links, and the unnamed `$DATA` logical and
   allocated sizes. Exclude DOS-only aliases when a Win32/POSIX name exists;
   retain distinct real hard-link names. Named alternate data streams are not
   counted in v1 so Fast and Portable use the same user-visible size contract.
7. Descend a reparse directory only when the MFT contains real children whose
   parent is that directory (as with OneDrive Files On-Demand). Never resolve
   or graft the external reparse target; an empty junction therefore remains
   an opaque leaf. Filter unused/deleted records and stale parent
   references whose sequence does not match. Individual bad live records are
   counted and skipped; corrupt volume geometry, MFT mapping or an error rate
   above the documented threshold fails the engine.

Before and after parsing, query the USN journal when available and record only
its journal id/next-USN counters in memory. A changed counter makes the result
`best_effort_live`; it does not normally fail a scan because active system
volumes constantly change. V1 does not persist or replay the journal.

## Compact tree and exact semantics

Do not build absolute paths or a `PathBuf` per MFT row. Use packed numeric
records plus UTF-16 name arenas:

```text
FileMeta: record+sequence, flags, logical bytes, allocated bytes, mtime
NameLink: parent reference, file index, UTF-16 offset+length, namespace
```

Keep one `FileMeta` per base record and one `NameLink` per real name. Build a
compact parent→children adjacency (CSR or first-child/next-sibling indices),
then walk only descendants of the requested root. A deep-directory scan still
reads the volume MFT, but does not transmit siblings or allocate GUI nodes for
them.

Each visible hard-link path gets its own scan-local DU node so actions rebuild
the path the user clicked. Charge logical and allocated bytes exactly once per
base file within the requested subtree, deterministically to the first link in
the final traversal; other links show zero charged bytes. This matches the
portable scanner's once-per-scan rule. Record both raw UTF-16 name components
and a lossy display string: actions reconstruct `PathBuf` from the raw arena,
never from the displayed `String`. This requires extending the current DU
path-arena/event seam; silently converting an ill-formed NTFS name is not
acceptable.

Emit parent-before-child batches of at most 256 visible links, with stable
scan-local ids. The adapter produces the existing `DiskUsageFact` sequence and
raw path-arena components together. It must preserve package opacity,
classification, apparent/allocated modes, progress, selection, context menus,
Open in New Tab and export without teaching those consumers about FRNs.

Memory gates are structural rather than a misleading fixed file-count cap:

- no full MFT byte buffer, absolute-path cache, `PathBuf` per record or
  duplicate name copy in the helper;
- compact metadata target at most 64 bytes per base record plus at most 24
  bytes per name link, excluding the single UTF-16 arena and adjacency;
- at most two encoded result batches pending in the helper and the existing
  bounded GUI queue;
- helper private bytes, GUI private bytes and handle counts recorded at 1M and
  4M entries; per-request helper memory disappears when a scan ends, the idle
  helper disappears when Ferail exits, and GUI result memory becomes reusable
  after the DU surface closes.

## Failure and fallback rules

- Failure before `Ready`: close the helper and automatically start Portable.
- Failure after `Ready` but before any `Batch`: same behavior.
- Failure after any batch: increment the GPUI scan generation, discard the
  partial Fast tree/path arena/queue, show a one-line fallback reason, then
  start a fresh Portable scan. Never mix facts from two engines.
- User cancellation or closing the tool never starts a fallback scan.
- Invalid helper data is treated as a failed engine, never trusted merely
  because the helper is signed/elevated.
- Logs contain engine, phase, elapsed time, counts and redacted error class;
  never root paths, filenames, raw records, pipe names, nonces or frame bytes.

## Isolated implementation sequence

Each step is a separately reviewable/revertible commit:

1. Add `ferail-ntfs` with byte-reader abstraction, neutral structs and corrupt/
   valid parser fixtures. No Win32, UI or elevation.
2. Add the Windows aligned raw reader and a console-only diagnostic that emits
   counts, never names. Compare geometry and sampled records with the documented
   FSCTLs. Do not package it yet.
3. Add compact name-link indexing, subtree traversal, hard-link/reparse sizing
   tests and deterministic neutral batches.
4. Add the dedicated elevated helper, signed packaging and authenticated,
   fuzzed private pipe. Still no user-facing engine choice.
5. Add `DiskUsageScanner`, raw UTF-16 path-arena components, generation-safe
   fallback and existing-fact adaptation.
6. Add the explicit Disk Usage engine UI, translations, progress/coverage
   wording, Settings preference and screenshot state.
7. Qualify on Windows. Only then publish Fast NTFS in a release build. Evaluate a
   separate read-only adapter for Flat/Search afterward; do not share the
   elevated helper's lifetime or index implicitly.

## Test and release gates

Parser tests run on every platform and cover valid resident/nonresident data,
fragmented and negative-delta runlists, USA failure, attribute-list extension
records, sparse/compressed files, multiple Win32/POSIX names, DOS aliases,
stale sequences, cycles, truncated buffers and arithmetic overflow.

On Windows, create a disposable NTFS VHDX fixture with nested directories,
hard links, sparse and compressed files, ADS, junctions, Unicode and long
names, an inaccessible directory and concurrent mutation. Save creation
scripts, not the resulting personal MFT image. For root and deep-subdirectory
scans compare Portable/Fast visible paths and totals, with explicit expected
exceptions caused by elevated coverage.

Also verify:

- normal startup is `asInvoker`; only explicit Fast selection shows UAC;
- UAC denial, pipe race, wrong PID, malformed/oversized/truncated frames,
  helper crash and timeout recover without a hang or orphan;
- cancellation during elevation, raw reads, parsing, ancestry construction and
  streaming completes promptly;
- 20 open/cancel/close cycles return memory and handles to the recorded
  baseline range;
- Fast beats Portable by a meaningful measured multiple on 1M and 4M entries
  without regressing ordinary listings, OneDrive no-hydration, WSL, 10k media,
  Flat 4M or macOS;
- packaged helper discovery and signatures work in a clean Windows Sandbox;
- the interim attestation behaves: a packaged build runs Fast NTFS normally; a
  helper replaced or truncated after packaging falls back to Portable with the
  "does not match this build" banner; and holding the helper with
  `FILE_SHARE_READ` across `ShellExecuteExW` does not itself provoke a sharing
  violation on any supported Windows version: this last one is unverified from
  a macOS development host and must be exercised on real hardware;
- logs, reports, metadata DB and crash bundles contain no requested path,
  filename, raw record, pipe identifier or protocol payload.

## Primary references

- Microsoft: [Opening physical disks and volumes with `CreateFile`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew)
- Microsoft: [`FSCTL_GET_NTFS_VOLUME_DATA`](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_get_ntfs_volume_data)
- Microsoft: [`NTFS_VOLUME_DATA_BUFFER`](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-ntfs_volume_data_buffer)
- Microsoft: [`FSCTL_GET_NTFS_FILE_RECORD`](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_get_ntfs_file_record)
- Microsoft: [`FSCTL_ENUM_USN_DATA`](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_enum_usn_data)
- Microsoft: [Walking USN record buffers](https://learn.microsoft.com/en-us/windows/win32/fileio/walking-a-buffer-of-change-journal-records)
- Microsoft: [`AdjustTokenPrivileges`](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-adjusttokenprivileges)
- Candidate parser API: [`ntfs` 0.4 documentation](https://docs.rs/ntfs/0.4.0/ntfs/)
