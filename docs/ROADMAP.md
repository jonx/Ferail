# Roadmap

This roadmap is feature-driven, but every stage is constrained by
[UI_NONBLOCKING.md](UI_NONBLOCKING.md).

## Stage 0: Keep The UI Alive

Status: in progress.

- Done: remove synchronous magic sniffing from navigation.
- Todo: move icon fetching out of navigation.
- Todo: make directory enumeration streaming and cancellable.
- Todo: add worker result generation ids consistently.
- Todo: add status/progress presentation for long tasks.

## Stage 1: Finder-Grade Table Stakes

- Inline rename over the row name cell.
- Robust new-folder collision handling.
- Copy/cut/paste files using NSPasteboard.
- Proper `NSWorkspace` trash.
- Native NSMenu context menu with Open, Open With, Reveal, Get Info, Copy Path,
  Move to Trash, duplicate, rename, and services where appropriate.
- Drop target support from Finder/other apps into current folder and tree nodes.
- Finder favorites/iCloud/network roots.

## Stage 2: Scale Architecture

- NodeStore with stable identity and cached display metadata.
- Streaming directory enumeration with cancellation.
- File watching via FSEvents.
- Persistent tab/session state.
- Background metadata DB.
- Debug overlays for frame time, task queue, cached/missing metadata.

## Stage 3: Ferail Intelligence

- Persistent Ant Trail heat.
- Ant Trail driven prefetch scheduler.
- Full magic table and persistent magic cache.
- Preview service for text, image, PDF/Quick Look, audio/video thumbnails.
- Disk usage scanner and treemap view.
- Duplicate finder with size, partial hash, full hash pipeline.

## Stage 4: Polish And Power

- Configurable columns and widths.
- Keyboard focus map and command palette.
- Per-tab sort/filter persistence.
- Accessibility announcements.
- Mac visual polish: vibrancy, correct titlebar hit testing, sidebar material.
- GPU renderer only if profiling says the soft renderer is the bottleneck.

## Deferred Or Not Applicable

- WSL support from Ferail is not a Mac v1 feature.
- Windows shell extension parity is not relevant to the Mac app, but the
  high-level "system context menu must feel native" requirement remains.
- Direct2D/GDI docs do not port directly; keep their architecture lessons only.
