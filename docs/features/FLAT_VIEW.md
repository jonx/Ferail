# Flat View (recursive listing of a location)

← [Feature notes](README.md) · [Architecture](../ARCHITECTURE.md) ·
[Search](SEARCH.md) · [Open work (TODO)](../TODO.md)

**Status: shipped, millions-first foundation.** Flat View is an uncapped,
files-only recursive snapshot with streaming progress, cancellation, a sortable
relative Path column, in-memory filtering, and surface-local identities. The
remaining scale work is listed in §4.5.

The idea: a toggle on the file list that turns the current location into a
*flat* listing — every file in every subfolder, as one list, with a new
**Path** column so a row's location is still readable. Turn it off and the
tab returns to the ordinary one-directory listing.

This note answers the two questions that were asked of it:

1. Can we cope with a location holding millions of files?
2. Should flat mode get its own custom view, or ride the existing list?

**Short answers.** (2) is easy: **no new view.** The table already
virtualizes correctly and is already an in-tree fork we control
(`crate::multi_table`); what flat mode needs is a different *row source*
and a leaner *row model*, not a different widget. (1) is the real work:
the widget scales, but the **row model, the identity maps, and the
per-batch apply path do not**. Measured today, one million flat rows would
cost ~1.3 GB of RAM (~620 MB of it never released) and roughly **nine
minutes of cumulative UI-thread time** during streaming. That is a Prime
Directive failure, not a slow feature. With the fixes in §4 the same
million rows land without global-path retention and with streaming work
proportional to each batch. There is deliberately no 200k product tier or
fixed row cap. "Any number" means the scanner never invents an arbitrary
limit; the honest in-memory ceiling is available RAM, with a page-backed store
planned for result sets that exceed it.

---

## 1. What already exists that this rides on

Flat mode is, mechanically, *subtree search with a query that matches
everything*. Almost all of the plumbing is shipped:

| Piece | Where | Reusable as-is? |
|---|---|---|
| Cancellable recursive walker | `ferail-fs-native::flat_subtree` | Shipped — an explicit match-all traversal, separate from empty search |
| Streaming results into the tab's table | `shell/search.rs` | Yes — same `LoadBatch` shape as directory loads |
| A tab-local surface that replaces the listing but keeps `current_dir` | `ToolResultSurface` ([shell/tab.rs:48](../../crates/ferail-gpui/src/shell/tab.rs#L48)) | Shipped — `Flat` beside `Search` |
| Per-row location string | Compact directory arena in `file_list.rs` | Shipped — real relative Path column |
| A new column that survives upgrades | `split_persisted_columns` trails unknown-to-the-spec columns as visible ([file_list.rs:2947](../../crates/ferail-gpui/src/file_list.rs#L2947)) | Yes — adding a 6th column is back/forward safe |
| Virtualized table we own | `crate::multi_table` (local fork of gpui-component's table) | Yes |

So the feature is *not* a from-scratch build. The work is almost entirely
in making the shared row path survive two orders of magnitude more rows.

## 2. Measurements

All numbers from this machine (Apple silicon, APFS SSD, warm page cache),
release builds. Corpus: the real `~/Source` tree — **661,558 entries,
58,090 directories, average path length 96 bytes**. Memory figures are RSS
deltas from a harness that builds the exact structures the app fills per
row; scripts live in the session scratchpad, not the repo.

### 2.1 Enumeration floor

Historical baseline before the native-reader work: a single-threaded DFS with
the walker's old syscall shape (`read_dir` +
`symlink_metadata` per dirent, plus `dirent.metadata()` for every matched
entry — and in flat mode *every* entry matches):

| Shape | Result |
|---|---|
| one stat per entry | 661,558 entries in **9.7 s** (68k entries/s) |
| two stats per entry (what flat mode would do) | 661,558 entries in **11.4 s** (58k entries/s) |
| `find` (no stat), for reference | 1.4 s |

**⇒ ~1M files ≈ 17 s warm, single-threaded.** Cold, on an external or
network volume, minutes. Flat mode is therefore a *scan*, not a
*listing*: it must stream, show progress, be cancellable, and never
pretend to be instant.

The shipped walker no longer has that shape. Flat View shares the same native
directory layer as ordinary listings, recursive search and Disk Usage. On
macOS, `getattrlistbulk` returns the row metadata in batches and a bounded
coordinator reads multiple directories concurrently only on local,
non-removable APFS. Other media use the conservative serial fallback. This
changes enumeration latency without changing Flat's compact rows, scan-local
identity, bounded worker/UI channel or viewport-only enrichment.

### 2.2 Memory per row

| Structure | Bytes/row | 1M rows |
|---|---|---|
| `FileListDelegate::entries: Vec<FileEntry>` (`size_of::<FileEntry>()` = 264 + strings) | 373 | 356 MB |
| `heats` / `is_favorited` / `tags` parallel vecs (`tags` is a `Vec<Vec<TagColor>>` — 24 B even when empty) | 24 | 23 MB |
| `delegate.paths: HashMap<NodeId, PathBuf>` — path copy #1 | 179 | 171 MB |
| `NodeStore` (`nodes` + `path_index`) — path copies #2, #3 | 332 | 316 MB |
| `NativeFs::Inner` (`paths` BTreeMap + `by_path` HashMap) — path copies #4, #5 | 317 | 303 MB |
| **Total** | **~1,360** | **~1.30 GB** |

Two things stand out:

- **Every path is stored five times.** ~800 of the 1,360 bytes per row is
  path duplication across three subsystems.
- **Two of those subsystems never shrink.** Neither `NodeStore` nor
  `NativeFs::Inner` has any `remove`, `clear`, or `retain` — the maps only
  grow, for the process lifetime. Flattening one big tree therefore leaks
  **~620 MB permanently**, even after the user toggles flat mode off and
  navigates away. TODO already anticipates this under *"give tool results
  (DU, dupes, search) a per-scan arena id namespace that drops with the
  surface"* — flat mode makes that item a prerequisite rather than a
  nice-to-have.

### 2.3 UI-thread cost

| Operation at 1M rows | Time |
|---|---|
| `sort_in_place(Name, asc)` on unsorted input | **222 ms** (+~100 MB transient for the cached keys) |
| same, on already-sorted input | 48 ms |
| `sort_in_place(Size, desc)` | 52 ms |
| One `refresh_file_list_favorited_in_tab` pass | **279 ms** |

All of these run on the UI thread today. A 222 ms hitch when the user
clicks a column header is survivable (and could be moved to a worker);
the favorited pass is not, because of how it is called — see next.

### 2.4 The quadratic streaming apply — the real blocker

`refresh_file_list_favorited_in_tab` ([shell.rs:4175](../../crates/ferail-gpui/src/shell.rs#L4175))
iterates **every row in the listing**, doing a HashMap lookup and a
`PathBuf` *clone* per row. It is called **once per streamed batch** — from
the directory load path ([shell.rs:4145](../../crates/ferail-gpui/src/shell.rs#L4145))
and from the search path ([shell/search.rs:502](../../crates/ferail-gpui/src/shell/search.rs#L502)).
The call site's own comment says *"Cheap (HashMap lookups across the new
batch)"* — but the implementation walks the whole model, not the batch.

Batch size is 256 (`DEFAULT_SEARCH_BATCH`). So the total work is
`Σ k·cost` over ~3,900 batches:

> **1M rows streamed in 256-row batches ⇒ ~544 s (~9 minutes) of
> cumulative UI-thread time**, with the final batch alone blocking the
> UI for 279 ms.

It is invisible today only because searches return few hits and folders
hold few files. Flat mode makes the tail the normal case.

The same shape repeats for the other per-batch passes —
`refresh_file_list_selection_in_tab` (linear `position()` scan for the
lead), `restore_filtered_out_against_model_in_tab`,
`recompute_live_range_in_tab` — and the directory path additionally calls
`append_entries_sorted`, i.e. **re-sorts the entire model on every batch**
(~94 s cumulative at 1M rows). Search already uses the unsorted
`append_entries`; flat mode must too.

### 2.5 Whole-listing background workers

Two workers seed themselves from *all* entries after a load:

- **`prefetch::start`** — magic sniffing + quarantine xattrs. At 1M rows
  that is **opening a million files** and a million DB writes. Flat now uses a
  separate viewport-scoped pass with overscan: visible Format, Description,
  and quarantine data still work, while off-screen files stay unopened and
  Flat paths are not persisted to the metadata DB.
- **`folder_sizes::start`** — recursive size per directory row. In a flat
  listing the directory rows are *nested inside each other*, so this
  re-walks the same subtree once per level. Pathological by construction.

(The Finder-tag reader is already capped at `TAG_READ_CAP = 1000` rows —
the right precedent to copy.)

The whole-list passes remain off on Flat. Its viewport detail pass records one
byte of unseen/in-flight/complete state per row so revisiting a viewport does
not repeat I/O; the derived text itself lives only on rows actually viewed.

### 2.6 Two O(N)-on-paint hazards

- `build_drag_snapshot` ([file_list.rs:791](../../crates/ferail-gpui/src/file_list.rs#L791))
  walks all entries and clones a `PathBuf` per *selected* row, lazily on
  the first selected-row render. Select-All in a 1M-row flat view ⇒ ~1M
  clones and ~130 MB **during paint**.
- Select-All itself materializes a `HashSet<NodeId>` of every row (~48 MB
  at 1M) and `refresh_file_list_selection_in_tab` clones it per batch.

### 2.7 Scroll geometry precision

`Pixels` is `f32`, and `uniform_list` computes
`content_height = item_height × item_count`, deriving the visible range by
dividing the scroll offset back out. At 1M rows × ~28 px ≈ 28M px the
float ULP is 2 px, so sub-row scroll precision degrades and small wheel
ticks can be lost; at 10M rows it is ~16 px, i.e. half a row of jitter.
Cosmetic, not fatal, but it is the ceiling that argues against ever
treating "tens of millions of rows" as a supported mode rather than a
capped one.

## 3. Verdict on the two questions

**Can we cope with millions?** Not with today's row model — but the limits
are all in code we own, and none of them require a different UI widget:

| Rows | Today | With §4 fixes |
|---|---|---|
| ≤ 50k | fine | fine |
| ~200k | ~1 min of UI-thread stutter during streaming, ~270 MB | fine, ~60–80 MB |
| 1M | ~9 min of UI-thread stutter, 1.3 GB (620 MB leaked) | ~17 s scan, ~250–400 MB, smooth |
| 10M+ | no | needs an on-disk/indexed model — out of scope |

**A new custom view?** No.

- The table is already virtualized (`uniform_list` renders only the
  visible range) and is already a **local fork** we can extend
  (`crate::multi_table`, forked for row-event control).
- A parallel view would fork every behaviour the list already carries —
  selection semantics, drag/drop, context menus, column persistence,
  sorting, keyboard nav, tags, thumbnails — and those are exactly the
  behaviours users will expect flat rows to keep.
- What *is* genuinely new is the **row source and row model**. Build that,
  keep the view.

The one thing worth splitting is the *storage* behind `FileListDelegate`,
so a flat listing and a directory listing can be backed by different row
stores behind the same delegate API.

## 4. Design proposal

### 4.1 Row model: a compact flat row + a directory arena

Never store a full path per row. Intern directories:

```rust
/// One interned directory: its leaf name and its parent's index.
struct DirArena { names: Vec<Box<str>>, parents: Vec<u32> }   // ~40 B per *directory*

struct FlatRow {
    dir: u32,            // index into the arena — the Path column
    name: Box<str>,      // leaf only
    size: u64,
    mtime_unix: i64,
    created_unix: i64,
    kind: EntryKind,
    flags: u8,           // hidden / locked / quarantined / hazards
}
```

`~/Source` has 58k directories for 661k entries — roughly one arena entry
per eleven rows. A row drops from ~1,360 bytes to **~100 bytes plus the
display strings the paint path actually needs**, and the Path column
renders by walking the arena (bounded, allocation-light, cacheable per
visible range).

Display strings (`display_size`, `display_kind`, …) are currently
pre-formatted per row for the no-alloc-on-paint contract. At flat-mode
scale they should be formatted **per visible range** into a small ring
cache instead — same contract, one-viewport-worth of allocation rather
than one-listing-worth.

### 4.2 Identity: a per-surface arena, not the global maps

Flat rows must **not** mint global `NodeId`s in `NativeFs` + `NodeStore`.
Give the flat surface its own id namespace that is dropped with the
surface (TODO's "per-scan arena id namespace"), and promote a row into
global identity only when it is acted on — opened, previewed, selected for
a file operation. That removes 3 of the 5 path copies *and* the permanent
leak in one move.

### 4.3 Apply path: make per-batch work proportional to the batch

- `refresh_file_list_favorited_in_tab` → take a row range and touch only
  the new rows. (Worth fixing on its own merits: it is quadratic for large
  ordinary directories today.)
- Same for the selection / filtered-out / live-range passes.
- Use `append_entries` (unsorted) while streaming, as search already does;
  apply the sort once at `Done`, on a worker if the model is large.
- Never impose a fixed row cap. Stream until completion, cancellation, or an
  explicit allocation failure. Any future memory-pressure guard must say why
  the scan stopped and preserve the partial snapshot; it must never look like
  a complete result.

### 4.4 Semantics to settle before building

These are product decisions, not technical ones; naming them now avoids a
rewrite later.

- **Do folders appear as rows?** Recommend: files only by default, with a
  toggle. Folder rows in a flat list are ambiguous (their children are
  already listed) and they drag the folder-size worker in with them.
- **What does the Path column show?** Recommend: path *relative to the
  flat root*, with the root shown once in the breadcrumb — matching the
  existing `with_location` behaviour, and sortable (grouping a flat list
  by directory is the single most useful sort it has).
- **Freshness.** The watcher is deliberately non-recursive
  ([fs_watcher.rs](../../crates/ferail-gpui/src/fs_watcher.rs)), and
  recursively watching a huge tree is not viable. Flat mode is a
  **snapshot with an explicit Refresh**, exactly as SEARCH.md already
  concludes for walker-backed views. Say so in the UI.
- **Filter interaction.** The filter box should filter the flat model
  in-memory (Tier 0 semantics over flat rows). Pressing Return inside flat
  mode is already a recursive search of the same root — the two must not
  fight; simplest is that a search *replaces* flat mode and clearing it
  returns to flat.
- **Navigation.** Double-clicking a folder row (if folders are shown)
  leaves flat mode and navigates. Back should restore flat mode as a
  history state.
- **Walk policy.** Reuse the walker's Mac-safe rules unchanged: packages
  opaque, symlinks not followed, iCloud placeholders never materialized,
  hidden honoured by the existing toggle. State them in the UI where they
  change what the user sees (a `.app` counted as one item).
- **Toggle placement + icon.** A toggle button beside the existing
  list/grid `ViewMode` control. Per [ICONS.md](ICONS.md) it needs its own
  glyph — the spare upstream pool has nothing that reads as "include
  subfolders"; `folder-tree` from Lucide in house style is the natural
  vendor. Not `list`/`layers` (both read as view-density).
- **Persistence.** Flat mode is per tab and should *not* persist across
  launches by default — reopening a window into a multi-minute scan is a
  bad first frame.

### 4.5 Phasing

1. **Phase 0 — shared streaming remediation (shipped).** Favorites are
   computed only for each incoming batch, normal directories sort once at
   completion instead of once per batch, and the walker reuses its one stat.
   Flat's worker/UI channel is bounded and applies coalesced batches at most
   ten times per second, so a warm SSD cannot queue gigabytes of paths or
   starve the event loop by forcing one repaint per 256 rows.
2. **Phase 1 — uncapped Flat surface + local identity (shipped).** Explicit
   match-all walker, files-only list, compact per-directory path arena,
   scan-local IDs, Path column, progress/cancel/Refresh, snapshot-local
   filtering, and no whole-list prefetch or folder-size workers.
3. **Phase 2 — compact row payload + asynchronous indexes (in progress).** The
   shared `FileEntry` is down from 264 to 160 inline bytes: immutable texts use
   shared allocations, absent quarantine details cost one pointer, raw/display
   names share storage when equal, and Flat's path index shares the row's leaf
   name. Empty heat/tag/favorite vectors are not materialized; directory
   strings are single-owned; repeated display sizes and extension types use a
   capped surface-local interner. Viewport-scoped detail prefetch preserves
   Format, Description, and quarantine affordances without persistent Flat
   paths. Select All is now a compact complement (`all except exceptions`), so
   four million rows do not become four million hash-table entries; status
   totals and painting stay constant-time, and drag payload creation is bounded.
   Copy File List reconstructs huge path lists in yielding UI batches. A
   dedicated sub-100-byte Flat row is deliberately deferred after real-world
   measurements reached about 1.1 GB for 4.1 million rows. The remaining scale
   work is off-thread sort/filter indexes and segmented scroll coordinates.
4. **Phase 2.5 — shared native enumeration (shipped).** macOS batches stat-like
   attributes with `getattrlistbulk`; local internal APFS scans use a bounded
   directory worker pool, while removable/network/unknown media stay serial.
   The same implementation serves ordinary listings, recursive search and
   Disk Usage, and cancellation clears queued directories immediately.
5. **Phase 3 — page-backed scale.** Add a spillable/on-disk row and sort index
   for data sets larger than RAM. Spotlight can
   enumerate a scope near-instantly, and `ferail-meta` could hold a
   queryable index. Both change freshness and completeness guarantees
   (Spotlight excludes trees; an index can be stale), so either is a
   *latency* path that must fall back to the walker for correctness —
   never the only path.

## 5. Risks / what not to do

- **Do not fix a Prime Directive symptom by chunking the quadratic pass.**
  The pass must become proportional to the batch; slicing 279 ms into
  frames just spreads the stall.
- **Do not let flat mode start `prefetch` or `folder_sizes` unscoped.**
  Sniffing a million files is worse than the listing itself.
- **Do not add a second view implementation.** Every behaviour it forks
  (selection, DnD, menus, columns) is a behaviour that will drift.
- **Do not cap.** A truncated flat listing that looks complete is
  a correctness lie, and file operations get run against it.

## 6. Verification plan (when built)

- `cargo test` for the walker's match-everything mode (cancellation,
  stale-generation drop, package/symlink/iCloud policy).
- A row-model memory test asserting bytes-per-row stays under budget —
  the number in §2.2 is the regression baseline.
- A streaming-apply test asserting per-batch work is O(batch), not O(model).
- Screenshots at 1k / 100k rows into `screenshots/flat-view-*.png`,
  including the Path column and live progress.
- Manual: flatten a large tree on an external drive, confirm the UI stays
  live throughout, cancel mid-scan, toggle off, and confirm RSS returns to
  its pre-scan level (the §2.2 leak is the thing being tested).
