# Ferail Feature Notes

← [Documentation map](../README.md) · [Project README](../../README.md) ·
[Architecture](../ARCHITECTURE.md) · [Status](../STATUS.md) ·
[Open work](../../TODO.md)

One note per feature: what it is, how it is built, and why it is bounded the
way it is. These describe the finished design, not the current position.
Whether a feature ships, ships with follow-ups, or is a design note only is
recorded once, in [STATUS.md](../STATUS.md); what is left to do is in
[TODO.md](../../TODO.md).

Every note in this directory is listed below.

## Responsiveness and data flow

Everything here serves the [prime directive](../ARCHITECTURE.md#prime-directive):
keep the UI off the I/O path.

- [STREAMING_ENUMERATION.md](STREAMING_ENUMERATION.md): stream directory
  contents so large folders stay responsive.
- [LAZY_METADATA.md](LAZY_METADATA.md): defer expensive metadata out of the
  render path.
- [FRESHNESS.md](FRESHNESS.md): keep subtree-derived caches (folder sizes, Get
  Info size) fresh through mtime + TTL, ancestor invalidation, and an
  activation refresh, without a recursive watcher.
- [MOUSE_PREDICTOR.md](MOUSE_PREDICTOR.md): prewarm metadata ahead of hover.
- [STATUS_PROGRESS.md](STATUS_PROGRESS.md): status bar and task progress with
  cancellation.
- [POWER.md](POWER.md): sleep, wake, and keeping a long transfer alive.
- [NON_NEGOTIABLE.md](NON_NEGOTIABLE.md): the reusable method for making a
  project rule unbreakable for coding agents, with the prime directive as the
  worked example.

## File identity and metadata

- [MAGIC_SNIFFING.md](MAGIC_SNIFFING.md): magic-byte content detection.
- [MAGIC_DESCRIPTION.md](MAGIC_DESCRIPTION.md): the magic-first `Format`
  column, mismatch cues, and quarantine badges.
- [MEDIA-TAGS.md](MEDIA-TAGS.md): audio tags, properties, and cover art, and
  where they surface.
- [METADATA_DB.md](METADATA_DB.md): SQLite-backed persistent metadata store and
  schema versioning.
- [DUPLICATES.md](DUPLICATES.md): duplicate finder (size → partial-hash →
  full-hash stages).
- [CHECKSUMS.md](CHECKSUMS.md): streaming SHA-256 generation and optional
  comparison with a checksum copied from a trusted source.
- [SIDECARS.md](SIDECARS.md): NFO/DIZ recognition and preview, safe
  multi-file SFV/checksum verification and generation, and release-folder
  awareness.
- [SEARCH.md](SEARCH.md): file search in tiers: in-directory filter, recursive
  subtree walk, and OS-index-backed global search (Spotlight / MFT / Tracker).
- [FLAT_VIEW.md](FLAT_VIEW.md): an uncapped recursive list with a relative
  Path column, surface-local storage, progress and cancellation.

## Navigation and interaction

- [FAVORITES.md](FAVORITES.md): sidebar favorites/bookmarks model,
  drag-and-drop, and acceptance checklist.
- [SIDEBAR_LAYOUT.md](SIDEBAR_LAYOUT.md): persistent section order,
  disclosure, and the three densities.
- [ferail-selection-dnd-spec.md](ferail-selection-dnd-spec.md): node
  selection and drag-and-drop spec.
- [ferail-windows-instances-tabs-spec.md](ferail-windows-instances-tabs-spec.md):
  windows, instances, tabs, and closed-tab undo.
- [ANT_TRAIL.md](ANT_TRAIL.md): navigation history ("ant trail").
- [CONTEXT_MENU.md](CONTEXT_MENU.md): the menu plan per surface, native action
  delegation, and the editor that lets a user hide, reorder and separate
  entries.
- [OPEN_WITH.md](OPEN_WITH.md): the Open With submenu: how each OS registers
  file-type handlers, what ships, and a *study* for user-defined custom tools.
- [DOCK.md](DOCK.md): parking the window against a screen edge as an
  auto-hiding drawer.

## Panels and tools

- [PREVIEW.md](PREVIEW.md): preview pane with async text/image rendering.
- [TOOL_RESULTS.md](TOOL_RESULTS.md): shared tab-local result surfaces for
  Search, Duplicate Finder, and docked Disk Usage.
- [TARGET_PANEL.md](TARGET_PANEL.md): "Pick as Target": a pinned, frozen
  second listing used as a source or destination for file operations, and the
  batched-transfer queue it makes possible.
- [VIEWER.md](VIEWER.md): viewer window: big preview, slideshow, sticky
  zoom across entries.
- [VIDEO-MPV.md](VIDEO-MPV.md): the libmpv video backend, live grading, and
  the transparent-colour (chroma-key) stacking the window server composites.
- [FILE_OPS.md](FILE_OPS.md): copy/paste/move engine: progress,
  cancellation, collision policy, clipboard verbs, and what the Trash can do.
- [ARCHIVES.md](ARCHIVES.md): browsing inside an archive without extracting,
  the capability matrix per format, and ZIP editing as a reviewable
  transaction.
- [BULK_RENAME.md](BULK_RENAME.md): pattern-rule bulk rename modal with
  live before→after preview and batch undo.
- [TEXT_EDITOR.md](TEXT_EDITOR.md): built-in lightweight text editor:
  open, fix, save, close, with safe in-place saves and refusal states.
- [IMAGE_EDITOR.md](IMAGE_EDITOR.md): built-in image redaction/annotation
  editor: rectangle + brush, save-copy or confirmed overwrite.
- [DISK_USAGE.md](DISK_USAGE.md): disk-usage window: scanning, treemap, and
  top-list views.
- [SYSTEM_STATS.md](SYSTEM_STATS.md): the CPU and redraw figures in the status
  bar, and why they read the way the platform's own task manager reads.

## Look, feel and text

- [ICONS.md](ICONS.md): every icon's source, attribution, and command mapping,
  plus the process for adding new ones. **Update it whenever you add, move, or
  repurpose an icon.**
- [THEMES.md](THEMES.md): the accent override that ships and the theming layer
  planned on top of it.
- [LOCALIZATION.md](LOCALIZATION.md): English-as-key strings, the bundled
  language packs, and the translate-anywhere workflow.
- [PRIVATE_MODE.md](PRIVATE_MODE.md): capture-safe presentation and a
  process-global interaction lock for screenshots of real sessions.

## Privacy, health and updates

- [DIAGNOSTICS.md](DIAGNOSTICS.md): the diagnostic bundle and what it
  deliberately leaves out.
- [FREEZE_DIAGNOSTICS.md](FREEZE_DIAGNOSTICS.md): freeze reports, and the
  shutdown watchdog that names what outlived a quit.
- [UPDATES.md](UPDATES.md): the opt-in update check and the Windows
  Install-and-Restart path.

## Porting and verification

- [WINDOWS_COMPATIBILITY_PLAN.md](WINDOWS_COMPATIBILITY_PLAN.md): the tracked
  Windows reliability and compatibility campaign, with a per-item issue ledger
  and Windows-only acceptance gates.
- [WINDOWS_FAST_NTFS.md](WINDOWS_FAST_NTFS.md): the Windows-only
  implementation and validation contract for Fast NTFS Disk Usage.
- [windows-port.md](windows-port.md): handoff notes and lessons from the
  Windows `Ferail-Win32` predecessor.
- [linux-port.md](linux-port.md): orientation for the Linux port:
  freedesktop/D-Bus/XDG mapping of the shell surface.
- [mac_port.md](mac_port.md): Mac-side verification checklist after the port.
- [aros-port.md](aros-port.md): the AROS research port and its GPUI platform
  backend.
- [aros-building.md](aros-building.md): the AROS cross-build and how to run
  the result.
- [SCREENSHOTS.md](SCREENSHOTS.md): the headless screenshot CLI and the
  visual dev loop: render any UI state to a PNG off-screen for verification.
- [WINDOWS_HEADLESS_SCREENSHOTS.md](WINDOWS_HEADLESS_SCREENSHOTS.md): running
  that CLI without a desktop session on Windows.
- [TESTING_OVERLAYS.md](TESTING_OVERLAYS.md): debug overlays for frame time,
  task queue, and metadata visibility.

The Windows acceptance matrix and the operational handover live one directory
across, in [docs/testing/](../testing/WINDOWS_RELIABILITY_TEST_PLAN.md).
