# Disk Usage

Walks a directory tree off the UI thread and shows the result as an
interactive squarified treemap. It now docks into the active tab as a
[Tool Result Surface](TOOL_RESULTS.md), while the standalone GPUI window
renderer remains available for pop-out work. Ported in iter-6.0…6.4 from the
Ferail-Win32 predecessor's spec
(`docs/done/DISK_USAGE.md` in the Ferail-Win32 repo); the data model
and layout algorithm are shared verbatim, the worker and visual
control are macOS-native rewrites.

## Status

Shipped with follow-ups. The docked Disk Usage result surface, scanner, treemap,
Top-N panel, package handling, category filtering, allocated/apparent size
modes, screenshot path, CLI, native batched APFS enumeration, bounded directory
parallelism and scan-local identity storage all ship. APFS-clone-aware sizing,
the privileged NTFS fast backend, richer iCloud download-state handling, and
explicit dock/pop-out state migration remain open — see "Still open" below.

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
- **Right-click a rect** (shipped): the file-list verbs on the resolved
  selection — Open, Open in New Tab for a single item (a file opens its
  containing folder and is selected), Reveal in Finder,
  Get Info (when opened from a
  shell), Copy (real file URLs), Copy Path(s), an **Export as HTML**
  submenu (Copy/Save This Folder for a single folder target,
  Copy/Save Whole View), Zoom In/Out, Move to Trash. Finder targeting
  rule: right-clicking a rect outside the selection retargets to just
  it; right-clicking a member acts on the whole set. A successful
  trash re-scans the root (honest totals) and reloads any shell tabs
  showing the affected directories.
- **Right-click the background**: Zoom Out, plus Copy/Save View as
  HTML for the whole current focus.
- **One menu layer by design**: rects record themselves as the target
  on right-mouse-down and the treemap's single context menu routes on
  it — per-rect ContextMenu layers stacked (their overlay hitboxes
  paint above the rects and don't stop propagation), opening two
  colliding menus and wiping the selection.
- **HTML export** (shipped): `ferail_disk_usage::treemap_html_*`
  renders the same tree through the same layout pipeline into
  self-contained HTML (inline styles, no JS) — "Copy as HTML" puts a
  paste-anywhere `<figure>` fragment on the clipboard; "Save as
  HTML…" writes a standalone page to `~/Downloads` and reveals it.
  The category palette is canonical in that crate
  (`category_color_rgba`), shared by the GPUI view, so window and
  export can't drift.

Selection (shipped): plain click replaces, `Cmd+Click` toggles; the
footer shows the single item's name+size or "N selected · total".
Clicking a Top-N row selects the matching file in the treemap, and
vice versa. Keyboard (shipped, `DiskUsage` context, claimed by clicking
the treemap): `Enter` zooms into the selected folder, `Backspace` zooms
out, `Escape` clears the selection, and `Cmd+C` / `Cmd+I` /
`Cmd+Backspace` mirror Copy / Get Info / Move to Trash on the treemap
selection. (User-remappable command ids for these are a follow-up —
the `disk_usage.*` ids in keymap.rs are still placeholders.)

## Architecture

```
+---------------- worker pool (scan_disk_usage) ---------------------+
| native batched reads; bounded parallel folders on local APFS       |
| coordinator owns policy/facts; cancellable; bounded result batches |
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
| `ferail-disk-usage` | Pure data model + squarified treemap. No I/O, no platform deps. Houses `DiskUsageTree`, `DiskUsageNode`, `DiskUsageLayoutNode`, `DiskUsageFact`, `compute_treemap`, `hit_test`, `build_layout_node`, `FileCategory`. |
| `ferail-fs-native` | `NativeFs::scan_disk_usage` worker plus the shared native directory reader. macOS uses `getattrlistbulk`; other filesystems/platforms safely fall back to `read_dir` and cached `DirEntry` metadata. A bounded coordinator parallelizes local, non-removable APFS directory reads and keeps all descent policy and fact creation on one thread. |
| `ferail-gpui` | `disk_usage.rs` — the Disk Usage view: squarified treemap + top-list views, hover/selection state, and worker orchestration, all as GPUI elements. |

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

The GPUI surface calls `NativeFs::scan_disk_usage_local(root, batch_size,
cancel, descend_packages, id_base, on_batch, on_progress)`. The public
`scan_disk_usage` compatibility entry point has the same scan semantics but
uses process-global identities.

- The shared coordinator owns a bounded directory queue. On local internal
  APFS it uses up to eight I/O workers; removable, network, unknown and
  non-APFS volumes stay serial. Workers return batches of at most 256 entries
  and never mutate the tree or invoke UI callbacks.
- macOS requests name, kind, sizes, times, flags, file id, link count and mount
  status together via `getattrlistbulk`. A missing attribute or unsupported
  filesystem falls back safely; the normal path does not issue one `stat` per
  file. Symlinks are never followed and count as zero-byte leaves.
- Per-subdirectory read failures are absorbed and emit
  `ContainerScanCompleted` for the bad dir so the UI doesn't hang in
  "Scanning" forever. The final summary states how many folders were skipped.
  The top-level open failure is the only thing that returns an error.
- macOS package directories (`.app`, `.bundle`, `.framework`,
  `.plugin`, `.kext`, `.xcodeproj`) are emitted as `NodeKind::File`
  leaves when `descend_packages` is `false`; the scanner walks inside
  them to compute a Finder-style rolled-up total without exposing inner
  children in the treemap.
- Apparent size (`metadata.len()`) and allocated size
  (`MetadataExt::blocks() * 512`) are both stored. APFS-clone-aware
  deduplication remains deferred (see Still open).
- IDs generated for the interactive surface live in a reserved scan-local
  namespace. The view stores only one parent id per node and reconstructs a
  path on an explicit action. Closing or refreshing Disk Usage drops that
  arena and its tree; millions of scan paths are not retained by NativeFs's
  process-lifetime navigation map.

### macOS access

Full Disk Access is about **coverage**, not speed. Native batched reading works
without it. If a scan actually encounters TCC-protected folders, the result is
kept as an explicit partial scan: the skipped count is shown in warning colour
and an action opens the Full Disk Access pane. The same optional action lives
under Settings › Performance › Disk Usage access. Ferail copies its app path
for the system picker and asks the user to relaunch after granting access; it
does not claim to detect the setting reliably.

Starting Disk Usage on a subdirectory scans only that subtree. Starting at a
volume root scans that volume while retaining the existing `du -x` boundary
rule and macOS firmlink exceptions.

## Verification

The same `paint_du` runs in the live window and the headless
screenshot path (see `screenshot::run_disk_usage`), so visuals stay in
lockstep by construction.

```sh
# Live GUI:
cargo run --bin Ferail
#   then press Cmd+Shift+D

# Headless PNG via main bin:
cargo run --bin Ferail -- \
  --screenshot /tmp/du.png \
  --disk-usage ~/Source/Ferail \
  --width 1400 --height 900 --theme dark

# Standalone CLI (also produces PNG via --png):
cargo run --bin disk_usage_cli -- ~/Source/Ferail --png /tmp/cli.png
```

Tests: `cargo test -p ferail-disk-usage` (model, aggregate, layout,
hit-test) and `cargo test -p ferail-fs-native` (scanner integration
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
