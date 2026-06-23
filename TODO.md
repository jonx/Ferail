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
- **Notifications & undo coverage for mutations.** Success feedback is now
  intentionally quiet for immediate visible work: rename/new-folder stay silent
  on success, and task-backed copy/move/duplicate/compress only toast after the
  task surfaced. Remaining gaps:
  - **Cross-volume move undo** — `UndoOp::MoveBack` is registered only for
    same-volume moves; cross-volume moves fall back to copy-undo or none.
  - Extend actionable raw-error messages beyond the common file-op/search/dupe
    paths to every remaining mutation surface.
  - **Error-notification UX** — add a **copy** button so the user can copy the
    message (and full technical detail) to the clipboard, and an
    **expand/unfold** control to reveal the complete technical error when the
    toast text is truncated. Notifications render through the gpui-component
    `Notification` (`window.push_notification`, `Root::render_notification_layer`),
    so this needs either an extension to the pinned gpui-component fork or a
    custom notification body that carries a short summary plus the collapsible
    full error.
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
- **Hidden-file affordances.** When *show hidden* is on, render hidden entries
  dimmed/greyed (row text + icon) so they read as distinct from normal files.
  When hidden files are *off*, surface their presence passively — aggregate
  count and total size of the hidden entries in the current folder — somewhere
  unobtrusive (status bar / folder footer / Get Info) so the user knows hidden
  content exists and how much space it occupies.
- Context-menu follow-ups: compact Finder-style tag swatch row, async Open With
  prewarm if cold-cache stutter appears, and per-target enable/disable rules for
  read-only volumes, missing files, and permission-denied targets.
- Tags checkmarks over a multi-selection: the Tags submenu now reads only the
  clicked row's `tags`, but the toggle applies to the whole resolved selection.
  Make the checkmarks a true group state (✓ = applied to all targets,
  mixed-state for partial) by projecting per-target tag sets into `TargetCap`
  and reading them through `MenuTargets::all`, mirroring the bulk/anchor model
  Clear Quarantine now uses (docs/features/CONTEXT_MENU.md → Command
  availability over a group).

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
- **Permanent delete (Shift+Delete).** Add a `DeletePermanently` action that
  deletes the selection outright, bypassing Trash/Recycle Bin, **behind a
  mandatory confirmation modal** ("Permanently delete N items? This can't be
  undone."). Bind it to `shift-delete` in `keymap.rs` (Windows convention; pair
  with the platform-appropriate chord on macOS, e.g. `cmd-shift-delete`). Reuse
  the empty-trash confirmation pattern in `on_empty_trash` (`file_ops.rs`) and
  add a permanent-delete fs call alongside `move_to_trash` / `MoveToTrash`.
  There is no undo — it skips the `(original, trashed)` pairs Cmd+Z relies on —
  so the confirmation cannot be optional.
- File-ops: Windows pasteboard **volume-identity parity** (CF_HDROP copy/paste
  itself shipped — docs/features/FILE_OPS.md).
- Recents follow-ups: recently-opened **files** (needs a file-open signal — we
  only log folder visits today); optionally a dedicated recents store decoupled
  from the heat map (today Clear/Remove also clears that folder's heat).
- **Refresh folder sizes after size-changing ops.** After a mutation that
  changes a directory's contents — trash/delete, move, copy/paste, duplicate,
  compress — invalidate and recompute the affected folder-size rows instead of
  waiting for a navigation/reload (today the only trigger is `folder_sizes::start`
  from `finish_directory_load_in_tab`). The cache contract in `folder_sizes.rs`
  validates a row by the folder's *own* mtime, but a directory's mtime bumps only
  on *direct* child changes, so a delete deep in a subtree leaves a stale size;
  recompute both the directories whose contents changed and the current folder's
  own aggregate. The durable fix is the watcher/FSEvents-driven invalidation
  already flagged in `folder_sizes.rs` and under Responsiveness & Data
  Architecture.
  - This must also catch **external changes from third-party apps** — deletes,
    adds, or edits made outside Feraille (another file manager, a terminal `rm`,
    an installer). Feraille can't self-report those, so only a live filesystem
    watcher (FSEvents / `ReadDirectoryChangesW` / inotify) closes the gap; it
    should refresh the *listing* (rows appearing/disappearing) and the folder
    *sizes* together, since both go stale the same way. Pairs with the existing
    watcher items under Favorites (**Missing transitions**) and Responsiveness
    & Data Architecture (**NodeStore identity** → watcher events).

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
  mid-read). Add an explicit cloud-placeholder state before reads that may
  fault remote content in.
- Viewer follow-ups ([docs/features/VIEWER.md](docs/features/VIEWER.md)): swap
  the `qlmanage` shell-out for `QLThumbnailGenerator`; pinch-to-zoom; live
  playlist sync via the watcher (skip deleted entries); audio-file playback; a
  watchdog for eligible-but-unplayable videos stalling auto-advance; slideshow
  transitions once the animation-budget review lands. Video frame surface ships
  — follow-ups: per-frame copy on a `CVDisplayLink` background pull if 4K60
  shows cost, precise/scrubbing seek (`seekToTime:` tolerance-zero), volume
  control. Windows parity: Ctrl/F11 chords, `IShellItemImageFactory` fallback,
  Media Foundation video frame source feeding the shared `RenderImage` path.
- **mpv video backend → retire VLC** ([docs/features/VIDEO_MPV.md](docs/features/VIDEO_MPV.md)).
  Add a libmpv provider behind the existing `VideoBackend` seam (runtime
  `dlopen`, SW render into the BGRA pull buffer) that plays the same broad set
  *and* applies denoise/sharpen/deband/grain **live** via `vf set` — fixing the
  VLC limitation that forces a stream re-open on every filter change. macOS
  first. Iterations: (1) crate + playback + frame pull, (2) transport, (3) live
  grade, (4) live `vf` filters + `set_enhance` seam method + Plugins option,
  (5) **retire VLC** once mpv is verified — remove the crate/feature and delete
  the seamless-reopen / `video_pending_seek` / `video_repause` machinery.
- **Color-key transparency** (follow-on, [docs/features/VIDEO_MPV.md](docs/features/VIDEO_MPV.md)
  §Color-key). Rides the mpv backend's per-pixel alpha pass: key a chosen color
  to transparent so the video shows through. v1 = RGB-distance + tolerance
  slider. Open product fork before it starts: see-through to an in-app backdrop
  (cheap) vs. to the desktop via a transparent window (the "wow" version, ~3×).

## Metadata & Intelligence

- **Magic detection**: the table (~67 signatures + structured parsers for
  exe/zip/image/audio/video) is solid; expand the long tail and add the CLI
  modes (see CLI section).
- **Directories get a file-format label (bug).** Folders are showing a magic
  *format* in the Format/Description columns — observed as two folders rendered
  `ZIP archive · N files` under `dev-angular/Archive`. One of them
  (`sba-latest-test`) has no extension, so the label is not an extension-derived
  `display_kind`; it is a `display_magic` value applied to a directory row by the
  magic prefetch / SQLite cache (`prefetch.rs`, `e.display_magic =
  row.magic_label`) — likely a stale path-keyed cache entry or the worker
  sniffing a dir. Guard so directory rows never receive a magic label at the
  worker entry *and* at the prefetch apply; `format_label` already
  belt-and-suspenders folders in the *mismatch* check, but the label itself
  still leaks the archive kind into the column.
- **Quarantine / provenance UI**: badge halo + clear-quarantine action ship.
  Add Gatekeeper assessment, code-signature identity, and in-list provenance
  display (where-from is cached but only shown in the preview pane).
- **Ant Trail**: heat map (visit-count tint) ships; add prediction/prewarming
  and time-decay (heat is cumulative, no recency weighting today).
- **Mouse predictor** ([docs/features/MOUSE_PREDICTOR.md](docs/features/MOUSE_PREDICTOR.md)):
  pure pointer prediction module, Ant Trail blend, task-scheduler integration,
  debug overlay, and pointer-path performance tests.
- **APFS clone-aware disk-usage sizing**: the duplicate finder detects clones +
  hard links and excludes them from reclaimable bytes, but the **disk-usage
  scanner still counts every hard-link name and clone at full size** — add
  `(dev, inode)` de-dup and clone-aware sizing there.
- Disk Usage follow-ups from the feature doc: richer iCloud download-state
  handling once the existing path-prefix cloud glyph is not enough.

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
- Streaming-enumeration tests: delayed batches, cancellation, stale generation
  delivery, and partial-error delivery; surface partial enumeration errors in
  the task/notification UI instead of logging only.
- Duplicate/Disk Usage fast-walk follow-up: platform bulk enumeration
  (`getattrlistbulk`, NTFS MFT/USN, Linux `statx`/`io_uring`) after device and
  filesystem identity are modeled.

## Settings, Commands & Accessibility

- Diagnostics, activity trail & issue reporter
  ([docs/features/DIAGNOSTICS.md](docs/features/DIAGNOSTICS.md)). Phases 1-3
  shipped: the activity-trail ring buffer + hooks; `diagnostics.rs` health
  checks surfaced as a Settings → Diagnostics page and the `--doctor` CLI; and
  the issue reporter (`report.rs`) that bundles diagnostics + trail + an
  optional screenshot into a `.zip` and reveals it. Remaining follow-ups:
  (a) the **in-app redaction modal** (drag-to-black-box over the screenshot
  before bundling) — an unverifiable-headless UI, build it with visual testing;
  (b) an **OS-level window capture** so the bundle's screenshot works on a clean
  Windows build (today it uses `render_to_image`, which needs the gpui_windows
  patch and is omitted gracefully otherwise); (c) move `run_checks()` off the
  UI thread if a slow/network config dir makes the one-time probe in
  `SettingsView::new` noticeable.
- Settings "Saved" feedback pill or toast (changes persist silently today).
- **Themes & color customization** ([docs/features/THEMES.md](docs/features/THEMES.md)).
  Phase 0 shipped: a selection-accent override + Appearance color picker
  (`selection_colors`), shared by the list and grid. Remaining (scoped in the
  note): bundled themes + a theme picker (Phase 1), a drop-in user themes folder
  with hot-reload via `ThemeRegistry::watch_dir` (Phase 2), and a generalized
  accent-override layer (Phase 3).
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
- Screenshot CLI deferred flags: either implement deterministic `--splitter`,
  `--scroll`, `--ui-scale`, and `--mac-chrome` behavior or remove/warn clearly
  where the current harness cannot honor them.
- Add debug overlays for frame time, task queue, cached/missing metadata,
  layout bounds, hit regions, and injected slow I/O.

## Cross-Platform

- Windows deferred ports (windows-port.md §6b): third-party shell-extension
  context-menu verbs (`IContextMenu`) and WSL integration. The near-term
  behavior-breaking stubs (CF_HDROP clipboard, `WM_DEVICECHANGE` volume
  observer, text-naming modal) all shipped.
- Linux port ([docs/features/linux-port.md](docs/features/linux-port.md)):
  `feraille-gpui` now **builds and runs** on Linux (verified on WSL2 / Ubuntu
  24.04 under WSLg + lavapipe — launches a Wayland window, opens its XDG SQLite
  metadata DB, enumerates folders, runs prefetch + folder-sizes). Done and
  tested: volumes (`/proc/self/mountinfo` + `statvfs`), trash (freedesktop
  spec), download-provenance / MoTW (`user.xdg.origin.url`), plain-text
  clipboard (`arboard`), and Open With (freedesktop MIME + `.desktop` scan).
  Remaining shell stubs to fill with real XDG portal / freedesktop work: the
  file-URL clipboard (`text/uri-list`), file-type icons (icon-theme lookup),
  thumbnails/previews (XDG thumbnail cache), and the dark/volume/power observers
  (D-Bus / udisks2 / logind). These need a real desktop (icon themes, mounts,
  session events) to verify meaningfully — best paired with the next item.
- Linux headless screenshots: implement `render_to_image` in `gpui_wgpu`
  (offscreen render target + `copy_texture_to_buffer` readback, BGRA/RGBA) and
  wire it through both `gpui_linux` window backends (Wayland + X11), mirroring
  the `gpui_windows` D3D11 patch. Unlocks `--screenshot` on Linux so the GUI can
  be visually verified the same way as macOS/Windows.
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
