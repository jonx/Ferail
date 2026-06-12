# Feraille TODO

← [Project README](README.md) · [Architecture](docs/ARCHITECTURE.md) ·
[Feature notes](docs/features/README.md)

This is the single list of unfinished work. Keep architecture and current
program rules in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md); keep deep
feature notes in [docs/features/](docs/features/README.md).

## Highest Priority

- Favorites polish — the v1 shipped per [docs/features/FAVORITES.md](docs/features/FAVORITES.md);
  remaining items are inline-rename text field (today's rename uses a
  native NSAlert prompt), live filesystem-watcher integration for
  Missing state transitions (mount/unmount transitions now ship via
  the NSWorkspace volume watch), Locate-via-NSOpenPanel, ~150ms fade/collapse +
  dedup-pulse animations, arrow-key sidebar focus + Delete-on-focus
  removal + Cmd+Option+click new-window modifier, drop-file-onto-
  favorite move/copy (waiting on the file-op workers below), and the
  full acceptance-checklist sweep with screenshots.
- Add Recents as a first-class sidebar section, fed by Ant Trail or a
  dedicated recent-open list, with clear/remove actions.
- Persist file-table column order after drag reorder, alongside column
  widths.
- Finish Trash as a first-class model: restore/original location, empty
  trash, per-volume trash awareness, confirmations for destructive bulk
  operations, and undo where possible.
- File-ops follow-ups (docs/features/FILE_OPS.md — Cmd+C/V/Option+V
  with progress, cancel, and collision dialog shipped 2026-06-13):
  per-item collision resolution, Windows pasteboard (CF_HDROP) and
  volume-identity parity, cross-volume move undo, and a decision on
  cut semantics.
- Drag-into-app follow-ups (file list folder rows, pane background, and
  sidebar tree rows shipped 2026-06-13 — docs/features/FILE_OPS.md):
  drops on tabs, breadcrumb segments, and favorites; auto-scroll and
  auto-expand during drag; Cmd+Option alias-drop.
- Complete toolbar density: refresh, new folder, sort, group, view mode,
  action overflow, and discoverable tooltips/shortcuts.
- Complete status/task feedback: selected count and size, total visible
  size, free space, active task count, cancel buttons, recent task history,
  and task registration for magic, thumbnails, enumeration, copy/move, and
  trash.
- Add notification and undo coverage for mutations: trash, rename, new
  folder, copy/move, permission errors, and long-running completions.

## Responsiveness And Data Architecture

- Extend notification-driven worker plumbing to remaining long-running
  flows that still poll for progress, especially disk usage scanning,
  preview generation, search, copy/move, and duplicate finding.
- Move all remaining expensive metadata reads out of synchronous UI paths,
  including preview generation and any large-folder bookkeeping
  (context-menu warming and Finder-tag reads moved off-thread in the
  2026-06 sweep).
- Audit render paths for accidental `PathBuf` resolution or filesystem
  calls; keep path resolution behind controlled filesystem/native-shell
  boundaries.
- Finish the stable NodeStore identity model for rename, move, mount
  changes, Ant Trail, selection, watcher events, and metadata cache keys.
- Add cancellation tokens consistently for enumeration, preview, thumbnails,
  disk usage, search, copy/move, and duplicate finding.
- Add slow-path tests or fixtures for slow folders, network volumes, cloud
  placeholders, permission failures, and stale worker results.

## File List, Sidebar, And Navigation

- Audit hover, focus, and selected states across file rows, sidebar rows,
  theme tiles, tabs, breadcrumbs, and menus so active state is visually
  consistent.
- Add sidebar collapse-to-icons and narrow-window behavior.
- Add "Reveal in Browse" and "Remove from Favorites" context actions.
- Add Finder-style roots beyond Home and Volumes where useful: iCloud,
  Network, external disks, removable media, and user custom locations.
- Add breadcrumb completion and richer segment menus.
- Persist per-tab sort/filter/scroll state where it is not already stable.
- Add configurable visible columns and column widths/order reset.

## Preview, Metadata, And Intelligence

- Add real previews: text, image, PDF, Quick Look, audio/video thumbnails,
  all async and cancellable. (The viewer window now covers big image
  viewing — docs/features/VIEWER.md; this bullet is the preview *pane*
  provider system.)
- Viewer follow-ups (docs/features/VIEWER.md): swap the qlmanage
  shell-out for `QLThumbnailGenerator`, pinch-to-zoom gesture mapping,
  live playlist sync via the watcher (skip deleted entries), Windows
  parity (Ctrl/F11 chords, `IShellItemImageFactory` fallback, Media
  Foundation video overlay), audio-file playback, a watchdog for
  eligible-but-unplayable videos stalling slideshow auto-advance, and
  slideshow transitions once the animation budget review lands.
- Expand magic detection beyond the small high-confidence table; add
  recursive/mismatch-only CLI modes and structured output.
- Improve quarantine/provenance UI: Gatekeeper assessment, code-signature
  identity, clear-quarantine action, and better selected-row badge halo.
- Finish Ant Trail prediction/prewarming and decay.
- Add duplicate finder with size, partial-hash, and full-hash stages.
- Add file-name hazard surfacing for leading/trailing whitespace,
  zero-width/control characters, bidi overrides, and confusing combining
  sequences.
- Add APFS clone-aware disk-usage sizing.

## Settings, Commands, And Accessibility

- Add the Settings "Saved" feedback pill or toast.
- Add accent-color customization once the theme token path is ready.
- Add command palette UI over the command catalogue.
- Add user-overridable key bindings.
- Ensure every icon-only button has a tooltip with shortcut, every
  truncated string has a tooltip, and menu shortcuts render via `Kbd`.
- Finish keyboard accessibility: tab order, focus rings, arrow navigation,
  Escape behavior, and Settings from anywhere.
- Add accessibility announcements for file operations and long-running
  tasks.
- Add IME/composition support for text input and rename flows.

## CLI And Automation

- Extend `feraille magic` with `--json`, `--csv`, `--recursive`,
  `--mismatch-only`, and `--limit`.
- Extend `feraille du` with structured output, filters, and parity with
  the Disk Usage window's largest-file model.
- Add useful non-GUI commands for future automation: metadata reset,
  duplicate finding, cache inspection, and command catalogue listing.
- Add a plugin or scripting story only after the command and permission
  model is explicit.

## Testing, Packaging, And Polish

- Rebuild deterministic screenshot fixtures for the active GPUI shell,
  settings pages, disk usage, task popover, errors, empty folders, and
  narrow layouts.
- Add debug overlays for frame time, task queue, cached/missing metadata,
  layout bounds, hit regions, and injected slow I/O.
- Ship as a real `.app` bundle with `Info.plist`, bundle identifier,
  `icns` icon, file associations, and stable TCC identity.
- Rework the app icon to macOS conventions and generate an iconset.
- Add code signing / notarization flow when sharing builds outside the
  development machine.
- Add visual polish still missing from the GPUI shell: vibrancy/materials,
  titlebar hit testing, sharper row density, empty/error illustrations, and
  animation budget review.

## Cross-Platform Build

- Decide how to share the local-zed `gpui_windows::render_to_image` patch
  across machines. Today the override only exists on the Windows dev box
  (the `[patch]` block in `Cargo.toml` pointed at `../zed/` which doesn't
  exist on Mac — removed in `af39f0a`). The Windows screenshot harness
  needs the patch; Mac doesn't. Two viable shapes, pick one:
  - **A. Per-machine `.cargo/config.toml`** with the local-path `[patch]`,
    gitignored. Each dev maintains their own; Mac falls through to upstream.
    Lowest friction, no committed record of the patch shape.
  - **B. Publish the local zed branch as a GitHub fork** and rewrite the
    `[patch]` block to point at `git = "<fork-url>", rev = "..."`. Works
    identically on both platforms; requires pushing each iteration before
    Feraille sees it. Better long-term once the gpui_windows change is
    stable enough to upstream-PR.

## Cleanup

- Keep `cargo clippy --workspace --all-targets` at zero warnings (it
  is, as of 2026-06-12). `multi_table/` carries a module-level
  `#![allow]` for style lints by policy — it's the pinned
  gpui-component fork; don't extend those allows elsewhere.
- Delete old soft-renderer crates when the GPUI shell is the only shipped
  app and no longer needs them as parity reference.
- Remove stale references to old specs or deleted migration ledgers as code
  and docs settle.
- Keep this file pruned: when an item ships, delete it here and let git
  history plus release notes carry the record.
