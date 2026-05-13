# Porting log — `feraille-app` → `feraille-gpui`

Single source of truth for the harvest. One section per feature.
Update at every commit boundary; the file answers "what's left?"
without anyone needing to remember.

**Direction:** harvest from the old soft-rendered binary (`feraille-app`)
into the new GPUI shell (`feraille-gpui`). The old code is read-only
reference. Changes to the old crate are limited to deletions + bug
fixes for shipped users.

**Statuses:**

- `Not started` — code exists in old, not yet ported
- `In progress` — partial port landed; finishing in a follow-up
- `Ported ✅` — fully landed in the new app; old code can be deleted at cutover
- `N/A` — not applicable on the new stack (Windows-only, soft-render-only, etc.)

---

## Foundations (Stage 0–1)

### Workspace + dependency graph
Status: Ported ✅ (Stage 0)
Old location: `crates/feraille-gpui/Cargo.toml` previously linked only
`feraille-core` + `feraille-fs-native`.
New location: Same Cargo.toml, now linking all 7 domain crates
(feraille-design, feraille-meta, feraille-disk-usage,
feraille-shell-mac added in Stage 0).
Notes: `cargo check` passes. All domain crates verified clean of
`feraille-render` / `feraille-controls` deps in Phase 2 audit.

### Observability (obs.rs)
Status: Ported ✅ (Harvest Stage 1.a)
Old location: `crates/feraille-app/src/obs.rs` (~190 lines)
New location: `crates/feraille-gpui/src/obs.rs`
Notes: Verbatim port + minor branding edits ("Feraille (gpui)" in
banner / crash header) + bumped `LOG_THRESHOLD` from 60 → 90.
`log_info!` / `log_warn!` / `log_error!` macros that previously
lived at the top of `feraille-app/src/main.rs` are now
`#[macro_export]` inside obs.rs so any feraille-gpui module reaches
them via `crate::log_info!(id, "...")`. Verified: running
feraille-gpui prints the same startup banner + arg log line.

### Entry point — main.rs equivalence
Status: In progress
Old location: `crates/feraille-app/src/main.rs::main()`
New location: `crates/feraille-gpui/src/main.rs`
Notes: GUI dispatch + `--screenshot` fork exist; obs::init,
metadata DB open, magic prefetch, quarantine prefetch missing.
Stage 1 fills them in.

### Persistent metadata DB (feraille-meta)
Status: In progress (handle opened in Stage 1.b; hydration / write-
through arrives in later stages)
Old location: `crates/feraille-app/src/main.rs::App::open_metadata_db`
+ write-through helpers across navigate / magic / quarantine paths.
New location: `crates/feraille-gpui/src/shell.rs::open_metadata_db`
+ `Shell::metadata_db: Option<Arc<Mutex<MetadataDb>>>`
Notes: Stage 1.b opened the handle at the canonical location
(`~/Library/Application Support/Feraille/metadata.db`) and stored
it on Shell. Wrapped in `Arc<Mutex<_>>` so Stage 4 background
prefetch workers can share it. Hydration of ant trail / layout /
tabs + write-through on navigate are deferred to the stages that
consume them.

### Native macOS menu bar — titles via catalogue
Status: Ported ✅ (Harvest Stage 3.b — gpui::Menu uses
`feraille_core::commands` titles).
Old location: `feraille_shell_mac::install_app_menu` driven by
`feraille_core::commands` catalogue.
New location: `crates/feraille-gpui/src/main.rs::install_app_menus`
+ `title(id, fallback)` lookup helper.
Notes: The new app stays on `gpui::Menu` / `cx.set_menus` (not the
feraille-shell-mac NSMenu bridge) because gpui's Menu API directly
takes the gpui Action type — no callback marshalling required.
What we harvest is the *titles + structure* from the catalogue;
each `MenuItem::action` looks up its label via `title("file.new_tab",
"New Tab")` so a CommandSpec edit re-flows into the menu without
touching main.rs. Item set: app menu (About / Settings / Quit),
File (New Tab / Close Tab / New Folder / Rename / Open / Reveal in
Finder / Move to Trash), Edit (Copy Path), Go (Back / Forward /
Enclosing Folder), View (Find / Show Hidden Files / Refresh). The
feraille-shell-mac variant (with About panel, theme submenu,
checkmark-state, callback dispatch) becomes available at cutover
or earlier if we want the About-panel polish.

---

## CLI (Stage 2)

### Screenshot CLI flag parity
Status: Ported ✅ (parse + apply, with some flags stubbed for later stages)
Old location: `crates/feraille-app/src/screenshot.rs::parse_args` (25 flags)
New location: `crates/feraille-gpui/src/screenshot.rs::parse_args` (25 flags)
Notes: All 25 old flags now parse. Functional today:
`--screenshot`, `--width`, `--height`, `--scale`, `--theme`,
`--navigate` (repeatable), `--new-tab` (repeatable), `--tab`,
`--select-row`, `--select-name`, `--show-hidden`, `--filter`
(syncs both the Input widget and the underlying filter_text),
`--search` (focuses filter), `--preview`, `--sort col[-desc]`
(name / size / kind / magic / mtime, folders-first), `--rename`,
`--inline-rename` (falls back to modal rename), `--new-folder`,
`--settings`, `--expand <path>` (reveals + lazy-expands tree
ancestors — landed in Stage 9.c). Stubbed (parse but emit a
`log_warn` on apply, pending the stage that wires them up):
`--properties` (→ Stage 8), `--edit-mode` (→ Stage 9), `--ui-scale`
(→ Stage 9), `--simulate-toast` / `--simulate-progress` /
`--simulate-task-panel` (→ Stage 5), `--shortcuts-help[-filter]`
(→ Stage 9), `--disk-usage` / `--du-depth` / `--du-coloring` (→
Stage 7), `--splitter`, `--scroll` (Table scroll API not yet
exposed), `--mac-chrome` (N/A — GPUI has native chrome). New
pure-logic port: `SortColumn` enum + `sort_in_place` comparator
inside `crates/feraille-gpui/src/file_list.rs` (carries forward the
folders-first behaviour from `feraille-controls::sort_entries`).

---

## Command catalogue + keymap (Stage 3)

### Command catalogue → first-class keymap
Status: Ported ✅ (Harvest Stage 3 — catalogue drives every
keybinding; unbound commands log a warning so the gap is visible.)
Old location: `crates/feraille-core/src/commands.rs` (65 commands,
already in domain layer) + `crates/feraille-app/src/main.rs::
keystroke_to_command` (bespoke matcher).
New location: `crates/feraille-gpui/src/keymap.rs::install` walks
`feraille_core::commands::all_commands()` once at startup;
`crates/feraille-gpui/src/shell.rs::init` now just calls into it.
Notes:
- `Shortcut → gpui keybind string` translation via
  `translate_shortcut` (cmd / shift / alt prefixes + lowercased
  key name).
- Each `CommandId` whose action is implemented (16 today) routes to
  the existing gpui Action type. The remaining ~30 catalogue
  entries (Disk Usage / cursor-nav / Get Info / zoom / breadcrumb-
  edit / etc.) log a one-time warning at startup with the binding
  that's currently dropped — visibility for the porter, harmless
  to users.
- Tab cycling (Cmd+T / Cmd+W / Ctrl+Tab / Ctrl+Shift+Tab) and the
  ClearFilter escape aren't catalogue commands (Phase 5.5
  invention); installed by `install_extras` alongside.
- One behavioural change worth noting: the catalogue assigns Cmd+R
  to `disk_usage.refresh`, not `file.refresh`. `file.refresh`
  binds F5 only. The previous new-app Cmd+R-refreshes-file-list
  shortcut is therefore inactive until Stage 7 wires Disk Usage.
  Same direction as the old app — it's the catalogue's design.

---

## Background prefetch (Stage 4)

### Magic byte sniffing + quarantine prefetch (data pipeline + UI)
Status: Ported ✅ (data pipeline in Stage 4, UI surfacing in
Stage 6).
UI:
- Magic column added between Kind and Modified in `file_list.rs`,
  reads `entry.display_magic` (truncated text style).
- Quarantine badge: 7×7 red dot overlaid top-right of the icon
  via a `.relative()` wrapper + `.absolute().top(-1).right(-1)`
  positioning + `rgb(0xFF3B30)` fill. Verified on real
  Mark-of-the-Web files (downloaded screenshots).
- `cx.notify()` on the outer Shell after the prefetch apply was
  needed; `TableState::refresh` alone didn't propagate the
  delegate mutation up to the Shell view tree.
- Diagnostic logs (`prefetch: starting for N rows`,
  `prefetch: worker returned N rows`, `prefetch: apply ran`)
  gated at LOG_THRESHOLD=90 stay in the source — cheap, helpful.
Old location: `feraille-app/src/main.rs::start_magic_prefetch` +
`start_quarantine_prefetch` (separate workers in the old app).
New location: `crates/feraille-gpui/src/prefetch.rs::start`.
Notes: Old app had two parallel pipelines (magic, quarantine); the
new app fuses them into one cx.spawn pass per `load_path` —
single iteration, single DB lock acquisition per row, single
batch back to the foreground executor. Pattern:
  1. Snapshot rows on the foreground executor into `Vec<PrefetchSeed>`
     (path + mtime + size + already-cached flags).
  2. `cx.background_executor().spawn` runs the I/O off the main
     thread: cache lookup via `feraille_meta::MetadataDb::get_file`,
     fall back to `feraille_fs_native::detect_magic` +
     `feraille_fs_native::fetch_quarantine_info` on miss, write-
     through to DB via `upsert_file`.
  3. `shell_weak.upgrade().update(cx, …)` applies the batch on
     the foreground executor. Bounds-checked: a re-enumerate before
     the batch arrives just drops the stale indices.
The fields are now populated in `FileEntry::{display_magic,
is_quarantined}`; rendering them lands in Stage 6.

---

## Tasks + status bar + toasts (Stage 5)

### TaskRegistry
Status: Ported ✅ (Harvest Stage 5.a — verbatim from old app)
Old location: `crates/feraille-app/src/tasks.rs`
New location: `crates/feraille-gpui/src/tasks.rs`
Notes: Pure logic, lifted as-is with 8 tests carried over (all
pass). Same Kind enum (Enumeration, IconPrefetch, MagicPrefetch,
QuarantinePrefetch, DiskUsage, FileOp). `Shell` owns the registry
as `Rc<RefCell<TaskRegistry>>` so the prefetch worker can register
/ retire its job from the foreground executor without taking a
mutable Shell borrow.
Wiring landed: `prefetch::start` calls `begin` on launch + `end`
when the worker's batch is applied (Stage 5.a). Future call sites
(Disk Usage scan, file-op copy/move) follow the same pattern.

### Status bar
Status: Ported ✅ (Harvest Stage 5.b)
Old location: `feraille-app/src/main.rs` status-bar paint arm.
New location: `crates/feraille-gpui/src/status_bar.rs::render`
called from `Shell::render`'s right-column footer slot.
Notes:
- Left: "<N> item(s)" entry count for the active tab.
- Middle: task label — `task.label` when exactly one is in flight,
  "N tasks running" when more, hidden otherwise.
- Right: a 120-DIP thin progress strip. Indeterminate mode shows
  a 30%-width stripe (animation deferred); determinate mode shows
  the primary task's fraction.
- `--simulate-progress <p>` CLI flag (negative = indeterminate)
  forces the strip visible for screenshots without spinning up
  real work; landed alongside.

### Task panel (popover)
Status: Not started (Stage 5 follow-on)
Old location: `crates/feraille-app/src/task_panel.rs` (soft-renderer
coupled).
New location: `crates/feraille-gpui/src/task_panel.rs` — rewrite
using gpui-component Popover. Trigger from a click on the status
bar's task-label region.
Notes: Old impl too tied to the soft renderer to copy. List of
active tasks with cancel buttons. `--simulate-task-panel` is
already implemented as "inject 2 fake tasks into the registry";
the popover surface itself is what's pending.

### Toast notifications
Status: Ported ✅ (Harvest Stage 5.c — uses gpui-component's
Notification primitive)
Old location: `feraille-controls::ToastStack` + drive points in
`feraille-app/src/main.rs`.
New location: `Window::push_notification` (via
`gpui_component::WindowExt`) called from the shell-side trigger
points + the screenshot CLI flag.
Notes:
- `--simulate-toast <text>` pushes an error-styled notification
  with `autohide(false)` for predictable capture.
- `Root::render_notification_layer` is added to the Shell's render
  alongside `render_dialog_layer` so the notification list overlay
  draws on top of the shell content.
- Headless capture caveat: gpui's `render_to_image` doesn't fully
  composite absolute-positioned overlays (dialogs have the same
  partial-render issue). The toast surfaces correctly in the live
  window — the screenshot path is "best effort" for overlay
  layers and shouldn't gate the stage.

---

## Disk Usage (Stage 7)

### Disk Usage scan state + cancellation + layout cache
Status: Not started
Old location: `crates/feraille-app/src/disk_usage_state.rs` (~150 lines, pure logic)
New location: `crates/feraille-disk-usage` (move into the existing
shared crate per the mix-decision; sibling module to the squarified
treemap).
Notes: Renderer-agnostic. Bring the file across, fix imports.

### Disk Usage window prefs (persistent geometry)
Status: Not started
Old location: `crates/feraille-app/src/disk_usage_prefs.rs` (~50 lines)
New location: `crates/feraille-disk-usage` (same crate as the state).
Notes: Pure stdlib text-file persistence.

### Disk Usage window (UI)
Status: Not started
Old location: `crates/feraille-app/src/disk_usage_window.rs`
(soft-renderer paint loop + custom event dispatch).
New location: `crates/feraille-gpui/src/disk_usage_window.rs` (rewrite).
Notes: Second GPUI window opened by `view.disk_usage` command
(Cmd+Shift+D). Treemap rendered through a custom gpui Element
(rounded rects + text — no soft renderer). Reuses the
context-menu pattern from the main shell.

### Headless `--disk-usage` CLI path
Status: Not started
Old location: `feraille-app/src/screenshot.rs::run_disk_usage`
New location: `crates/feraille-gpui/src/screenshot.rs` (Stage 7)
Notes: Renders one frame of the treemap into a PNG and exits.
`--du-depth` and `--du-coloring` flags ride along.

---

## Properties / Get Info + Quick Look (Stage 8)

### Get Info / Properties pane
Status: Ported ✅ (Harvest Stage 8 — pane content expanded; full
Cmd+I toggle is Stage 9 polish.)
Old location: `feraille-app/src/main.rs::toggle_properties` + the
Settings-modal-dual-use detail pane.
New location: `crates/feraille-gpui/src/shell.rs::preview` —
expanded to include Magic + Quarantined sections when the
selected entry has them.
Notes: Preview pane (Phase 4.d) was the foundation. Stage 8 added:
- "Magic" row showing `entry.display_magic` (populated by the
  Stage 4 prefetch — "PNG image" for a PNG etc.).
- Red "Quarantined" section header + "Mark of the Web: com.apple.
  quarantine" row when `entry.is_quarantined`. Future polish:
  show agent / ISO date / where-from URLs once those fields ride
  through to FileEntry (currently only in the DB).
Cmd+I to toggle the pane is deferred to Stage 9 (the pane is
always visible today).

### Quick Look (Space-bar action)
Status: Ported ✅ (Harvest Stage 8 — show panel; thumbnail in
preview pane deferred.)
Old location: `feraille_shell_mac::show_quick_look(&[&Path])`
already exists and was used by the old app.
New location: New `QuickLook` action in
`crates/feraille-gpui/src/shell.rs`. Space-bar on the active row
calls `feraille_shell_mac::show_quick_look` with the selected
path. Same code the old app used; the harvest is just the action
+ keybind glue.
Notes: Inline thumbnail in the preview pane (via
`fetch_quick_look_thumbnail`) is a follow-on — needs the
worker-thread + cache by-path pattern. Out of scope for Stage 8 v1.

---

## Polish (Stage 9)

### Ant Trail heat tint
Status: Not started
Old location: `feraille-app/src/main.rs` ant-trail visit tracking
backed by `feraille-meta`. Visible as subtle background heat tint
on entries.
New location: `crates/feraille-gpui/src/file_list.rs` render_tr
hook (Stage 9).
Notes: Domain types already in feraille-core; persistence in
feraille-meta. Just need the visual treatment.

### Go Home (Cmd+Shift+H)
Status: Ported ✅ (Harvest Stage 9.a)
Old location: `feraille-app/src/main.rs` `go.home` dispatch arm.
New location: `crates/feraille-gpui/src/shell.rs::on_go_home`.
Notes: Trivial: `self.navigate(home_dir(), cx)`. Wired into the
Go menu (after Enclosing Folder) and the catalogue keymap.

### Hierarchical tree view in sidebar
Status: Ported ✅ (Harvest Stage 9.c)
Old location: `crates/feraille-controls/src/filetree.rs` (a full
virtualized tree with sections, lazy children, type-ahead, etc.)
driven by `feraille-app/src/main.rs::rebuild_tree_sections` +
`spawn_tree_load`.
New location: `crates/feraille-gpui/src/tree.rs` (new) — `TreeSection`
SidebarItem impl + `TreeRowSpec` + `render_tree_row`. Driving state
lives on `Shell`: `expanded: HashSet<PathBuf>` (which folders are
currently open) + `tree_children: HashMap<PathBuf, Vec<TreeChild>>`
(per-path child cache so we read_dir once per directory).
Notes:
- Click on a row's label navigates to that path. Click on the caret
  (▸ / ▾) toggles expand-collapse via `Shell::toggle_expand`. The
  caret's on_click calls `cx.stop_propagation()` so the row's
  navigate handler doesn't also fire.
- Lazy enumeration: `ensure_tree_children` reads_dir on first expand,
  caches the result keyed by parent path. Subsequent toggles use the
  cache. Folder-only filtering (the file pane shows files; the tree
  shows hierarchy).
- Collapsing a folder removes the path AND all descendants from
  `expanded` so a re-open doesn't preserve obsolete sub-expansions.
  Cache is kept (cheap memory, fast re-expand).
- `--expand <path>` CLI flag: each path is canonicalised, then every
  ancestor is added to `expanded` + ensured in the cache, so the
  screenshot can show a fully-revealed tree branch.
- Sidebar wrapper still gpui-component's `Sidebar`. `TreeSection`
  impls `Collapsible + SidebarItem` so it slots in alongside future
  non-tree sections.
- Locations & Volumes share the same `TreeRow` rendering. Old
  feraille-controls had 4 sections (Recents, Favorites, Locations,
  Volumes); Recents + Favorites are deferred to a follow-on
  (require Ant Trail persistence integration with the tree view).

### UI zoom (Cmd+= / Cmd+- / Cmd+0)
Status: Not started
Old location: `feraille-app/src/main.rs::nudge_ui_scale` +
`reset_ui_scale` driven by `feraille_design::Tokens::scaled`.
New location: `crates/feraille-gpui/src/shell.rs` (Stage 9).
Notes: `feraille-design` linked in Stage 0 already. Apply scale to
tokens; persist via app_state.

### Breadcrumb edit mode (Cmd+L)
Status: Ported ✅ (Harvest Stage 9.b)
Old location: `feraille-app/src/main.rs::enter_breadcrumb_edit_mode`.
New location: `crates/feraille-gpui/src/shell.rs::on_edit_breadcrumb`
+ a `breadcrumb_editing` flag that flips the breadcrumb render to
an `Input` widget. Enter parses the path (with `~` expansion) and
navigates; Blur cancels.
Notes: `--edit-mode` CLI flag now functional.

### System theme follow ("Auto")
Status: Partially ported (Stage 9.a — startup detection only;
live observer + explicit "Auto" preference deferred.)
Old location: `feraille_shell_mac::start_system_theme_observer` +
ThemePreference::System branch in feraille-app.
New location: `crates/feraille-gpui/src/main.rs` — initial theme
defaults to `feraille_shell_mac::system_is_dark()` when no
`--theme` flag is set, instead of gpui-component's hard-coded
Light default. Live observer (responding to System Settings →
Appearance changes after launch) defers to a follow-on:
SettingsView needs an "Auto" radio option + an
`Arc<AtomicBool>` shared with a `start_system_theme_observer`
callback that the foreground executor polls.

### Keyboard-shortcuts help overlay (Cmd+/)
Status: Ported ✅ (Harvest Stage 9.b)
Old location: `feraille-app/src/main.rs` shortcuts_modal.
New location: `crates/feraille-gpui/src/keyboard_help.rs::render`
+ Shell state (`shortcuts_help_filter: Option<String>`,
`shortcuts_help_input: Entity<InputState>`).
Notes:
- Catalogue (`feraille-core::commands::all_commands`) is the
  source of truth; `format_shortcut` renders chords as macOS
  glyphs (⌘ ⇧ ⌥ + key).
- Modal renders inline in `Shell::render` as the topmost overlay
  layer (after dialog + notification layers). Backdrop dismisses
  on outside-click; the card stops propagation so inside-clicks
  don't dismiss.
- Live filter: typing in the search input narrows the catalogue
  by title or by formatted-chord substring (case-insensitive).
- `--shortcuts-help` opens with empty filter;
  `--shortcuts-help-filter <text>` seeds the filter — both
  functional via the CLI now.

---

## Native context menu (Stage 10)

### NSMenu via feraille_shell_mac::menu (MenuPlan)
Status: Not started
Old location: `feraille_shell_mac::menu::{MenuPlan, show_context_menu}`
already exists and is used by old app.
New location: Replaces the gpui-component PopupMenu in
`crates/feraille-gpui/src/file_list.rs::context_menu` (Stage 10).
Notes: Gains: Open With submenu (Launch Services), Tags row, Share
menu item, Services submenu (AppKit-populated). Domain code is
free via dep; the work is the threading through Shell handlers.

---

## Already ported in earlier phases (5.5)

### Live filter (Cmd+F)
Status: Ported ✅ (Phase 5.5.a)
Notes: gpui-component Input in the toolbar; name/kind substring
match; Escape clears.

### Real macOS file icons
Status: Ported ✅ (Phase 5.5.b)
Notes: NSWorkspace iconForFile via `feraille_fs_native::fetch_icon_rgba`
+ kind-keyed cache in `crates/feraille-gpui/src/icons.rs`.

### New Folder + Rename modals
Status: Ported ✅ (Phase 5.5.c)
Notes: gpui-component Dialog with TextInput. Cmd+Shift+N + F2.

### Tabstrip
Status: Ported ✅ (Phase 5.5.d)
Notes: Per-tab current_dir + history + selection; shared
filter/show-hidden/Table entity. Cmd+T new, Cmd+W close,
Ctrl+(Shift+)Tab cycle.

### Reveal in Finder / Copy Path / Move to Trash
Status: Ported ✅ (Phase 5.c)
Notes: Via /usr/bin/open -R, cx.write_to_clipboard, and
`feraille_fs_native::move_to_trash` respectively.

### OS drag-out
Status: Ported ✅ (Phase 5.e)
Notes: ExternalPaths on file rows; macOS backend handles
NSFilePromise + NSPasteboard automatically.

### Live file-system watcher
Status: Ported ✅ (Phase 5.b)
Notes: notify crate; 250 ms foreground-executor poll.

### Window state persistence (last_dir + show_hidden)
Status: Ported ✅ (Phase 5.d)
Notes: `crates/feraille-gpui/src/app_state.rs`. Window bounds
persistence deferred — gpui doesn't expose a clean observer hook.

### Settings panel (Appearance / Files / Layout / About)
Status: Ported ✅ (Phase 3)
Notes: `crates/feraille-gpui/src/settings.rs` — sidebar nav,
PreviewTile theme picker, live hidden-files count, Narrow/Medium/
Wide segmented control, About card.

---

## Not applicable

### Windows shell context menu / WSL integration
Status: N/A
Notes: New stack targets macOS-first; Windows isn't a v1 target.

### Soft renderer hot-path optimisations (icon prefetch chunking, etc.)
Status: N/A
Notes: The whole class of "paint must not allocate" / "prefetch
chunked across event-loop ticks" disappears with the GPU
pipeline. Background work is via `cx.spawn` + foreground executor
now.

### Custom rounded-rect / circle compositors
Status: N/A
Notes: GPUI draws via shape primitives natively. The old
`feraille-controls::primitives::draw` module exists only because
the soft renderer was rect-only.
