# Feraille Notes

This file is the short chronological decision log. Product status lives in
[docs/FEATURE_LEDGER.md](docs/FEATURE_LEDGER.md); architecture rules live in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
[docs/UI_NONBLOCKING.md](docs/UI_NONBLOCKING.md).

## Current Direction

Feraille is now documented as the macOS-first port and UI rewrite of Ferail.
The old Windows docs have been reconstructed into Mac-aware docs under `docs/`
and the existing specs have been cleaned to stop describing macOS as a dev-only
target.

The strongest architectural rule is now explicit: the UI thread must never do
I/O while painting or handling immediate interaction.

## Shipped Slices

- Soft renderer and design tokens.
- Virtualized list, scrollbar, splitter, focus ring, tabstrip, breadcrumb, file
  tree, status bar.
- Real home-folder enumeration.
- Tree expand/collapse and reveal-on-navigation.
- Screenshot CLI for headless visual verification.
- Breadcrumb edit mode.
- File open, refresh, hidden toggle, delete-to-Trash fallback.
- File columns and sorting.
- macOS system icons.
- In-memory Ant Trail heat.
- Trackpad scroll routing by pane.
- Magic column and magic detection.
- Get Info panel.
- macOS chrome inset.
- Context menu slice, copy path, reveal in Finder.
- Drag-out slice.
- Rename and new-folder dialogs.
- Search/filter dialog.
- Preview info pane.
- Magic sniffing moved off the UI thread after a real hang report.
- Iter-5.6: icon prefetch chunked on the main thread (NSWorkspace is
  main-thread-only, so worker pattern doesn't apply). `IconChunkTick`
  events drain 4 keys at a time with generation tokens; the
  `ProgressStrip` reflects in-flight state.
- Iter-5.7.1: inline (in-row) rename in the list pane. F2 on the list
  starts a `TextInput` overlay anchored to the row's Name column via the
  new `VirtualizedList::row_name_rect`; ESC cancels, Enter or
  click-outside commits, scroll-offscreen auto-cancels. The modal rename
  dialog stays for tree-pane / context-menu invocation.
- Iter-5.7.2: real Trash semantics via
  `NSFileManager.trashItemAtURL:resultingItemURL:error:`. Cmd+Z undo
  from Finder, audible feedback, per-volume `.Trashes` for non-boot
  volumes. The `~/.Trash` rename + cross-volume copy fallback is gone;
  failures now error visibly instead of silently delete-on-cross-volume.
- Iter-5.7.3: Toast primitive (`feraille-controls/primitives/toast.rs`)
  with bounded stack, fade-out, and bottom-right paint. User-facing
  `log_error!` sites (rename / create_dir / trash / open_with_default /
  inline rename) now also push an error toast so failures are visible
  in-app, not stderr-only.
- Iter-5.7.4: streaming-enumeration spec drafted at
  [docs/features/STREAMING_ENUMERATION.md](docs/features/STREAMING_ENUMERATION.md).
  Trait shape, caller contract, worker shape, batch size, cancellation
  semantics, and migration plan. Single-batch wrapper is the safe first
  shipping step. No code yet — implementation is a multi-iter project.
- Iter-5.8: command-name registry as the load-bearing identity. New
  `feraille_core::commands` module with `CommandId`, `Shortcut`, and a
  static catalogue (`app.about`, `app.settings`, `file.new_tab`,
  `file.get_info`, `view.toggle_hidden`, `go.back/forward/parent/home`).
  NSMenu items are emitted from the catalogue; clicking fires a
  registered callback with the `CommandId`, which `App::user_event`
  dispatches to existing methods. Shortcuts become *one* of several
  possible bindings instead of the action's identity, opening the door
  to user-remappable bindings, a command palette, and scripting.
- Iter-5.9: command catalogue grows to cover every existing keyboard
  shortcut, and the keyboard handler is migrated to dispatch through
  it. Ten new commands (`file.refresh`, `file.new_folder`,
  `file.move_to_trash`, `file.copy_path`, `file.reveal_in_finder`,
  `view.search`, `view.edit_breadcrumb`, `view.toggle_preview`,
  `window.next_tab`, `window.prev_tab`) appear automatically as menu
  items. `App::dispatch_command` is the single entry point reached
  from both `AppEvent::Command` (menu) and `keystroke_to_command`
  (keyboard). Alternates that don't fit the one-shortcut-per-command
  model (Ctrl+H, bare Backspace / Delete, Alt+arrows) now redirect
  to dispatch instead of duplicating method calls.
- Iter-5.9 (cont.): streaming directory enumeration shipped alongside
  the catalogue migration. Per the spec at
  [docs/features/STREAMING_ENUMERATION.md](docs/features/STREAMING_ENUMERATION.md):
  `NativeFs::enumerate_streaming` is a worker-side function that reads
  `std::fs::read_dir`, accumulates `FileEntry` rows into 256-row
  batches, and invokes a callback per batch with cancellation between
  entries. `feraille-app` owns the threading and event-loop dispatch:
  new `start_enumeration` spawns via `obs::spawn_logged`, posts
  `AppEvent::EnumerationBatch` per batch and `EnumerationDone` at the
  end, both gated on a generation token. `goto_path` and
  `refresh_active_tab` migrated; tree-pane callers untouched (small
  folders, no benefit). Per-batch the visible list re-filters and
  re-sorts; scroll/cursor preserved across batches when refreshing
  (F5), reset when navigating. Surveyed Ferail first — its
  `EnumerationService` had the same worker + channel + generation
  shape, but with no cancel flag and PostMessageW dispatch — confirmed
  the design and avoided its known gaps.
- Iter-5.10: `file.close_tab` (Cmd+W) joins the catalogue with
  `validateMenuItem:` semantics. `AppMenuTarget` gains a per-item
  validate selector that reads a `TAB_COUNT` thread-local snapshot;
  when there's only one tab Close Tab is greyed out and Cmd+W falls
  through to Close Window in the Window submenu. Host updates the
  snapshot via `feraille_shell_mac::set_tab_count` on every tab open
  / close and at startup. Mirrors Finder's Cmd+W behaviour.
- Iter-5.11: `app.settings` (Cmd+,) opens a placeholder NSAlert that
  shows current state (theme, hidden-files, sidebar width, tab count)
  and notes that a real Settings window lands later. Native AppKit
  modal — no Feraille soft-rendered chrome. Makes Cmd+, do something
  visible without forcing a Settings UI design pass yet.
- Iter-5.12: Help submenu. New `Category::Help` and two commands —
  `help.github` opens the project URL via `NSWorkspace.openURL:`,
  `help.shortcuts` (Cmd+/) pops a native modal listing every
  catalogue command grouped by category, with the canonical
  ⌘⌥⇧+key glyph for its `default_shortcut`. Cheat sheet is
  auto-generated; adding a command + shortcut to
  `feraille_core::commands` makes it appear here without touching
  the Help code. `show_settings_placeholder` collapsed into a
  generic `feraille_shell_mac::show_alert(title, body)` that both
  Settings and Help share. New `feraille_shell_mac::open_url(url)`
  helper.
- Iter-5.15: tiny task manager. New `crates/feraille-app/src/tasks.rs`
  (`TaskRegistry`, `ActiveTask`, `TaskKind`, `TaskProgress`) and
  `task_panel.rs` (popover paint + hit-test). The three per-feature
  `Option<ProgressTaskId>` fields collapse into per-feature
  `Option<TaskId>` plus a single shared `task_strip_token` that lives
  while the registry is non-empty — so two overlapping tasks no longer
  steal the strip from each other (the prior behaviour had the visual
  going idle once the most recent task finished, even if older ones
  were still running). `App::begin_task` / `end_task` route through
  the registry and the strip together; `cancel_task` knows how to halt
  enumeration (cancel flag) and icon prefetch (bump generation, drop
  queue). Magic prefetch stays uncancellable. `format_status` appends
  the primary task's label when one task runs and "N tasks running"
  when more do. Clicking the status bar while any task is active
  toggles the popover; Escape or click-outside closes it. The popover
  is anchored bottom-right above the status bar with inline chrome
  matching `ModalPanel`'s look. Verified with the new
  `--simulate-task-panel` screenshot flag.
- Iter-5.14: real volume names + groundwork for capacity bars. New
  `feraille_fs_native::VolumeInfo { path, name, total_bytes,
  available_bytes, is_local, is_removable }` and
  `volume_info_for_path(path)` query a single batched
  `NSURL.resourceValuesForKeys:` call with the cached, mount-stamped
  keys (`LocalizedName`, `TotalCapacity`, `AvailableCapacity`,
  `IsLocal`, `IsRemovable`). Capacity is gated on `is_local` so we
  don't issue SMB/NFS round-trips for network shares; the chosen
  capacity key is the cheap cached one (not the
  `ForImportantUsageKey` variant which scans purgeable content), so
  no spin-ups for sleeping disks. `list_volumes()` now returns
  `Vec<VolumeInfo>`; the boot row in Locations uses the real
  `LocalizedName` instead of the hardcoded "Macintosh HD". Boot
  firmlink (`/Volumes/<bootname>`) is filtered out by name match.
  Sets up rendering a per-row "space used" bar in a follow-up iter.
- Iter-5.16.0: hold the previous folder visible until the first
  enumeration batch lands. `goto_path` and `refresh_active_tab` no
  longer clear `tab.all_entries` / `tab.entries` synchronously — the
  swap now happens atomically inside the `EnumerationBatch` handler
  on the first batch for a generation. New
  `App::enumeration_pending_first_batch` is set in `start_enumeration`
  (proxy path only) and cleared either by that first-batch swap or,
  for empty/error finishes, by `EnumerationDone` dropping the held
  rows and rebuilding visible. Headless `start_enumeration` still
  overwrites synchronously so screenshot output is unchanged.
  Removes the residual same-pane flash on slow filesystems
  documented in c0ae03d.
- Iter-5.16.1: tree-pane ancestor enumeration off the main thread.
  Both `reveal_in_tree`'s ancestor walk on navigation and
  `TreeEvent::ExpandRequested` on user click used to call
  `fs.enumerate(id)` synchronously on the UI thread, stalling input
  and paint on slow volumes. New `App::spawn_tree_load(id)` spawns a
  worker via `obs::spawn_logged`; results return through
  `AppEvent::TreeChildrenLoaded { generation, id, entries, error }`.
  `App::tree_load_generation: u64` plus
  `tree_pending: HashMap<NodeId, u64>` give per-id staleness gating:
  duplicate spawns for the same id are deduped, and
  `App::invalidate_tree(id)` (used by `refresh_active_tab`) drops the
  pending entry so a stale result fails the gate. The handler
  populates and re-runs `ensure_visible(selected)` so a deep reveal
  target scrolls into view as ancestors expand around it. Headless
  callers fall through to the synchronous path —
  `cargo run -- --screenshot --expand <path>` keeps the same output.
  Closes the last synchronous-tree-enum hazard noted in 449c3d6.
- Iter-7: Disk Usage polish pass — every "open item" except APFS
  clones. Bundle rolled-up size makes `.app`/`.framework` cells
  show their real Finder size instead of the inode-stat (96 B).
  `SizeMode::{Apparent, Allocated}` and a new `is_cloud` flag are
  scanned and surfaced (cloud glyph overlay on
  `~/Library/Mobile Documents/`). `TreemapColoring::AgeHeat` tints
  by `mtime` (cool → warm). Category legend chips above the
  treemap dim non-matching cells via a new
  `treemap::paint(..., filter_category, ...)` parameter. Top-N
  panel gained scroll, click-to-sort buttons (Size/Name/Age), and
  a parent-folder subtitle row. Right-click on a multi-selection
  acts on the whole set ("Move 3 Items to Trash"). Auto-rescan on
  active-tab navigation (opt-in via View → Follow Tab Navigation,
  on by default). DU window geometry persists to
  `~/Library/Application Support/Feraille/du_window.txt` so
  resizes stick across runs. Refresh button now has hover + press
  visual states; click fires on release inside the rect (canonical
  macOS button behaviour). New in-DU toast surface for
  Move-to-Trash failures. Menu checkmarks reflect every toggle
  state. Cmd+R bound globally to `disk_usage.refresh`. APFS clone
  dedup is still deferred — concrete sketch in
  `docs/features/DISK_USAGE.md`.
- Iter-6.0…6.4: Disk Usage feature. New `feraille-disk-usage` crate
  holds the pure model + squarified treemap (ported from Ferail's
  `DISK_USAGE.md`). `NativeFs::scan_disk_usage` is the worker — DFS
  via `read_dir`, `symlink_metadata`, batched fact callback,
  cancel-flag, throttled progress. `feraille-controls::treemap`
  paints a `Vec<TreemapRect>` with depth-blue + category-tint fills,
  hover/select borders, and rect-size-gated labels. The DU window is
  a dedicated second `winit` Window opened on `Cmd+Shift+D`,
  realized lazily via `try_realize_disk_usage_window` so the
  command dispatcher doesn't need `&ActiveEventLoop`. Layout is two
  rows of header (path + Refresh; volume name + free/used + capacity
  bar), treemap pane on the left, draggable splitter, Top-N largest
  files panel on the right. Right-click yields Reveal/Open/Copy
  Path/Move-to-Trash/Zoom into; trashing surgically removes the
  subtree and rebuilds Top-N without re-scanning. Same `paint_du`
  function backs both the live window and the headless
  `--screenshot --disk-usage` path so visuals stay in lockstep. A
  standalone `disk_usage_cli` bin shares the same scan/aggregate
  pipeline for terminal use.

## Important Bug Lesson: Magic Sniffing Hang

A folder click froze the app. The macOS hang stack showed:

```text
window_event -> handle_tree_event -> navigate -> goto_path -> prefetch_magic
-> detect_magic -> read
```

The problem was not tree expansion itself. Navigation synchronously sniffed file
headers on the main thread. One slow/special file was enough to hang the UI.

Fix: magic prefetch now runs on a worker and returns through a winit user event.
Results carry a generation id and current folder so stale results are ignored.

Rule reinforced: no filesystem reads on the UI hot path.

## Current Known Gaps

- Icon fetching is now chunked on the main thread (iter-5.6); it no longer
  blocks navigation, but `NSWorkspace.iconForFile:` itself still runs on the
  UI thread because the API is main-thread only.
- Preview pane is metadata-only.
- Context menu is a hardcoded slice, not final NSMenu/services behavior.
- NodeStore identity model is not fully ported.
- Status progress/task aggregation now ships a tiny per-task popover
  (iter-5.15). ETAs, byte counts, and copy/move integration still pending.
- Persistent Ant Trail, metadata DB, disk usage, duplicate finder, and full
  preview providers are pending.

See [todo.md](todo.md) for the structured near-term / later split.

## Docs Rebuild

The Ferail Markdown reconstruction created:

- [docs/FEATURE_LEDGER.md](docs/FEATURE_LEDGER.md)
- [docs/UI_NONBLOCKING.md](docs/UI_NONBLOCKING.md)
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- [docs/ROADMAP.md](docs/ROADMAP.md)
- [docs/porting/FERAIL_DOCS_MAP.md](docs/porting/FERAIL_DOCS_MAP.md)
- [docs/features](docs/features)
- [docs/TESTING_OVERLAYS.md](docs/TESTING_OVERLAYS.md)

The source map records how every Markdown file from `../Ferail` was folded
into Feraille.
