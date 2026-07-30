# Feraille Architecture

← [Project README](../README.md) · [Feature notes](features/README.md) ·
[Open work (TODO)](../TODO.md)

Feraille is a macOS-first file manager written in Rust. It began as a
port and UI rewrite of the Windows project Ferail,
but the active application is now the GPUI shell:

- `feraille-gpui` opens the desktop app.
- `feraille` is the command-line entry point for non-GUI utilities.

All new product work belongs in `crates/feraille-gpui`.

## Prime Directive

The UI must never stop. This rule is non-negotiable; every other design
choice in the app bends around it.

Painting, rendering, hit testing, hover, selection, scrolling, text
input, keyboard input, resize, and modal drawing must not perform I/O.
That includes filesystem reads, directory enumeration, metadata queries,
magic sniffing, SQLite queries, thumbnail or preview generation,
network/cloud access, symlink or alias resolution, and shell queries that
can block.

The hot path may only read already-cached app state, update small
in-memory interaction state, draw placeholders, and enqueue work through
a constant-time scheduler.

The rule extends beyond render code: **semantic event handlers (action
handlers, click handlers, subscriptions) run on the UI thread too**, and
a single blocking syscall there freezes every open window. A call that
returns in microseconds on a local SSD can take *seconds* against a
spun-down external drive, a cold network mount, or a cloud placeholder —
so "it's fast on my machine" is never an argument. Calls that look
innocent but block on slow media include:

- `Path::exists` / `metadata` / `canonicalize` / `read_dir` — all stat.
- `notify`'s FSEvents `Watcher::watch()` — canonicalizes the path
  internally (this froze navigation onto sleeping drives until watch
  registration moved to the `fs-watcher` worker thread).
- NSWorkspace / LaunchServices lookups, xattr reads, Quick Look.

### The compliant pattern

Every feature that touches the filesystem follows the same shape (see
`Shell::load_path_for_tab` for the canonical example):

1. A semantic event (navigation, refresh, selection) **schedules** work;
   it never performs it.
2. The work runs on `cx.background_executor()` (or a dedicated worker
   thread), carrying a generation counter and a cooperative cancel flag.
3. Results stream back through entity updates
   (`this.update(cx, …)`) — the only place UI state mutates.
4. A result that arrives after the user moved on (generation mismatch,
   tab closed, path changed) is **dropped**, not applied.
5. If the work might be slow, the UI shows cached/placeholder state
   immediately and upgrades when results land (e.g. the file pane's
   skeleton loading view appears only after `SLOW_LOAD_INDICATOR_DELAY`
   without a first batch — fast loads never flash it).

### Enforcement

The directive is enforced by the program, not just this document —
`feraille_core::path_guard` holds two debug-build tripwires:

- **Render guard**: render implementations hold a `RenderPathGuard`;
  resolving a NodeId to a path while one is alive panics
  (`assert_path_resolution_allowed`).
- **UI-thread guard**: `boot::run_gui` marks the UI thread at startup
  (`mark_ui_thread`), and known-blocking entry points in
  `feraille-fs-native` call `assert_off_ui_thread` — running one on the
  UI thread panics in debug builds with a pointer back to this section.
- **Lint wall**: `crates/feraille-gpui/clippy.toml` disallows the
  syscalls most likely to freeze the UI (`canonicalize`, blocking
  `Command::output`/`status`, raw `notify` watch registration) and the
  crate denies `clippy::disallowed_methods`. A legitimate off-thread
  use carries a per-site `#[allow]` with a justification comment — that
  annotation is the review marker.

When one of these fires, the fix is never to remove the guard: move the
work to the background executor and report back through an entity
update. When you add a new blocking entry point, add the assert to it.

## Crate Boundaries

```text
feraille-gpui        active GPUI app and CLI entry points
  |-- feraille-core          domain types, command catalogue, NodeId, FileEntry
  |-- feraille-fs-native     native filesystem, metadata, magic, volumes, trash
  |-- feraille-shell-mac     AppKit/Cocoa integrations
  |-- feraille-meta          SQLite-backed metadata and layout persistence
  |-- feraille-disk-usage    pure disk-usage model, facts, aggregation, treemap
  |-- feraille-design        shared visual constants (color, spacing, typography)
  `-- gpui-component         UI primitives for shell, settings, tables, menus

feraille-shell-win32 Windows reference/platform shell crate, not macOS v1 UI
```

Rules:

- `feraille-core` has no UI or platform dependencies.
- Domain crates do not import GPUI, renderers, or app shell state.
- UI code uses `NodeId`, `FsBackend`, cached display strings, and explicit
  node/path handoff points.
- Raw `PathBuf` use is allowed at controlled boundaries: filesystem
  backends, worker setup, native shell calls, CLI commands, and persisted
  user state. Rendering code must not resolve paths or query the filesystem.
- `feraille-shell-mac` owns AppKit/Cocoa details and does not paint UI.
- `feraille-disk-usage` is pure logic; scanning lives in
  `feraille-fs-native`, and rendering lives in `feraille-gpui`.

## Data Model

`NodeId` is the opaque identity used by the UI for filesystem objects.
`NativeFs` owns the current `NodeId <-> PathBuf` mapping. GPUI shell state
keeps a `NodeStore` so tabs, sidebar rows, table rows, context menus, and
worker results can speak in stable ids where possible.

Path-identity contract: both maps key on
`feraille_core::node_store::normalize_path_key`, a lexical-only
normalization (trailing slashes, `.` segments, doubled separators fold
together; no filesystem access). Case, symlinks, and `..` are
deliberately NOT folded by the key — case-insensitivity is a per-volume
property and folding would corrupt identity on case-sensitive volumes;
the other two need filesystem knowledge. Paths from outside the app
(typed breadcrumbs, CLI args, external drops) are canonicalized once at
their entry boundary; internal flows are consistent by construction
because children are built from already-registered parent paths.

`FileEntry` is the file-list row model. It contains preformatted display
fields:

- `name`
- `display_name`
- `name_has_hazards`
- `kind`
- `size`
- `mtime_unix`
- `display_size`
- `display_mtime`
- `display_kind`
- `display_magic`
- `is_quarantined`
- `quarantine`

`FileEntry::format_label()` is the shared rule for the file list's
`Format` column: prefer magic-detected content, fall back to
extension-derived kind, and flag real mismatches.

Raw name vs. display name: `name` is the on-disk leaf (the bytes `readdir`
returned) and is the *only* value used to reconstruct a path — joins,
renames, opens. `display_name` is what the user reads. They differ on macOS
because of its two inherited path separators: HFS/classic Mac OS used the
colon, Unix/NeXTSTEP the slash, so the POSIX layer stores a `:` *inside* a
name component where Finder shows a `/` (a file `ls` reports as `a:b` is
`a/b` in Finder). `feraille_fs_native::paths::display_leaf` performs the
Finder-parity swap (`:` → `/`, macOS only; identity elsewhere) when the
backend builds each `FileEntry`, and its inverse `on_disk_leaf` (`/` → `:`)
runs on names typed into the rename / New-Folder fields. Every user-facing
surface — list rows, grid cells, the preview header, Get Info, breadcrumb
segments, the window title, sidebar-tree labels, drag-ghost chips, and the
search/sort keys — renders `display_name` (or routes a path leaf through
`display_leaf`); only path operations touch `name`. This is the seam where
future per-platform display quirks plug in. `name_has_hazards` is
precomputed (`name_hazards::has_hazards(&display_name)`) so the dense row
paint decides on a bool whether to draw the deceptive-character highlight,
never running the analysis itself (Prime Directive).

Each tab owns its current directory, node id, history, filter, sort,
selection, and scroll state. The shell owns cross-tab state such as the
sidebar, task registry, notifications, file watcher, preview cache, and
metadata caches.

## Work Scheduling

Expensive work follows one pattern:

1. A semantic event schedules work: navigation, selection, visible row,
   preview request, context-menu request, disk-usage scan, or CLI command.
2. The request carries enough identity to reject stale results:
   generation id, tab/folder/node id, path where unavoidable, and file
   metadata when needed.
3. Work runs on a background executor or in a bounded main-thread chunk
   when AppKit requires the main thread.
4. Results return to the UI through GPUI `cx.spawn`/entity update
   boundaries.
5. The UI applies the result only if it still matches current state.
6. Render reads the cached result on a later frame.

Main-thread-only AppKit work, such as some `NSWorkspace` icon calls, must
be chunked across ticks with small per-frame budgets. Chunking is a
fallback for APIs that cannot safely move to workers.

## Active UI Structure

The GPUI shell uses native macOS chrome with a gpui-component title bar.
The title bar owns global navigation controls and the filter input.

The main shell is split into resizable panels:

- Sidebar
- Virtualized file table
- Preview pane

The sidebar has three concepts:

- **Favorites:** flat shortcuts such as Home, Applications, Desktop,
  Documents, Downloads, Trash, Movies, Music, and Pictures.
- **Browse:** a single expandable Home tree.
- **Volumes:** mounted volumes with capacity bars and drive icons.

The file table is backed by `gpui_component::table::TableState`.
Columns are Name, Size, Format, and Modified. Columns can be sorted,
resized, and reordered. Cell rendering is keyed by column id so moved
columns keep headers and content aligned.

The preview pane shows the current selection's media (a content thumbnail —
Quick Look, embedded audio cover art, or an mpv video poster frame; see
docs/features/PREVIEW.md — or inline highlighted text) plus the name and a
"Get Info" button. The dense attribute rows it used to carry now live in the
Get Info popup.

Get Info opens a standalone, resizable, movable window (`crate::entry_info`)
via the `GetInfo` command (Cmd+I, context menu, toolbar) — not a modal, so
several can be open at once for different files. The same `EntryInfoView`
also renders inline in the preview pane (an `embedded` mode that drops the
window chrome and defers scrolling/notifications to the shell window). A
background `gather()` composes a platform-neutral
`feraille_core::entry_info::EntryInfo` from POSIX stat
(`feraille-fs-native::stat_info`), batched NSURL resource values
(`feraille-shell-mac::resource_values`), volume info, magic, and tags — never
on the paint path. The record drives a dense, editable form: Locked /
Invisible / Hide-extension toggles, color labels, a POSIX permission grid,
and an on-demand "Calculate" recursive size. Edits write through the native
crates (chflags / chmod / setResourceValue / tag write), reload the affected
directory, and re-gather. The model is neutral so Windows/Linux fill the
subset they can read.

Settings are implemented with `gpui_component::setting::Settings`.
The settings sidebar is searchable, category rows have icons, and fields
write directly to app state.

Disk Usage opens as a separate GPUI window. Scanning is performed by
`NativeFs::scan_disk_usage`; aggregation and layout live in
`feraille-disk-usage`.

## Typography And UI Scale

Division of labour: the **gpui-component theme** (`cx.theme()`) owns colors
and the **base font size** (`theme.font_size`, default 16px — the rem base
that `Root::render` pumps into the window `rem_size` every frame). It does
*not* provide a named multi-tier type scale for chrome — only that one base
plus per-widget `Sizable` tiers. `feraille-design` fills that gap: a named
scale layered on top of the theme's rem base.

All chrome text is sized through that one design-token scale — never gpui's
raw `.text_xs()` / `.text_sm()` Tailwind helpers. Those bake in a looser
scale that can't be retuned in one place and silently drift whenever a
component defaults to a different tier (that is how the Get Info permission
grid ended up rendering its `r`/`w`/`x` labels oversized).

- **Source of truth:** `feraille_design::TextTokens::BASE` — six tiers
  (`xxs` 10, `xs` 11, `sm` 12, `md` 13, `lg` 15, `xl` 18 logical px; a dense
  Zed-aligned scale). Retune the whole app by editing those six numbers.
- **Applied via:** the `crate::text::TextScale` extension trait. Use
  `.text_scale_xs()` … `.text_scale_xl()` (or `.text_token(TextSize::…)`)
  anywhere you would reach for `.text_xs()`. It sets the font size
  **rem-relative** (`token_px / 16`), so it cascades exactly like gpui's
  helpers.
- **UI zoom:** `Shell::ui_scale` (Cmd+= / Cmd+- / Cmd+0, persisted) feeds the
  framework's own hook — `Shell::apply_ui_zoom` writes
  `theme.font_size = 16 * ui_scale`, and `Root` copies that into the window
  rem size each frame. Because every text size is rem-relative the whole
  window scales together, **including Root-level overlays** (notifications,
  dialogs) — which a `set_rem_size` confined to the shell subtree would miss.
  Re-applied after a `Theme::change` (appearance flip can reset the base).
  `ui_scale == 1.0` is the gpui default, a no-op. Fixed-px *layout* scaling
  (pane widths, row heights) is still TODO.
- **gpui-component widgets** (Checkbox, Button, Switch, …) carry their own
  text via the `Sizable` trait, not the tokens. Size them to match the dense
  scale — `.xsmall()` inline with body text, `.small()` in dialogs — instead
  of leaving the `Medium` (16px) default. `Sizable` is also rem-relative, so
  these zoom too.
- **Chrome icons** scale with text via the `crate::text::IconScale` trait:
  `gpui::svg(…).icon_px(24.0)` sizes a glyph rem-relative (its `px` is the
  size at `ui_scale == 1`), so the sidebar icons, the cloud/star/eject
  accessories, and the file-list badges grow with zoom. gpui-component's
  `Icon` already inherits the rem-scaled ambient font size unless given an
  explicit `with_size(px(…))` — the one such site (sidebar locations)
  pre-multiplies by `ui_scale` instead, since `Size` is px-only.
- **Exceptions kept on explicit `px`:** glyph affordances pinned to a
  fixed-size box (disclosure triangles, the favorites `+`, the viewer seek
  grip); the code-block preview font (a separate "content font" axis, cf.
  Zed's buffer vs UI font); grid thumbnails + their overlay badges (their own
  icon-size axis — the size slider); and the drag-ghost chip. These are
  deliberately outside the UI scale.

**Rule for new code:** size chrome text with `text_scale_*` / `text_token`,
chrome icons with `icon_px`, component widgets with `Sizable`; never add a raw
`.text_xs()` or a bare `px(N)` font/icon size for chrome.

## Context Menus And Native Actions

Context menus use gpui-component menus where possible. Menu handlers set
or read a semantic target: selected row, context row, sidebar path,
tree path, breadcrumb segment, tab, or disk-usage item.

Native actions are delegated to platform crates:

- default open and reveal in Finder
- macOS Trash
- Open With candidates
- Finder tags
- share/services where wired
- clipboard operations

File mutations reload affected UI state through the shell rather than
mutating rendered rows in place.

## Persistence

`feraille-meta` is the persistent store for app metadata and layout state.
It stores derived file metadata, Ant Trail data, window/layout state, and
other app-owned records.

Simple UI preferences also flow through `crates/feraille-gpui/src/app_state.rs`.
Anything persisted must be treated as a cache or user preference, never as
the sole source of filesystem truth.

## Command Surfaces

Feraille has three command surfaces:

- GPUI actions and keybindings in the app.
- macOS menu items generated from the command catalogue.
- CLI subcommands through the `feraille` binary.

Useful CLI commands:

```sh
cargo run --bin feraille-gpui
cargo run --bin feraille -- magic <path>...
cargo run --bin feraille -- du [--top N] [--packages] <path>
cargo run --bin feraille-gpui -- --screenshot screenshots/shell.png
cargo run --bin feraille-gpui -- --reset-db <scope>
```

The command catalogue lives in `feraille-core` so menus, shortcuts,
settings, and future command-palette work share one identity layer.

Most shortcuts bind under `SHELL_CONTEXT`, so they only fire when a
Shell window has focus. A command that must still work when the process
is resident with **zero windows** — `window.new_window` (Cmd+N),
`go.go_to_folder` (Cmd+G) — binds with no context and pairs the
Shell-level handler with an App-level `cx.on_action` fallback in
`boot`. Element handlers win the bubble phase and stop propagation, so
the fallback only runs when the action reached no window; it checks
`cx.windows().is_empty()` before opening one, which keeps it inert
while a window (or a dialog inside one) owns the key.

## macOS Privacy (TCC) And Bundling

A directory read that hits macOS privacy protection comes back as
`EnumerationError::PermissionDenied` and the file pane shows an "Access
required" state that deep-links to Full Disk Access (`shell/loading.rs`,
`shell/render.rs`).

To get the *automatic* per-folder consent prompt instead (the
"…would like to access files in your Documents folder" dialog), Feraille
must run as a code-signed `.app` bundle whose `Info.plist` declares the
matching `NS*UsageDescription` strings. The prompt only fires for
promptable categories (Desktop/Documents/Downloads/removable/network);
arbitrary folders still need Full Disk Access, which can't be prompted.
`scripts/bundle-mac.sh` assembles and signs the bundle from
`packaging/macos/Info.plist`. Running the loose `cargo run` binary cannot
prompt — it has no bundle identity or usage strings.

## Observability And Failures

The app installs a panic hook and compact crash report path through
`crates/feraille-gpui/src/obs.rs`. Worker failures should surface through
logs, status/task state, notifications, or visible error states. They
must not freeze the interface.

Failure policy:

- Keep current UI usable.
- Preserve selection and scroll where possible.
- Prefer stale or missing metadata over blocking.
- Drop stale worker results.
- Report errors through the app, not only stderr.

## Documentation Layout

- The [project README](../README.md) is the entry point and overview.
- This file is the architecture source of truth.
- Root [TODO.md](../TODO.md) is the unfinished-work list.
- [docs/features/](features/README.md) contains deeper feature notes and
  design references, organized by the [feature index](features/README.md).
- Root [NOTES.md](../NOTES.md) is the decision log for in-progress spec work.

Do not add new phase ledgers or duplicate roadmaps under `docs/`. Put
current architecture here and unfinished work in root `TODO.md`.
