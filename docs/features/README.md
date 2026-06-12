# Feraille Feature Notes

Deeper design notes and specifications for individual features. These complement
the [architecture source of truth](../ARCHITECTURE.md) and the
[open-work list](../../TODO.md) — they capture *why* a feature is shaped the way
it is, not the current crate structure.

← Back to the [project README](../../README.md) ·
[Architecture](../ARCHITECTURE.md) · [TODO](../../TODO.md)

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
- [DISK_USAGE.md](DISK_USAGE.md) — disk-usage window: scanning, treemap, and
  top-list views.

## Porting & verification

- [windows-port.md](windows-port.md) — handoff notes and lessons from the
  Windows `Ferail` predecessor.
- [mac_port.md](mac_port.md) — Mac-side verification checklist after the port.
- [TESTING_OVERLAYS.md](TESTING_OVERLAYS.md) — debug overlays for frame time,
  task queue, and metadata visibility.
