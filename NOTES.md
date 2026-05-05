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

- Directory enumeration is still eager in places and must become streaming and
  cancellable.
- Icon fetching is cached but still too close to navigation.
- Preview pane is metadata-only.
- Context menu is a hardcoded slice, not final NSMenu/services behavior.
- Trash is a fallback, not final `NSWorkspace` trash.
- NodeStore identity model is not fully ported.
- Status progress/task aggregation is not implemented.
- Persistent Ant Trail, metadata DB, disk usage, duplicate finder, and full
  preview providers are pending.

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
