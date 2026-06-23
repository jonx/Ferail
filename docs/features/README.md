# Feraille Feature Notes

Deeper design notes and specifications for individual features. These complement
the [architecture source of truth](../ARCHITECTURE.md) and the
[open-work list](../../TODO.md) — they capture *why* a feature is shaped the way
it is, not the current crate structure.

← Back to the [project README](../../README.md) ·
[Architecture](../ARCHITECTURE.md) · [TODO](../../TODO.md)

## Implementation Status

Audited against the Rust sources on 2026-06-20. "Shipped with follow-ups" means
the primary user-facing feature exists and the remaining work is tracked in
[TODO.md](../../TODO.md).

| Feature note | Status | Public-ready summary |
| --- | --- | --- |
| [ANT_TRAIL.md](ANT_TRAIL.md) | Shipped with follow-ups | Folder visit counts, sidebar heat, recents hydration, and DB persistence ship; prediction and decay remain open. |
| [CONTEXT_MENU.md](CONTEXT_MENU.md) | Shipped with follow-ups | Mac-native context menus, Open With, Services, Share, tags, Quick Look, Duplicate, Compress, and Trash ship; compact tag row and async Open With prewarm remain open. |
| [DISK_USAGE.md](DISK_USAGE.md) | Shipped with follow-ups | Disk Usage window, scanner, treemap/top list, package handling, category filters, and CLI basics ship; APFS clone-aware sizing and richer views remain open. |
| [DUPLICATES.md](DUPLICATES.md) | Shipped with follow-ups | Size/partial/full-hash duplicate finder, clone/hard-link awareness, card panel, virtualization, and cleanup actions ship; faster enumeration and more benchmarks remain open. |
| [FAVORITES.md](FAVORITES.md) | Shipped with follow-ups | Favorites persistence, sidebar, drag/drop, locate, rename, remove, keyboard actions, and cross-platform modal flow ship; tag favorites and file-watch missing transitions remain open. |
| [FILE_OPS.md](FILE_OPS.md) | Shipped with follow-ups | Copy, cut, paste, move, trash, collision policy, progress, cancellation, undo, and platform integration ship; mutation toast/undo gaps remain open. |
| [ICONS.md](ICONS.md) | Reference | Complete icon inventory: source (NSWorkspace / local Lucide bundle / upstream), attribution, command→icon map, and the rules for adding new icons. Flags missing/weak/reused glyphs. |
| [LAZY_METADATA.md](LAZY_METADATA.md) | Shipped with follow-ups | Shared NodeStore, path guard, cached row metadata, background prefetch, metadata DB, and process-owned caches ship; rename/move identity completion remains open. |
| [MAGIC_DESCRIPTION.md](MAGIC_DESCRIPTION.md) | Shipped | Format/Description columns, mismatch cues, quarantine badges, and structured descriptions ship. |
| [MAGIC_SNIFFING.md](MAGIC_SNIFFING.md) | Shipped with follow-ups | Structured magic detector, DB cache, async prefetch, and quarantine fusion ship; long-tail formats, cloud skip rules, and debug views remain open. |
| [METADATA_DB.md](METADATA_DB.md) | Shipped | SQLite metadata DB, schema versioning, favorites, file metadata, folder usage, and cache reset scopes ship. |
| [MOUSE_PREDICTOR.md](MOUSE_PREDICTOR.md) | Future | Design note only; no pointer prediction/prewarm scheduler is implemented yet. |
| [POWER.md](POWER.md) | Shipped with follow-ups | macOS sleep/wake handling, transfer idle-sleep prevention, and Windows/Linux shell surfaces exist; Windows display events and Power Request API are still deferred. |
| [PREVIEW.md](PREVIEW.md) | Shipped with follow-ups | Info pane, Quick Look thumbnails, inline text/Markdown/code preview, caches, scroll chaining, and viewer handoff ship; audio/archive providers and true cancellation remain open. |
| [SCREENSHOTS.md](SCREENSHOTS.md) | Shipped with follow-ups | Headless screenshot CLI and simulated UI states ship; deterministic fixture coverage and a few deferred flags remain open. |
| [SEARCH.md](SEARCH.md) | Shipped with follow-ups | In-folder filter, recursive walker, Spotlight/global search, streaming results, cancellation, and task integration ship; filters/operators and Linux/Windows indexers remain open. |
| [STATUS_PROGRESS.md](STATUS_PROGRESS.md) | Shipped with follow-ups | Task registry, status strip, task popover, cancellation flags, recent history, and screenshot simulation ship; completion notifications and accessibility announcements remain open. |
| [STREAMING_ENUMERATION.md](STREAMING_ENUMERATION.md) | Shipped with follow-ups | Directory enumeration is worker-driven, batched, cancellable, and notification-based; slow-path/stale-result tests and partial-error UI remain open. |
| [TESTING_OVERLAYS.md](TESTING_OVERLAYS.md) | Future | Debug-overlay design remains unimplemented beyond screenshot simulation hooks. |
| [THEMES.md](THEMES.md) | Planned (Phase 0 shipped) | User-facing theming plan: the selection-accent override + color picker ship; bundled themes, a theme picker, a drop-in user themes folder, and a generalized override layer are scoped but unbuilt. |
| [TOOL_RESULTS.md](TOOL_RESULTS.md) | Shipped with follow-ups | Shared tab-local result surface for Search, Duplicate Finder, and docked Disk Usage ships; pop-out/state migration remains open. |
| [VIDEO-MPV.md](VIDEO-MPV.md) | Planned | Replace the VLC video backend with libmpv (live filters, alpha) and build an N-layer transparent-colour (chroma-key) compositor on top. Phase 0 spike gates it; nothing shipped yet. |
| [VIEWER.md](VIEWER.md) | Shipped with follow-ups | Viewer window, playlist navigation, images, Quick Look fallback, VLC-backed video, slideshow, zoom, and controls ship; richer playback/playlist polish remains open. |
| [feraille-selection-dnd-spec.md](feraille-selection-dnd-spec.md) | Partial | Selection, row drag, external file drops, and many acceptance points ship; edge auto-scroll and favorite-row drops remain open. |
| [feraille-windows-instances-tabs-spec.md](feraille-windows-instances-tabs-spec.md) | Partial | Process/window state split, tabs, closed-tab undo, and shared caches are in place; full multi-window/tear-off completion remains open. |
| [linux-port.md](linux-port.md) | Partial port | The Linux shell crate compiles behind stubs; real clipboard/trash/open-with/volume/power/preview integrations remain open. |
| [mac_port.md](mac_port.md) | Shipped with follow-ups | macOS is the primary implementation path; remaining items are mostly packaging, polish, and verification. |
| [windows-port.md](windows-port.md) | Partial port | Windows shell surface has major compatibility work; shell-extension verbs, WSL integration, and some power/screenshot infrastructure remain open. |

## Responsiveness & data flow

Everything here serves the [prime directive](../ARCHITECTURE.md#prime-directive):
keep the UI off the I/O path.

- [STREAMING_ENUMERATION.md](STREAMING_ENUMERATION.md) — stream directory
  contents so large folders stay responsive.
- [LAZY_METADATA.md](LAZY_METADATA.md) — defer expensive metadata out of the
  render path.
- [MOUSE_PREDICTOR.md](MOUSE_PREDICTOR.md) — prewarm metadata ahead of hover.
- [STATUS_PROGRESS.md](STATUS_PROGRESS.md) — status bar and task progress with
  cancellation.

## File identity & metadata

- [MAGIC_SNIFFING.md](MAGIC_SNIFFING.md) — magic-byte content detection.
- [MAGIC_DESCRIPTION.md](MAGIC_DESCRIPTION.md) — the magic-first `Format`
  column, mismatch cues, and quarantine badges.
- [METADATA_DB.md](METADATA_DB.md) — SQLite-backed persistent metadata store and
  schema versioning.
- [DUPLICATES.md](DUPLICATES.md) — duplicate finder (size → partial-hash →
  full-hash stages).
- [SEARCH.md](SEARCH.md) — file search in tiers: in-directory filter, recursive
  subtree walk, and OS-index-backed global search (Spotlight / MFT / Tracker).

## Navigation & interaction

- [FAVORITES.md](FAVORITES.md) — sidebar favorites/bookmarks model, drag-and-drop,
  and acceptance checklist.
- [feraille-selection-dnd-spec.md](feraille-selection-dnd-spec.md) — node
  selection and drag-and-drop spec.
- [feraille-windows-instances-tabs-spec.md](feraille-windows-instances-tabs-spec.md)
  — windows, instances, tabs, and closed-tab undo.
- [ANT_TRAIL.md](ANT_TRAIL.md) — navigation history ("ant trail").
- [CONTEXT_MENU.md](CONTEXT_MENU.md) — context menus and native action
  delegation.

## Panels & tools

- [PREVIEW.md](PREVIEW.md) — preview pane with async text/image rendering.
- [TOOL_RESULTS.md](TOOL_RESULTS.md) — shared tab-local result surfaces for
  Search, Duplicate Finder, and docked Disk Usage.
- [VIEWER.md](VIEWER.md) — viewer window: big preview, slideshow, sticky
  zoom across entries.
- [VIDEO-MPV.md](VIDEO-MPV.md) — replacing the VLC video backend with libmpv,
  and the N-layer transparent-colour (chroma-key) compositor it enables.
- [FILE_OPS.md](FILE_OPS.md) — copy/paste/move engine: progress,
  cancellation, collision policy, clipboard verbs.
- [DISK_USAGE.md](DISK_USAGE.md) — disk-usage window: scanning, treemap, and
  top-list views.

## Look & feel

- [ICONS.md](ICONS.md) — every icon's source, attribution, and command mapping,
  plus the process for adding new ones. **Update it whenever you add, move, or
  repurpose an icon.**

## Porting & verification

- [windows-port.md](windows-port.md) — handoff notes and lessons from the
  Windows `Ferail` predecessor.
- [linux-port.md](linux-port.md) — orientation for starting a Linux port:
  freedesktop/D-Bus/XDG mapping of the shell surface, and the first change that
  makes the app compile on Linux.
- [mac_port.md](mac_port.md) — Mac-side verification checklist after the port.
- [SCREENSHOTS.md](SCREENSHOTS.md) — the headless screenshot CLI and the
  visual dev loop: render any UI state to a PNG off-screen for verification.
- [TESTING_OVERLAYS.md](TESTING_OVERLAYS.md) — debug overlays for frame time,
  task queue, and metadata visibility.
