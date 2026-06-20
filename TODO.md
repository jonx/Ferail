# Feraille TODO

← [Project README](README.md) · [Architecture](docs/ARCHITECTURE.md) ·
[Feature notes](docs/features/README.md)

This is the single list of unfinished work, grouped by area and ordered by
priority. Keep architecture and current program rules in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md); keep deep feature notes in
[docs/features/](docs/features/README.md). When an item ships, delete it here
and let git history plus release notes carry the record.

## Highest Priority — finish in-flight features

- **Favorites polish** ([docs/features/FAVORITES.md](docs/features/FAVORITES.md)).
  The v1 plus the cross-platform rename modal, Locate flow, drop-onto, add/dedup
  animations, and keyboard focus/delete/new-window have shipped. Remaining:
  - Live filesystem-watcher for **Missing transitions** on delete/move (today
    only mount/unmount re-evaluates state, via the NSWorkspace volume watch;
    file-level changes flip state only on re-hydrate). Nice-to-have per spec
    §5.3 / §11.10; deferred.
  - Tag favorites (§9) — pin a tag as a favorite. (Saved-search favorites are
    covered by **Smart Folders / Saved Searches** under High-Value Features.)
  - Tear-off / collapse **remove** animation (add fade-in + dedup pulse shipped;
    the §3.2 collapse-on-remove still pops rather than animating).
- **Notifications & undo coverage for mutations.** Today copy/move/trash show
  success toasts and register undo; the gaps:
  - Success notifications for **rename, new folder, duplicate, compress**
    (these go through `spawn_file_op`, which only surfaces *errors*).
  - Undo for **duplicate** and **compress** (no `UndoOp` today).
  - **Cross-volume move undo** — `UndoOp::MoveBack` is registered only for
    same-volume moves; cross-volume moves fall back to copy-undo or none.
  - Completion toasts for long-running **search / disk-usage / dupes** (they
    land in Recent history but push no success notification).
  - Specific handling for permission errors (today a generic failure toast).
- **Persist file-table column order** after drag-reorder. `move_column`
  reorders the live vec but never persists; widths already persist.
- **Grid marquee / rubber-band selection** — the last grid-parity gap now that
  per-cell adornments (tag dots, star, heat tint, cut dimming, tooltip) are
  painted. No list equivalent to copy; new background drag-rect gesture.

## High-Value Features — mostly wiring over subsystems we already own

Net-new, but each sits on plumbing that already exists, so the build is small
relative to the daily value. Ordered by bang-for-buck.

- **Bulk rename with regex / pattern rules.** A self-contained modal over the
  current selection: literal + regex find/replace, sequence numbering, case
  transforms, and metadata tokens (date / dimensions / counter), with a live
  before→after preview. Reuses the unified `open_text_prompt` naming modal and
  the existing bulk file-op engine — no new subsystem. Highest daily value; the
  system explorers and power tools all have it and we don't.
- **Smart Folders / Saved Searches.** Wire the reserved
  `FavoriteTarget::SavedSearch` (favorites.rs) into a real feature: pin a search
  as a favorite that re-runs live on click — Spotlight-backed where available,
  with the search-glyph icon already rendering. Mostly wiring (favorite type +
  a persistent search identity; search mode is ephemeral per tab today), not new
  architecture. Consolidates the prior "saved smart folders" notes under Search
  and Favorites — this is the canonical entry.
- **Clipboard history stack.** A bounded ring buffer of recent copies/cuts plus
  a paste-picker modal (e.g. Cmd+Shift+V) to choose an older entry. We already
  own the clipboard plumbing in `shell/file_ops.rs` and `CF_HDROP` on Win32 —
  this is a small buffer + picker on top.
- **File-level frecency → search ranking.** Extend the Ant Trail (already logging
  folder visits in SQLite, with a decay concept) to file opens, and feed
  frequency × recency × relevance into search result ordering. An extension of
  the existing table + decay model. Shares the file-open signal the Recents
  "recently-opened files" item needs, and pairs with the Ant Trail decay item
  under Metadata & Intelligence.
- **Command-palette polish** (canonical detail under Settings & Commands). The
  Cmd+K overlay in `keyboard_help.rs` already doubles as a palette; the gap is
  arrow-key navigation between matches and splitting "Commands" vs "Keyboard
  Shortcuts". Low-effort, validated as worth doing — listed here so it's not
  buried.

## File List, Sidebar & Navigation

- Audit hover, focus, and selected states across file rows, sidebar rows,
  theme tiles, tabs, breadcrumbs, and menus so active state is visually
  consistent.
- Add sidebar collapse-to-icons and narrow-window behavior; give the sidebar a
  keyboard focus region (also unblocks the Favorites arrow-key item above).
- Add "Reveal in Browse" file-list context action.
- Add Finder-style roots beyond Home and Volumes where useful: iCloud,
  Network, external disks, removable media, and user custom locations.
- Add breadcrumb completion and richer segment menus.
- Toolbar **grouping by kind / date** — a shared list+grid sort/render model
  (group headers with members beneath). Deferred from the density pass.
- Persist per-tab sort/filter/scroll state where it is not already stable.
- Add configurable visible columns and a widths/order reset.

## File Ops, Trash & Drag

- Drag follow-ups: **auto-scroll near the list edges** while dragging (needs
  `UniformListScrollHandle` offset access); **drops on favorite rows** (shared
  with the Favorites drop-onto item). Drag gestures need interactive testing —
  not headlessly drivable.
- Trash follow-ups: general **"Put Back"** for items trashed in earlier
  sessions or by Finder (needs Finder's private put-back metadata — may stay
  session-scoped); a richer **Trash browsing view** (original-location column).
  Windows Recycle Bin restore is blocked (`SHFileOperationW` doesn't report the
  recycled location).
- File-ops: Windows pasteboard **volume-identity parity** (CF_HDROP copy/paste
  itself shipped — docs/features/FILE_OPS.md).
- Recents follow-ups: recently-opened **files** (needs a file-open signal — we
  only log folder visits today); optionally a dedicated recents store decoupled
  from the heat map (today Clear/Remove also clears that folder's heat).

## Search

Base recursive/global search ships ([docs/features/SEARCH.md](docs/features/SEARCH.md))
with live streaming result updates and selectable engine (Spotlight + walker
fallback). Remaining is the UX the system explorers have and we don't:

- Filter chips (kind / date / size), query operators, and glob/regex queries.
- Saved smart folders — see **Smart Folders / Saved Searches** under High-Value
  Features (needs a persistent search identity; search mode is ephemeral today).
- Windows NTFS MFT + USN and Linux Tracker/Baloo engines behind the same
  `SearchEngine` selection.

## Preview, Get Info & Viewer

- Get Info follow-ups (editable inspector + detachable per-item window ship):
  **inline rename inside the popup** (name is read-only there; F2 still
  renames); **undo coverage** for attribute/permission/tag edits; combined
  **multi-item Get Info**; real Windows/Linux gather (unix `stat_info` already
  yields perms/dates; NSURL/volume-format reads are macOS-only).
- Preview-pane providers (Quick Look image/PDF/media, inline text + markdown,
  and scroll-chaining all ship): audio **waveform / video thumbnail strip**
  beyond the QL poster, **archive/package summaries**, and per-provider
  cancellation tokens (today stale results are dropped at apply, not cancelled
  mid-read).
- Viewer follow-ups ([docs/features/VIEWER.md](docs/features/VIEWER.md)): swap
  the `qlmanage` shell-out for `QLThumbnailGenerator`; pinch-to-zoom; live
  playlist sync via the watcher (skip deleted entries); audio-file playback; a
  watchdog for eligible-but-unplayable videos stalling auto-advance; slideshow
  transitions once the animation-budget review lands. Video frame surface ships
  — follow-ups: per-frame copy on a `CVDisplayLink` background pull if 4K60
  shows cost, precise/scrubbing seek (`seekToTime:` tolerance-zero), volume
  control. Windows parity: Ctrl/F11 chords, `IShellItemImageFactory` fallback,
  Media Foundation video frame source feeding the shared `RenderImage` path.

## Metadata & Intelligence

- **Magic detection**: the table (~67 signatures + structured parsers for
  exe/zip/image/audio/video) is solid; expand the long tail and add the CLI
  modes (see CLI section).
- **Quarantine / provenance UI**: badge halo + clear-quarantine action ship.
  Add Gatekeeper assessment, code-signature identity, and in-list provenance
  display (where-from is cached but only shown in the preview pane).
- **Ant Trail**: heat map (visit-count tint) ships; add prediction/prewarming
  and time-decay (heat is cumulative, no recency weighting today).
- **APFS clone-aware disk-usage sizing**: the duplicate finder detects clones +
  hard links and excludes them from reclaimable bytes, but the **disk-usage
  scanner still counts every hard-link name and clone at full size** — add
  `(dev, inode)` de-dup and clone-aware sizing there.

## Responsiveness & Data Architecture

- Finish the stable **NodeStore identity** model for rename, move, mount
  changes, Ant Trail, selection, watcher events, and metadata cache keys.
- Add **cancellation tokens** consistently for enumeration, preview, thumbnails,
  disk usage, search, copy/move, and duplicate finding (most register tasks
  now, but several still drop stale results at apply rather than cancelling).
- Move remaining expensive metadata reads off synchronous UI paths (preview
  generation, large-folder bookkeeping).
- Audit render paths for accidental `PathBuf` resolution or filesystem calls;
  keep resolution behind the filesystem / native-shell boundaries.
- Add slow-path tests or fixtures for slow folders, network volumes, cloud
  placeholders, permission failures, and stale worker results.

## Settings, Commands & Accessibility

- Settings "Saved" feedback pill or toast (changes persist silently today).
- Accent-color customization once the theme token path is ready.
- Command palette: arrow-key selection between matches (today Enter runs the top
  match), and a distinct "Commands" vs "Keyboard Shortcuts" mode if the dual
  role confuses.
- User-overridable key bindings (installed from the catalogue today, no UI).
- Ensure every icon-only button has a tooltip with shortcut, every truncated
  string has a tooltip, and menu shortcuts render via `Kbd`.
- Keyboard accessibility: tab order, focus rings, arrow navigation, Escape
  behavior, and Settings-from-anywhere.
- Accessibility announcements for file operations and long-running tasks.
- IME / composition support for text input and rename flows.

## CLI & Automation

- Extend `feraille magic` with `--json`, `--csv`, `--recursive`,
  `--mismatch-only`, and `--limit` (today: paths in, tab-separated label out).
- Extend `feraille du` with structured output and filters (today: `--top`,
  `--packages`); reach parity with the Disk Usage window's largest-file model.
- Add useful non-GUI commands for automation: metadata reset, duplicate
  finding, cache inspection, command-catalogue listing.
- Add a plugin or scripting story only after the command and permission model
  is explicit.

## Packaging & Polish

- Rework the app icon to macOS conventions and generate the iconset (the bundle
  script already builds `.icns` from a PNG source; the icon *art* is the gap).
- Add code signing / notarization flow for sharing builds outside the dev
  machine (the bundle does ad-hoc / Developer-ID `codesign` today).
- Visual polish still missing from the GPUI shell: vibrancy/materials, titlebar
  hit testing, sharper row density, empty/error illustrations, animation-budget
  review.
- Rebuild deterministic screenshot fixtures for the shell, settings pages, disk
  usage, task popover/panel, errors, empty folders, and narrow layouts.
- Add debug overlays for frame time, task queue, cached/missing metadata,
  layout bounds, hit regions, and injected slow I/O.

## Cross-Platform

- Windows deferred ports (windows-port.md §6b): third-party shell-extension
  context-menu verbs (`IContextMenu`) and WSL integration. The near-term
  behavior-breaking stubs (CF_HDROP clipboard, `WM_DEVICECHANGE` volume
  observer, text-naming modal) all shipped.
- Windows power follow-ups ([docs/features/POWER.md](docs/features/POWER.md)):
  display on/off events (`PBT_POWERSETTINGCHANGE` +
  `RegisterPowerSettingNotification` for `GUID_CONSOLE_DISPLAY_STATE`), and
  switching the idle-sleep guard from per-thread `SetThreadExecutionState` to
  the process-wide Power Request API if a transfer ever asserts from a
  thread-pool worker.
- Publish the gpui fork carrying the `gpui_windows::render_to_image` patch and
  point the `[patch]` block at `git = "<fork-url>", rev = "..."` so the Windows
  screenshot harness builds identically to macOS (today a local-path override).

## Cleanup

- Keep `cargo clippy --workspace --all-targets` at zero warnings. `multi_table/`
  carries a module-level `#![allow]` for style lints by policy (pinned
  gpui-component fork); don't extend those allows elsewhere.
- Remove stale references to old specs or deleted migration ledgers as code and
  docs settle.
