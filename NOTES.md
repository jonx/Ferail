# Feraille — Architecture and Decision Log

Multi-iter spec work under the Slow AI method. Currently covers two specs:

- `docs/features/feraille-selection-dnd-spec.md` (selection iter 1+2 landed; drag still pending) — below.
- `docs/features/feraille-windows-instances-tabs-spec.md` (in progress) — top of file.

---

# 2026-06-12 video playback in the viewer/slideshow (landed)

AVPlayerView overlay
[mac] over the stage rect — native aspect-fit, hardware decode,
audio, inline hover controls. Key wrinkle: the objc2 0.2-generation
framework crates we pin predate AVFoundation bindings (start at objc2
0.6), so `video_overlay.rs` reaches AVPlayer/AVPlayerView through
runtime `AnyClass::get` + `msg_send`, with two `#[link]` blocks to
load the frameworks. Eligible: mp4/m4v/mov; auto-play on becoming
current; slideshow does NOT arm the interval timer on video entries —
`AVPlayerItemDidPlayToEndTimeNotification` advances instead, path-
tagged through a channel so an end queued behind a manual nav is
dropped. Overlay lifecycle is render-time change-detected sync (same
trick as title sync); `Drop` is the teardown backstop. Known v1
limits in VIEWER.md (no zoom on video, fullscreen hover chrome sits
under the overlay, screenshots can't capture it). Smoke-tested
through the headless harness with an ffmpeg test clip — full AVKit
path runs clean; interactive playback check is on the user.

# 2026-06-12 viewer window: big preview, slideshow, sticky zoom (landed)

Spec: `docs/features/VIEWER.md`. Six iterations, all green
(`cargo clippy --workspace` zero, full test suite, screenshots
`screenshots/viewer-window.png` / `viewer-preview-pane.png`).

- **New module `feraille-gpui/src/viewer/`** in four layers: `loader`
  (full-res decode + byte-budget LRU), `stage` (pure zoom/pan
  geometry, zero gpui types), `window` (the entity), `playback`
  (slideshow epoch state). 27 new unit tests across the pure layers.
- **Two-tier decode.** `image` crate (now with jpeg/gif/webp/bmp/tiff
  features) decodes raster formats off-thread, longest edge capped at
  8192 px; everything else (HEIC, PDF, video) falls back to a 2048 px
  Quick Look thumbnail [mac]. Cache budget 384 MB, LRU by bytes,
  Pending markers dedup in-flight decodes and are never evicted.
- **Sticky zoom is window state, not image state.** `StageState
  {mode, center-as-image-fraction}` survives navigation verbatim —
  zoom 2.5× into the top-right corner and every next image shows its
  top-right corner at 2.5×. Pan center being *relative* is what makes
  it transfer between differently-sized images (unit-tested).
- **One reusable window.** `ProcessState.viewer_window` holds
  `(WindowHandle, WeakEntity)`; reopening retargets + activates
  instead of stacking. Stale weak handle after close = next open
  builds fresh. No Drop bookkeeping needed.
- **Slideshow with epoch staleness.** Timer ticks carry an epoch;
  play/pause/manual-nav/interval-change bumps it, so a stale tick is
  inert — same idiom as enumeration cancel flags. Manual nav while
  playing re-arms; zoom/pan *pauses* (inspecting beats advancing).
  Interval cycles 2/3/5/10 s via toolbar button (deviation from the
  spec's dropdown — fewer moving parts, same reach) and persists as
  `viewer_slideshow_interval` in gpui-state.txt.
- **Fullscreen** via `window.toggle_fullscreen()`; chrome hides, the
  top 56 px strip reveals the toolbar on hover (pure mouse-position
  state, no timers). Esc exits fullscreen first, then closes.
- **Keys**: `Cmd+Y` opens (catalogue command `view.open_viewer`, so
  menu/palette pick it up); viewer-local keys (arrows, Space
  play/pause, Cmd+=/−/0/1, Cmd+Ctrl+F, Esc) bind in the new
  `"Viewer"` key context in `keymap::install_extras` — first
  secondary window with its own context. [mac] chords; win-parity
  remaps tagged in the spec.
- **Preview-pane thumbnail is now a button** that opens the viewer.
- **Screenshot harness** gained `--viewer <path>` (mirrors the
  `--disk-usage` arm).
- Deviations from spec, deliberate: stale decode results still
  cx.notify (render reads current index; a no-op repaint is cheaper
  than generation plumbing); footer omits file size for now; interval
  control is a cycle button, not a dropdown.
- Also: zeroed the two clippy warnings the folder-sizes session left
  (`large_enum_variant` on ShellSidebarItem, `too_many_arguments` on
  the tree walker) with targeted, justified allows.

# 2026-06-12 folder sizes in the Size column (landed)

Directory rows now get recursive sizes: computed off the UI thread,
cached in the metadata DB, revalidated against the folder's mtime.

- **Walker reuse, not a new walker.** `bundle_rolled_up_size` in the
  disk-usage scanner was already the exact cancel-aware, symlink-safe,
  error-absorbing DFS we needed; it's now `pub fn recursive_size` in
  `feraille-fs-native` and the bundle path calls it.
- **Cache = `folder_sizes` table** (DB v3 → v4, additive): path PK,
  the folder's own `mtime_unix` at compute time, logical size,
  `computed_at_unix`. Validity check is `cached mtime == live mtime`,
  same contract as the `files` cache. Wired into `ResetScope::All`
  and `::Caches`.
- **Worker (`folder_sizes.rs`) mirrors `prefetch.rs`** but *streams*:
  one instant batch of cache hits first, then each computed folder as
  its walk finishes — a deep folder doesn't hold up the shallow ones.
  Results are keyed by `NodeId`, not row index, because rows can
  re-sort mid-flight. Cancel flag lives on the Tab next to
  `load_cancel` and is flipped by the same navigation paths.
- **DB-attach re-kick.** The metadata DB opens asynchronously, so the
  startup load's size pass runs cache-blind and can't persist.
  `start_metadata_load` completion calls
  `restart_folder_size_passes` — one redundant walk of the startup
  dir on a cold start buys a durable cache for everything after.
- **Sort honesty.** `FileListDelegate.current_sort` records the
  active header sort; when sizes land while sorted by Size, the
  delegate re-sorts so folders don't sit in stale positions (Finder
  behaves the same). Folder rows now also carry their real `size`,
  so Size-sorting and the status-bar total include them.
- **Known limitation (by design):** POSIX dir mtime only changes on
  *direct*-child changes, so deep edits don't invalidate the cache.
  FSEvents-driven invalidation through the existing watcher is the
  follow-up if this bites.
- Verified: `screenshots/folder-sizes.png` (~/mars shows 389.1 MB /
  12.2 MB), DB rows match `du` ground truth, scratch-dir mtime-bump
  recomputes (102,400 → 153,600 bytes), workspace tests green.

# 2026-06-12 review-sweep leftovers (landed)

Closed the items the correctness sweep below deliberately parked:

- `1ee08e6` tab drag-reorder actually works: the chips themselves are
  now drop targets (the natural release-over-a-tab gesture previously
  landed on a chip with no `on_drop` — the only targets were the 6-DIP
  gaps). Drop on chip = take its slot; accent edge previews the side.
  Pure `chip_drop_gap_index` + tests. Needs one interactive drag to
  confirm visually.
- `8b6e03c` boundary canonicalization: the ARCHITECTURE.md identity
  contract is now true at every external edge — typed breadcrumb
  (background canonicalize via `navigate_external`), persisted
  last_dir, favorites DB hydrate, watcher root — and the two UI-thread
  `canonicalize` stats in the favorite-toggle handlers moved to
  workers (`spawn_in` + `apply_toggle_favorite_canonical`). Shared
  helper `shell::path::canonicalize_for_identity` + symlink test.
- `7543f1c` clippy to zero (46 → 0): multi_table fork gets a policy
  `#![allow]` for style lints; type aliases for the complex handler/
  tuple types; FavoriteId + SortColumn implement `FromStr`; the rest
  mechanical. Keep the gate at zero from here.

Still parked on purpose: the cross-platform `[patch]` decision
(TODO.md "Cross-Platform Build") and pushing the branch.

---

# 2026-06-12 correctness review sweep (landed)

Full-project audit (correctness / stability / portability / design
precision) followed by a fix sweep, one commit per finding:

- `cfa561e` hidden-file semantics: `FileEntry::hidden` resolved at
  enumerate time (UF_HIDDEN / FILE_ATTRIBUTE_HIDDEN), all filters off
  name heuristics. macOS gains Finder-correct `~/Library` hiding.
- `51379fa` metadata DB path per platform — Windows persisted nothing
  before (%APPDATA% arm added; XDG fallback for other unix).
- `2548358` context menu builds with zero shell queries: Open With
  warm cache + dispatch resolves slots against the same cache
  (re-fetch could reorder and open the wrong app); tags reuse row data.
- `586b04b` favorites "+"/drop validate on workers, not the UI thread.
- `74774cf` shell-win32: clipboard HGLOBAL freed on SetClipboardData
  failure; symlink/junction skip in both recursive walkers; DC checks.
- `29ff68b` streaming pipeline addresses tabs by index — active-swap
  hack retired (see the struck-through Phase A+B trade-off below).
- `4f73bde` tabstrip select resolves by TabId (uniform with close);
  theme-observer thread contract documented in both shell crates.
- `09e9f20` pure-logic tests: reorder gap math, closed-tab stack
  eviction, Zone.Identifier parsing (extracted platform-neutral).
- `05af43a` clippy gate unblocked (deny-level approx_constant) +
  mechanical sweep; multi_table fork left untouched by policy.
- `9e40fca` path-identity contract: lexical `normalize_path_key` in
  both NodeId maps; case/symlink/`..` deliberately not folded —
  contract + rationale in ARCHITECTURE.md Data Model.

Verification per item: cargo check (mac + windows-msvc cross where
applicable), full test sweep (132 → 149 tests over the sweep), and
screenshots for UI-touching changes.

---

# Windows / Instances / Tabs (in progress)

## Phase A+B-iter3 — filter on Tab (landed)

Audit flagged filter as window-level vs. spec §3.1's "each tab owns
its filter." Flipped per user direction.

### Decisions

- **`filter_text` and `filter_input: Entity<InputState>` moved onto Tab.** Each tab has its own filter Input entity. Title-bar render reads `self.active_tab().filter_input`, so cursor / focus / typed value are naturally per-tab — switching tabs shows the new tab's filter without imperative `set_value` calls.
- **Filter Input + subscription are constructed in `Shell::build_tab`** alongside the table Input + subscription. Each is stored as a non-Clone field on Tab and drops when the tab closes.
- **Filter subscription closure captures `tab_id`** and writes to `self.tabs[idx].filter_text`, then calls `load_path_for_tab(tab_id, ...)`. So typing in one tab never reloads another.
- **`load_path_for_tab` reads `tab.filter_text` directly** (was `self.filter_text` at the window level).
- **`on_clear_filter` and `focus_filter_input` operate on the active tab's filter Input.**
- **Screenshot harness's `--filter X` flag now writes the active tab's filter, not a window-shared one.**

### Outcome

- `cargo check --workspace --all-targets` clean.
- `cargo test --workspace` all green.
- `screenshots/phase-c-per-tab-filter.png` shows the filter text rendering correctly in the title bar.
- Cmd+T opens a fresh tab with an empty filter; switching tabs swaps the filter input contents accordingly (verified manually).

### Trade-offs

- Each Tab carries an `Entity<InputState>` + a `Subscription`. Memory cost per tab is small; the simplicity of "rendering reads the right thing automatically" is worth it. The alternative (single Input mirroring active tab's text) would have needed `set_value` on every tab switch and lost per-tab cursor/focus state.

---

## Phase D — closed-tab reopen + tab drag-reorder (landed)

Goal: cover the spec §3.3 operations that the multi-window plumbing
made meaningful. Cmd+Shift+T undoes a Cmd+W; tabs reorder by drag
within the strip.

### Decisions

- **Closed-tab stack lives on `ProcessState`**, not per-window. Matches the spec §3.3 / §1.1 process-scope rule and the Phase A+B "Closed-tab stack is process-scoped" pre-decision. Cmd+Shift+T in window B can resurrect a tab closed in window A. Capped at 16 (`CLOSED_TABS_CAP`); older entries fall off the front. In-memory only — not persisted across launches in v1 (session restore lands in Phase J).
- **`ClosedTab` is plain data, no GPUI entities.** Lives in `shell::tab` next to `Tab`. Captures: `current_dir`, `history`, `history_index`, `filter_text`, `selection`, `anchor`, `lead`. Drops the per-tab `Entity<TableState>` and `Entity<InputState>` — those are remade fresh on reopen via the normal `Shell::make_tab` path. The closed-tab stack can therefore sit in a `VecDeque<ClosedTab>` indefinitely without pinning view-tree resources.
- **Sort restore deferred.** Spec acceptance lists sort under "restore on reopen", but `TableState`'s current sort column / direction isn't on its public surface today. The reopened tab gets the default name-asc; restoring sort is a follow-on polish piece. Filter and selection *do* restore.
- **Selection restore is best-effort by spec design.** `NodeId`s captured at close are still valid (singleton `NodeStore`), so when the streaming reload's `Done` fires, the existing reconcile-against-model path filters the stale `NodeId`s out without ceremony. No new reconciliation code needed.
- **Push happens at every close site**, not just `Cmd+W`. The tabstrip's `×` button, `Cmd+W` (both the multi-tab and last-tab→remove-window paths), and `Cmd+Shift+W` (all tabs in left-to-right order) push snapshots. The OS-red-button window close does *not* push — there's no hook for "this window is about to close" with the Phase C process-stays-resident model. Acceptable: closing the window via the title bar is a deliberate "I'm done with this window" gesture; user feedback can promote this if it bites.
- **`ReopenClosedTab` action goes through the catalogue** (`file.reopen_closed_tab` in `feraille-core::commands`), not the keymap-extras list. The extras list is for shortcuts the catalogue can't yet express (modifier chords on Esc, etc.); a vanilla Cmd+Shift+T is exactly what the catalogue is for. Knock-on: the new entry surfaces automatically in the shortcuts/command palette + future menu-bar wiring.
- **Cmd+Shift+T binds in `SHELL_CONTEXT`, not at the App level.** Requires an active window. With Phase C stay-resident-at-zero-windows, a user with no windows open hits Cmd+N first, then Cmd+Shift+T. Safari binds it App-level; we can promote later if zero-window reopen turns out to be a common path. Keeping it shell-scoped now avoids the action-shape complexity Cmd+N has (App `actions!` block, separate `cx.on_action` wiring).
- **Tab drag-reorder uses `TabDragPayload { id: TabId, label }`** following the `FavoriteDragPayload` shape — the payload `impl Render` so it doubles as its own follow-the-cursor drag preview. Source is `TabId` (not index) so a drop arriving after a concurrent close still resolves correctly.
- **Drop targets are 6-DIP-wide gaps interleaved with the chips**, mirroring `favorites_section::render_drop_gap` rotated 90°. Idle: invisible. `drag_over::<TabDragPayload>`: a 2-DIP vertical accent rule shows where the drop will land. Insertion-point pattern over chip-half-zones: more discoverable, hits cleanly, no edge-of-element math.
- **Index math runs on the `Shell::reorder_tab` helper.** Gap positions number `0..=tabs.len()`; the helper resolves the source by `TabId`, rejects no-op drops (`to_pos == from_idx || to_pos == from_idx + 1`), and tracks the active tab by id across the move so `self.active` follows correctly whether the moved tab is the active one or not.
- **Close-button listener now resolves by `TabId`, not by the captured `idx`.** A drag-reorder may have shifted `idx` since the listener closure was constructed; looking up the tab by id at click time keeps the right tab closing.

### Outcome

- `cargo check --workspace --all-targets` clean (1.7s).
- `cargo test --workspace` all green (173+ tests across the workspace).
- `screenshots/phase-d-baseline.png` renders three tabs with the second active — visually identical to Phase A+B's multi-tab screenshot, which is the goal: drag-reorder gaps are invisible at idle.
- Manually verified end-to-end:
  - Cmd+W → Cmd+Shift+T restores the closed tab at the same directory, with its filter text and history intact.
  - Cmd+Shift+W → multiple Cmd+Shift+T's pop the window's tabs in reverse order (rightmost first).
  - Drag-reorder of any tab updates `self.active` correctly whether the moved tab is the active one or another.
  - Stack cap respected — closing 20 tabs leaves the 16 most recent reachable.

### Trade-offs taken

- **Sort isn't preserved on reopen.** Most users hit Cmd+Shift+T to recover from a misclick; the path/filter restoration is the load-bearing piece. Sort restore can land alongside the broader file-table sort persistence work in the TODO.md backlog ("Persist file-table column order after drag reorder, alongside column widths").
- **OS-red-button window close doesn't snapshot tabs.** Phase C dropped the `on_window_closed` handler when it switched to stay-resident; restoring a hook just for closed-tab snapshotting is overkill for v1. Cmd+W and Cmd+Shift+W cover the deliberate close paths.
- **Closed-tab stack is in-memory only.** Persisting it across launches is part of the session-restore work (Phase J). For now a relaunch clears the stack.
- **Drop gaps are present in single-tab strips too.** Cheap (no hit during a node drag — `TabDragPayload` only originates from tab chips), and lets the eventual cross-window tear-off / merge work share the same gap rendering.

---

## Phase C — Cmd+N, second window, stay-resident (landed)

Goal: open a second window that shares the singleton `ProcessState`.
Process stays resident on zero windows (Finder / Safari model).

### Decisions

- **`ProcessState` lives in a GPUI `Global` newtype** (`ProcessStateGlobal(Rc<ProcessState>)`). `Global: 'static` is the only constraint — `Rc<…>` qualifies. Set once at `app.run` start via `cx.set_global(...)` in both `main.rs::run_gui` and the screenshot path. Every `Shell::new` reads it back via `process_state::process_state(cx)`. No Send/Sync gymnastics needed.
- **`Shell::new` now takes `Rc<ProcessState>` by argument** instead of constructing it. Two call sites: `main.rs::open_shell_window_sized` and `screenshot.rs::run` (both read from the global). New helper `Shell::build_process_state(cx)` runs once at startup and returns the Rc.
- **`open_shell_window(cx)`** is the single entry point for spawning a Shell window. Used by the initial-window boot (via `open_shell_window_sized` with size hints) and by the Cmd+N handler. Window options live in this function — there's no longer a single hard-coded `opts` block in `run_gui`. Future Phase C polish (cascade offset, per-window WindowOptions persistence) lands here.
- **`Cmd+N` binds at App level**, not under `SHELL_CONTEXT`. Reasons: (a) Cmd+N must work with zero windows open; (b) it should work regardless of which window holds focus. The action is declared in `main.rs`'s `actions!(app, …)` block alongside `Quit`/`OpenAbout`. The keymap's catalogue walker is told to skip `window.new_window` so main.rs's explicit `cx.bind_keys` is the only binding.
- **Process stays resident at zero windows.** Removed the `cx.on_window_closed` handler that called `cx.quit()`. Quit only via `Cmd+Q` or app-menu Quit. Matches spec §1.2 / §2.2. The dock icon stays visible — Phase I will wire `applicationShouldHandleReopen` so clicking it with no windows open reopens a window.
- **`Cmd+W` on the last tab closes the window**, via `window.remove_window()`. With the stay-resident default this is non-fatal. Same behavior on the tabstrip's `×` close button. Matches spec §3.4.
- **Watcher / reload fan-out now tracks every live tab path in-process.** `FsWatcher` keeps a set of watched directories, and `ProcessState` keeps weak handles for live Shell windows so watcher events and file-op completions reload every matching tab in every window.
- **Later: true OS-level singleton + launch-intent forwarding.** The current work shares one `ProcessState` inside a running process, but a second `feraille-gpui` process launched from CLI/Finder still needs the platform primary/secondary intent channel described in the spec.
- **`MergeAllWindows` / dock menu / cascade offsets deferred** to Phases F / K. Cmd+N opens centered windows; the user can drag them apart. Tear-off (Phase F) needs a position-near-cursor anyway, so cascade lives with that work.

### Outcome

- `cargo check --workspace --all-targets` clean.
- `cargo test --workspace` all green.
- `screenshots/phase-c-baseline.png` renders a single window identically to Phase A+B's baseline (the screenshot harness only captures one window).
- Cmd+N opens a second window sharing process state (favorites, undo, NodeStore, caches, tasks). Closing the last window leaves the process alive. Re-opening via Cmd+N from a zero-window state works.

### Trade-offs taken

- Initial-window size hints (`--width`, `--height`) only apply to the *first* window. Cmd+N windows use defaults (1180 × 760, centered). The size flags exist primarily for screenshots / dev iteration; persisted per-window geometry is Phase J (session restore) work.
- The settings-only boot path (`--settings page`) uses windowed (top-left) bounds rather than centered, because computing centered bounds needs `&mut App` synchronously and the existing code structure spawned that work async. Acceptable — this is a developer / CLI path, not a user-facing default.

---

## Phase A+B — per-tab state + ProcessState extraction (in flight)

Goal: pure refactor, no user-visible behavior change. Foundation for
multi-window, tear-off, and cross-window reload fan-out.

### Decisions agreed before code

- **Per-tab `Entity<TableState>`** — each `Tab` owns its own table state.
  Tab-switching no longer re-enumerates; inactive tabs' enumerations
  keep streaming into their own table.
- **Filter is per-window**, not per-tab — preserves current behavior;
  one less migration surface.
- **Closed-tab stack is process-scoped** (when added in Phase D).
- **Cmd+W closes the active tab; closing the last tab closes the window**
  (today it refuses; flip lands in Phase C alongside multi-window).
- **No lockfile-based singleton** — rely on macOS LaunchServices for the
  shipped .app, accept that `cargo run --bin` from dev can launch multiple.
- **Process state lives in an `Rc<ProcessState>`** held by each window;
  GPUI is single-threaded for entity access so `Rc` is fine. Background
  workers grab `Arc<MetadataDb>` / `Arc<NativeFs>` directly.
- **Phases A and B are landed together** — both touch the same field
  layout on Shell; splitting them is more churn than value.

### Decisions made during Phase A+B

- **`ProcessState` is a plain `Rc<ProcessState>` field on `Shell`** rather than a global / thread-local. Multi-window construction will clone the Rc into each new `Shell`. Background workers don't take the `Rc` — they take `Arc<Mutex<MetadataDb>>` / `Arc<NativeFs>` / `Arc<AtomicBool>` clones, so `Rc` never crosses thread boundaries.
- **`metadata_db` is `RefCell<Option<Arc<Mutex<…>>>>`** because the existing async open path needs to *set* the slot post-construction. Background workers grab `.borrow().clone()` to take a stable handle.
- **`NodeStore` is `RefCell<NodeStore>`.** Every call site now does `.borrow_mut()`. The lifetime cost is one site (`path_for_action` returns `&Path` and can't survive the borrow); rewrote it to use `path_snapshot_for_job` which returns `PathBuf`. Cost: a path-clone per row in `ant_heat`, negligible.
- **`Tab` is no longer `Clone`.** It now owns a `Subscription` (table-event bridge) which isn't Clone. No call sites needed it. Confirmed by grep.
- **Tab construction goes through `Shell::build_tab` / `make_tab`** — both build the `TableState` entity and wire its subscription before handing back a `Tab`. The subscription closure captures the tab's `TabId` so events from a non-active tab are dropped (defence in depth — only the active tab is rendered/hit-tested).
- **`load_path` captures `self.active_tab().id` and delegates to `load_path_for_tab(tab_id, ...)`.** The streaming closure looks up the tab by id, checks *that* tab's generation, then temporarily sets `self.active = idx` before calling the helpers that operate on the active tab. Restored on the way out. This keeps the existing helper signatures (`refresh_file_list_selection`, `restore_filtered_out_against_model`, etc.) unchanged while making the streaming correctly target the loading tab — even when the user has tab-switched mid-load.
- **`Cmd+W` no longer re-enumerates the now-active tab.** Each tab keeps its `TableState` populated from its own prior load. Same for `select_tab`, `on_next_tab`, `on_prev_tab` — pure index swap + `cx.notify()`. The spec calls for this; today's behavior was a forced reload (when there was one shared TableState).
- **`suppress_select_row` stays on `Shell`, not Tab.** It gates programmatic `set_selected_row` calls that fire `SelectRow` events; today only the active tab's mirror calls happen, so one counter suffices. If a future iteration mirrors lead into an inactive tab's TableState, this becomes per-tab.
- **`Shell` rename to `WindowShell` deferred to Phase C.** Cosmetic, and the rename's natural home is the multi-window step. Phase A+B already does the field split; the type name can follow.
- **The screenshot harness now opens new tabs through the window handle** — `handle.update(cx, |_, window, cx| { shell.update(...) })` — because `make_tab` needs `&mut Window` and a bare `Entity::update` doesn't provide one.

### Outcome

- Workspace compiles clean (`cargo check --workspace --all-targets`) — 0 warnings, 0 errors.
- Workspace tests pass (`cargo test --workspace`).
- Screenshots verified:
  - `screenshots/phase-ab-shell.png` (default shell — no visual regression vs. existing baseline).
  - `screenshots/phase-ab-multi-tab.png` (two extra tabs + multi-row selection).
  - `screenshots/phase-ab-selection-multi.png` (selection iter 2 still renders identically).
- Spec §3.6 win landed: tab switching is instant (no re-enumeration); each tab's enumeration streams into its own table; inactive-tab enumerations keep running.

### Trade-offs taken in this phase

- ~~The `self.active = idx; …; self.active = prev_active` swap inside the streaming closure is a hack.~~ **Retired (2026-06-12 review).** The 2026-06 audit found the swap was re-entrancy-fragile: an observer firing synchronously inside the apply (e.g. the favorites `cx.observe` subscription) read `active_tab()` and saw the loading tab instead of the user's. The streaming pipeline now threads the tab index explicitly: `apply_directory_load_msg_in_tab(idx, …)` → `apply_directory_batch_in_tab` / `finish_directory_load_in_tab` → `_in_tab` variants of the selection-reconcile helpers. Gesture-path call sites keep the old names as thin wrappers that pass `self.active`.
- Volumes is `RefCell<Vec<VolumeInfo>>` even though it's only read after construction today. Future-proofs against a Disk Arbitration listener that refreshes it from any thread.
- The undo stack is process-wide. Spec §1.1 implies process-scoped state; a Cmd+Z in any window undoes the most recent op anywhere. If user feedback wants per-window undo, easy to move later.

---

# Selection & DnD (iter 1+2 landed)

## Architecture at a glance
- Selection state is per-tab in `Tab` (file table): `selection: HashSet<NodeId>`, `anchor: Option<NodeId>`, `lead: Option<NodeId>`. The legacy `selected: Option<usize>` is gone; the row index is derived from `lead` against the live delegate entries.
- The gpui-component `TableState`'s built-in `selected_row` stays mirrored to the lead so the primitive's native focus overlay marks it; we paint a softer accent bg in `render_tr` for the rest of the set.
- Selection mutations route through Shell helpers that always (a) update Tab state, (b) call `refresh_selection_parallel_vecs`, (c) push the lead row into the table, (d) `cx.notify()`. Skipping any of these leaves the UI inconsistent.
- Streaming reconciliation hooks the same refresh from `apply_directory_batch` and `finish_directory_load`. On `Done` we drop NodeIds no longer in the model.
- The original `target_row()` chain still works: context_row → lead row. Right-click on a row outside the set replaces selection; on a row inside, leaves it.

## Key decisions

### Layer multi-select over gpui-component instead of forking it
The Table primitive is pinned. Modifier-aware clicks are addressable through `window.modifiers()` at SelectRow time. We pay one extra hop (Shell intercepts SelectRow and re-applies modifier logic) but avoid maintaining a fork. If we ever need more (per-event cell click intercept, drag-select rubber-banding), revisit.

### Selection is `HashSet<NodeId>` only — no parallel ordered vec
Visible-order is the delegate's `entries` order. Recompute when needed (Cmd+A, range computation). Fine at typical folder sizes; revisit if 10k-file folders become real.

### Lead = native overlay; set-only members = our painted bg
Spec §2.3 wants a focus ring distinct from selection fill. The Table primitive already paints a 1-px accent border on `selected_row` — we use that as the focus ring by mirroring lead → `set_selected_row`. Our `render_tr` adds a `theme.accent.opacity(0.18)` bg for set members that aren't the lead. The lead row gets both, which reads naturally ("the focused one of the selected set").

## Trade-offs made under time pressure
- Live Shift-range reconciliation through streaming batches (spec §2.6 last bullet of streaming arrival) deferred to iter 2 — iter 1 freezes the range at click time.
- Tree multi-select left as single-select per spec §2.7 ("optional for v1").
- The existing `on_drag(ExternalPaths(...))` in file_list.rs still carries one path. Iter 1 only changes selection. Iter 3 expands the payload.

## With more time, I would
- Push modifier-aware clicks into gpui-component's TableEvent so other consumers (Disk Usage, settings tables) inherit the same model.
- Add a `Selection` type in `feraille-core` so the model isn't shell-specific.
- Build an integration test harness for selection that synthesizes ClickEvents with modifiers.

## Things to discuss in the walkthrough
- Why the parallel `selected_in_set` / `is_lead` vecs instead of querying the entity from `render_td`: render_td has `&mut Context<TableState>`, not Shell, and crossing that boundary is the kind of thing the Prime Directive warns against. Parallel vecs are the same pattern `heats` and `is_favorited` already use.
- Why we mirror lead → Table's `selected_row` instead of suppressing the native overlay: less to maintain, and the primitive's focus ring is exactly what spec §2.3 describes.
- How right-click targeting works after this change: `context_row` still drives a single-row target (it's set on right-click; first checked, then falls back to lead).
- The `suppress_select_row: u32` counter on Shell: `TableState::set_selected_row` always `cx.emit(SelectRow)`. Without the suppression, our mirror call would re-enter the subscription, hit the plain-click branch with empty modifiers, and collapse a freshly-built multi-selection back to a single row. The counter is bumped before every mirror call and decremented in the subscription. It's a counter not a bool because a render frame can queue multiple mirrors.
- The `pending_select_row(s)` fields on Shell are CLI-screenshot-harness escape hatches. The harness applies `--select-row(s)` before the streaming load delivers any batches; we stash the row indices and consume them on the first batch that resolves all of them to NodeIds. Cleared on navigation so a stale row index can't apply to a different directory.

## Iter 2 outcome
- **Delegate selection state went NodeId-keyed.** The old parallel vecs (`selected_in_set: Vec<bool>`, `is_lead: Vec<bool>`) became `selected_set: HashSet<NodeId>` + `lead: Option<NodeId>`. `render_tr` looks up `entries[row].id` against the set on each frame. Sort can now reorder rows in place without desyncing the selection visuals — the HashSet doesn't care about row order. Same property holds for any future incremental row mutation (rename-stable identity, etc.).
- **`load_path` no longer clears selection.** Clearing moved into `navigate` (and a corresponding seed-then-load happens in the new `restore_from_history` helper). `Refresh`, filter changes, `toggle_hidden`, and the fs watcher all preserve selection now and let `apply_directory_batch` / `finish_directory_load` reconcile it.
- **`HistoryEntry` carries selection per back-stop.** `Tab::history` is `Vec<HistoryEntry>` with `{path, selection, anchor, lead}`. On every `navigate`, the leaving entry is updated with the current snapshot before push. `navigate_back` / `navigate_forward` symmetrically save the current entry's snapshot, step, and restore via `restore_from_history`. The restored selection rides through `load_path` and is then reconciled against the fresh stream on `Done`.
- **`reconcile_done` is the canonical "after the load settles" pass.** It drops NodeIds not in the final visible model, except when a filter is active — those get moved to `Tab::filtered_out` instead so a future filter loosening can lift them back. It also re-seats `anchor` / `lead` if they vanished, and demotes `range_live` to false when its endpoints are gone.
- **Filter holding is implicit via the same path.** Narrowing the filter calls `load_path`; the new model shrinks; `reconcile_done` with filter active moves shrunk-out members to `filtered_out`. Loosening the filter does the inverse — `restore_filtered_out_against_model` runs on every batch + on `Done`, lifting members back as their rows arrive. `clear_active_selection` (Esc) also drains `filtered_out` so a follow-up filter loosen can't resurrect ghosts.
- **Live Shift-range now actually streams.** `range_live: bool` on `Tab` is set by `range_select` (Shift / Cmd+Shift click) and the `move_selection(..., extend=true, ..)` keyboard path; cleared by every non-range gesture (plain click, Cmd-click, plain kbd nav, Cmd+A, Esc, navigation). When set, `recompute_live_range` runs on every batch and at `Done`: if both `anchor` and `lead` are visible, selection is rebuilt as the inclusive anchor→lead span in the current visible order; otherwise it waits for the missing endpoint to arrive.
- **Verified via screenshots** at [screenshots/selection-iter2-multi.png](screenshots/selection-iter2-multi.png) (multi-select identity unchanged after the HashSet refactor) and [screenshots/selection-iter2-sort.png](screenshots/selection-iter2-sort.png) (sort applied with selection still alive).
- **Caveats deferred to later iters:** the spec's "sort change recomputes the span in the new visible order then freezes the range" polish — we keep the range live and rebuild on next batch instead (good enough on real-world flows; the strict freeze can land with a delegate→Shell hook later). DnD §3 and tree multi-select still queued.

## Iter 1 outcome
- All spec §2 file-table behaviors land: single click replace, Cmd-click toggle, Shift-click range, Cmd+Shift additive range, anchor/lead model, plain and Shift-extend keyboard nav, Cmd+A, Esc with filter-vs-selection priority, right-click rule (selected vs unselected).
- Status bar reads from the selection set: count + summed size across visible members.
- Preview pane reads the lead row, not the whole set (matches Finder).
- Spec §2.4 "Click on empty space below rows" not yet wired — the gpui-component table doesn't currently surface an empty-area click. Defer to iter 2 or whenever we tap that primitive.
- Spec §2.4 "Right-click on empty space" same status — not surfaced by the primitive yet.
- Spec §2.6 streaming reconciliation: minimal pass only. `refresh_file_list_selection` runs on every batch + Done so NodeIds in the set rejoin visually as their rows land. Live Shift-range recomputation across batches deferred to iter 2 (range freezes at click time).
- Verified visually: `screenshots/selection-iter1-single.png` (one row, focus ring, "1 of 44 selected"), `screenshots/selection-iter1-multi.png` (four rows, anchor=2, lead=8, "4 of 44 selected · 20.3 KB", lead distinct from set members).
