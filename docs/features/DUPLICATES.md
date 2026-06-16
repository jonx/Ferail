# Duplicate Finder

The duplicate finder is I/O-heavy, so it lives behind the worker/task/progress
architecture and the [prime directive](../ARCHITECTURE.md#prime-directive): the
UI never blocks, the app stays navigable, and results stream in incrementally.

This is Feraille's clearest **lead** over the default file managers: Finder,
File Explorer, and the mainstream Linux managers ship **no** built-in duplicate
finder — users reach for Gemini, dupeGuru, Czkawka, or rmlint. Feraille builds
it in.

## Status

**Shipped.** Find Duplicates (Cmd+Shift+U, the View menu, or the command
palette) runs the funnel below off the UI thread, cache-backed by the `files`
table so rescans skip full hashing, and streams grouped results into the tab
with a reclaimable-bytes summary. Hard links are detected so the reclaim figure
doesn't lie. The funnel itself (size → xxh3 partial → BLAKE3 full, paranoid
byte-verify, dataless-skip, cancellation) and the `DupeHashCache` are
unit-tested.

**Honest scope — finding is solid; *managing* is still basic.** Results are
adjacent grouped rows in a normal file-list tab, so you can preview, select,
and delete via the usual actions — but there is **no bulk-management view yet**:
no "keep newest, trash the rest", no select-all-but-one, no per-group actions.
That, plus **APFS clone detection** + `clonefile` zero-copy remediation (only
hard links are detected today), are the real follow-ups. Dedicated tools like
Gemini still beat us on the *cleanup* UX; we match them on the *detection*.

## Target pipeline (the funnel)

Every standalone tool worth copying — rmlint, czkawka, jdupes — converges on
the same progressive funnel, because it minimizes I/O:

1. **Walk + group by size.** Reuse the
   [`scan_disk_usage`](../../crates/feraille-fs-native/src/disk_usage_scanner.rs)
   DFS. Any file with a unique size cannot be a duplicate — drop it with **zero
   hashing**. This eliminates the vast majority of candidates for free.
2. **Partial hash** (first 64 KB, `xxhash-rust::xxh3`) only on size-collision
   groups.
3. **Full hash** (`blake3`) only on partial-hash collisions — or, with a
   `paranoid` toggle, byte-for-byte comparison instead (rmlint-style: roughly
   as fast as hashing on the small surviving set, and removes hash-collision
   risk entirely).
4. **Group by full hash** and present.

The `xxh3` + `blake3` pipeline ports by intent from Ferail
(`crates/ferail-core/src/hash/pipeline.rs`).

## Not killing the CPU

The decisive insight: **duplicate finding is I/O-bound, not CPU-bound.**
BLAKE3/xxh3 run at multi-GB/s; the disk is the bottleneck. So:

- **Persistent hash cache is the biggest win.** Key hashes on
  `(path, size, mtime)` against the `files` table and the *second* scan of a
  tree skips hashing entirely — this is czkawka's main speed lever. Mirror the
  read-through / write-through dance the prefetch worker already does
  ([prefetch.rs](../../crates/feraille-gpui/src/prefetch.rs)): look up before
  hashing, write back on miss.
- **Bounded reader concurrency, not greedy parallelism.** Throwing a thread per
  file at one physical disk causes seek thrashing and is *slower*. Cap the
  reader pool; on SSD a modest pool saturates bandwidth, and the cap protects
  UI responsiveness. (rmlint's refinement — one reader thread per *physical*
  disk — is a worthwhile later optimization once we detect device identity.)
- **Run cool.** Schedule off the UI thread, back off during active interaction,
  honor cancellation between every stage.

## Mac correctness the CLI tools mostly ignore

- **Skip dataless / cloud placeholders.** Never download an iCloud file just to
  hash it. Detect the dataless flag (and `is_icloud_path`) and exclude unless
  the user explicitly opts to scan them.
- **Hard links and APFS clones are zero-extra-cost.** Two paths sharing storage
  (same inode = hard link; `clonefile`/reflink = APFS clone) are "duplicates"
  that occupy no extra bytes. Detect and flag them so we don't urge users to
  "reclaim" space that isn't actually used. This also makes **`clonefile`-based
  dedup a future zero-copy remediation** — replace a true duplicate with a
  clone instead of deleting.
- **Bundles as units.** Compare `*.app` / `*.bundle` / `*.framework` as whole
  packages (`descend_packages = false`), not as thousands of inner-file dupes.
- **File identity vs. duplicate bytes are different concepts** — surface both
  honestly in the results.

## Worker shape

A pure-function worker in `feraille-fs-native` (`dupes.rs`), driven exactly
like the disk-usage scanner — batched facts, `AtomicBool` cancel, throttled
progress, host owns the thread:

```rust
pub enum DupeFact {
    Candidate { node: NodeId, path: PathBuf, size: u64 },
    GroupConfirmed {
        full_hash: String,
        members: Vec<NodeId>,
        bytes_each: u64,
        hardlinked: Vec<NodeId>,   // share an inode — no extra bytes
        cloned: Vec<NodeId>,       // APFS clone — no extra bytes
    },
}

impl NativeFs {
    pub fn find_duplicates(
        &self,
        root: &Path,
        opts: &DupeOpts,            // paranoid, scan_cloud, follow_packages, min_size
        cancel: &AtomicBool,
        on_batch: impl FnMut(Vec<DupeFact>),
        on_progress: impl FnMut(DupeStats),
    ) -> Option<EnumerationError>;
}
```

## GPUI integration

A `DupesState` mirroring
[`DiskUsageState`](../../crates/feraille-gpui/src/disk_usage.rs): generation
gate, `cancel`, `msg_queue`, drain timer, `TaskKind::DuplicateScan` via
`begin_with_cancel`. Results render in a grouped, navigable view modeled on
`DiskUsageTree`, with safe delete/move routed through the existing
`feraille-shell-mac` file_ops + quarantine-aware deletion
([FILE_OPS.md](FILE_OPS.md)). Wire the duplicate command into the menu /
command palette.

## Rules (invariant)

- Hashing never runs on the UI thread.
- Results stream incrementally; the app stays navigable during scans.
- Scans are cancellable at every stage; stale results are dropped by generation.
- Progress is visible in the status bar / task panel.

## Todo

- `feraille-core` hash module (`xxh3` partial + `blake3` full, paranoid compare).
- `feraille-fs-native` funnel worker with size → partial → full stages.
- Persistent hash cache wiring against the `files` table.
- Hard-link + APFS clone detection (and, later, `clonefile` dedup remediation).
- `DupesState` + grouped duplicate-view UI.
- Safe delete/move actions.
- Tests: funnel correctness, cache hit on rescan, hardlink/clone classification,
  cancellation, stale-result drop.
