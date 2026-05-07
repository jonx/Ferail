# Disk Usage

Walks a directory tree off the UI thread and shows the result as an
interactive squarified treemap in a dedicated window. Ported in
iter-6.0…6.4 from the Ferail predecessor's spec
(`/Users/jkn/Source/Ferail/docs/done/DISK_USAGE.md`); the data model
and layout algorithm are shared verbatim, the worker and visual
control are macOS-native rewrites.

## Status

Done (iter-6.4). APFS-clone-aware sizing, age-heatmap coloring, and
allocated-vs-apparent toggle are deferred — see "Open items" below.

## Surface

- **Open**: `Cmd+Shift+D` (`view.disk_usage`). Opens (or focuses) a
  separate `winit` window rooted at the active tab's current
  directory. The main window is unaffected.
- **Refresh**: `[Refresh]` button in the header strip
  (`disk_usage.refresh`). Cancels any in-flight scan and re-walks the
  same root, preserving the descend-packages setting.
- **Zoom in**: `Enter` on the selected rect, or "Zoom into" in the
  context menu, drills into a container.
- **Zoom out**: `Backspace` (`disk_usage.zoom_out`) pops one level off
  the zoom path. Active only when the DU window has focus and the path
  is non-empty.
- **Top-N panel**: toggleable via `disk_usage.toggle_topn`. Shows the
  50 largest individual files anywhere in the scanned tree, sorted by
  size descending. Selection-synced with the treemap. Auto-hides when
  the window is narrower than 700 DIPs.
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
                       | EventLoopProxy<AppEvent>
                       v
+---------------- DU window (main thread) --------------------------+
|  AppEvent::DiskUsageBatch / Progress / Done                       |
|  generation gate drops stale events                               |
|  apply_facts -> debounced rebuild of layout cache + Top-N         |
|  paint_du(state, viewport, splitter, renderer, tokens):           |
|    header (path/total/status/refresh) | volume row (free/used/bar) |
|    treemap pane (squarified, hover/selection)                     |
|    splitter | Top-N pane                                          |
+-------------------------------------------------------------------+
```

### Crates

| Crate | Responsibility |
|---|---|
| `feraille-disk-usage` | Pure data model + squarified treemap. No I/O, no platform deps. Houses `DiskUsageTree`, `DiskUsageNode`, `DiskUsageLayoutNode`, `DiskUsageFact`, `compute_treemap`, `hit_test`, `build_layout_node`, `FileCategory`. |
| `feraille-fs-native` | `NativeFs::scan_disk_usage` worker — DFS via `read_dir`, `symlink_metadata` (no follow), absorbs per-subdir permission errors, batched fact callback (`DEFAULT_DU_BATCH = 256`), throttled progress (~250 ms). |
| `feraille-controls` | `treemap` module — stateless `paint`/`hit_test_at` for a `Vec<TreemapRect>` plus `TreemapColoring` enum. Pure paint contract; no allocations on hover/selection. |
| `feraille-app` | `disk_usage_state.rs`, `disk_usage_window.rs` — owns the second winit Window, softbuffer surface, soft renderer, and the `paint_du` orchestration. Routes window events by `WindowId`. |

### Data flow

1. `Cmd+Shift+D` → `App::open_or_focus_disk_usage` posts a
   `PendingDiskUsageOpen` and spawns the worker. The window itself is
   created on the next `user_event` / `window_event` tick when
   `&ActiveEventLoop` is in scope (`try_realize_disk_usage_window`).
2. Worker emits `DiskUsageFact`s in batches over the
   `EventLoopProxy<AppEvent>`. Stale generations are dropped at the
   gate.
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
  leaves when `descend_packages` is `false`. Iter-6.4 reports their
  immediate `metadata.len()` only — proper bundle totals via a
  child-only sum are deferred.
- Apparent size (`metadata.len()`) for now; allocated size and
  APFS-clone-aware deduplication are deferred (see Open items).

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

## Open items

- **APFS clones**: apparent size double-counts blocks shared by
  cloned files. Block-aware sizing needs `fcntl(F_LOG2PHYS_EXT)` per
  extent — too expensive for the v1 walker.
- **iCloud / dataless files**: counted at their dataless size, which
  matches Finder. A future iter could surface cloud-status in
  tooltips via `NSURL`'s `ubiquitousItemDownloadingStatusKey`.
- **Bundle rolled-up size**: when `descend_packages` is off, packages
  currently report only their leaf-of-stat size. A child-only sum
  would give Finder-style "1.2 GB for `Xcode.app`" totals.
- **Allocated vs apparent toggle**: surface `metadata.blocks() * 512`
  as an alternate size mode to make on-disk fragmentation visible.
- **Age heatmap coloring**: the data model already carries `mtime`;
  the menu just doesn't expose the alternate color mode yet.
- **DU window toast surface**: errors from Move-to-Trash log to
  stderr today. A small toast pane in the DU header chrome would
  surface them inline.
