# Ferail — Node Selection & Drag-and-Drop Specification

Behavioral spec for selecting filesystem nodes and for drag-and-drop, written against the constraints in `ARCHITECTURE.md` and `STREAMING_ENUMERATION.md`.

The governing rule from the architecture doc applies to everything here: **the UI must never stop.** Selection, hit testing, hover, and drag are explicitly named as hot-path interactions that must not perform I/O. This spec is written so that every selection and drag operation reads only cached in-memory state and enqueues anything expensive through the scheduler.

---

## 1. Scope and vocabulary

This spec covers selection and drag-and-drop for **nodes** — filesystem objects identified by `NodeId` — as they appear in:

- the **file table** (the virtualized `TableState`-backed list),
- the **Browse tree** (the expandable Home tree in the sidebar),
- and, where noted, the interaction *targets* in **Favorites** and **Volumes**.

Selection state is **per-tab** for the file table (the architecture doc states each tab owns its selection and scroll state). The tree's expansion/selection is sidebar state owned by the shell, shared across tabs. These are two distinct selection models and §2.1 keeps them separate on purpose.

Terms:

- **Selection set** — the set of `NodeId`s currently selected in a given context.
- **Anchor** — the node a range selection extends *from*. Set on a plain click or the first ctrl/cmd-click.
- **Lead / cursor** — the node with keyboard focus; the node a range selection extends *to*. Rendered with a focus ring distinct from selection fill.
- **Active context** — which surface (table of tab N, or tree) currently owns selection input. Only one at a time.

---

## 2. Selection model

### 2.1 Two independent selection contexts

The **file table** and the **Browse tree** each hold their own selection set, anchor, and lead. They do not share selection. Clicking a node in the tree does not select a row in the table and vice versa — but it *does* drive navigation (clicking a tree node navigates the active tab, per the existing context-menu/navigation model).

Only one context is "active" for keyboard input at a time, determined by focus. Tab key and click move focus between contexts. The inactive context renders its selection in a **dimmed** style (selection fill at reduced emphasis) so the user can still see what's selected there without it competing with the active context.

Rationale: this matches Finder and every dual-pane manager. Merging them into one global selection set creates ambiguity the moment both surfaces show the same folder.

### 2.2 Selection set semantics

- The selection set is a set of `NodeId`s. Order of insertion is **not** semantically meaningful for the set itself, but operations that need an order (e.g. "open all", drag payload) use **visible row order** at the moment of the operation, not insertion order.
- The selection set only ever contains nodes that are **currently in the view's model**. When the model changes (streaming batch arrives, filter changes, navigation, sort), the set is reconciled — see §2.6.
- Empty selection is a valid, common state.
- The selection set is **in-memory interaction state** — exactly the kind of "small in-memory interaction state" the hot path is allowed to mutate. It is not persisted as filesystem truth. (Whether selection is restored across app launches is a separate UI-preference question; default: not restored. Selection *is* preserved across same-session navigation back/forward via tab history — see §2.6.)

### 2.3 Anchor and lead

- **Anchor** is set when the user starts a fresh selection gesture: a plain click, or the first cmd-click into an empty selection. It is the fixed end of a range.
- **Lead** is the moving end: it follows the most recent click or the keyboard cursor.
- A plain click sets anchor = lead = the clicked node.
- Range operations (§2.4) compute the inclusive span between anchor and lead **in visible order** and that span becomes (or unions into) the selection.
- If the anchor node leaves the model (deleted, filtered out, navigated away), the anchor is re-seated to the nearest still-valid node in the prior anchor's direction, or cleared if none. Lead follows the same rule. Never let anchor/lead dangle pointing at a `NodeId` not in the model.

### 2.4 Mouse selection gestures (file table)

| Gesture | Behavior |
|---|---|
| **Click** on a row | Replace selection with just that row. anchor = lead = row. |
| **Click** on empty space below rows | Clear selection. anchor/lead cleared. |
| **Cmd+Click** on an unselected row | Add row to selection. lead = row. anchor = row (so a subsequent Shift+Click ranges from here). |
| **Cmd+Click** on a selected row | Remove row from selection. lead = row. If the set is now empty, anchor cleared; else anchor = row. |
| **Shift+Click** on a row | Range-select: selection becomes the inclusive span from anchor to clicked row in visible order. lead = clicked row. anchor unchanged. If there was no anchor, treat clicked row as anchor (behaves like a plain click). |
| **Cmd+Shift+Click** | Union the anchor→clicked range *into* the existing selection (additive range). lead = clicked row, anchor unchanged. This lets a user build several disjoint ranges. |
| **Double-click** a row | Does not change selection semantics beyond a single-click; triggers **open** (file → default open via platform crate; folder → navigate active tab). The open is a semantic event scheduled per the work-scheduling pattern; it does not block. |
| **Right-click** on an **unselected** row | Select just that row (replace), then open context menu. The menu's target is the selection. |
| **Right-click** on a **selected** row | Do **not** change the selection. Open context menu targeting the whole current selection. (This is what makes "right-click → operate on all 12 selected" work.) |
| **Right-click** on empty space | Clear selection (or leave empty), open the folder-background context menu (New Folder, Paste, etc.). |

Notes:
- "Visible order" means the table's current sort + filter order. Streaming may still be delivering rows; range selection operates over whatever is currently in the model. If a Shift+Click range's endpoints are both present but rows *between* them are still streaming in, see §2.6 for how the range reconciles as batches arrive.
- The modifier mapping uses **Cmd** for discontiguous toggle (macOS convention), not Ctrl. Ctrl+Click is reserved for the right-click-equivalent on macOS and must be treated as a right-click, not as a toggle.

### 2.5 Keyboard selection (active context)

All keyboard selection reads cached model state only.

| Key | Behavior |
|---|---|
| **Up / Down** | Move lead one visible row; selection becomes just that row; anchor = lead. (Plain navigation.) |
| **Shift+Up / Shift+Down** | Extend: move lead one row, selection = anchor→lead span. anchor unchanged. |
| **Cmd+Up / Cmd+Down** | Move lead to first / last visible row; selection = just that row; anchor = lead. (No selection extend — matches macOS list behavior. Cmd+Shift+Up/Down extends to first/last.) |
| **Cmd+Shift+Up / Cmd+Shift+Down** | Extend selection to first / last visible row. |
| **Home / End** | Same as Cmd+Up / Cmd+Down for tables that receive them. |
| **Page Up / Page Down** | Move lead by one viewport height of rows; plain = replace, Shift = extend. |
| **Cmd+A** | Select all rows currently in the model (respecting active filter). anchor = first visible, lead = last visible. |
| **Esc** | Clear selection. If a drag or rename or inline operation is in progress, Esc cancels that first and only clears selection on a second press. |
| **Type-ahead** (printable characters) | Move lead to the first row whose `display_name` starts (case-insensitively) with the typed buffer, scrolling it into view. Consecutive keystrokes within ~0.75s accumulate into a longer prefix; the same single character pressed repeatedly cycles through every match, wrapping. After the idle timeout the buffer resets. Plain navigation semantics (replace selection). Works in both the list and icon-grid views; matching uses the already-cached `display_name` — no I/O. Implemented in `Shell::on_typeahead_key` (`shell/selection.rs`), wired as an `on_key_down` on the shell root; gpui matches keybindings to actions *before* key listeners, so only unbound printable characters reach it. |
| **Space** | Trigger Quick Look / preview for the lead node, if Ferail wires Quick Look. Does not change selection. |
| **Return / Enter** | Open the selection (same as double-click). With multiple selected, open all, with a sane cap and a confirmation above some threshold (e.g. >10). |
| **Arrow Left / Right** in the **table** | Reserved for column-less navigation no-op, OR collapse/expand if the table ever shows a tree column. In flat table mode, Left/Right do nothing to selection. |

Tree-specific keyboard (when the **Browse tree** is the active context):

| Key | Behavior |
|---|---|
| **Up / Down** | Move lead through *visible* (expanded) tree rows. |
| **Right** | If lead node is a collapsed expandable folder → expand it (expansion is a scheduled enumeration if children aren't cached, NOT a blocking read). If already expanded → move lead to first child. |
| **Left** | If lead node is expanded → collapse it. If collapsed (or a leaf) → move lead to parent. |
| **Return** | Navigate the active tab to the lead node. |
| Selection extend (Shift) | Tree multi-select is **optional** for v1 — see §2.7. If not supported, Shift+Up/Down in the tree behaves as plain navigation. |

### 2.6 Reconciling selection with a changing model

This is the section that matters most given streaming enumeration. The model under the selection is not stable; the spec must say exactly what happens.

**On streaming batch arrival** (`LoadMsg::Batch`):
- New rows are appended/merged into the model per the existing streaming logic.
- The selection set is **unchanged** in identity — the same `NodeId`s stay selected.
- If a newly-arrived row's `NodeId` is in the selection set (possible after a navigation that preserved selection, or a refresh), it renders selected.
- A pending Shift-range whose far endpoint had not yet streamed in: the range is recomputed each time the model changes so that when the in-between rows arrive, they join the selection. Concretely — store the range as (anchor NodeId, lead NodeId) and **recompute the span on every model change** while the range gesture is "live", rather than freezing a set of NodeIds at click time. A range stops being "live" the moment any non-range gesture occurs.

**On `LoadMsg::Done`**:
- Reconcile: drop from the selection set any `NodeId` not present in the final model. (Handles the case where selection was optimistically preserved across navigation but some nodes no longer exist.)
- Re-seat anchor/lead per §2.3.

**On navigation (new path committed):**
- Per the architecture doc, navigation commits immediately. Selection for the *new* path starts **empty** by default.
- Exception — **back/forward history**: when returning to a directory via tab history, restore the selection set and scroll position that tab had for that directory if still valid. This requires each tab's history entry to carry its selection snapshot. Reconcile against the freshly streamed model on `Done` (nodes may have vanished while away).

**On filter change:**
- Rows filtered *out* are removed from the model's visible set. Their `NodeId`s are **kept in a "hidden selection" holding set**, not discarded — so clearing the filter restores them to the visible selection. Operations (§3, context menu) act only on the *visible* selection while a filter is active; the holding set is purely for restore. On navigation away, the holding set is dropped.
- Rows filtered *in* (filter loosened) rejoin; if their `NodeId` is in the holding set, they render selected again.

**On sort change:**
- Selection set identity is unchanged. Anchor and lead keep their `NodeId`s. Any live range is recomputed in the new visible order (a range that was contiguous in name-order may become non-contiguous in size-order — that's correct and expected; the range "freezes" into a plain set at that point, because the next model change after a sort is a structural change, not a range extension).

**On external mutation (file watcher / our own file ops):**
- The architecture doc says file mutations reload affected UI state through the shell rather than mutating rendered rows in place. After such a reload, reconcile selection: keep `NodeId`s that survive, drop the rest, re-seat anchor/lead.
- A node that was *renamed*: identity stability across rename is explicitly NodeStore's job and listed as not-yet-done in the streaming doc. **Until NodeStore provides rename-stable identity, a renamed node is treated as gone-and-new**: it drops out of the selection. Document this as a known limitation; do not fake it.

### 2.7 Tree multi-selection

Multi-select in the Browse tree is **optional for v1**. Recommendation: ship single-select for the tree first (one lead node, click navigates), and treat the file table as the place where multi-select and bulk operations live. If/when tree multi-select is added, it follows the same anchor/lead/modifier model as §2.4–2.5, with "visible order" meaning the flattened visible (expanded) tree order.

If the tree stays single-select in v1: a drag *from* the tree (§3) carries exactly one node.

### 2.8 What selection must never do

- Never query the filesystem to decide selectability. Every node in the model is selectable. There is no "is this still there" check on click — the model is the truth at interaction time, reconciliation (§2.6) handles divergence.
- Never resolve a path on click. Selection deals in `NodeId`. Path handoff happens later, at the scheduled-work boundary.
- Never block to load children when extending selection or moving the lead.
- Never mutate rendered rows in place to show selection — selection is in-memory state the render reads on the next frame, consistent with the work-scheduling model.

---

## 3. Drag and drop

Drag-and-drop is a hot-path interaction during the drag (hover, hit-testing, the insertion indicator) and a **scheduled operation** at the drop. The drag itself touches only cached state. The drop enqueues a file operation through the scheduler; it never performs the move/copy inline on the UI thread.

### 3.1 What can be dragged

| Source | Payload |
|---|---|
| File table rows | The current **selection set** (visible order). If the press lands on an unselected row, selection first replaces to that row (§3.3), then the drag carries that one node. If the press lands on a selected row, the drag carries the whole selection. |
| Browse tree node | The lead tree node (one node in v1; the selection set if/when tree multi-select lands). |
| Breadcrumb segment | The single node for that segment. |
| Favorites entry | Out of scope here — covered by the Favorites spec (reorder / tear-off). A Favorites entry is a *shortcut*, not a node payload. But a node *can be dropped onto* the Favorites section (that's the "add favorite" path in the Favorites spec). |
| Volumes entry | A volume root node, draggable like any folder node. |

The drag payload is a set of `NodeId`s plus the controlled `NodeId → PathBuf` map for exactly those nodes (the same controlled-handoff pattern streaming uses to hand paths to the UI layer). Paths are needed because the drop may go to an external app (§3.7) or to a platform crate that operates on paths. Resolving those paths happens **at drag start, once**, on the interaction state that's already cached — if a path isn't cached, that's a NodeStore/cache gap, not a reason to do I/O mid-drag.

### 3.2 Drag threshold

Mouse-down then movement past a small threshold (~4px) starts a drag. Below threshold on release = click (§2.4). This is the same disambiguation model as the Favorites reorder spec; keep the threshold constant shared.

### 3.3 Press-on-unselected vs press-on-selected

- **Press on an unselected row, then drag:** selection replaces to that row at the moment the drag threshold is crossed (not at mouse-down — mouse-down alone might be the start of a click). Drag carries that one node.
- **Press on a selected row, then drag:** selection is untouched; drag carries the whole selection set.
- **Press on a selected row, release without dragging (a click):** *now* selection replaces to that row. (Standard behavior: clicking one of several selected items collapses selection to it — but only on mouse-up, because mouse-down had to wait to see if it was a drag.)

### 3.4 Drag visuals

- A drag image: for a single node, its icon + name. For multiple, a stacked-badge style image with a count badge ("12"). The drag image is built from cached display fields and the cached icon if warmed; if the icon isn't warm yet, use the generic kind icon — never block to fetch the real one.
- Valid drop targets highlight on hover (whole-row / whole-folder highlight).
- Between-row insertion indicators are **not** used for the file table (the table is sorted, not manually ordered — you cannot drop "between" rows in a sorted list). Insertion indicators *are* used in manually-ordered contexts (Favorites), which is that spec's concern.
- The cursor / drag image reflects the resolved operation: move vs copy vs alias vs rejected (§3.6).
- Auto-scroll: dragging near the top/bottom edge of the table or the sidebar scrolls it, speed proportional to edge penetration. Auto-expand: hovering a collapsed tree folder for ~0.7s expands it (expansion is scheduled enumeration, not a blocking read; the expand may visibly populate a moment later).

### 3.5 Drop targets

| Target | Result |
|---|---|
| A **folder row** in the file table | Move/copy the payload into that folder (operation per §3.6). |
| **Empty space** in the file table | Move/copy the payload into the **current directory** of that tab. No-op if the payload is already in this directory and the op resolves to move. |
| A **file row** (non-folder) in the file table | Reject — you cannot drop nodes into a file. (Exception: if Ferail later supports "open with" by drop onto an app bundle, an `.app` row is a valid target meaning "open these with this app." Out of scope for v1; reject for now.) |
| A **Browse tree** folder node | Move/copy the payload into that folder. |
| A **Volumes** entry | Move/copy into the volume root. Cross-volume → resolves to copy (§3.6). |
| A **Favorites** entry | Move/copy into the folder the favorite points at. (Dropping *onto* a favorite is a file op; dropping *between* favorites is a Favorites reorder — that distinction lives in the Favorites spec.) |
| A **breadcrumb segment** | Move/copy into that ancestor folder. |
| Another **Ferail window / tab** | Same rules, targeting that tab's folder or the hovered folder in it. |
| **Outside the app** (Finder, other apps) | Standard macOS drag-out (§3.7). |
| The payload's **own folder** or a node **inside the payload's own selection** | Reject (you can't move a folder into itself or into its own descendant — but note §3.6: the descendant check requires care and is the one place a bounded check is allowed). |

### 3.6 Resolving the operation: move vs copy vs alias

The resolved operation depends on modifiers and on volume relationship, matching macOS:

| Condition | Operation |
|---|---|
| Same volume, no modifier | **Move** |
| Different volume, no modifier | **Copy** |
| **Option** held | Force **Copy** (even same volume) |
| **Cmd** held | Force **Move** (even cross-volume) |
| **Cmd+Option** held | Make **Alias** at the destination |
| Source is read-only / payload can't be moved | Falls back to **Copy**; if copy also impossible, **Reject** |
| Drop target == source directory and op == Move | **No-op** (don't churn the filesystem) |
| Target is inside the payload (descendant) | **Reject** |

The same-volume-vs-different-volume determination needs to know each node's volume. This must come from **cached metadata** (volume id should be part of cached node state, or derivable from the cached volume list + the node's path prefix). If volume identity for a node is not cached, the drag visual may show a provisional operation and the **final** operation is resolved at drop time inside the scheduled file-operation worker — the worker is allowed to do that I/O; the drag hover is not. The hover should degrade gracefully (show "move or copy" ambiguity) rather than block.

The **descendant check** ("is the target inside the payload") is the one bounded exception where the drop handler may need to walk parent links. Do it on cached path/parent data if available; if it genuinely requires resolution, it happens in the scheduled worker before the operation commits, and the operation aborts cleanly if it turns out to be a self-drop. Never walk the filesystem on the UI thread to answer this during hover — during hover, if uncertain, allow the drop and let the worker reject it with a clean error surfaced through the task/notification system.

### 3.7 Dragging out to other apps / Finder

- Ferail must publish the standard macOS pasteboard flavors for file drags (file URLs / promised files) so a drag to Finder, Mail, etc. works. This is `ferail-shell-mac`'s responsibility — the pasteboard/AppKit details live there, the GPUI layer just initiates the drag with a `NodeId` payload and lets the shell crate translate to the platform representation.
- Dragging *in* from Finder / other apps: the dropped pasteboard items are external paths. These enter through the shell-mac boundary, get registered as nodes (NodeStore), and the operation proceeds as a normal copy/move into the target. The architecture's "raw `PathBuf` at controlled boundaries" rule covers this — the inbound path arrives at the shell-mac / worker boundary, not in render code.

### 3.8 Executing the drop

This follows the architecture's work-scheduling pattern exactly:

1. The drop is a **semantic event**. It carries: payload `NodeId`s + their paths, the resolved (or provisional) operation, the target node id + path, a generation/identity stamp.
2. The UI **immediately** reflects intent without doing the I/O: the drag visual ends, a task appears in the task registry ("Moving 12 items…"), selection/scroll are preserved.
3. The actual move/copy/alias runs on a **background executor** (a file-operation worker), with the same cancellation discipline the streaming doc calls for as "remaining work — extend cancellation to copy/move."
4. Progress returns through the channel/notification pattern — not polling — to drive a progress UI in the task registry / notifications.
5. On completion, affected directories are reloaded **through the shell** (per the architecture's "reload affected UI state rather than mutate rendered rows in place"). Both the source directory and the destination directory reload if visible in any tab.
6. Selection after the operation: ideally the moved/copied nodes are selected **in the destination** once it reloads (so a move feels like the items "went with you" if you navigate there). At minimum, the source view reconciles its selection (the moved-out nodes drop from the source selection on its reload). This depends on NodeStore identity to do well; until then, do the minimum and document it.
7. Errors (permission denied, name collision, disk full, self-drop discovered late) surface through the task/notification UI with the operation's identity — never a freeze, never stderr-only. Partial success is first-class, same as streaming: if 9 of 12 moved before an error, the 9 are done, the 3 are reported, the UI stays usable.

### 3.9 Name collisions

When a drop's destination already contains an item with the same name:
- The collision is detected by the **file-operation worker**, not the UI thread.
- The worker pauses that item and emits a "needs decision" event for it. The UI shows a dialog (gpui-component Dialog): **Keep Both** (auto-rename the incoming — "name 2"), **Replace**, **Skip**, with an **"Apply to all"** checkbox for multi-item drops.
- While the dialog is open the rest of the operation can continue for non-colliding items (or pause entirely — pick one; "continue non-colliding, queue collisions" is better UX but more work, "pause all until answered" is acceptable for v1).
- The decision flows back to the worker through the same channel.

### 3.10 Cancellation

- A file operation in the task registry has a cancel affordance.
- Cancel sets the worker's `AtomicBool` (same pattern as enumeration cancellation), checked between items.
- Already-completed items in a partially-done operation are **not** rolled back (a move that completed 9 of 12 and is cancelled leaves 9 moved — rolling back filesystem operations is not safe to promise). The task UI reports the final partial state clearly.
- Cancelling does not freeze: the worker observes the flag between items and winds down.

---

## 4. Interaction with streaming enumeration

Because directory contents stream in, selection and drag have to behave well mid-load. Consolidated rules:

- **Selecting during a load is allowed.** The user can click and Cmd+Click rows that have already arrived while more stream in. New batches don't disturb the existing selection (§2.6).
- **Cmd+A during a load** selects everything *currently* in the model. If the user expects "all" to mean the final set, that's a reasonable surprise to handle: re-applying Cmd+A after `Done`, or having Cmd+A while loading show a subtle "selecting visible — load in progress" hint, are both acceptable. Minimum: Cmd+A selects the current model and that's documented.
- **A live Shift-range** spanning not-yet-arrived rows recomputes as batches land (§2.6).
- **Starting a drag during a load** is allowed; the payload is whatever's selected now.
- **Dropping into a folder that is currently being enumerated in another tab**: fine — the drop schedules a file op, the enumeration is a separate worker, and the destination tab reloads through the shell when the op completes. Generation stamps keep the two from interfering.
- **Navigating away mid-load** cancels enumeration (existing behavior) and starts the new path with empty selection (§2.6).

---

## 5. Component mapping (gpui-component)

- **Table / TableState** — owns row rendering, sort, resize, reorder; selection is layered on as per-tab interaction state keyed by `NodeId`, read by the cell renderer to draw selected/lead styling.
- **Tree** — Browse tree selection and expansion.
- **Menu** — context menus; target is the selection set or the context node per §2.4.
- **Dialog** — name-collision decisions (§3.9), the >N-items open confirmation (§2.5).
- **Notification** — drag-out rejections, operation errors, partial-success reports.
- Task registry (shell-owned) — in-progress file operations with progress and cancel.
- GPUI drag-and-drop primitives + `ferail-shell-mac` — the actual platform pasteboard for external drag in/out (§3.7).
- GPUI `Entity` + `cx.spawn` — the drop → worker → reload-through-shell boundary (§3.8).

---

## 6. Implementation order

1. **Single selection, file table** — click to select, click-empty to clear, Up/Down, the lead/focus ring. Reconciliation on navigation and on `Done`.
2. **Multi-selection, file table** — Cmd+Click toggle, Shift+Click range, Cmd+Shift additive range, anchor/lead model, Cmd+A, Esc.
3. **Keyboard selection** — full §2.5 table including type-ahead.
4. **Selection reconciliation with streaming** — §2.6 in full: live ranges, batch arrival, `Done` reconciliation, filter holding set, sort recompute, history restore.
5. **Right-click targeting** — the selected-vs-unselected right-click rule (§2.4), context menu target wiring.
6. **Drag within the table** — threshold, press-on-selected vs unselected, drag image, folder/empty-space drop targets, the work-scheduled drop (§3.8) with task registry + reload-through-shell.
7. **Operation resolution** — move/copy/alias modifiers, same/cross-volume from cached metadata, the self-drop and descendant rejects (§3.6).
8. **Drop onto sidebar** — tree nodes, Volumes, Favorites entries, breadcrumb segments as drop targets.
9. **Name-collision dialog** (§3.9) and **cancellation** (§3.10).
10. **External drag in/out** via shell-mac (§3.7).
11. **Tree multi-select** (§2.7) — only if/when wanted; single-select tree ships before this.
12. **Post-operation selection-in-destination** (§3.8 step 6) — gated on NodeStore identity work.

---

## 7. Acceptance checklist

Selection:
- [ ] Click selects one row; click on empty clears.
- [ ] Cmd+Click toggles individual rows; Shift+Click selects an inclusive range; Cmd+Shift+Click adds a range to existing selection.
- [ ] Anchor and lead behave correctly across mixed modifier gestures.
- [ ] Lead/focus ring is visually distinct from selection fill and from hover.
- [ ] Up/Down, Shift+Up/Down, Cmd+Up/Down, Cmd+Shift+Up/Down, Page Up/Down, Cmd+A, Esc all behave per §2.5.
- [x] Type-ahead moves the lead using cached names with no I/O (list + grid; prefix accumulation, repeated-letter cycling, scroll-into-view).
- [ ] File table and Browse tree hold independent selections; the inactive context renders dimmed.
- [ ] Right-click on a selected row keeps the selection; on an unselected row replaces it; both open a menu targeting the selection.
- [ ] Selection survives streaming batches unchanged in identity.
- [ ] A live Shift-range fills in correctly as in-between rows stream in.
- [ ] On `Done`, selection drops nodes no longer in the model; anchor/lead re-seat.
- [ ] Navigation starts empty selection; back/forward restores the prior selection for that directory, reconciled against the fresh model.
- [ ] Filtered-out selected nodes are restored to selection when the filter is cleared.
- [ ] Sort change keeps selection identity; live range freezes sensibly.
- [ ] No selection operation ever resolves a path or touches the filesystem on the UI thread.

Drag and drop:
- [ ] Drag threshold disambiguates click vs drag; press-on-unselected vs press-on-selected payload rules hold.
- [ ] Drag image shows single icon+name or a stacked image with count.
- [ ] Drop onto a folder row, empty space, tree node, volume, favorite, breadcrumb segment all route to the correct destination.
- [ ] Drop onto a file row is rejected; self-drop and descendant-drop are rejected.
- [ ] Move/copy/alias resolves correctly from modifiers and volume relationship.
- [ ] Same-dir move is a no-op.
- [ ] The drop schedules a background file operation; the UI never blocks; a task appears immediately.
- [ ] Operation progress arrives via channel/notification, not polling.
- [ ] Source and destination directories reload through the shell on completion.
- [ ] Name collisions raise a Keep Both / Replace / Skip dialog with Apply-to-all.
- [ ] Operations are cancellable via `AtomicBool`; partial completion is reported, not rolled back.
- [ ] Errors and partial success surface through the task/notification UI, never stderr-only, never a freeze.
- [ ] External drag out to Finder and drag in from Finder both work through the shell-mac boundary.
- [ ] Auto-scroll at table/sidebar edges and auto-expand of hovered tree folders work during a drag.
