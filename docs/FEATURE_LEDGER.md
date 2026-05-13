# Feature Ledger

This ledger tracks Feraille against the Windows predecessor `../Ferail`.
Status labels:

- **Done:** usable in Feraille now.
- **Partial:** a slice exists, but not final parity.
- **Todo:** not started or only documented.
- **Mac-only:** new opportunity or replacement for a Windows-only feature.
- **N/A:** Windows-specific and not relevant to macOS v1.

## Core UX

| Feature | Ferail source | Feraille status | Notes |
|---|---|---:|---|
| Window, layout, tree/list/status | `docs/notes/todo.md`, `docs/design/UI_ARCHITECTURE.md` | Done | Soft-rendered Mac UI with tabstrip, breadcrumb, tree, list, status. |
| Dense virtual file list | `docs/design/SPECS.md`, `LAZY_TEXT_DISPLAY.md` | Partial | Virtualized list exists. Final million-row model needs streaming enumeration and tighter per-item memory. |
| Tree navigation and lazy expand | `todo.md`, `UI_ARCHITECTURE.md` | Done | Cached re-reveal and fold/unfold redraw fixed. First expand still does synchronous enumerate today. |
| Tabs | `todo.md` | Done | Per-tab path, history, scroll, filter. Persistence is Todo. |
| Breadcrumb edit mode | `todo.md` | Done | `Cmd+L` / `Ctrl+L`; completion is Todo. |
| Keyboard navigation | `todo.md`, `SPECS.md` | Partial | Core keys exist. Full Finder-grade focus model and remapping are Todo. |
| Search/filter | `todo.md` | Done | `Cmd+F` / `Ctrl+F` live filter over current folder by name/kind/magic. |
| Status bar | `STATUSBARPROGRESSCONTROL.md` | Partial | Text status, single shared progress strip, and a bottom-right task popover (iter-5.15) ship; per-task ETA/bytes pending future copy/move workers. |
| Error/empty states | `SPECS.md` | Todo | Specs exist; UI states not implemented. |

## File Actions

| Feature | Ferail source | Feraille status | Notes |
|---|---|---:|---|
| Open file/default app | `todo.md` | Done | Uses platform opener. |
| Refresh | `todo.md` | Done | `F5`, preserves cursor by name when possible. |
| Show hidden files | `todo.md` | Done | `Cmd+Shift+.` and `Ctrl+H`. |
| Delete/Trash | `todo.md` | Partial | Fallback move to `~/.Trash`; final `NSWorkspace` trash is Todo. |
| Rename | `todo.md` | Partial | Modal dialog exists. Inline row rename remains Todo. |
| New folder | `todo.md` | Done | Modal dialog. Needs collision UX polish. |
| Copy path | `todo.md` | Done | Uses macOS clipboard helper. |
| Reveal in file manager | `WARMUP_RIGHT_CLICK.md` | Done | Reveal in Finder exists. |
| Copy/cut/paste files | `todo.md` | Todo | Needs NSPasteboard and file-operation worker. |
| Context menu | `WARMUP_RIGHT_CLICK.md` | Done | Plan-driven NSMenu with full Finder-equivalent action set across list / tree / treemap / background; multi-selection aware; Open With via Launch Services; Tags via URLTagNamesKey; Share via NSSharingServicePicker. Quick Actions / Services submenus deferred (need responder-chain wiring). |

## Shell And Platform Integration

| Feature | Ferail source | Feraille status | Notes |
|---|---|---:|---|
| Real macOS icons | Ferail icon parity goal | Done | `NSWorkspace` icon fetch, cached by kind/extension. Needs async boundary. |
| Native macOS chrome | Mac rewrite opportunity | Done | Transparent titlebar and traffic-light inset. Vibrancy Todo. |
| Drag out to OS | `todo.md` | Partial | AppKit drag-out slice exists. Drop target and modifiers Todo. |
| Drag into app/tree/list | `todo.md` | Todo | Needs NSPasteboard drop target and async file operations. |
| Windows shell context menu | `WARMUP_RIGHT_CLICK.md` | N/A | Use macOS NSMenu/services/Finder actions instead. |
| WSL integration | `todo.md` | N/A | Mac analogs are SSHFS/network volumes/cloud mounts, not WSL. |
| Shell namespace roots | `todo.md` | Partial | Home and `/Volumes`. Finder favorites/iCloud/Network are Todo. |

## Metadata And Intelligence

| Feature | Ferail source | Feraille status | Notes |
|---|---|---:|---|
| Magic sniffing | `MAGIC_SNIFFING.md` | Partial | Small table ported; async worker in app; iter-8.2 persists results via `feraille-meta` (write-through on `MagicBatch`, hydrate during `start_magic_prefetch`). iter-8.3 also persists the macOS `com.apple.quarantine` flag + agent + ISO + where-from URLs in the same `files` row. Full content-sniffed magic database still Todo. |
| Preview pane | `todo.md`, `MAGIC_SNIFFING.md` | Partial | Metadata/info pane exists. Text/image/PDF/Quick Look previews Todo. |
| Ant Trail heat | `ANT_TRAIL.md` | Partial | In-memory heat stripe; iter-8.1 added persistence via `feraille-meta` (write-through on navigate, hydrate on open). Heat survives restarts. Prediction/prewarming and decay still Todo. |
| Metadata database | `todo.md`, `MAGIC_SNIFFING.md`, [features/METADATA_DB.md](features/METADATA_DB.md) | Done | iter-8: `feraille-meta` crate (rusqlite + bundled SQLite). Hydrate-on-open + write-through for Ant Trail (8.1), magic cache (8.2), quarantine cache (8.3); window inner-size + sidebar/preview splitters + tab list persist on close (8.4); DU window geometry migrated from `du_window.txt` into `layout_state.du_*` (8.5). Schema is move-keyed by absolute path — `(dev, ino)` migration deferred to NodeStore work. |
| Disk usage / treemap | `DISK_USAGE.md`, [features/DISK_USAGE.md](features/DISK_USAGE.md) | Done | iter-6: dedicated `Cmd+Shift+D` window, async cancellable scanner, squarified treemap, volume header, Top-N, right-click Reveal/Trash/Open/Copy Path/Zoom. iter-7 polish: bundle rolled-up size, allocated/apparent toggle, age-heatmap coloring, category-filter legend chips, Top-N scroll/sort/parent subtitle, iCloud cloud-glyph overlay, multi-selection right-click, auto-rescan on navigation, geometry persistence, refresh button hover/press, in-window toast surface, menu checkmarks, Cmd+R refresh. APFS-clone-aware sizing still deferred (sketch in feature doc). |
| Duplicate finder | `SPECS.md`, `todo.md` | Todo | Size/partial/full hash pipeline, all off-thread. |
| Mouse prediction | `MOUSE_PREDICTOR.md` | Todo | Future prewarm scheduler; must be pure in pointer path. |

## Performance Architecture

| Feature | Ferail source | Feraille status | Notes |
|---|---|---:|---|
| Paint is read-only | `CLAUDE.md`, `LAZY_TEXT_DISPLAY.md` | Policy | See [UI_NONBLOCKING.md](UI_NONBLOCKING.md). Must be enforced continuously. |
| Async enumeration | `todo.md`, `SPECS.md`, [features/STREAMING_ENUMERATION.md](features/STREAMING_ENUMERATION.md) | Spec | Spec drafted iter-5.7.4. Implementation pending. |
| Cancellation tokens | `todo.md` | Todo | Needed for enumeration, preview, search, thumbnails, disk usage. |
| Lazy display metadata | `LAZY_TEXT_DISPLAY.md` | Partial | Current `FileEntry` caches display strings. NodeStore-style identity is Todo. |
| Status progress | `STATUSBARPROGRESSCONTROL.md` | Partial | Iter-5.15: `TaskRegistry` + popover surfaces every active task; cancellation wired for enumeration and icon prefetch. ETAs and copy/move integration are Todo. |
| Debug overlays | `TESTING_OVERLAYS.md` | Todo | Reconstructed in [TESTING_OVERLAYS.md](TESTING_OVERLAYS.md). |
