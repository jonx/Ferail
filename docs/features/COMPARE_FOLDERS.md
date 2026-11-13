# Compare Folders

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

Compare Folders colours the two panes of a [split view](SPLIT_VIEW.md) by how
their folders differ, and copies what is missing or newer from one side to the
other in one click.

## What the user gets

| Command | Shortcut | Effect |
| --- | --- | --- |
| Compare Folders | Cmd+Option+K | Compare the two panes' folders, or stop comparing. |

The command is in the View menu and the command palette. While the window is
split, the toolbar shows it as a toggle beside the Split View button.

- **Rows.** Each direct child of both folders is paired by name and tinted:
  blue for a name on one side only, amber for different content, green for
  identical content. A row stays untinted until its verdict lands, and stays
  so when its content could not be checked.
- **Toolbar.** While comparing, two arrow buttons sit beside the Compare
  button: copy missing and newer to the left, and to the right (top and
  bottom in a stacked split). Hovering the Compare button shows a card with
  the legend, the count of each state, *Comparing…* while verdicts are still
  arriving, and what the sync buttons do. Nothing is added above the panes.
- **When it cannot start.** The notice names the missing condition: no split
  view, or a pane whose listing is not a folder's direct contents (Include
  Subfolders, search results, an archive, a tool result).
- **Following the panes.** A pane that navigates, reloads or shows another tab
  is compared again. Closing the split, or a pane showing search results, an
  archive or another listing that is not a folder, ends the comparison.
- **Hidden files** take part when Show Hidden Files is on. `.DS_Store` never
  takes part.

## How a verdict is reached

`ferail_fs_native::folder_compare` holds the worker.

1. `list_compare_items` lists each folder through the native directory reader.
2. `pair_listings` pairs both listings by name and settles what the listing
   alone decides: a name on one side, a file against a folder, two files of
   different sizes. Every other pair is `Pending`.
3. `verify_pair` settles a pending pair by reading: two files are hashed with
   `file_content_hash`, the duplicate finder's BLAKE3 hash read through its
   `DupeHashCache`, so a file hashed before is not read again; two folders are
   walked recursively, structure first, and stop at the first difference; two
   symlinks compare their targets.

A verdict is never assumed. An unreadable file, an undownloaded cloud
placeholder or a cancelled check is `Unknown`, never `Identical`.

## GPUI integration

`shell/compare.rs` owns `Shell::compare`, a `FolderCompare`. Starting a
comparison bumps `Shell::compare_generation`, raises the previous run's cancel
flag, registers a cancellable `FolderCompare` task and runs the worker on the
background executor. Files are checked before folders. Verdicts stream back in
batches and are dropped when their generation is stale.

Verdicts live in a name-keyed `CompareMarks` map shared with both panes'
`FileListDelegate::compare_marks`. `render_tr` tints a row with one map lookup;
it reads no file. The tint is the theme's `info`, `warning` or `success` token
at low opacity, under the selection fill. Counts and sync availability are
recomputed when a batch lands, not while rendering.

`Shell::sync_compare` runs with the split area's render and only schedules
work. `Shell::load_path_for_tab` calls `compare_after_tab_load`, so a reload
after a sync compares again.

## Sync

`sync_plan` turns the verdicts into one transfer per click:

- a name present on the source side only is copied, a folder as a whole;
- a file that differs and has a newer modification time on the source side
  replaces the older one.

Nothing is deleted. Two folders that differ are not merged, because the
transfer engine replaces a folder as a whole; opening them in both panes
compares and syncs their contents. A file against a folder of the same name,
equal modification times and unsettled verdicts are left alone.

The plan runs through `Shell::spawn_transfer_op_with_presets` as an ordinary
copy, with progress, cancellation and the collision prompt for anything that
appeared since the comparison. Planned replacements carry a preset Replace
policy and do not prompt. Undo is offered when the copy replaced nothing.

## Out of scope

- Grid view tints.
- Deleting extra items, and two-way merging inside folders that differ.
- Comparing by anything other than name.
