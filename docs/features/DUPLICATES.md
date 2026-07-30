# Duplicate Finder

The duplicate finder is I/O-heavy, so it lives behind the worker/task/progress
architecture and the [prime directive](../ARCHITECTURE.md#prime-directive): the
UI never blocks, the app stays navigable, and results stream in incrementally.

This is Ferail's clearest **lead** over the default file managers: Finder,
File Explorer, and the mainstream Linux managers ship **no** built-in duplicate
finder — users reach for Gemini, dupeGuru, Czkawka, or rmlint. Ferail builds
it in.

## Status

**Shipped.** Find Duplicates (Cmd+Shift+U, the View menu, or the command
palette) runs the funnel below off the UI thread, cache-backed by the `files`
table so rescans skip full hashing, and streams grouped results into the tab
with a reclaimable-bytes summary. Hard links are detected so the reclaim figure
doesn't lie. The funnel itself (size → xxh3 partial → BLAKE3 full, paranoid
byte-verify, dataless-skip, cancellation) and the `DupeHashCache` are
unit-tested.

**Managing is shipped too.** Two presentations, chosen in Settings
(`DupePresentation`):

- **Grouped** — confirmed groups as adjacent rows in a normal file-list
  tab; preview, select, and delete via the usual actions.
- **Panel** (default) — a dedicated card view (one collapsible card per group) with
  group-level cleanup: **keep-newest** (per group or "keep newest
  everywhere"), **select-all-but-one** with a per-row "keep this" radio,
  and **trash the marked set** through the standard quarantine-aware trash
  flow. The card list is virtualized with the shared GPUI virtual-list helper
  and owns a scrollbar, so large result sets do not instantiate every card at
  once. Selection rides the tab's existing selection set, so the panel and the
  table address the same nodes. After a cleanup the retained model is pruned
  and the reclaim summary recomputed.

**APFS clone awareness (macOS).** Both hard links *and* `clonefile` clones
are detected and excluded from the reclaimable figure (clones share storage
via distinct inodes, so `(dev, inode)` alone misses them — we compare the
physical block mapping with `fcntl(F_LOG2PHYS_EXT)`). The panel also offers
**"Dedup → clones"**: replace a group's redundant copies with clones of the
keeper, reclaiming the bytes without deleting any file (behind a confirm,
macOS/APFS only).

## Target pipeline (the funnel)

Every standalone tool worth copying — rmlint, czkawka, jdupes — converges on
the same progressive funnel, because it minimizes I/O:

1. **Walk + group by size.** Reuse the
   [`scan_disk_usage`](../../crates/ferail-fs-native/src/disk_usage_scanner.rs)
   DFS. Any file with a unique size cannot be a duplicate — drop it with **zero
   hashing**. This eliminates the vast majority of candidates for free.
2. **Partial hash** (first 64 KB, `xxhash-rust::xxh3`) only on size-collision
   groups.
3. **Full hash** (`blake3`) only on partial-hash collisions — or, with a
   `paranoid` toggle, byte-for-byte comparison instead (rmlint-style: roughly
   as fast as hashing on the small surviving set, and removes hash-collision
   risk entirely).
4. **Group by full hash** and present.

The `xxh3` + `blake3` pipeline ports by intent from Ferail-Win32
(`crates/ferail-core/src/hash/pipeline.rs`).

## Not killing the CPU

The decisive insight: **duplicate finding is I/O-bound, not CPU-bound.**
BLAKE3/xxh3 run at multi-GB/s; the disk is the bottleneck. So:

- **Persistent hash cache is the biggest win.** Key hashes on
  `(path, size, mtime)` against the `files` table and the *second* scan of a
  tree skips hashing entirely — this is czkawka's main speed lever. Mirror the
  read-through / write-through dance the prefetch worker already does
  ([prefetch.rs](../../crates/ferail-gpui/src/prefetch.rs)): look up before
  hashing, write back on miss.
- **Bounded reader concurrency, not greedy parallelism.** Throwing a thread per
  file at one physical disk causes seek thrashing and is *slower*. Cap the
  reader pool; on SSD a modest pool saturates bandwidth, and the cap protects
  UI responsiveness. (rmlint's refinement — one reader thread per *physical*
  disk — is a worthwhile later optimization once we detect device identity.)
- **Run cool.** Schedule off the UI thread, back off during active interaction,
  honor cancellation between every stage.

## Fast enumeration (future, per-platform speed note)

Stage 1 (walk + bucket by size) is the part that scales with *file
count*, not file *size* — on a cold cache over a large tree the
`read_dir` + `symlink_metadata`-per-entry walk dominates wall-clock long
before any hashing starts. Each platform exposes a bulk path that
collapses the per-entry `stat` syscalls, and the biggest wins read the
filesystem's own index directly:

- **Windows — read the MFT / USN journal directly.** On NTFS every
  file's name, size, timestamps, and `(volume, file-reference-number)`
  identity live in the **Master File Table**. Reading it in bulk via
  `DeviceIoControl(FSCTL_ENUM_USN_DATA)` against the volume handle (or
  `FSCTL_GET_NTFS_FILE_RECORD` for raw records) enumerates *millions* of
  files in seconds with **no per-file `stat`** — this is exactly how
  Everything/`voidtools` and WizTree are instant where Explorer crawls.
  Needs an elevated/volume handle and an NTFS volume; fall back to the
  normal walk on FAT/exFAT/network shares. The USN record's
  `FileReferenceNumber` is the Windows analogue of `(dev, inode)` for
  hard-link collapsing. Hard links specifically need
  `FindFirstFileNameW`/`OpenFileById` to resolve all names of a record.
- **Linux — batch the directory read, defer the stat.** `getdents64`
  already returns names in bulk; the size/identity stat is the cost.
  `statx` with `AT_STATX_DONT_SYNC` avoids forcing network round-trips,
  and `io_uring` can pipeline thousands of `statx` calls without a
  syscall per file. Reading ext4 inode tables / the journal directly
  (the MFT analogue) is possible but filesystem-specific and needs raw
  device access — not worth it versus batched `statx` for v1.
- **macOS — `getattrlistbulk`.** One syscall returns name + size +
  `(dev, inode)` + flags for a whole directory batch, eliminating the
  `symlink_metadata`-per-entry cost the current walker pays. This is the
  Mac-safe bulk primitive (Spotlight's `mdfind` is the index analogue
  but excludes unindexed volumes and can't see sizes reliably).

All three keep the funnel identical downstream — they only make the
candidate-gathering walk cheaper. The same speedup applies verbatim to
[disk usage](../../crates/ferail-fs-native/src/disk_usage_scanner.rs),
which shares the DFS. Sequenced as a deliberate follow-up: correctness
and the cache come first, raw-index enumeration is a power-user speed
lever once device/filesystem identity detection exists.

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

A pure-function worker in `ferail-fs-native` (`dupes.rs`), driven exactly
like the disk-usage scanner — batched facts, `AtomicBool` cancel, throttled
progress, host owns the thread:

```rust
pub struct DupeMember {
    pub node: NodeId,
    pub path: PathBuf,
    pub mtime_unix: i64,           // drives "keep newest"
    pub file_id: Option<(u64, u64)>,
    pub is_hardlink: bool,         // shares an inode — no extra bytes
    pub is_clone: bool,            // APFS clone — distinct inode, shared blocks
}

pub enum DupeFact {
    Group {
        full_hash: String,         // empty in paranoid mode
        bytes_each: u64,
        members: Vec<DupeMember>,
        distinct_occupants: usize, // members that own storage; reclaim = bytes_each * (this - 1)
    },
}

impl NativeFs {
    pub fn find_duplicates(
        &self,
        root: &Path,
        opts: &DupeOpts,            // paranoid, scan_cloud, follow_packages, min_size
        cache: Option<&dyn DupeHashCache>,
        batch_size: usize,
        cancel: &AtomicBool,
        on_batch: impl FnMut(Vec<DupeFact>),
        on_progress: impl FnMut(DupeStats),
    ) -> Option<EnumerationError>;
}

// Zero-copy remediation (macOS/APFS): unlink the victim, clonefile the keeper
// into its path. Reclaims the bytes without losing the file.
pub fn clone_dedup(keeper: &Path, victim: &Path) -> Result<(), String>;
```

Per-member `is_hardlink` / `is_clone` flags (rather than separate id
lists) are the single source of truth: the GPUI layer reads them straight
onto the row note and the retained `DupeGroupView`, and `distinct_occupants`
is just the count of members where neither flag is set.

## GPUI integration

A scan is a per-tab [Tool Result Surface](TOOL_RESULTS.md), like search
([SEARCH.md](SEARCH.md)). The tab carries `ToolResultSurface::Duplicates`,
whose `DupeViewMode` stores the scan root, running group / reclaim counts, and
the resolved `presentation` cached at launch so render never reads settings.
`shell/dupes.rs` runs `find_duplicates` off the UI thread via
`begin_with_cancel(TaskKind::DuplicateScan, …)`, cache-backed by `DbHashCache`,
and streams confirmed groups into the tab's table — and, for the panel, into a
retained `Vec<DupeGroupView>` (`Tab::dupe_groups`) that the selection helpers
and group actions operate on. Generation-gated so a stale batch from a
superseded scan is dropped.

The dedicated card view lives in `shell/dupe_panel.rs`; `file_pane_body`
swaps it in when `presentation == Panel`, independent of list/grid view
mode. The panel uses `multi_table::v_virtual_list` and `VirtualListScrollHandle`
to render only the visible card range, with stable per-group row heights and a
panel-owned scroll offset. Cleanup actions route through the same
quarantine-aware trash flow as `on_move_to_trash` ([FILE_OPS.md](FILE_OPS.md));
the panel owns the post-trash prune because a results tab's watcher reload is
suppressed. `clone_dedup` (macOS) backs "Dedup → clones".

## Rules (invariant)

- Hashing never runs on the UI thread.
- Results stream incrementally; the app stays navigable during scans.
- Scans are cancellable at every stage; stale results are dropped by generation.
- Progress is visible in the status bar / task panel.

## Shipped

- Funnel worker (size → xxh3 partial → blake3 full, paranoid byte-verify,
  dataless-skip, cancellation) + `DbHashCache` rescan fast path.
- Hard-link **and** APFS clone detection; both excluded from reclaim.
- Grouped-rows view **and** the virtualized dedicated panel with group actions
  (keep-newest, all-but-one, trash-marked) + macOS `clone_dedup` remediation.
- Tests: funnel correctness, cache hit on rescan, hardlink/clone
  classification + reclaim exclusion, keep-newest / all-but-one selection,
  cancellation, stale-result drop.

## Follow-ups

- Whole-bundle (`*.app`) comparison as a unit, not inner-file dupes.
- APFS-volume gating for "Dedup → clones" (today it's macOS-gated and falls
  back to a toast on non-APFS); per-physical-disk reader pool.
- Raw-index fast enumeration (see "Fast enumeration" above).
