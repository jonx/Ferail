# Disk Usage

Walks a directory tree off the UI thread and shows the result as an
interactive squarified treemap. It now docks into the active tab as a
[Tool Result Surface](TOOL_RESULTS.md), while the standalone GPUI window
renderer remains available for pop-out work. Ported in iter-6.0…6.4 from the
Ferail predecessor's spec
(`docs/done/DISK_USAGE.md` in the Ferail repo); the data model
and layout algorithm are shared verbatim, the worker and visual
control are macOS-native rewrites.

## Status

Shipped with follow-ups. The docked Disk Usage result surface, scanner, treemap,
Top-N panel, package handling, category filtering, allocated/apparent size
modes, screenshot path, and CLI all ship. APFS-clone-aware sizing, richer
iCloud download-state handling, and explicit dock/pop-out state migration remain
open — see "Still open" below.

## Surface

- **Open**: `Cmd+Shift+D` (`view.disk_usage`). Opens Disk Usage docked in the
  active tab, rooted at the tab's current directory. The breadcrumb row shows
  the shared result-surface pill and close button, so the docked DU header uses
  a compact "Disk Usage" title instead of repeating the full path.
- **Open in window**: the result-surface header's pop-out button, or the
  `disk_usage.open_in_window` command, opens the same root in the standalone
  Disk Usage window and returns the tab to normal browsing. The same
  `DiskUsageView` entity moves across, so scan progress, zoom, selection, and
  Top-N state are preserved. In a standalone window the header shows the full
  root path because there is no shell breadcrumb around it.
- **Dock in tab**: standalone Disk Usage windows opened from the shell show a
  Dock in Tab button. It docks the same root into the owning shell's active tab
  and closes the window, preserving the same live view state.
- **Refresh**: `[Refresh]` button in the header strip
  (`disk_usage.refresh`). Cancels any in-flight scan and re-walks the
  same root, preserving the descend-packages setting.
- **Zoom in**: `Enter` on the selected rect, or "Zoom into" in the
  context menu, drills into a container.
- **Zoom out**: `Backspace` (`disk_usage.zoom_out`) pops one level off
  the zoom path. Active only when the DU view has focus and the path is
  non-empty.
- **Top-N panel**: toggleable via `disk_usage.toggle_topn`. Shows the
  50 largest individual files anywhere in the scanned tree, sorted by
  size descending. Selection-synced with the treemap.
- **Descend into packages**: `disk_usage.toggle_packages`. By default
  `.app`, `.bundle`, `.framework`, `.plugin`, `.kext`, `.xcodeproj`
  are treated as opaque leaves, matching Finder. Toggling re-scans.
- **Right-click**: Reveal in Finder, Open, Copy Path, Move to Trash,
  Zoom into. Move-to-Trash succeeds → the affected subtree is
  surgically removed from the in-memory tree and Top-N is rebuilt; no
  re-scan is needed.

Selection: plain click replaces, `Cmd+Click` toggles, `Escape` clears.
Clicking a Top-N row selects the matching file in the treemap, and
vice versa.

## Architecture

```
+------------------- worker thread (scan_disk_usage) ----------------+
|  std::fs::read_dir DFS, batches DiskUsageFact, throttles progress  |
|  cancellable via Arc<AtomicBool>; flushes buffer + exits on cancel |
+--------------------------------------------------------------------+
                       |
                       | Arc<Mutex<VecDeque<ScanMsg>>>
                       v
+---------------- DiskUsageView entity (main thread) ---------------+
|  bounded queue drain from worker messages                         |
|  generation gate drops stale events                               |
|  apply_facts -> debounced rebuild of layout cache + Top-N         |
|  render(host bounds):                                             |
|    header (path/total/status/refresh) | volume row (free/used/bar) |
|    treemap pane (squarified, hover/selection)                     |
|    Top-N pane                                                     |
+-------------------------------------------------------------------+
```

### Crates

| Crate | Responsibility |
|---|---|
| `feraille-disk-usage` | Pure data model + squarified treemap. No I/O, no platform deps. Houses `DiskUsageTree`, `DiskUsageNode`, `DiskUsageLayoutNode`, `DiskUsageFact`, `compute_treemap`, `hit_test`, `build_layout_node`, `FileCategory`. |
| `feraille-fs-native` | `NativeFs::scan_disk_usage` worker — DFS via `read_dir`, `symlink_metadata` (no follow), absorbs per-subdir permission errors, batched fact callback (`DEFAULT_DU_BATCH = 256`), throttled progress (~250 ms). |
| `feraille-gpui` | `disk_usage.rs` — the Disk Usage view: squarified treemap + top-list views, hover/selection state, and worker orchestration, all as GPUI elements. |

### Data flow

1. `Cmd+Shift+D` → `Shell::on_open_disk_usage` creates a `DiskUsageView`
   entity and stores it in `Tab::tool_result`.
2. Worker emits `DiskUsageFact`s into the view's bounded queue. The foreground
   drain task applies a limited number of messages per tick so the UI keeps
   breathing.
3. Each batch is applied to `DiskUsageTree`. Layout is rebuilt at most
   every 80 ms (`DU_LAYOUT_DEBOUNCE_MS`) until the scan completes; the
   `mark_complete` path forces one final rebuild so the last batch's
   facts reach the screen.
4. Layout is re-laid (and the Top-N heap re-sorted) on bounds change,
   zoom-in/out, or tree epoch advance. Hover/selection do not
   recompute layout — only repaint with different overlays.

### Worker contract

`NativeFs::scan_disk_usage(root, batch_size, cancel, descend_packages,
on_batch, on_progress) -> Option<EnumerationError>`:

- Iterative DFS via an explicit `Vec<PathBuf>` stack — no Rust
  recursion, so deep trees can't blow the stack.
- `symlink_metadata` only; symlinks count as 0-byte leaves to keep
  the walk cycle-safe.
- Per-subdir `read_dir` failures are absorbed and emit
  `ContainerScanCompleted` for the bad dir so the UI doesn't hang in
  "Scanning" forever. The top-level open failure is the only thing
  that returns an error.
- macOS package directories (`.app`, `.bundle`, `.framework`,
  `.plugin`, `.kext`, `.xcodeproj`) are emitted as `NodeKind::File`
  leaves when `descend_packages` is `false`; the scanner walks inside
  them to compute a Finder-style rolled-up total without exposing inner
  children in the treemap.
- Apparent size (`metadata.len()`) and allocated size
  (`MetadataExt::blocks() * 512`) are both stored. APFS-clone-aware
  deduplication remains deferred (see Still open).

## Verification

The same `paint_du` runs in the live window and the headless
screenshot path (see `screenshot::run_disk_usage`), so visuals stay in
lockstep by construction.

```sh
# Live GUI:
cargo run --bin Feraille
#   then press Cmd+Shift+D

# Headless PNG via main bin:
cargo run --bin Feraille -- \
  --screenshot /tmp/du.png \
  --disk-usage ~/Source/Feraille \
  --width 1400 --height 900 --theme dark

# Standalone CLI (also produces PNG via --png):
cargo run --bin disk_usage_cli -- ~/Source/Feraille --png /tmp/cli.png
```

Tests: `cargo test -p feraille-disk-usage` (model, aggregate, layout,
hit-test) and `cargo test -p feraille-fs-native` (scanner integration
against a temp-dir fixture, with permission and cancellation cases).

## Iter-7 polish (shipped)

- **Bundle rolled-up size** — when `descend_packages=false` the
  scanner walks inside `.app`/`.framework`/etc. to compute a
  Finder-style total instead of reporting the inode-stat size.
- **Allocated vs apparent size** (`SizeMode::{Apparent, Allocated}`)
  — `MetadataExt::blocks() * 512` is read at scan time and stored on
  every `DiskUsageNode`. Toggle via View → Size: Apparent / Allocated;
  re-aggregation is cheap.
- **Age-heatmap coloring** (`TreemapColoring::AgeHeat`) — leaves are
  tinted on a cool→warm gradient by `mtime`. Two-year-old files land
  deep in the warm zone.
- **Category legend chips** — click "Image"/"Video"/etc. above the
  treemap to dim everything else. Click again or "All" to clear.
- **Top-N panel polish** — scrollable, click-to-sort by Size / Name /
  Age, with the parent folder as a subtitle row.
- **iCloud cloud glyph** — files under
  `~/Library/Mobile Documents/` get a ☁ overlay in the top-right of
  their cells. Coarse path-prefix detection; doesn't yet
  distinguish downloaded vs placeholder.
- **Right-click on multi-selection** — when the click target is part
  of the existing selection, the menu acts on the whole set
  ("Move 3 Items to Trash", "Reveal 3 in Finder", "Copy 3 Paths").
- **Docked host sizing** — the GPUI view measures its host element so the
  treemap fits either the active tab or a standalone window.
- **Cmd+R refresh** — refreshes the active DU view when focus is inside it.
- **Menu checkmarks** — toggle states for Top-N / packages /
  follow-navigation / coloring / size-mode reflect live values.
- **Refresh button** has hover + pressed visual states matching the
  main window's button styling.
- **DU toast surface** — Move-to-Trash failures surface through the GPUI
  notification layer instead of only stderr.

## Still open

- **APFS clone-aware sizing**: apparent and allocated sizes both
  double-count blocks shared by clones. Genuine deduped on-disk
  total needs `fcntl(F_LOG2PHYS_EXT)` per extent, plus a global
  set of `(device, physical_block, length)` tuples to find
  overlaps. Cost: ~one syscall per file plus a substantial heap.
  Sketch:

  ```rust
  // Per file, on macOS:
  // 1. open(path) read-only
  // 2. fcntl(fd, F_LOG2PHYS_EXT, &log2phys) in a loop incrementing
  //    log2phys.l2p_devoffset until it returns -1 / no more extents
  // 3. for each extent: insert (st_dev, l2p_devoffset, length) into
  //    a per-volume IntervalSet; union overlaps
  // 4. on scan completion: per volume, sum the union's unique bytes
  ```

  Until this lands, the **Allocated** size mode is the closest
  available proxy — it doesn't dedupe but does reflect real block
  occupancy.

- **iCloud download status**: the cloud glyph is currently
  path-prefix only. A future iter can read NSURL's
  `ubiquitousItemDownloadingStatusKey` per file to distinguish
  `Downloaded` / `NotDownloaded` / `Current` and use a different
  glyph or color tint.
