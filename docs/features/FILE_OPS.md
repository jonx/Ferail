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

Open follow-ups in [TODO.md](../../TODO.md): drag-into-app drop
targets feeding `spawn_transfer_op`, per-item collision resolution,
Windows pasteboard (CF_HDROP) + volume identity, cut semantics
decision.

## Platform tags

**[mac]** = macOS-only today; **[win-parity]** = named Windows
equivalent for later. Untagged = platform-neutral.

## Existing anchors (verified 2026-06-13)

| What | Where |
|---|---|
| `spawn_file_op(reload_path, op, label, cx)` — background op + reload broadcast, no progress/cancel/notify | `crates/feraille-gpui/src/shell.rs:1733` |
| `TaskRegistry::begin(kind, label, cancellable)` / `update(id, f32)` / `end(id)`; `cancellable` flag exists but nothing reads it | `crates/feraille-gpui/src/tasks.rs` |
| Task panel renders label + progress strip, "cancel buttons land when task kinds pick up per-task cancel hooks" | `crates/feraille-gpui/src/task_panel.rs` |
| gpui modal dialogs (`window.open_dialog` + Input) used by Rename / NewFolder | `crates/feraille-gpui/src/shell/file_ops.rs:446-559` |
| Finder-style collision naming `pick_suffixed_name(parent, stem, ext, "copy")` | `crates/feraille-shell-mac/src/file_ops.rs:38` **[mac]** |
| NSPasteboard URL *writing* (Services path); no reading helper exists | `crates/feraille-shell-mac/src/services.rs:115` **[mac]** |
| Undo stack `UndoOp::{Rename, DeleteFolder, …}` + `apply_fs()` | `crates/feraille-gpui/src/shell.rs:72` |
| Reload fan-out `broadcast_reload_for_process(process, paths, cx)` | `crates/feraille-gpui/src/shell.rs:1715` |
| Cancel-aware walker precedent `recursive_size(root, cancel)` | `crates/feraille-fs-native/src/disk_usage_scanner.rs:246` |

## Architecture

### The engine — `feraille-fs-native/src/file_ops.rs` (platform-neutral)

Pure, synchronous, worker-thread functions; the GPUI side owns
scheduling. Mirrors the `recursive_size` contract: cooperative
`&AtomicBool` cancel, progress via callback.

```rust
pub struct OpPlan {
    pub sources: Vec<PathBuf>,     // top-level items being copied/moved
    pub dest_dir: PathBuf,
    pub total_bytes: u64,          // file bytes (dirs walk free)
    pub total_items: u64,          // files + dirs, for item-granular progress
    pub conflicts: Vec<PathBuf>,   // top-level dest paths that already exist
}

pub enum CollisionPolicy { Replace, KeepBoth, Skip }

pub struct OpOutcome {
    pub created: Vec<(PathBuf, PathBuf)>, // (source, destination) per top-level item actually produced
    pub skipped: u64,
    pub replaced: u64,
    pub cancelled: bool,
}

pub fn plan_transfer(sources: &[PathBuf], dest_dir: &Path,
                     cancel: &AtomicBool) -> Result<OpPlan, String>;

pub fn run_copy(plan: &OpPlan, policy: CollisionPolicy,
                progress: &mut dyn FnMut(u64 /*done*/, u64 /*total bytes*/),
                cancel: &AtomicBool) -> Result<OpOutcome, String>;

pub fn run_move(plan: &OpPlan, policy: CollisionPolicy,
                progress: &mut dyn FnMut(u64, u64),
                cancel: &AtomicBool) -> Result<OpOutcome, String>;

pub fn same_volume(a: &Path, b: &Path) -> bool;  // unix: MetadataExt::dev()
                                                 // [win-parity: GetVolumePathNameW]
```

Rules:

- **Copy** streams files in 8 MiB chunks (`io::copy` over a bounded
  buffer) so progress ticks and cancel lands mid-file. A cancelled
  in-flight file is deleted; completed items stay (outcome reports
  `cancelled: true`). Symlinks are recreated as symlinks, never
  followed (same stance as the disk-usage walker). Permissions copy
  best-effort (`fs::set_permissions` after each file; failures are
  logged into the error, not fatal).
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
   the collision dialog, then resume) → `run_copy`/`run_move` with a
   throttled progress callback (~10 Hz) that re-enters via
   `entity.update` → `tasks.update(id, frac)`.
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
4. **Later** (tracked in TODO, not this arc): drag-into-app drop
   targets feeding the same `spawn_transfer_op`; drop-onto-favorite;
   per-item collision resolution; Windows pasteboard parity.

## Verification

Per iteration: `cargo test -p feraille-fs-native -p feraille-gpui`,
clippy zero, workspace check. End-to-end manual script: copy a folder
tree onto itself (keep-both names), paste 1 GB into another volume
(progress strip + cancel mid-way), Cmd+Option+V across volumes,
undo a move. Screenshot: task panel with a determinate FileOp +
cancel button.
