# Feraille — Selection & DnD: Architecture and Decision Log

Working log for the `docs/features/feraille-selection-dnd-spec.md` work,
written under the Slow AI method.

## Architecture at a glance
- Selection state is per-tab in `Tab` (file table): `selection: HashSet<NodeId>`, `anchor: Option<NodeId>`, `lead: Option<NodeId>`. The legacy `selected: Option<usize>` is gone; the row index is derived from `lead` against the live delegate entries.
- The gpui-component `TableState`'s built-in `selected_row` stays mirrored to the lead so the primitive's native focus overlay marks it; we paint a softer accent bg in `render_tr` for the rest of the set.
- Selection mutations route through Shell helpers that always (a) update Tab state, (b) call `refresh_selection_parallel_vecs`, (c) push the lead row into the table, (d) `cx.notify()`. Skipping any of these leaves the UI inconsistent.
- Streaming reconciliation hooks the same refresh from `apply_directory_batch` and `finish_directory_load`. On `Done` we drop NodeIds no longer in the model.
- The original `target_row()` chain still works: context_row → lead row. Right-click on a row outside the set replaces selection; on a row inside, leaves it.

## Key decisions

### Layer multi-select over gpui-component instead of forking it
The Table primitive is pinned. Modifier-aware clicks are addressable through `window.modifiers()` at SelectRow time. We pay one extra hop (Shell intercepts SelectRow and re-applies modifier logic) but avoid maintaining a fork. If we ever need more (per-event cell click intercept, drag-select rubber-banding), revisit.

### Selection is `HashSet<NodeId>` only — no parallel ordered vec
Visible-order is the delegate's `entries` order. Recompute when needed (Cmd+A, range computation). Fine at typical folder sizes; revisit if 10k-file folders become real.

### Lead = native overlay; set-only members = our painted bg
Spec §2.3 wants a focus ring distinct from selection fill. The Table primitive already paints a 1-px accent border on `selected_row` — we use that as the focus ring by mirroring lead → `set_selected_row`. Our `render_tr` adds a `theme.accent.opacity(0.18)` bg for set members that aren't the lead. The lead row gets both, which reads naturally ("the focused one of the selected set").

## Trade-offs made under time pressure
- Live Shift-range reconciliation through streaming batches (spec §2.6 last bullet of streaming arrival) deferred to iter 2 — iter 1 freezes the range at click time.
- Tree multi-select left as single-select per spec §2.7 ("optional for v1").
- The existing `on_drag(ExternalPaths(...))` in file_list.rs still carries one path. Iter 1 only changes selection. Iter 3 expands the payload.

## With more time, I would
- Push modifier-aware clicks into gpui-component's TableEvent so other consumers (Disk Usage, settings tables) inherit the same model.
- Add a `Selection` type in `feraille-core` so the model isn't shell-specific.
- Build an integration test harness for selection that synthesizes ClickEvents with modifiers.

## Things to discuss in the walkthrough
- Why the parallel `selected_in_set` / `is_lead` vecs instead of querying the entity from `render_td`: render_td has `&mut Context<TableState>`, not Shell, and crossing that boundary is the kind of thing the Prime Directive warns against. Parallel vecs are the same pattern `heats` and `is_favorited` already use.
- Why we mirror lead → Table's `selected_row` instead of suppressing the native overlay: less to maintain, and the primitive's focus ring is exactly what spec §2.3 describes.
- How right-click targeting works after this change: `context_row` still drives a single-row target (it's set on right-click; first checked, then falls back to lead).
- The `suppress_select_row: u32` counter on Shell: `TableState::set_selected_row` always `cx.emit(SelectRow)`. Without the suppression, our mirror call would re-enter the subscription, hit the plain-click branch with empty modifiers, and collapse a freshly-built multi-selection back to a single row. The counter is bumped before every mirror call and decremented in the subscription. It's a counter not a bool because a render frame can queue multiple mirrors.
- The `pending_select_row(s)` fields on Shell are CLI-screenshot-harness escape hatches. The harness applies `--select-row(s)` before the streaming load delivers any batches; we stash the row indices and consume them on the first batch that resolves all of them to NodeIds. Cleared on navigation so a stale row index can't apply to a different directory.

## Iter 2 outcome
- **Delegate selection state went NodeId-keyed.** The old parallel vecs (`selected_in_set: Vec<bool>`, `is_lead: Vec<bool>`) became `selected_set: HashSet<NodeId>` + `lead: Option<NodeId>`. `render_tr` looks up `entries[row].id` against the set on each frame. Sort can now reorder rows in place without desyncing the selection visuals — the HashSet doesn't care about row order. Same property holds for any future incremental row mutation (rename-stable identity, etc.).
- **`load_path` no longer clears selection.** Clearing moved into `navigate` (and a corresponding seed-then-load happens in the new `restore_from_history` helper). `Refresh`, filter changes, `toggle_hidden`, and the fs watcher all preserve selection now and let `apply_directory_batch` / `finish_directory_load` reconcile it.
- **`HistoryEntry` carries selection per back-stop.** `Tab::history` is `Vec<HistoryEntry>` with `{path, selection, anchor, lead}`. On every `navigate`, the leaving entry is updated with the current snapshot before push. `navigate_back` / `navigate_forward` symmetrically save the current entry's snapshot, step, and restore via `restore_from_history`. The restored selection rides through `load_path` and is then reconciled against the fresh stream on `Done`.
- **`reconcile_done` is the canonical "after the load settles" pass.** It drops NodeIds not in the final visible model, except when a filter is active — those get moved to `Tab::filtered_out` instead so a future filter loosening can lift them back. It also re-seats `anchor` / `lead` if they vanished, and demotes `range_live` to false when its endpoints are gone.
- **Filter holding is implicit via the same path.** Narrowing the filter calls `load_path`; the new model shrinks; `reconcile_done` with filter active moves shrunk-out members to `filtered_out`. Loosening the filter does the inverse — `restore_filtered_out_against_model` runs on every batch + on `Done`, lifting members back as their rows arrive. `clear_active_selection` (Esc) also drains `filtered_out` so a follow-up filter loosen can't resurrect ghosts.
- **Live Shift-range now actually streams.** `range_live: bool` on `Tab` is set by `range_select` (Shift / Cmd+Shift click) and the `move_selection(..., extend=true, ..)` keyboard path; cleared by every non-range gesture (plain click, Cmd-click, plain kbd nav, Cmd+A, Esc, navigation). When set, `recompute_live_range` runs on every batch and at `Done`: if both `anchor` and `lead` are visible, selection is rebuilt as the inclusive anchor→lead span in the current visible order; otherwise it waits for the missing endpoint to arrive.
- **Verified via screenshots** at [screenshots/selection-iter2-multi.png](screenshots/selection-iter2-multi.png) (multi-select identity unchanged after the HashSet refactor) and [screenshots/selection-iter2-sort.png](screenshots/selection-iter2-sort.png) (sort applied with selection still alive).
- **Caveats deferred to later iters:** the spec's "sort change recomputes the span in the new visible order then freezes the range" polish — we keep the range live and rebuild on next batch instead (good enough on real-world flows; the strict freeze can land with a delegate→Shell hook later). DnD §3 and tree multi-select still queued.

## Iter 1 outcome
- All spec §2 file-table behaviors land: single click replace, Cmd-click toggle, Shift-click range, Cmd+Shift additive range, anchor/lead model, plain and Shift-extend keyboard nav, Cmd+A, Esc with filter-vs-selection priority, right-click rule (selected vs unselected).
- Status bar reads from the selection set: count + summed size across visible members.
- Preview pane reads the lead row, not the whole set (matches Finder).
- Spec §2.4 "Click on empty space below rows" not yet wired — the gpui-component table doesn't currently surface an empty-area click. Defer to iter 2 or whenever we tap that primitive.
- Spec §2.4 "Right-click on empty space" same status — not surfaced by the primitive yet.
- Spec §2.6 streaming reconciliation: minimal pass only. `refresh_file_list_selection` runs on every batch + Done so NodeIds in the set rejoin visually as their rows land. Live Shift-range recomputation across batches deferred to iter 2 (range freezes at click time).
- Verified visually: `screenshots/selection-iter1-single.png` (one row, focus ring, "1 of 44 selected"), `screenshots/selection-iter1-multi.png` (four rows, anchor=2, lead=8, "4 of 44 selected · 20.3 KB", lead distinct from set members).
