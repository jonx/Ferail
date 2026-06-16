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

The UI must never stop.

Painting, rendering, hit testing, hover, selection, scrolling, text
input, keyboard input, resize, and modal drawing must not perform I/O.
That includes filesystem reads, directory enumeration, metadata queries,
magic sniffing, SQLite queries, thumbnail or preview generation,
network/cloud access, symlink or alias resolution, and shell queries that
can block.

The hot path may only read already-cached app state, update small
in-memory interaction state, draw placeholders, and enqueue work through
a constant-time scheduler.

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

The preview pane shows the current selection's media (Quick Look thumbnail
or inline highlighted text) plus the name and a "Get Info" button. The dense
attribute rows it used to carry now live in the Get Info popup.

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
