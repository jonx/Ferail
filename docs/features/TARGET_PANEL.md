# Target Panel: Pick as Target

Design for a second, **pinned** file listing beside the main one: a folder
chosen explicitly by the user that acts as a source or a destination for file
operations, and as the staging surface for batched transfers.

← [Feature notes index](README.md) · [Architecture](../ARCHITECTURE.md) ·
[TODO](../../TODO.md) · [File operations](FILE_OPS.md) ·
[Selection & DnD spec](ferail-selection-dnd-spec.md)

<!-- toc depth=2 -->

- [What is built](#what-is-built)
- [The problem](#the-problem)
- [The model](#the-model)
- [It is a surface, not a tab](#it-is-a-surface-not-a-tab)
- [Reusing the list view](#reusing-the-list-view)
- [Drop semantics](#drop-semantics)
- [Column layout](#column-layout)
- [Phase 2: batched transfers](#phase-2-batched-transfers)
- [Prime Directive](#prime-directive)
- [Strings](#strings)
- [Open questions](#open-questions)

<!-- /toc -->

## What is built

**Design note only (2026-08-26).** Nothing here is implemented. The code
references below are the seams this design leans on, all verified against the
sources at the time of writing.

## The problem

Reorganizing files across folders currently means one of:

- dragging onto a tab chip, a tree row, or a favorite, all of which work
  today, and all of which are *aimed* gestures at a target you cannot see;
- opening a second window, where "the other side" is not a concept the app can
  name, so no command can target it;
- copy, navigate, paste, losing the source view in the process.

What none of them give is a folder that is **simultaneously visible and
addressable**: something a command can refer to as "the other side" without the
user aiming a pointer at it.

## The model

The user right-clicks a folder, in the file list, the tree, the breadcrumb,
or Favorites, and picks **Pick as Target**. A panel opens on the right showing
that folder's contents. It stays there.

Three properties define it:

- **Frozen.** The panel never navigates. Double-clicking a folder inside it
  does not descend into it, and a drag hovering a folder row does not
  spring-load. The panel's path changes only by an explicit *Pick as Target* on
  another folder, and the panel closes only by an explicit close. This is a
  stated invariant, not an implementation detail: **the target changes by user
  command and by nothing else.**
- **Source or destination, both directions.** Items can be sent from the main
  list to the target, and pulled from the target into the current folder.
- **Not a preview.** It is a distinct panel with its own visibility and its own
  lifecycle. See "Why not the preview pane" below.

### Why frozen

Freezing is what makes the panel cheap and predictable. It removes the
breadcrumb, the history and back/forward, `Cmd+L` path editing, and
spring-load. It also means the panel watches exactly **one** path for its
entire lifetime, so a single `FsWatcher` subscription keeps it honest, no
navigation churn, and no dependency on a reload fan-out loop that a future
change might forget to update.

### Why not the preview pane

The preview pane already has a second claimant: a docked archive workbench
overrides it through `Shell::preview_override`
([shell.rs:1043](../../crates/ferail-gpui/src/shell.rs#L1043),
[render.rs:3071](../../crates/ferail-gpui/src/shell/render.rs#L3071),
[archive.rs:558](../../crates/ferail-gpui/src/archive.rs#L558)). Routing the
target listing through the same field would force an arbitration rule, who
wins when an archive is docked while a folder is pinned?, and force every site
touching that field to honour it. A separate panel has no such question.

The preview also *follows* the selection by design (`set_target` is documented
as "cheap and idempotent: hosts call it on every selection change"), which is
the opposite of frozen. Sharing the surface would mean explaining to the user
why their preview stopped previewing.

### Why not a dual-pane split

A full second pane means duplicating the tab strip, the focus handling, and
every hard-coded `ElementId` in the pane subtree, and it puts pressure on the
283 `self.tabs` sites. It buys browsing on both sides, which is exactly what
this design deliberately does not want. The target panel reaches the same
outcome (an unambiguous "other side" that commands can name) at a fraction of
the surface. If a split is ever added, it composes: each pane simply becomes
another source.

## It is a surface, not a tab

The codebase already distinguishes *surfaces*: things that own a table without
being a tab. `FileListDelegate` carries an `asset_scope` whose doc comment says
so outright: *"Unlike TabId this also exists for archive/tool tables, and
prevents one surface's local generation counter from retiring another surface's
work"* ([file_list.rs:522](../../crates/ferail-gpui/src/file_list.rs#L522)).
The archive workbench and the tool-result tables are the existing members.

The target panel is a third member:

- its own entity, holding its own `TableState` and `FileListDelegate`
  (an ordinary constructor over shared caches:
  [file_list.rs:1164](../../crates/ferail-gpui/src/file_list.rs#L1164));
- its own `AssetWorkScope`, so its icon/thumbnail/metadata work cannot retire
  the active tab's;
- its own enumeration generation and cancel flag, scheduled exactly like
  `Shell::load_path_for_tab`;
- one `FsWatcher` subscription on its fixed path.

It deliberately does **not** reuse `Tab`. A `Tab` carries `nav`, `history`,
`history_index`, `platform_namespace`, `tool_result` and the marquee, all dead
weight for a frozen listing, and dead fields on a central type are how a domain
model rots.

## Reusing the list view

The list view is reused wholesale: the same `multi_table` table, the same
`FileListDelegate`, the same columns, sorting, row rendering, drag sources,
cut-marker dimming
([file_list.rs:2255](../../crates/ferail-gpui/src/file_list.rs#L2255)), and the
same context menus. This is the point: a target listing that behaved
differently from the main listing would be a second thing to learn.

One coupling must be broken first. Row and background menus dispatch Actions
into the Shell's focus handle
(`menu.action_context(self.shell_focus.clone())`,
[file_list.rs:3636](../../crates/ferail-gpui/src/file_list.rs#L3636)), and the
Shell's handlers resolve their target against the **active tab**. Reused
verbatim, a right-click → Move to Trash inside the panel would delete from the
main list.

The fix is contained, because that question funnels:

- `Shell::resolve_targets(context_row, cx)`
  ([shell.rs:2864](../../crates/ferail-gpui/src/shell.rs#L2864)) is where every
  path-targeted command decides what it acts on;
- `action_entries_visible_order` is its consuming wrapper and
  `action_target_count` its counting twin;
- `file_list::resolve_menu_targets`
  ([file_list.rs:363](../../crates/ferail-gpui/src/file_list.rs#L363)) is
  already a free function taking `entries` and `selected` as parameters, so the
  menu-side resolver needs **no change at all**.

So the work is: make the *focused surface* explicit, and have `resolve_targets`
resolve rows against that surface's delegate instead of hard-coding
`active_tab()`. The same notion serves keyboard invocations, which today fall
through to the active tab's selection and lead row. `context_row: Option<usize>`
gains a surface alongside the index. There are 27 `context_row` sites in the
crate, concentrated in three functions.

A large part of the menu needs even less. Verbs that act on a path rather than
on a row selection: Reveal, Copy Path, Get Info, Open Terminal Here, Add to
Favorites, already route through `Shell::context_target`, which exists
precisely because *"sidebar items aren't part of the file list"*
([shell.rs:950](../../crates/ferail-gpui/src/shell.rs#L950)). The panel stages
that field the same way the sidebar does, and those verbs work unmodified.

## Drop semantics

- **Onto the panel background**: the pinned folder is the destination.
- **Onto a folder row inside the panel**: that folder is the destination.
  Valid, and highlighted by the existing `drag_over` treatment.
- **No spring-load anywhere in the panel.** `Shell::spring_load` is keyed by
  row index and must not arm for this surface: drilling in would break the
  frozen invariant, and a drag that silently retargets the panel is exactly the
  surprise this design is avoiding.
- **Dragging out of the panel** works through the existing row drag source, so
  the main list, the tree, Favorites and other applications are all reachable
  without new machinery.

## Column layout

The panel is a right-hand `resizable_panel` in the existing splitter group,
beside the preview pane rather than instead of it. Both may be open at once.

This follows a decision already recorded in the code: an earlier auto-hide of
the preview below 900px was removed because it *"silently suppressed the
explicit toggle"*
([render.rs:4334](../../crates/ferail-gpui/src/shell/render.rs#L4334)). Same
rule here: show what the user asked for at any width and let the per-panel
minimums keep the layout sane. Current bounds for reference: the file pane
floors at 360, the preview runs 260–640
([shell.rs:1764](../../crates/ferail-gpui/src/shell.rs#L1764)). The target
panel needs a wider floor than the preview's 260 for a listing to be usable.

## Phase 2: batched transfers

With an unambiguous other side, transfers can be **staged instead of executed**:
queue operations in either direction, review the queue, then apply once. The
model already exists in this codebase: the archive workbench keeps a journal
of pending operations, renders the table of contents as a projection of the
original plus that journal, shows a pending-change count, and offers Save
Changes / Revert with close protection ([ARCHIVES.md](ARCHIVES.md)).
`Cmd+X` is the same idea with a queue depth of one: `cut_marker` records intent,
rows dim, and `Paste` commits.

Two things must be honest about the difference:

- **A filesystem batch is not atomic.** The archive's commit reduces to one
  temporary file and one rename. Five hundred cross-volume copies are five
  hundred independent operations. Call it a *batch*, never a transaction, and
  report partial application truthfully.
- **Validation happens at commit, not at queue time.** Staged operations hold
  `NodeId`s (so they survive renames and navigation) but must be re-resolved
  and re-validated immediately before they run: collisions, free space,
  permissions, vanished sources. The queue-time check is a preview for the
  user; only the commit-time check is authoritative. A stale plan buys latency,
  never correctness. `ArchiveStamp` is the precedent
  ([archive/mod.rs:1269](../../crates/ferail-fs-native/src/archive/mod.rs#L1269)).

A commit is one task with one progress entry and one undo record. The undo ops
are already shaped for it: `MoveBack`, `MoveBackCross` and `RemoveCreated` all
take vectors of pairs
([shell.rs:149](../../crates/ferail-gpui/src/shell.rs#L149)).

Ownership follows the architecture invariants: the plan type is platform-neutral
domain state in `ferail-core`, validation is native filesystem work in
`ferail-fs-native` behind `assert_off_ui_thread`, and the panel and queue UI
live in `ferail-gpui`.

## Prime Directive

The panel adds a second concurrent enumeration source, so it takes the same
shape as `Shell::load_path_for_tab`: scheduled from a semantic event, run on
the background executor, guarded by a generation counter and a cancel flag,
results dropped if the target changed underneath. Rendering, hover, hit-testing
and menu construction read cached state only. Drop-source inspection can
enumerate directories and therefore belongs in a background task, exactly as it
does for the archive workbench.

## Strings

*Pick as Target* is the command. The panel header names the pinned folder, and
closing it is how the target is cleared. Every string here is user-visible and
must be wrapped and translated into both bundled packs in the change that adds
it ([Localization](LOCALIZATION.md)).

## Open questions

- Double-clicking a row in the panel: inert, or open in the main tab? Inert is
  safest for the frozen invariant; opening in the main tab is the only reading
  that does not navigate *inside* the panel.
- Whether the target survives a restart. Not in v1: say so rather than
  implying otherwise.
- Whether the panel is per-window (like the preview pane it sits beside) or
  per-tab. Per-window is the better fit for a destination: the target should
  hold still while the user wanders across tabs collecting items.
