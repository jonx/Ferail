# Bulk Rename

Pattern-rule rename over the current multi-selection, with a live
before→after preview. A self-contained modal: no new subsystem — it rides
the shared dialog surface, the task registry, and the undo stack.

← Back to [feature notes](README.md) · Source:
`crates/ferail-gpui/src/bulk_rename.rs`

## What ships

- **Entry points.** File-list context menu "Rename N Items…" (shown when
  the resolved target set has 2+ items — the multi-selection twin of the
  SingleOnly "Rename…"), and the `BulkRenameSelected` action. With one
  resolved target the action degrades to the shared inline filename editor; with
  none it's a no-op. No default keybinding in v1.
- **The modal.** Find/Replace inputs (literal or regex via a toggle), a
  case-transform button group (None / lower / UPPER / Title), a template
  input with Start/Pad counter fields, a summary line
  ("N of M will be renamed · K conflicts"), and up to 12 preview rows of
  `before → after` (conflict rows in the danger color with the reason
  inline; unchanged rows muted; "…and X more" past 12).
- **OK gating.** An invalid regex is one plan-level error line (never
  per-row), and OK refuses to close while the plan has an error or any
  conflict.

## The rule pipeline

Each file's *display name* runs through three stages (the on-disk leaf
mapping — macOS `/`→`:` — happens only at apply, via `on_disk_leaf`):

1. **Find/replace** over the whole name. Literal `str::replace`, or
   `regex::Regex::replace_all` with `$1`..`$9` capture references. An
   empty Find skips the stage.
2. **Case transform on the stem only** (last-dot split; no dot ⇒ all
   stem; a leading dot — `.gitignore` — is stem, not a separator).
   Title case = first letter of each whitespace-separated word
   uppercased, rest lowered; Unicode-aware via `char::to_uppercase` /
   `to_lowercase` (ß → SS works, no chrono/unicode dep).
3. **Template** (skipped when empty). Tokens:

   | Token    | Expands to                                                    |
   | -------- | ------------------------------------------------------------- |
   | `{name}` | the post-transform stem                                       |
   | `{ext}`  | the extension *without* the dot (`""` when none)              |
   | `{n}`    | `Start + item index`, zero-padded to `Pad` (e.g. 3 → `007`)   |
   | `{date}` | file mtime as `YYYY-MM-DD` (UTC, proleptic-Gregorian math)    |

   A template with no `{ext}` token gets `.{ext}` appended automatically
   when the file had an extension — a template can't silently strip
   extensions. (Corollary: `{name}.{ext}` on an extensionless file
   yields a trailing dot; the preview shows it.)

## Conflict rules

Detected in the plan, all case-insensitive (APFS/NTFS default):

- **duplicate target** — two changed rows collapse onto the same name;
- **empty name** — the pipeline produced nothing;
- **name already taken** — the target equals a batch member's
  *unchanged* name (an item not being renamed away).

Targeting another *changed* row's old name is **not** a conflict — that's
a renumbering chain. The apply worker orders pairs by dependency (a pair
waits until the pair occupying its destination has vacated) and breaks
pure cycles (a two-file swap) by parking one file under a temporary
sibling name. On-disk collisions with non-batch siblings can't be seen by
the plan; the apply step's guarded rename fails those per item (plain
`fs::rename` would clobber on Unix — the one exception is a case-only
rename of the same file on a case-insensitive volume, checked by
dev+ino).

## Apply + undo semantics

Apply runs on the background executor behind a task-registry row,
per-item resilient: failures collect while the rest proceed. On
completion: a success toast ("Renamed N items", or "…, K failed — first
error"), a reload broadcast for the affected parents, and **one**
`UndoOp::RenameBatch` holding only the successfully renamed
`(original, renamed)` pairs. Undo ("Undid bulk rename") replays the pairs
backwards through the same chain/cycle-aware worker with the
don't-clobber guard `MoveBack` uses.

## Responsiveness

The dialog body is one entity whose render reads a *cached* plan; plan
rebuilds happen only on semantic events (input change, toggle click) —
synchronously up to 5 000 items, above that on the background executor
with a generation tag so stale results drop. No debounce: the plan is
pure string work and the `regex` crate is linear-time. The selection
snapshot (paths, display names, mtimes) is captured once at open from
cached rows — the dialog never touches the filesystem.

## Verification

`--bulk-rename` in the screenshot harness opens the dialog headlessly
(seeds a 4-row selection when `--select-rows` is absent):

```sh
cargo run --bin ferail-gpui -- --screenshot screenshots/bulk-rename.png \
  --navigate <folder> --bulk-rename
```

Engine + worker unit tests live at the bottom of `bulk_rename.rs`.

## Follow-ups

- `{dimensions}` token (image width×height from the metadata DB) once a
  cheap cached lookup is plumbed to the modal.
- Saved rule presets (persist recent/named rules in the metadata DB).
- A default keybinding (deferred in v1 — keymap changes were pending).
- Number-only inputs for Start/Pad (plain inputs today; non-numeric text
  falls back to 1 / 0).
