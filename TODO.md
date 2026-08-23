# Ferail TODO

← [Project README](README.md) · [Architecture](docs/ARCHITECTURE.md) ·
[Feature notes](docs/features/README.md)

This is the single list of unfinished work, grouped by area and ordered by
priority. Keep architecture and current program rules in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md); keep deep feature notes in
[docs/features/](docs/features/README.md). When an item ships, delete it here
and let git history plus release notes carry the record.

## Highest Priority — finish in-flight features

- **Windows reliability and compatibility campaign.** The 0.6.5 tester report
  is tracked item by item in
  [docs/features/WINDOWS_COMPATIBILITY_PLAN.md](docs/features/WINDOWS_COMPATIBILITY_PLAN.md):
  first make crashes diagnosable and isolate third-party preview handlers,
  then bound preview/file-detail work to the viewport, repair Explorer open /
  reveal / clipboard behavior, add the native Windows context menu strictly on
  demand (no selection prefetch), and introduce Shell namespace locations
  without touching the ordinary filesystem fast path. Windows-specific work is
  accepted only after the exact
  [Windows reliability test plan](docs/testing/WINDOWS_RELIABILITY_TEST_PLAN.md)
  passes interactively on a real Windows machine, including its 4,194,304-row
  regression gate.
- **Notifications & undo coverage for mutations.** Success feedback is now
  intentionally quiet for immediate visible work: rename/new-folder stay silent
  on success, and task-backed copy/move/duplicate/compress only toast after the
  task surfaced.
  - ✅ **Cross-volume move undo** — shipped on main (`b2c5ca3`): `UndoOp::MoveBack`
    now covers cross-volume moves.
  - ✅ **Actionable raw-error messages** — shipped on main (`b2c5ca3`): structured
    failure reports across the mutation surfaces.
  - ✅ **Error-notification UX** — shipped: `shell::error_notification` shows a
    one-line headline with **Show details** (full message, scrollable) and
    **Copy** (whole message to the clipboard), and keeps the toast from
    auto-hiding; the structured failure reports route through it. 2026-08-21:
    the nine remaining dynamic-text error toasts (trash/dedup/save/move
    failures, disk-full) were moved onto it too; the plain `Notification::error`
    sites left are short fixed strings where expand/copy adds nothing.
- ✅ **Persist file-table column order** — shipped on main (`c0f1de2`): column
  order AND widths now persist across launches.

## High-Value Features — mostly wiring over subsystems we already own

Net-new, but each sits on plumbing that already exists, so the build is small
relative to the daily value. Ordered by bang-for-buck.

- ✅ **Bulk rename with regex / pattern rules** — shipped on main (`689406d`):
  self-contained modal over the selection with literal + regex find/replace,
  sequence numbering, case transforms, and a live before→after preview
  (docs/features/BULK_RENAME.md).
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

- ✅ **Flat view — one recursive list with a Path column**
  ([docs/features/FLAT_VIEW.md](docs/features/FLAT_VIEW.md)) — shipped as an
  uncapped, cancellable files-only snapshot on the existing virtualized list.
  It uses an explicit match-all walker, scan-local identities and a compact
  per-directory path arena, so toggling it off releases every recursive path;
  no Flat path enters `NodeStore` or `NativeFs`'s lifetime maps. Streaming
  application is O(batch), filtering is snapshot-local, Refresh rescans, and
  whole-list magic/folder-size workers stay off. The shared `FileEntry` payload
  is down from 264 to 160 bytes, empty Flat decorations are no longer allocated,
  names/directories/repeated display values share storage, and visible rows
  retain viewport-scoped Format/Description/quarantine enrichment. Select All
  is symbolic (`all except exceptions`), status/painting remain bounded, and
  Copy File List yields between path batches. Remaining million-plus polish:
  build sort/filter indexes off-thread, add segmented scroll geometry, and a
  page-backed spill path for result sets larger than RAM. A dedicated sub-100
  byte Flat row is deferred unless later measurements justify its complexity.
- Finish the hover/focus/selected-state consistency audit. First pass shipped:
  the icon grid gained its missing hover wash (`table_hover`) and an
  out-of-range sort-icon `opacity(7.)` was fixed. Remaining is the larger
  unification — the app carries a bespoke `ferail_design` token set that is
  dead for color while every surface pulls ad-hoc from the gpui-component theme
  plus `selection_colors`, giving five different hover treatments and three
  selection systems. Wire one semantic token layer (or standardize every
  surface onto the existing `*_hover` / `*_active` / `ring` tokens) so tabs,
  breadcrumbs, rows, grid, and sidebar read as one system.
- Add sidebar collapse-to-icons and narrow-window behavior; give the sidebar a
  keyboard focus region (also unblocks the Favorites arrow-key item above).
- Add "Reveal in Browse" file-list context action.
- More Finder-style sidebar roots. Shipped: an **iCloud Drive** Location
  (surfaced only when the ubiquity container exists) and a distinct
  `network.svg` glyph for **network mounts** (`is_local == false`).
  Removable/external disks already carry the eject affordance, and arbitrary
  user locations are covered by Favorites. Remaining if wanted: a dedicated
  Network browse root and splitting Volumes into Internal / External / Network
  sections.
- Breadcrumb completion (Cmd+L path edit) and a per-segment **Go to Subfolder**
  menu have shipped; richer completion (inline segment-mode filtering) is the
  remaining polish.
- Toolbar **grouping by kind / date** — a shared list+grid sort/render model
  (group headers with members beneath). Deferred from the density pass.
- Persist per-tab sort/filter/scroll state where it is not already stable.
- ✅ **Hidden-file affordances** — shipped: with *show hidden* on, hidden rows
  (list + grid) render dimmed via the cut-row opacity treatment (cut wins over
  hidden); with it off, the status bar shows a passive "N hidden · X B" chip
  next to the Show-hidden toggle, fed by a `HiddenSummary` the enumeration's
  `Done` message carries (counted before the text filter, so typing a filter
  doesn't perturb it). Optional follow-up: mirror the summary into folder Get
  Info via the Calculate walk.
- **Open With — custom tools, “Other…”, and multi-selection**
  ([docs/features/OPEN_WITH.md](docs/features/OPEN_WITH.md)). System handler
  enumeration already ships on all three platforms behind the warm off-thread
  cache. The gaps: the submenu is `SingleOnly` even though `open_with_slot`
  already resolves the whole selection and calls `open_with_app_many`; an empty
  candidate set hides the submenu entirely (an unknown extension gets *zero*
  LaunchServices handlers — exactly when a specific tool is most wanted);
  there is no “Other…” chooser (`gpui::PathPromptOptions` has no
  type filter and a macOS `.app` is a directory, so this needs a platform
  `NSOpenPanel` entry point); and no user-defined tools. The tool model should
  be a sibling of `ferail_core::terminal::TerminalSpec` — program +
  pre-split argv tokens + per-token placeholder substitution, argv never a
  shell string — persisted as a shareable `tools.json` rather than in the
  recreate-on-version-mismatch metadata DB, matched only against cached
  `FileEntry` fields, and dispatched through closure-backed menu items (the
  twelve `OpenWithSlot` actions don't extend). Phase 1 is the three cheap gaps;
  custom tools follow.
- **User-customizable context menus — hide the entries you never use**
  ([docs/features/CONTEXT_MENU.md](docs/features/CONTEXT_MENU.md#customizing-which-entries-appear-planned)).
  The table header already does exactly this for columns (✓/blank closure
  items, persisted, with Reset), and `split_persisted_columns` supplies the
  storage rules to copy: unknown keys ignored, unmentioned entries default to
  visible, never let the set go empty. **Blocked on one prerequisite**: the
  menus are imperative chains (`menu.menu(tr!("Rename…"), Box::new(…))`, ~40
  row entries + 8 background), so an entry's identity is its Rust action type
  — there is no action↔`CommandId` bridge and the labels are duplicated
  against the catalogue. Build each menu from a
  `(CommandId, Availability, label, action)` table instead; that also
  de-duplicates the labels, makes the menus introspectable for the Cmd+K
  palette / Shortcuts page, and is the same refactor user-defined tools need
  ([OPEN_WITH.md](docs/features/OPEN_WITH.md) §5.6) — do it once for both.
  Then: the editor picks a **surface** first (row, background, header,
  breadcrumb, favorites, locations, recents, tree, treemap — visibility is per
  `(surface, command)`); **separators collapse** in one post-pass over the
  built item list (drop leading, collapse runs, drop trailing) rather than
  per-`if` bookkeeping; and **menu-open cost must not rise** — parse the spec
  once into memory (never a `load()` per entry, never I/O at menu-open time)
  and resolve each entry by array index, which is cheaper than the `tr_raw`
  every entry already pays for its label. Preference ANDs with `Availability`,
  never overrides it.
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

- Drag follow-ups: **auto-scroll near the list edges** while dragging and
  **drops on favorite rows** have shipped. Remaining drag work needs
  interactive testing — not headlessly drivable.
- Trash follow-ups: general **"Put Back"** for items trashed in earlier
  sessions or by Finder (needs Finder's private put-back metadata — may stay
  session-scoped); a richer **Trash browsing view** (original-location column).
  Windows Recycle Bin restore is blocked (`SHFileOperationW` doesn't report the
  recycled location).
- Permanent-delete follow-up: `on_delete_immediately` reports failures as raw
  `format!("{}: {e}")` strings instead of the structured
  `FileOpError`/`file_op_failure_report` path that `on_move_to_trash` and
  `on_empty_trash` use — align it so permanent-delete failures get the same
  per-item classified report + coping actions. (The feature itself shipped as
  `DeleteImmediately` — mandatory confirmation, Shift+Delete /
  Option+Cmd+Delete, elevated retry — docs/features/FILE_OPS.md.)
- File-ops: Windows pasteboard **volume-identity parity** (CF_HDROP copy/paste
  itself shipped — docs/features/FILE_OPS.md).
- Recents follow-ups: recently-opened **files** (needs a file-open signal — we
  only log folder visits today); optionally a dedicated recents store decoupled
  from the heat map (today Clear/Remove also clears that folder's heat).
- **Refresh folder sizes after size-changing ops.** ✅ *In-app half shipped on
  main* via [FRESHNESS.md](docs/features/FRESHNESS.md): subtree-derived caches now
  validate by mtime + TTL, invalidate the ancestor chain on in-app mutations, and
  force a size refresh when the window returns to the foreground (see also the
  **Cache freshness follow-ups** item under Responsiveness & Data Architecture).
  Historical context: after a mutation that changes a directory's contents —
  trash/delete, move, copy/paste, duplicate, compress — the affected folder-size
  rows must recompute instead of waiting for a navigation/reload; the old cache
  contract in `folder_sizes.rs` validated a row by the folder's *own* mtime, but a
  directory's mtime bumps only on *direct* child changes, so a delete deep in a
  subtree left a stale size.
  - This must also catch **external changes from third-party apps** — deletes,
    adds, or edits made outside Ferail (another file manager, a terminal `rm`,
    an installer). Ferail can't self-report those, so only a live filesystem
    watcher (FSEvents / `ReadDirectoryChangesW` / inotify) closes the gap; it
    should refresh the *listing* (rows appearing/disappearing) and the folder
    *sizes* together, since both go stale the same way. Pairs with the existing
    watcher items under Favorites (**Missing transitions**) and Responsiveness
    & Data Architecture (**NodeStore identity** → watcher events).
- **Write per-directory subtotals through the folder-size walk (drill-down
  reuse).** Today `recursive_totals` sums a subtree into one grand total and
  keeps no per-directory breakdown, so sizing `Downloads` walks every descendant
  but caches only the top-level rows; navigating *into* a subfolder then re-walks
  it from scratch. Make the walk cache a `folder_sizes` row (size + counts) for
  **every** directory it descends, keyed by each subdir's own path+mtime, so any
  later drill-down into a just-sized tree is a pure cache hit. Requires turning
  the current pre-order stack sum into a post-order accumulation (child totals
  bubble up to parents) and multiplies DB writes from a handful per listing to
  hundreds/thousands per top-level folder — batch them in one transaction and
  weigh the write amplification against the drill-down win before committing.
  Inherits the same deep-mtime staleness bound as the existing cache (a subdir
  row is validated by its *own* mtime, so a deep external edit hides until TTL).
  The counts columns and the shared cache contract are already in place (shipped
  with the folder Description counts), so this is purely a walker change. Only
  worth doing if drill-down re-walks prove noticeable on a slow disk.

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
  scroll-chaining, and now lofty-backed audio tags + cover art all ship — see
  [docs/features/MEDIA-TAGS.md](docs/features/MEDIA-TAGS.md)): audio
  **waveform / video thumbnail strip** beyond the QL poster,
  **archive/package summaries**, and per-provider cancellation tokens (today
  stale results are dropped at apply, not cancelled mid-read). Add an explicit
  cloud-placeholder state before reads that may fault remote content in.
  - **Audio waveform** — a SoundCloud-style peak view styled to the app's look
    (theme tokens, house stroke), shown in the preview stage for audio. lofty
    reads tags but not samples, so decode peak buckets off-thread with
    `symphonia`, cache them like previews, and paint bars through the existing
    preview-cache/staleness machinery. Design note in
    [MEDIA-TAGS.md](docs/features/MEDIA-TAGS.md#deferred-waveform-preview).
- Viewer follow-ups ([docs/features/VIEWER.md](docs/features/VIEWER.md)): swap
  the `qlmanage` shell-out for `QLThumbnailGenerator`; pinch-to-zoom; live
  playlist sync via the watcher (skip deleted entries); **audio-file playback**
  ships ([MEDIA-TAGS.md](docs/features/MEDIA-TAGS.md#in-viewer-audio-playback) —
  cover on the stage + play/pause/mute/loop/seek, autoplay unmuted, via the
  active backend; native mute now real (`AVPlayer setMuted:` /
  `IMFMediaEngine::SetMuted`); follow-up: an audible-output pass on a real run);
  a watchdog for eligible-but-unplayable videos stalling auto-advance; slideshow
  transitions once the animation-budget review lands. Video frame surface ships
  — follow-ups: per-frame copy on a `CVDisplayLink` background pull if 4K60
  shows cost, precise/scrubbing seek (`seekToTime:` tolerance-zero), volume
  control. Windows parity: Ctrl/F11 chords, `IShellItemImageFactory` fallback,
  Media Foundation video frame source feeding the shared `RenderImage` path.
- Similar Images comparison extension: turn the group-scoped viewer into an
  A/B workspace — pin one reference, compare the current candidate beside it
  with synchronized zoom/pan, then add optional opacity overlay, draggable
  wipe, and press-and-hold flicker. Keep side-by-side as the safe default for
  cropped or shifted images; evaluate automatic alignment separately. All
  comparison paths and decoded pixels must remain window-scoped and ephemeral.
- ✅ **mpv video backend → retire VLC** — shipped on main
  ([docs/features/VIDEO-MPV.md](docs/features/VIDEO-MPV.md)): libmpv provider behind
  the `VideoBackend` seam (runtime load, SW render into the BGRA pull buffer) with
  live `vf set` denoise/sharpen/deband/grain; VLC crate deleted (`3b1abc5`) and the
  seamless-reopen machinery removed (`c8b117f`). **Windows parity pending (this
  port):** confirm native Media Foundation video (`video_mf.rs`) still integrates
  after the viewer refactor, and the optional mpv plugin loads via `LoadLibraryW`.
- ✅ **Color-key transparency** — shipped on main
  ([docs/features/VIDEO-MPV.md](docs/features/VIDEO-MPV.md)): single-layer chroma
  key + eyedropper (`9fb9ff7`), N-layer compositing (`f9cbdc8`), and see-through
  transparent windows (`750eb6c`/`afeb437`). **Windows parity pending (this
  port):** transparent windows are the highest-risk Windows-specific gap
  (DWM/layered vs NSWindow) — top Phase 1 investigation.

## Metadata & Intelligence

- **Magic detection**: the table (~67 signatures + structured parsers for
  exe/zip/image/audio/video) is solid; expand the long tail and add the CLI
  modes (see CLI section).
- ✅ **Directories get a file-format label (bug)** — fixed: directory rows are
  guarded at all three layers (the worker derive skips `is_dir` seeds, magic
  sniffing refuses directories at `detect_magic_info`'s entry, and the prefetch
  apply never writes a magic label/description onto a `Directory` row), and the
  `MAGIC_REVISION` bump nulls already-poisoned path-keyed cache rows at next
  launch.
- **Quarantine / provenance UI**: badge halo + clear-quarantine action ship.
  Add Gatekeeper assessment, code-signature identity, and in-list provenance
  display (where-from is cached but only shown in the preview pane).
- **Ant Trail**: heat map (visit-count tint) ships; add prediction/prewarming
  and time-decay (heat is cumulative, no recency weighting today).
- **Mouse predictor** ([docs/features/MOUSE_PREDICTOR.md](docs/features/MOUSE_PREDICTOR.md)):
  pure pointer prediction module, Ant Trail blend, task-scheduler integration,
  debug overlay, and pointer-path performance tests.
- **APFS clone-aware disk-usage sizing**: hard-link `(dev, inode)` de-dup and
  filesystem-boundary handling (firmlink-aware `du -x` semantics) now ship in
  the scanner; **APFS clones still count at full size** (clones share extents
  without `nlink > 1`, so detecting them needs per-file clone-id queries —
  weigh the extra syscall per file before adding it).
- Disk Usage follow-ups from the feature doc: richer iCloud download-state
  handling once the existing path-prefix cloud glyph is not enough.

## Responsiveness & Data Architecture

- Finish the stable **NodeStore identity** model for rename, move, mount
  changes, Ant Trail, selection, watcher events, and metadata cache keys.
- **NodeId intern-map lifecycle**: `NativeFs`'s `NodeId ↔ PathBuf` maps (and
  `NodeStore`'s) are add-only — a disk-usage scan or duplicate sweep of a
  multi-million-file volume permanently pins one `PathBuf` per file for the
  process lifetime (GB-scale RSS surviving window close). A safe fix needs an
  id *lifecycle*, not an eviction hack: scan-minted ids interleave with ids
  live tabs/selections/history hold (the path-keyed map returns the same id to
  both), so range- or ownership-based forgetting can misdirect a later
  trash/rename through a stale `path_for`. Design: either refcount ids per
  holding surface, or give tool results (DU, dupes, search) a per-scan arena
  id namespace that drops with the surface, keeping the global map for
  navigation identity only.
- **Cache freshness follow-ups** ([docs/features/FRESHNESS.md](docs/features/FRESHNESS.md)).
  Subtree-derived caches now stay honest via mtime + TTL validity, exact
  ancestor invalidation on in-app mutations, and a forced size refresh when the
  window returns to the foreground. Remaining: invalidate **both** parents'
  ancestor chains on a cross-directory move (`spawn_file_op` reloads a single
  `reload_path` today); and reuse the same model for the next recursive
  aggregates (item counts, APFS clone-aware sizing) rather than a parallel one.
- Add **cancellation tokens** consistently for enumeration, preview, thumbnails,
  disk usage, search, copy/move, and duplicate finding (most register tasks
  now, but several still drop stale results at apply rather than cancelling).
- Move remaining expensive metadata reads off synchronous UI paths (preview
  generation, large-folder bookkeeping).
- **Prime Directive — known remaining UI-thread I/O** (from the 2026-07 audit;
  the enforcement layers — `path_guard::assert_off_ui_thread`, the
  `disallowed-methods` clippy deny in ferail-gpui — are live, these are the
  surviving violations):
  - ✅ `Shell::new` start-path validation (2026-08-21): the window now boots on
    the raw persisted last-dir and `resolve_start_path_then_load` does the
    `is_dir` + canonicalize on the background executor, then loads.
  - ✅ Grid / sidebar folder-icon warms (2026-08-21): `warm_path_icons_async`
    fetches on the background executor (`fetch_icon_rgba` made thread-safe
    by drawing a copy of the shared NSImage); `IconCache` tracks in-flight
    keys so the per-frame collectors converge.
  - ✅ `app_state::save()` — was already coalesced onto a writer thread
    (stale bullet; `app_state.rs` module docs describe it).
  - NSPasteboard reads/writes in copy/cut/paste handlers run on the main
    thread. Fast (no per-path stat — handlers pre-collect cached `is_dir`),
    listed for strict-compliance completeness only.
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

- **Localization follow-ups** (docs/features/LOCALIZATION.md — the catalog,
  packs, Settings UI and the export → translate → import flow ship):
  - Translate backend error text (`ferail-fs-native`, `ferail-archive`) and
    the failure-report bodies, once bug reports can carry the English
    alongside.
  - Locale-aware numbers, sizes and dates.
  - RTL mirroring (blocked on gpui layout support).
  - Contribute Ferail's languages to gpui-component's own `ui.yml` so the
    widgets' OK/Cancel follow too.
  - Optional in-app translation provider on top of the same file format
    (deliberately left out of v1 to avoid API-key handling).

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

- Extend `ferail magic` with `--json`, `--csv`, `--recursive`,
  `--mismatch-only`, and `--limit` (today: paths in, tab-separated label out).
- Extend `ferail du` with structured output and filters (today: `--top`,
  `--packages`); reach parity with the Disk Usage window's largest-file model.
- Add useful non-GUI commands for automation: metadata reset, duplicate
  finding, cache inspection, command-catalogue listing.
- Add a plugin or scripting story only after the command and permission model
  is explicit.

## Packaging & Polish

- Rework the app icon to macOS conventions and generate the iconset (the bundle
  script already builds `.icns` from a PNG source; the icon *art* is the gap).
- Bundle an **LGPL** libmpv inside the `.app` so mpv playback works out of the
  box. ✅ *Step 1 shipped:* every release build (DMG, Windows ZIP, .deb) now
  compiles `--features mpv` in — the provider dlopens a user-installed libmpv
  and falls back to the native player without one, and the .deb `Recommends:
  libmpv2` (Debian 12+ / Ubuntu 23.04+; jammy only has libmpv1, whose
  soname the loader doesn't probe). *Remaining — ship the library itself.*
  Homebrew's libmpv chain is **GPL-3.0** — its ffmpeg is built with x264/x265
  — so it cannot ship inside an MIT/Apache DMG without making the whole binary
  GPL. The viable path: build ffmpeg `--disable-gpl --disable-nonfree`
  (decoders/demuxers only — the *encoders* are GPL; the H.264/HEVC/AV1/VP9
  **decoders** are LGPL, and decoding is all the viewer needs) as **static
  libs linked into libmpv** built with its LGPL option, yielding a single
  self-contained `libmpv.dylib` — one file to place in `Contents/Frameworks/`
  and sign, instead of relocating Homebrew's ~47-dylib closure with
  `install_name_tool`. Sign it *before* the outer bundle (`bundle-mac.sh`
  signs only the app today, so notarization would fail), probe the bundle
  ahead of Homebrew in `default_mpv_path()` (the resolver already probes
  `Contents/Frameworks/libmpv.2.dylib` under a hint dir), and add the LGPL
  notices plus a corresponding-source offer — pin the ffmpeg/mpv sources +
  build script in-repo, an ongoing obligation on every rebuild. **Verify
  early:** the viewer's live vf chain must survive an LGPL ffmpeg — ffmpeg's
  `eq` filter is GPL-gated, so the grade path may need `colorlevels`/`hue`
  there; test the exact chain with the headless probe
  (`cargo run -p ferail-video-mpv --example probe`). Windows: same recipe →
  `libmpv-2.dll` beside the exe in the ZIP. Linux: nothing to bundle
  (distro libmpv via Recommends).
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

- **Filename display-convention parity.** macOS landed: a name's on-disk `:`
  shows as `/` and a typed `/` stores `:`, matching Finder, via
  `ferail_fs_native::paths::{display_leaf,on_disk_leaf}` (the seam for
  per-platform name presentation; see ARCHITECTURE.md "Raw name vs. display
  name"). Remaining per-platform quirks to consider on the same seam, none
  implemented yet:
  - ✅ **Windows name validation** — shipped: `paths::validate_leaf` rejects
    reserved device names (`CON`/`PRN`/`AUX`/`NUL`/`COM1`–`COM9`/`LPT1`–`LPT9`,
    with or without an extension), reserved characters (`<>:"|?*` + separators +
    controls), and trailing dot/space, with a user-facing message. Wired into
    the shared New Folder / rename modal (`open_named_prompt`), which keeps the
    dialog open on rejection so the name can be fixed; the favorite-*label*
    rename opts out (not a filesystem name). Identity/no-op off Windows.
  - macOS: HFS NFD normalization is cosmetic and renders fine today; revisit
    only if a normalization-sensitive comparison surfaces.
  - Optional: an informational (not red-hazard) note in Get Info when a name
    contains a `/`-shown-as-`:`, so the on-disk reality is discoverable.
- Windows deferred ports (windows-port.md §6b): third-party shell-extension
  context-menu verbs (`IContextMenu`) and WSL integration. The near-term
  behavior-breaking stubs (CF_HDROP clipboard, `WM_DEVICECHANGE` volume
  observer, text-naming modal) all shipped.
- **Windows release-readiness follow-ups** (from the 2026-07-31 pass —
  windows-port.md §2.2; the build fix, screenshot fallback, clippy and
  packaging all shipped there):
  - **Restore a build gate for Windows.** ✅ Done 2026-08-07:
    `.github/workflows/ci.yml` restores the `6d85def` workflow in full — the
    repo went public the same day, so all three platform legs
    (windows/ubuntu/macos over the shell crates + ferail-fs-native + meta +
    archive) and the windows ferail-gpui clippy job run on every push.
    Docs-only pushes skip CI; superseded runs are cancelled.
  - **Port window docking to Windows** (docs/features/DOCK.md). ✅ The sibling
    half of this shipped: the viewer's **Stay on Top** was dead on Windows
    because `content_ns_view` only matched `RawWindowHandle::AppKit`; it now
    also matches `RawWindowHandle::Win32` and `set_window_floating` is a real
    `SetWindowPos(HWND_TOPMOST/HWND_NOTOPMOST)`. The dock itself is **not**
    stub-filling: `dock.rs` computes frames in macOS **global screen space**
    (origin bottom-left, y-up), so a Windows arm has to map that onto Win32's
    top-left, y-down monitor rects — `MonitorFromWindow`/`GetMonitorInfoW` for
    `screen_visible_frame_for_window`, `GetCursorPos` for
    `current_mouse_location`, `SetWindowPos` for the frame — and a docked
    drawer (edge-slam reveal, auto-hide) cannot be verified headlessly, so it
    needs interactive testing on a real desktop. `Shell::window_ns_view` must
    stay AppKit-only until then: returning `Some(hwnd)` would let `set_dock`
    run against no-op primitives and show a docked state for a window that
    never moved. No dead UI in the meantime — the toolbar control is already
    `cfg!(target_os = "macos")`-gated and the actions are unbound elsewhere.
  - **Recycle Bin sidebar row.** macOS has a Trash location; Windows has none.
    It is a shell *virtual* folder (`FOLDERID_RecycleBinFolder`, CLSID
    `{645FF040-…}`), not a filesystem path, so it needs shell-namespace browsing
    rather than a `well_known_locations_for` entry — navigating to the raw
    `C:\$Recycle.Bin\<SID>` would show the `$R*`/`$I*` internals, which is worse
    than nothing. Pairs with the existing Trash-browsing-view item under File
    Ops.
  - **~50 px empty band under the title bar** that macOS does not have (compare
    `screenshots/win-baseline.png` against `docs/images/tour-shell.png`).
    Probably `TitleBar::title_bar_options()` reserving the macOS traffic-light
    strip on top of the Windows caption area — windows-port.md §5 predicted
    exactly this. Cosmetic, but it is the first thing a Windows user sees.
  - **Settings offers a "Spotlight" search engine on Windows**, where
    `resolve_spotlight` is macOS-only so it can never engage. Either hide the
    option off macOS or implement a real indexed backend (Windows Search
    `ISearchQueryHelper`, or the NTFS MFT+USN engine already listed under
    Search).
  - **Obtain an Authenticode certificate.** `scripts/package-win.ps1` wires
    signing end to end but a stock run is unsigned, and SmartScreen warns on
    every download of an unsigned binary. Reputation accrues *to the
    certificate*, so signing from the first public release matters more than
    signing later. This is the Windows analogue of the Developer ID +
    notarization line under Packaging & Polish.
- **Resilient file-op coping — Windows-native primitives.** ✅ *Shipped on
  `windows-parity`* (`ferail-shell-win32/src/elevation.rs`): `run_elevated_self`
  via `ShellExecuteExW` verb `"runas"` (UAC) + wait-for-exit powers **Retry as
  administrator**; `processes_using` via the **Restart Manager**
  (`RmStartSession`/`RmRegisterResources`/`RmGetList`) names the locking process;
  `force_close_processes` via `RmShutdown` (+ `TerminateProcess` fallback) powers
  the **"What's using it?" → "Close & retry"** toast flow. `elevation_available()`
  / `lock_diagnostics_available()` return true on Windows; verified end-to-end
  against a real exclusive lock. **Linux follow-up still open:** `pkexec` re-exec
  for `run_elevated_self` + `/proc/*/fd` scan for `processes_using`.
- Linux port ([docs/features/linux-port.md](docs/features/linux-port.md)):
  `ferail-gpui` now **builds and runs** on Linux (verified on WSL2 / Ubuntu
  24.04 under WSLg + lavapipe — launches a Wayland window, opens its XDG SQLite
  metadata DB, enumerates folders, runs prefetch + folder-sizes). Done and
  tested: volumes (`/proc/self/mountinfo` + `statvfs`), trash (freedesktop
  spec), download-provenance / MoTW (`user.xdg.origin.url`), plain-text
  clipboard (`arboard`), and Open With (freedesktop MIME + `.desktop` scan).
  Also done: **file-type icons** (`fetch_icon_rgba` — shared-mime-info MIME
  detection → freedesktop icon-theme lookup → PNG/SVG rasterization via
  `image`/`resvg`; cached per-kind, off the render path). Verified in WSL2 by
  dumping real theme glyphs to PNG (a document glyph for text, a distinct
  folder glyph) and by a smoke test; type-specific glyphs resolve where the
  theme ships them (WSL2's stripped Adwaita falls to the generic, same as
  Nautilus there). Also done: **thumbnails** (`fetch_quick_look_thumbnail` —
  shared freedesktop cache keyed by `md5(file-uri)`, `gdk-pixbuf-thumbnailer`
  generation on miss/stale; images in v1, video/PDF via totem/evince/Tumbler as
  a follow-up). Remaining shell stubs to fill with real XDG portal / freedesktop
  work: the file-URL clipboard (`text/uri-list`), and the dark/volume/power
  observers (D-Bus / udisks2 / logind). These need a real desktop (mounts,
  session events) to verify meaningfully — best paired with the next item.
  **Ubuntu/Debian packaging works (2026-08-08):**
  `resources/linux/ferail.desktop`, cargo-deb metadata
  (`[package.metadata.deb]` in ferail-gpui's Cargo.toml), and
  `WindowOptions::app_id = "ferail"` on every window
  (`base_window_options()`). Validated in an arm64 Ubuntu 24.04 QEMU VM:
  builds, installs, `ferail doctor` clean, GUI launches under Xvfb with
  `WM_CLASS = ferail` (details + build-host gotchas in
  [docs/features/linux-port.md](docs/features/linux-port.md) § packaging).
  The **amd64 + arm64** builds are CI's job: `.github/workflows/deb.yml`
  builds each in an ubuntu:22.04 container (pins the glibc floor), installs
  the result, smoke-tests `ferail doctor`, and on `v*` tags attaches both
  packages to the tag's GitHub release (manual dispatch leaves them as run
  artifacts). Remaining: taskbar-identity check on a real desktop session,
  and later `Exec=ferail %U` + `MimeType=inode/directory;` once the binary
  accepts a directory argument (today a bare path exits as an unknown
  subcommand).
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

## Open-Source Release

The repo is prepared for a **source-first** public release (dual MIT/Apache;
README, CONTRIBUTING, SECURITY, THIRD-PARTY-NOTICES in place; private checkout
paths scrubbed). Source-first is unblocked. Remaining:

- **`cargo-deny` for license / advisory drift.** No `deny.toml` today. Add one
  so a future `gpui` rev bump that changes the transitive license surface is
  caught mechanically (see the GPL note below).

### Before distributing a prebuilt binary

Publishing *source* is unaffected by the transitive GPL chain; a redistributable
*binary* is not. Do these only when building a download:

- ✅ **Sever the GPL-3.0 dependency edge** — shipped in 0.2.2 via a vendored
  `sum_tree` fork; **superseded 2026-08-08** by
  [`vendor/ztracing`](vendor/ztracing/README.md), a clean-room MIT/Apache
  no-op stub patched over the zed source, after gpui gained a *direct*
  `ztracing` dependency (zed 00cba838a) that the sum_tree fork could no longer
  sever. The stub also keeps GPL `zlog`/`ztracing_macro` out and needs no
  re-sync on gpui bumps. `cargo tree -p ferail-gpui -i ztracing` must print
  only the vendor path crate; `-i zlog` must print nothing.
  - ⚠️ **Lesson worth keeping: the lockfile lied.** Before this, `Cargo.lock`
    had been generated on a machine with the AROS `[patch]` active, and the
    `../zed-aros` fork it pointed at happens to drop `ztracing` — so the
    committed lock contained no `ztracing`, and THIRD-PARTY-NOTICES.md
    confidently (and wrongly) recorded the edge as already fixed upstream. It
    never was: upstream `sum_tree` at the pinned rev still carries
    `ztracing.workspace = true`. **Do not audit the licence surface from the
    lockfile alone** — resolve the graph.
  - Still worth doing eventually: one **published** zed fork referenced by
    `git =` URL, carrying both this severance and the
    `gpui_windows::render_to_image` patch (see the item at the end of
    Cross-Platform), which would retire both `vendor/sum-tree` and the local
    screenshot patch. Upstream fix tracked at
    <https://github.com/zed-industries/zed/issues/55470> (acknowledged but stuck
    in legal — do **not** assume it lands on a timeline).
  Context in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
- ✅ macOS code signing / notarization / `.dmg` — **shipped in 0.2.2** via
  `scripts/package-mac.sh` (Developer ID + hardened runtime, notarytool
  profile `D4Mac`, stapled; the released dmg passes
  `spctl --assess` as "Notarized Developer ID"). Still open: a **macOS
  release job in CI** — needs the signing cert (.p12) and a notary API key
  as encrypted repo secrets; until then a mac release is one local
  `scripts/package-mac.sh` run + `gh release upload`. Linux .debs already
  release from CI on `v*` tags (`.github/workflows/deb.yml`); Windows zip
  remains a local `scripts/package-win.ps1` run (and unsigned — see the
  Authenticode item under Cross-Platform).

## Cleanup

- Keep `cargo clippy --workspace --all-targets` at zero warnings. `multi_table/`
  carries a module-level `#![allow]` for style lints by policy (pinned
  gpui-component fork); don't extend those allows elsewhere.
- Remove stale references to old specs or deleted migration ledgers as code and
  docs settle.
