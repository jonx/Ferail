# Verified copies and integrity checks

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

Two features share one BLAKE3 hash store, the duplicate finder's cache in the
`files` table ([DUPLICATES.md](DUPLICATES.md),
[METADATA_DB.md](METADATA_DB.md)). Neither runs in the background: every byte
is read inside a copy or a check the user started.

## Verify copies

**Settings → Files → Copying → Verify copies**, off by default
(`verify_copies` in the settings file).

When it is on, every file the transfer engine copies is proved. The engine
copies it in 1 MiB pieces, hashing each piece as it is read from the source,
so the source is read once. It then reads the copy back from the destination,
hashes it again, and compares. This covers Copy, Paste, drops that copy, and
the copy half of a move between volumes.

- **A mismatch fails the item.** The copy is removed, and the item is reported
  through the ordinary per-item failure toast as "the copy did not match the
  original", with Retry. A copy that cannot be read back is treated the same
  way.
- **A move never deletes the source of an item that failed.** The source of a
  cross-volume move is deleted only after every file of that item verified.
- **Nothing to check:** a move on the same volume is a rename, and a
  same-volume copy on APFS is a clone that shares the source's blocks. Clones
  count as verified by construction and are not read back.
- **Progress** counts both reads: the total doubles, and the task label reads
  "Verifying" while a copy is being read back. The success toast ends in
  "verified" when every item verified.
- **Metadata** is preserved as on the unverified path. The bytes are written
  by the engine, so runs of zeros become holes rather than writes (a sparse
  file stays sparse), and on macOS `fcopyfile` then applies the source's
  xattrs, ACLs, flags, mode and timestamps.
- **The read-back** is a plain read. On macOS it sets `F_NOCACHE`, a hint that
  does not guarantee the bytes come from the medium rather than from pages
  still cached from the write.
- **Recorded hashes:** each verified copy's hash is stored for the destination
  and, for a copy, the source. Verify Integrity and Find Duplicates reuse
  them. The store's privacy rules apply: nothing is recorded under an excluded
  folder, and under Remember nothing it lasts for the session.

Elevated retries and Undo copy without verification.

## Verify Integrity

**Verify Integrity** in the file context menu, the File menu and the command
palette checks the selected files and folders, or the current folder when
nothing is selected. It opens a report in the tab, runs as a cancellable task,
and reads one file at a time.

For each file with a recorded hash:

| On disk now | Verdict | Record |
| --- | --- | --- |
| Same size and modification time, same hash | Verified OK | refreshed |
| Same size and modification time, different hash | **Corrupted** | kept |
| Different size or modification time | Modified since last check | replaced |
| Changed while it was being read | Modified since last check | unchanged |
| No recorded hash | Baseline recorded | created |
| Could not be read | Unreadable | unchanged |

A corrupted file keeps its old record, so every later run reports it again
until it is replaced. Corrupted files are listed with a button that selects
them in Ferail; the first 10,000 are listed and the counts go on.

What it cannot do:

- It only knows files hashed before: by a verified copy, Find Duplicates,
  Compare Folders, or an earlier Verify Integrity. The first run over a folder
  mostly records baselines.
- Modification times are compared to the second. A program that rewrites a
  file to the same size within the second its hash was recorded looks like
  corruption.
- Records age out under the Privacy retention setting. A run refreshes the
  records of files that verify.
- Files under an excluded folder are counted as not checked. Undownloaded
  cloud files are skipped rather than downloaded. Symlinks are not followed.

## Code

| Piece | Where |
| --- | --- |
| Verifying copy, read-back, `TransferOpts` | `crates/ferail-fs-native/src/file_ops.rs` |
| Integrity walk and `classify` | `crates/ferail-fs-native/src/integrity.rs` |
| Hash store adapter (`get_record`, `records`) | `crates/ferail-gpui/src/dupe_cache.rs` |
| Transfer UI: setting, label, toast | `crates/ferail-gpui/src/shell/file_ops.rs` (`spawn_transfer_op`) |
| Report | `crates/ferail-gpui/src/integrity_view.rs`, hosted by `verify_view.rs` |

Tests: `file_ops::verify_tests` and `integrity::tests` in `ferail-fs-native`,
`dupe_cache::tests` in `ferail-gpui`.
