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
- Recents follow-ups (sidebar section fed by the Ant Trail visit log,
  with remove/clear, shipped 2026-06-13): recently-opened *files*
  (needs a file-open signal — we only log folder visits today), and a
  dedicated recents store if we ever want it decoupled from the heat
  map (today Clear/Remove also clears that folder's heat, by design).
- Persist file-table column order after drag reorder, alongside column
  widths.
- Trash follow-ups (trash-undo via captured resulting URLs, Empty Trash
  with counted confirmation, and per-volume `.Trashes/<uid>` coverage
  shipped 2026-06-13): general "Put Back" for items trashed in earlier
  sessions or by Finder (needs Finder's private put-back metadata — may
  stay session-scoped by design), Windows Recycle Bin restore
  (`SHFileOperationW` doesn't report the recycled location), and a
  richer Trash browsing view (original-location column).
- File-ops follow-ups (docs/features/FILE_OPS.md — Cmd+C/V/Option+V with
  progress/cancel/collision shipped 2026-06-13; per-item collision
  resolution, Cut/Cmd+X with move-on-paste + dimmed rows shipped
  2026-06-18): Windows pasteboard (CF_HDROP) and volume-identity parity,
  and cross-volume move undo.
- Drag follow-ups (file list folder rows, pane background, sidebar tree
  rows shipped 2026-06-13; drops on tabs + breadcrumb segments,
  Cmd+Option alias-drop, drag cursor chip + spring-load for file/tree
  rows shipped 2026-06-18 — docs/features/FILE_OPS.md): auto-scroll near
  the list edges while dragging (needs `UniformListScrollHandle` offset
  access); drops on favorite *rows* (gaps accept folder-adds today). All
  drag gestures need interactive testing — not headlessly drivable.
- Toolbar density follow-ups (refresh, new folder, sort dropdown, and
  action-overflow menu shipped 2026-06-13): grouping by kind/date (a new
  sort/render model), deferred from the density pass on purpose.
- Grid/icon view parity with the list (interaction parity shipped
  2026-06-20: OS drag-out with ghost, drop-onto-folder + spring-load,
  right-click context menu via the shared TableState delegate, Finder-
  style blue selection — pill behind the label + wash/border — sharing
  `drop_onto_folder_row`/`spring_load_hover` with the list). Remaining:
  marquee/rubber-band selection (no list equivalent to copy); and the
  per-cell adornments the list row paints that the grid cell still does
  not — tag dots, favorite star, Ant Trail heat tint, cut-item dimming,
  and the truncated-name tooltip.
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
- Search **shipped** ([docs/features/SEARCH.md](docs/features/SEARCH.md)):
  Enter in the filter box runs a recursive/global search of the current
  folder and below, streaming results into the tab (engine selectable in
  Settings — Spotlight when available with the built-in walker as fallback).
  What ships is a single query box, so the real follow-ups are the UX the
  system explorers already have and we don't: filter chips (kind / date /
  size), query operators, **saved smart folders** (pinned, live-updating —
  cheap when Spotlight-backed), and live result updates. Also: glob/regex
  queries, and the Windows NTFS MFT + USN and Linux Tracker/Baloo engines with
  their ports (behind the same `SearchEngine` selection).
- Persist per-tab sort/filter/scroll state where it is not already stable.
- Add configurable visible columns and column widths/order reset.

## Preview, Metadata, And Intelligence

- Get Info popup follow-ups (the editable inspector shipped 2026-06-16 —
  `crate::entry_info`, neutral `feraille_core::entry_info` model, gather via
  `stat_info` + `resource_values`; edits cover Locked/Invisible/Hide-extension,
  color labels, POSIX permission grid, recursive "Calculate" size): inline
  rename inside the popup (name is read-only there today; F2/RenameSelected
  still renames); undo coverage for attribute/permission/tag edits (rename
  would reuse `UndoOp::Rename`); Stationery-pad and custom-icon reads
  (Finder-info `getattrlist`, no NSURL key); combined multi-item Get Info and
  a detachable per-item window; real Windows/Linux gather (the unix `stat_info`
  arm already yields perms/dates; NSURL/volume-format reads are macOS-only).
- Preview-pane provider follow-ups (image/PDF/media via Quick Look,
  inline text with syntax highlighting + formatted markdown all
  shipped — docs/features/PREVIEW.md): audio waveform / video
  thumbnail strip beyond the QL poster, archive/package summaries, and
  per-provider cancellation tokens (today stale results are dropped at
  apply, not cancelled mid-read).
- Preview scroll chaining. The preview pane has a nested scroll: the
  inline text/code box (bounded `max_h` + `overflow_scroll`) sits inside
  the pane's own vertical scroll (`preview_scroll`). The box is bounded on
  purpose so a long file doesn't push the Get Info details far down the
  pane. (Post the 2026-06-16 gpui-component bump the wheel now drives the
  inner box and the outer pane at the same time, rather than the older
  "trapped until you move off" behavior — either way it's not the goal.)
  The ideal is scroll-chaining: scroll the inner box, and at its top/bottom
  boundary forward the remaining delta to the outer pane. gpui's
  `overflow_scroll` auto-captures the wheel, so this needs a custom
  `on_scroll_wheel` (via `cx.listener`) that reads the inner `ScrollHandle`
  `offset()`/`max_offset()`, decides boundary vs. not, and drives the outer
  `preview_scroll` with `set_offset` (clamped) + `cx.notify` when at the
  edge — being careful not to double-scroll with the built-in handler.
  Not headlessly testable (needs real wheel/trackpad events), so it wants
  hands-on iteration. We tried collapsing to one big scroll instead, but
  then a big file buries the details, so we reverted to the bounded box.
- Viewer follow-ups (docs/features/VIEWER.md): swap the qlmanage
  shell-out for `QLThumbnailGenerator`, pinch-to-zoom gesture mapping,
  live playlist sync via the watcher (skip deleted entries), Windows
  parity (Ctrl/F11 chords, `IShellItemImageFactory` fallback, Media
  Foundation video frame source feeding the shared `RenderImage` path),
  audio-file playback, a watchdog for eligible-but-unplayable videos
  stalling slideshow auto-advance, and slideshow transitions once the
  animation budget review lands.
- Viewer video as a gpui frame surface (shipped 2026-06-18): replaced the
  native `AVPlayerView` overlay with a windowless `AVPlayer` +
  `AVPlayerItemVideoOutput` frame pull, so video draws through the same
  `stage::layout`/`img` path as stills — zoom/pan/fit/rotation and the
  gpui transport all "just work," and the video↔video black flash is gone
  (the poster shows until the first frame decodes). Earlier transport
  polish (2026-06-17): play/pause, frame-step, loop, seek bar,
  stay-on-top. Follow-ups: the per-frame copy runs on the main thread —
  a `CVDisplayLink` background pull if 4K60 shows cost; precise/scrubbing
  seek (tolerance-zero `seekToTime:`); volume control.
- Expand magic detection beyond the small high-confidence table; add
  recursive/mismatch-only CLI modes and structured output.
- Improve quarantine/provenance UI: Gatekeeper assessment, code-signature
  identity, clear-quarantine action, and better selected-row badge halo.
- Finish Ant Trail prediction/prewarming and decay.
- Duplicate finder **shipped**
  ([docs/features/DUPLICATES.md](docs/features/DUPLICATES.md)): Find
  Duplicates (Cmd+Shift+U / menu / palette) runs the size → xxh3 partial →
  BLAKE3 full funnel off the UI thread, cache-backed by the `files` table so
  rescans skip full hashing, and streams grouped results into the tab with a
  reclaimable-bytes summary; hard links are flagged. Follow-ups: a dedicated
  grouped panel with group-level actions (keep-newest, select-all-but-one) —
  the "Results view: Dedicated panel" setting currently falls back to grouped
  rows; APFS clone detection + `clonefile`-based zero-copy dedup remediation
  (only hard links are detected today).
- Add APFS clone-aware disk-usage sizing.

## Settings, Commands, And Accessibility

- Add the Settings "Saved" feedback pill or toast.
- Add accent-color customization once the theme token path is ready.
- Command palette follow-ups (the Cmd+K shortcuts overlay is now a
  working palette — filter, top match highlighted, Enter runs it,
  click any; shipped 2026-06-13): arrow-key selection between matches
  (today Enter runs the top match or you refine the filter), and a
  distinct "Commands" vs "Keyboard Shortcuts" title/mode if the dual
  role gets confusing.
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

- Windows parity with the predecessor `../Ferail` is tracked in
  [docs/features/windows-port.md](docs/features/windows-port.md) §6b
  (capability diff). Behavior-breaking stubs in `feraille-shell-win32`:
  file-URL clipboard (`CF_HDROP` — Cmd+C/Cmd+V copy-paste) and the
  volume device-change observer (`WM_DEVICECHANGE`) **shipped
  2026-06-20**; the remaining one is inline-rename text input (best as
  a gpui modal — also tracked under Favorites polish above, since the
  same modal serves both). Larger, deferred Windows-only ports: third-party
  shell-extension context-menu verbs (`IContextMenu`) and WSL
  integration.
- The Windows screenshot harness needs a `gpui_windows::render_to_image`
  patch that isn't upstream yet. Publish the gpui fork carrying it and point
  the `[patch]` block at `git = "<fork-url>", rev = "..."` so both platforms
  build identically, instead of depending on a local-path override. Best done
  once the `gpui_windows` change is stable enough to send as an upstream PR.

## Cleanup

- Keep `cargo clippy --workspace --all-targets` at zero warnings (it
  is, as of 2026-06-12). `multi_table/` carries a module-level
  `#![allow]` for style lints by policy — it's the pinned
  gpui-component fork; don't extend those allows elsewhere.
- Remove stale references to old specs or deleted migration ledgers as code
  and docs settle.
- Keep this file pruned: when an item ships, delete it here and let git
  history plus release notes carry the record.
