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

### Native macOS menu bar
Status: Not started (re-ordered after Stage 3)
Old location: `feraille_shell_mac::install_app_menu` driven by
`feraille_core::commands` catalogue.
New location: Replaces the stub `install_app_menus` in
`crates/feraille-gpui/src/main.rs` (was Stage 1.c; now Stage 3.b).
Notes: The new app currently builds a hand-rolled menu via
`gpui::Menu`. Replacing it depends on the command-catalogue
dispatcher from Stage 3 so menu items / keyboard shortcuts /
about-panel share one source of truth. Stage 1 covers obs + DB
foundations; the AppKit menu wiring lands once Stage 3's command
table is in place.

---

## CLI (Stage 2)

### Screenshot CLI flag parity
Status: In progress
Old location: `crates/feraille-app/src/screenshot.rs::parse_args` (25 flags)
New location: `crates/feraille-gpui/src/screenshot.rs::parse_args` (5 flags)
Notes: Flags already in new app: `--screenshot`, `--width`,
`--height`, `--theme`, `--settings`. Stage 2 adds the remaining
~20: --navigate, --new-tab, --tab, --expand, --select-row,
--select-name, --splitter, --scroll, --edit-mode, --show-hidden,
--filter, --search, --preview, --sort, --properties, --mac-chrome,
--rename, --inline-rename, --new-folder, --simulate-toast,
--simulate-progress, --simulate-task-panel, --shortcuts-help,
--ui-scale, --disk-usage, --du-depth, --du-coloring.

---

## Command catalogue + keymap (Stage 3)

### Command catalogue → first-class keymap
Status: In progress
Old location: `crates/feraille-core/src/commands.rs` (65 commands,
already in domain layer) + `feraille-app/src/main.rs` keystroke
dispatch.
New location: `crates/feraille-gpui/src/shell.rs` — currently uses
ad-hoc `actions!` for ~18 keybinds. Stage 3 rewires through the
shared catalogue so menu / keyboard / context-menu / native menu
bar all dispatch through one path.
Notes: New keybinds wired so far: NavigateParent/Back/Forward,
OpenSelected, Refresh, ToggleHidden, OpenSettings, CopyPath,
MoveToTrash, RevealInFinder, FocusFilter, ClearFilter, NewFolder,
RenameSelected, NewTab, CloseTab, NextTab, PrevTab.

---

## Background prefetch (Stage 4)

### Magic byte sniffing prefetch
Status: Not started
Old location: `feraille-app/src/main.rs::start_magic_prefetch` (~line 595)
+ DB write-through via `feraille-meta::MagicBatch`.
New location: `crates/feraille-gpui/src/shell.rs` (Stage 4)
Notes: `feraille_fs_native::detect_magic` is the sync function;
wrap in `cx.spawn` per the file_watcher pattern in 5.b. Hydrate
on navigate via `feraille_meta`. Adds `display_magic` column to
the file Table.

### Quarantine prefetch + display
Status: Not started
Old location: `feraille-app/src/main.rs::start_quarantine_prefetch`
+ `feraille_fs_native::fetch_quarantine_info` + DB row write.
New location: `crates/feraille-gpui/src/shell.rs` (Stage 4)
Notes: Sets `entry.is_quarantined` + populates `QuarantineDetails`.
Surface as a badge dot in the icon column (Stage 6) + in the Get
Info pane (Stage 8).

---

## Tasks + status bar + toasts (Stage 5)

### TaskRegistry
Status: Not started
Old location: `crates/feraille-app/src/tasks.rs` (~200 lines)
New location: `crates/feraille-gpui/src/tasks.rs` (Stage 5)
Notes: Pure logic, verbatim port. Same Kind enum (Enumeration,
IconPrefetch, MagicPrefetch, QuarantinePrefetch, DiskUsage, FileOp).

### Status bar
Status: Not started
Old location: `feraille-app/src/main.rs` status-bar paint arm.
New location: `crates/feraille-gpui/src/status_bar.rs` (Stage 5)
Notes: New gpui-component build. Shows active task count + shared
progress strip. Hover popover ties into the task panel.

### Task panel (popover)
Status: Not started
Old location: `crates/feraille-app/src/task_panel.rs` (~100 lines,
soft-renderer coupled).
New location: `crates/feraille-gpui/src/task_panel.rs` (Stage 5,
rewrite from scratch using gpui-component Popover).
Notes: Old impl too tied to the soft renderer to copy. List of
active tasks with cancel buttons.

### Toast notifications
Status: Not started
Old location: `feraille-controls::ToastStack` + drive points in
`feraille-app/src/main.rs`.
New location: gpui-component primitive (probably Notification) +
shell-side trigger points (Stage 5).
Notes: Time-based dismiss. `--simulate-toast <text>` CLI flag for
visual verification.

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
Status: Not started (basic preview pane exists from Phase 4.d)
Old location: `feraille-app/src/main.rs::toggle_properties` + the
Settings-modal-dual-use detail pane.
New location: `crates/feraille-gpui/src/properties.rs` (Stage 8).
Notes: Expand the current preview pane into a full Get Info panel
(magic, quarantine, full path, permissions, size on disk vs
apparent). Cmd+I binding.

### Quick Look (Space-bar action + thumbnails)
Status: Not started
Old location: `feraille_shell_mac::quick_look::{show, fetch_thumbnail}`
already exists and is used by old app.
New location: `crates/feraille-gpui/src/shell.rs` action handler +
the Get Info pane's inline thumbnail (Stage 8).
Notes: Domain code reused as-is via the linked feraille-shell-mac
crate. Just thread the call.

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

### UI zoom (Cmd+= / Cmd+- / Cmd+0)
Status: Not started
Old location: `feraille-app/src/main.rs::nudge_ui_scale` +
`reset_ui_scale` driven by `feraille_design::Tokens::scaled`.
New location: `crates/feraille-gpui/src/shell.rs` (Stage 9).
Notes: `feraille-design` linked in Stage 0 already. Apply scale to
tokens; persist via app_state.

### Breadcrumb edit mode (Cmd+L)
Status: Not started
Old location: `feraille-app/src/main.rs::enter_breadcrumb_edit_mode`.
New location: `crates/feraille-gpui/src/shell.rs::breadcrumb`
(toggle TextInput in place of segments) (Stage 9).
Notes: Enter navigates, Escape cancels.

### System theme follow ("Auto")
Status: Not started
Old location: `feraille_shell_mac::start_system_theme_observer` +
ThemePreference::System branch in feraille-app.
New location: `crates/feraille-gpui/src/main.rs` startup + a new
ThemeMode::Auto handling layer (Stage 9).
Notes: Current new app supports Light/Dark only. Add Auto via the
observer callback.

### Keyboard-shortcuts help overlay (Cmd+?)
Status: Not started
Old location: `feraille-app/src/main.rs` shortcuts_modal.
New location: `crates/feraille-gpui/src/keyboard_help.rs` (Stage 9).
Notes: Modal listing the full command catalogue with live filter.
The catalogue (feraille-core::commands) is the source of truth.

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
