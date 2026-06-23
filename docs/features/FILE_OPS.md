# File Operations: Copy, Paste, Move

Design for the copy/cut/paste/move worker stack — the TODO item "copy,
cut, paste, duplicate, and move workers with visible task progress and
collision handling". Duplicate/Compress/Trash/Rename/NewFolder already
exist; this document covers the engine they'll converge on and the new
clipboard verbs.

← [Feature notes index](README.md) · [Architecture](../ARCHITECTURE.md) ·
[TODO](../../TODO.md)

## Status

**Landed (v1, 2026-06-13)** — engine, progress + cancel UI, and the
clipboard verbs shipped. Deviations from the design, deliberate:

- The collision dialog renders all three policies as explicit buttons
  (Keep Both primary / Replace / Skip Existing) — the pinned
  gpui-component rev doesn't draw the plain Dialog's ok/cancel footer
  alongside custom children. ✕/Esc cancels the whole operation via
  the dropped-sender path.
- Verified end-to-end through the screenshot harness driving real
  keystrokes: Cmd+C → Cmd+V round-trip (byte-identical), collision
  dialog capture, Esc-cancel, Cmd+Option+V same-volume move.

**Drag-into-app (v1) also landed 2026-06-13**: folder rows in the file
table (accent ring on hover, drop surfaces as the fork's
`TableEvent::ExternalDrop`), the file pane background (drops into the
current directory), and Browse/Volumes tree rows all feed
`Shell::handle_external_drop`. Operation per dnd-spec §3.6 via
`TransferMode::Auto` — the worker resolves same-volume → Move,
cross-volume → Copy; Option forces Copy, Cmd forces Move; dropping
items onto the folder they're already in is a no-op (Option-drop
duplicates). Covers internal row drags and external Finder drags
through the same `ExternalPaths` payload. OS drag gestures can't be
driven headlessly — interactive verification pending.

**Fast-copy ladder + rich progress (2026-06-18):**

- **Speed ladder** in the engine, fastest legal mechanism per top-level
  item: (1) same-volume copy → `clonefile(2)` APFS copy-on-write —
  instant, zero bytes, whole tree in one syscall; (2) cross-volume copy →
  `copyfile(3)` with a status callback — kernel-optimized, preserves
  holes (sparse), xattrs/ACLs/flags, reports intra-file progress and
  honors cancel via `COPYFILE_QUIT`; (3) non-mac fallback → the chunked
  read/write loop. Same-volume move stays `rename(2)`. The old hand-rolled
  8 MiB loop dropped xattrs (tags/quarantine/where-from) and inflated
  sparse files — both fixed by `copyfile`. **[mac]** for tiers 1–2.
- **Atomic-snapshot progress** replaces the per-chunk channel: the worker
  bumps lock-free `TransferProgress` counters (no channel, no per-file
  alloc; the current-item name is published throttled to ~10 Hz), and the
  UI samples them on its own clock and derives rate/ETA. The copy can't
  be slowed or stalled by drawing its progress — Prime Directive, made
  structural. `plan_transfer`/`run_copy`/`run_move` take `&TransferProgress`
  instead of a `FnMut` callback.
- **Rich task display**: `TaskKind` is now split ambient vs. foreground —
  foreground (file ops, search, scans) wins the status-bar primary slot
  so a copy never hides under a later prefetch; the task panel shows
  counts · bytes · rate · ETA + the file in flight. Tasks are surfaced
  only after `SURFACE_DELAY` (150 ms) so instant clones never flicker.
  A planning phase ("Preparing — N items") covers the pre-transfer walk.

**Follow-ups shipped (2026-06-18):**

- **Drops on tabs + breadcrumb segments** — a tab chip accepts a file
  drop into *that tab's* folder (resolved by `TabId` at drop); each
  breadcrumb segment accepts a drop into that ancestor folder. Both
  reuse `handle_external_drop` (shell/render.rs).
- **Cmd+Option alias-drop** — holding the alias modifier on any drop
  target writes a Finder alias per source into the dest instead of
  copy/move (`make_alias_in`; `handle_external_drop`).
- **Per-item collision resolution** — the batch dialog became a per-item
  loop: one prompt per conflicting top-level item (Keep Both / Replace /
  Skip) with an "apply to the remaining N" checkbox that fills the rest.
  The engine's `run_copy`/`run_move` now take `Fn(&Path) -> CollisionPolicy`.
- **Cut (Cmd+X)** — copies URLs to the pasteboard and marks them
  (`ProcessState::cut_marker`, an `Rc<RefCell<Vec<PathBuf>>>` shared with
  each file-list delegate); the next plain Paste of exactly that set is a
  Move (then clears the mark), and marked rows render dimmed (0.45).

**Dragging *from* the list (2026-06-18):** rows already drag out to
Finder via `ExternalPaths` (NSFilePromise); added a Finder-like cursor
ghost (`DragBadge`: the item's icon or warmed Quick Look thumbnail +
name, or "N items" with a stacked-card hint) that tracks the pointer.
gpui paints the drag view at `mouse − cursor_offset` and `cursor_offset`
is the grab point within the *dragged element* — for a full-width row
that pins the ghost to the row's left edge, so the badge re-anchors
itself under the cursor with left/top padding equal to that offset. And
**spring-load**: dwelling a drag (~600 ms) over a folder row
drills into it (`TableEvent::DragHover` → `Shell::spring_load`), and over
a collapsed sidebar tree row expands it (`Shell::tree_drag_hover`), so
you can reach a nested destination without releasing. Implemented via
`on_drag_move::<ExternalPaths>` (Capture-phase, hands us element bounds +
cursor position).

Still open in [TODO.md](../../TODO.md): auto-scroll near the list edges
while dragging (needs `UniformListScrollHandle` offset access); drops on
favorite *rows* (gaps accept folder-adds today); Windows pasteboard
volume-identity parity and the `.lnk` alias-in-dest path. Note: all drag
gestures are OS-driven, so they can't be exercised by the screenshot harness —
verify interactively.

## Feedback UX Policy

Feraille's mutation feedback should stay calm. The UI itself is the confirmation
for direct, visible actions; notifications are for attention, ambiguity, or
failure.

### Success

Do not show success notifications for immediate, visible edits:

- New Folder.
- Rename.
- Quick alias creation.
- Instant copy/move/duplicate/compress work that never surfaced in the task
  UI.

Show success only when at least one of these is true:

- The task lived long enough to appear in the status bar or task panel.
- The result is not visually obvious from the current view.
- The action was destructive, delayed, or happened in another window/context.
- The user explicitly requested background work and may have moved on.

Implementation rule: task-backed actions use `TaskRegistry::end_and_was_surfaced`
to decide whether a success toast is warranted. Sub-150 ms work stays silent.

### Failure

Failures always surface. The notification should include:

- The user-facing operation name.
- The raw underlying error or enum/code when available.
- A practical next action.

Examples:

- `Rename failed: Permission denied (os error 13). Check permissions for the item or grant Feraille access in System Settings.`
- `Search failed: NotFound. The folder may have moved, been deleted, or been unmounted. Refresh the parent location and try again.`
- `Compress failed: ditto exited with 1: ... Free space on the destination volume or choose another destination.`

Do not hide technical details. Feraille users are expected to be comfortable
with OS/tool errors; the app adds context and advice instead of replacing the
real cause with vague friendly copy.

### Resilient failures — per-item reporting + coping (2026-06-23)

A copy/move no longer aborts the whole batch on the first item's failure, and
no longer flattens the cause into a bare string. The engine
(`feraille-fs-native/src/file_ops.rs`) classifies each per-item `io::Error`
into a **`FileOpError { kind, path, raw, os_code }`** (`FileOpErrorKind` =
`PermissionDenied | Locked | NotFound | NoSpace | ReadOnly | NameTooLong |
AlreadyExists | Other`), records it in **`OpOutcome.failed`**, and **keeps
going**. So a 10-item paste that trips on item 3 still attempts 4–10.

`spawn_transfer_op` surfaces a transparent toast — *"Move: 7 of 10 done · 3
failed"* with the first few items and their plain-language reasons — that
always appears (even sub-150 ms ops) and carries actions to **cope**:

- **Copy** — the raw detail to the clipboard for a bug report.
- **Retry** — re-run just the failed top-level items in-process.
- **Retry as administrator…** — shown when a failure is a bare permission
  denial and the platform can elevate. It re-runs the failed items elevated by
  re-launching this binary with `--elevated-op <descriptor>` (one OS auth
  prompt; the worker performs the op as root/admin and writes a result file).
  See `crate::elevation` + `platform_shell::{elevation_available,
  run_elevated_self}`.

`FileOpErrorKind::is_lock()` + `platform_shell::{processes_using,
force_close_processes}` back a future "the file is open in X — close it and
retry / force-close" affordance. The lock primitives are **Windows-native
(Restart Manager) and deferred** to the on-device session; macOS/Linux return
empty for now. The string-error surfaces (rename, duplicate, compress, alias)
share the one advice table via `classify_error_text`.

**Platform status:** transparency + classification are cross-platform; macOS
ships real osascript elevation; Windows/Linux elevation + lock detection are
stubbed (`elevation_available()` = false) pending the native pass — see
windows-port.md.

## Platform tags

**[mac]** = macOS-only today; **[win-parity]** = named Windows
equivalent for later. Untagged = platform-neutral.

## Existing anchors (verified 2026-06-13)

| What | Where |
|---|---|
| `spawn_file_op(reload_path, op, label, cx)` — background op + reload broadcast, no progress/cancel/notify | `crates/feraille-gpui/src/shell.rs:1733` |
| `TaskRegistry::begin_with_cancel(kind, label, flag)` / `update_transfer` / `end` / `end_failed`; foreground tasks keep status-bar priority | `crates/feraille-gpui/src/tasks.rs` |
| Task panel renders active + recent tasks, transfer details, and cancel buttons backed by cooperative flags | `crates/feraille-gpui/src/task_panel.rs` |
| gpui modal dialogs (`window.open_dialog` + Input) used by Rename / NewFolder | `crates/feraille-gpui/src/shell/file_ops.rs:446-559` |
| Finder-style collision naming `pick_suffixed_name(parent, stem, ext, "copy")` | `crates/feraille-shell-mac/src/file_ops.rs:38` **[mac]** |
| NSPasteboard URL read/write helpers for file copy, cut, and paste | `crates/feraille-shell-mac/src/lib.rs` **[mac]** |
| Undo stack `UndoOp::{Rename, DeleteFolder, …}` + `apply_fs()` | `crates/feraille-gpui/src/shell.rs:72` |
| Reload fan-out `broadcast_reload_for_process(process, paths, cx)` | `crates/feraille-gpui/src/shell.rs:1715` |
| Cancel-aware walker precedent `recursive_size(root, cancel)` | `crates/feraille-fs-native/src/disk_usage_scanner.rs:246` |

## Architecture

### The engine — `feraille-fs-native/src/file_ops.rs` (platform-neutral)

Pure, synchronous, worker-thread functions; the GPUI side owns
scheduling. Mirrors the `recursive_size` contract: cooperative
`&AtomicBool` cancel, progress via the shared `TransferProgress` sink.

```rust
pub struct OpPlan {
    pub sources: Vec<PathBuf>,     // top-level items being copied/moved
    pub dest_dir: PathBuf,
    pub total_bytes: u64,          // file bytes (dirs walk free)
    pub total_items: u64,          // files + dirs, for item-granular progress
    pub conflicts: Vec<PathBuf>,   // top-level dest paths that already exist
    pub source_bytes: Vec<u64>,    // per-source byte subtotal (clone/rename credit a whole item at once)
    pub source_items: Vec<u64>,    // per-source item subtotal
}

pub enum CollisionPolicy { Replace, KeepBoth, Skip }

pub struct OpOutcome {
    pub created: Vec<(PathBuf, PathBuf)>, // (source, destination) per top-level item actually produced
    pub skipped: u64,
    pub replaced: u64,
    pub cancelled: bool,
}

/// Lock-light progress sink shared worker↔UI. Worker bumps atomics
/// (no alloc, no channel); current-item name published throttled.
pub struct TransferProgress { /* atomics + Mutex<name> */ }
impl TransferProgress {
    pub fn new() -> Self;
    pub fn is_planning(&self) -> bool;          // pre-transfer walk phase
    pub fn planned(&self) -> u64;               // items counted so far while planning
    pub fn bytes_done(&self) -> u64;            // + bytes_total/items_done/items_total
    pub fn current(&self) -> Arc<str>;          // file in flight
    // hot path: add_bytes / add_items / note_current / note_planned / begin_transfer
}

pub fn plan_transfer(sources: &[PathBuf], dest_dir: &Path,
                     prog: &TransferProgress, cancel: &AtomicBool) -> Result<OpPlan, String>;

pub fn run_copy(plan: &OpPlan, policy_for: &dyn Fn(&Path) -> CollisionPolicy,
                prog: &TransferProgress, cancel: &AtomicBool) -> Result<OpOutcome, String>;

pub fn run_move(plan: &OpPlan, policy_for: &dyn Fn(&Path) -> CollisionPolicy,
                prog: &TransferProgress, cancel: &AtomicBool) -> Result<OpOutcome, String>;

pub fn same_volume(a: &Path, b: &Path) -> bool;  // unix: MetadataExt::dev()
                                                 // [win-parity: GetVolumePathNameW]
```

Rules:

- **Copy** picks the fastest legal mechanism per top-level item:
  same-volume → `clonefile` (instant CoW, whole tree, credits
  `source_bytes[i]` in one jump); cross-volume → `copyfile` per file
  (sparse + xattrs/ACLs/flags preserved, intra-file progress + cancel via
  the status callback); non-mac → 8 MiB chunked loop. A cancelled
  in-flight file is deleted; completed items stay (outcome reports
  `cancelled: true`). Symlinks are recreated as symlinks, never followed
  (same stance as the disk-usage walker). `clonefile` is atomic — a
  refusal (cross-volume, non-APFS) leaves no partial and falls back to
  the copy path. **[mac]** for clone + copyfile.
- **Move** takes the `fs::rename` fast path per top-level item when
  `same_volume(src, dest_dir)` — instant, no progress needed. Cross-
  volume falls back to copy-then-delete per item; the delete only runs
  if that item's copy fully succeeded.
- **KeepBoth** resolves via the Finder convention already shipped for
  Duplicate: first candidate `"name 2"`, then `"name 3"`, …
  (paste-collision flavor — `"name copy"` stays Duplicate's flavor).
  The suffix helper moves into the engine so both platforms share it
  (the shell-mac copy stays as a thin re-export until its callers
  migrate) — `pick_available_name(parent, stem, ext, scheme)`.
- **Replace** removes the destination (file or tree) just before
  copying that item, never earlier — a cancel before reaching the item
  must leave it untouched.
- The engine never touches the UI, the pasteboard, SQLite, or AppKit.

### GPUI worker — `spawn_transfer_op` (feraille-gpui)

`spawn_file_op`'s grown-up sibling, living next to it in `shell.rs`:

1. Registers `TaskKind::FileOp` with `cancellable: true` and a fresh
   `Arc<AtomicBool>`; the registry entry now carries the flag (iter B)
   so the task panel can render a ✕ that flips it.
2. Background: `plan_transfer` → (conflicts? hand back to the UI for
   the collision dialog, then resume) → `run_copy`/`run_move`, all
   sharing one `Arc<TransferProgress>`. A separate ~10 Hz sampler task
   reads that sink, derives EMA rate + ETA (suppressing the clone jump),
   and writes `tasks.update`/`update_transfer`; a `done` flag stops it on
   every exit path. No per-chunk channel, no per-file allocation — the
   worker never waits on the UI.
3. On completion: `tasks.end`, `broadcast_reload_for_process` for the
   destination (and sources' parents on move), `push_notification`
   (success with item count / error with the engine's message — fixing
   today's log-only failures), and undo registration.
4. Cancel: outcome reports partial state; notification says
   "Cancelled — N of M items copied"; reload still broadcast.

### Collision dialog

Conflicts found at plan time open a gpui dialog (`window.open_dialog`,
the Rename/NewFolder pattern — not NSAlert):

> **3 items already exist in "Documents"**
> [Replace] [Keep Both] [Skip] / Cancel

One policy for the whole batch (per-item resolution is a later iter).
Same-directory paste short-circuits: no dialog, auto-KeepBoth (pasting
next to the original obviously means "make me a copy").

### Clipboard verbs **[mac first]**

Finder semantics, not Windows cut:

- **Cmd+C — Copy**: writes the selection's file URLs to the general
  NSPasteboard (new `clipboard_copy_file_urls(paths)` in shell-mac,
  the `services.rs::write_selection` body generalized). Cross-app:
  Finder can paste what we copy and vice versa.
- **Cmd+V — Paste**: new `clipboard_read_file_urls() -> Vec<PathBuf>`
  **[mac]** *[win-parity: CF_HDROP]* reads URLs back; spawns a copy
  into the active tab's directory.
- **Cmd+Option+V — Move Paste**: same read, spawns a move (Finder's
  "Move Item Here").
- Cut (Cmd+X) is deliberately absent in v1, like Finder. The
  catalogue grows `file.copy`, `file.paste`, `file.move_paste`.
- Paste enables only when the pasteboard holds file URLs; the action
  no-ops with a status notification otherwise.

### Undo

- Move → `UndoOp::MoveBack(Vec<(from, to)>)` — renames each item back
  (cross-volume undo re-runs the engine in reverse, still as one op).
- Copy → `UndoOp::RemoveCreated(Vec<PathBuf>)`, registered **only when
  nothing was replaced** (undoing a replace would delete the only
  remaining version — unrecoverable, so we don't offer it; parity
  with the trash-undo deferral).

### Prime-directive compliance

Planning, copying, deleting all run on the background executor. The
collision dialog reads the already-computed plan. Progress re-entry is
throttled. Render paths read the task registry as today. The
pasteboard read happens in the action handler (semantic event), not
render — same boundary as Quick Look.

## Iterations

1. **Engine** — `feraille-fs-native/src/file_ops.rs`: plan/copy/move,
   `same_volume`, `pick_available_name`, chunked progress, cancel;
   tempdir unit tests (copy tree, keep-both, replace, skip, cancel
   mid-batch leaves completed items, same-volume rename move,
   symlink recreation).
2. **Progress + cancel UI** — `ActiveTask` carries
   `cancel: Option<Arc<AtomicBool>>`; task panel renders ✕ for
   cancellable tasks; `spawn_transfer_op`; error/success
   notifications (also retrofitting `spawn_file_op` failures from
   log-only to notification).
3. **Clipboard** — shell-mac pasteboard read/write helpers + win32
   stubs; `file.copy` / `file.paste` / `file.move_paste` catalogue
   commands + actions + handlers; collision dialog; undo ops.
4. **Later** (tracked in TODO, not this arc): remaining drag edge
   auto-scroll, favorite-row drops, Windows pasteboard volume-identity
   parity, and Windows alias-in-destination parity.

## Verification

Per iteration: `cargo test -p feraille-fs-native -p feraille-gpui`,
clippy zero, workspace check. End-to-end manual script: copy a folder
tree onto itself (keep-both names), paste 1 GB into another volume
(progress strip + cancel mid-way), Cmd+Option+V across volumes,
undo a move. Screenshot: task panel with a determinate FileOp +
cancel button.
