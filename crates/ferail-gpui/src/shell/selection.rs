use super::*;

/// Left padding of a grid row's cell flex (`.px_2()` ≈ 8px), subtracted
/// when mapping a pointer into per-cell content coordinates for marquee
/// hit-testing.
const GRID_ROW_PAD: f32 = 8.0;
/// Pointer travel (px) before a background press is treated as a marquee
/// sweep rather than a plain click that clears the selection.
const MARQUEE_THRESHOLD: f32 = 4.0;

impl Shell {
    // -- Selection model (spec §2) ---------------------------------
    //
    // Selection state lives on `Tab` as a HashSet<NodeId> + anchor +
    // lead. Every selection mutation routes through one of the
    // helpers in this block so the parallel render vecs in the
    // delegate (`selected_in_set`, `is_lead`) and the underlying
    // `TableState::selected_row` stay in lockstep. Read paths
    // (`target_row`, status bar, preview, screenshot driver) derive
    // a row index from the lead via `Tab::lead_row`.

    /// Apply a mouse-click gesture on a row, dispatching by
    /// modifiers per spec §2.4. The Table primitive has already
    /// stamped its own `selected_row = row_ix` before this fires
    /// (via `on_row_left_click -> set_selected_row`); in every
    /// branch below the lead also lands on `row_ix`, so the
    /// primitive's focus overlay tracks the lead without our help.
    pub(crate) fn apply_row_click_gesture(
        &mut self,
        row_ix: usize,
        modifiers: gpui::Modifiers,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.node_id_at_row(row_ix, cx) else {
            return;
        };
        let cmd = modifiers.secondary();
        let shift = modifiers.shift;
        if shift && cmd {
            // Cmd+Shift+Click: additive range — union anchor→row
            // into the existing set, lead = row, anchor unchanged.
            self.range_select(id, /* additive */ true, cx);
        } else if shift {
            // Shift+Click: replacement range from anchor to row,
            // lead = row. If no anchor, treat as plain click.
            self.range_select(id, /* additive */ false, cx);
        } else if cmd {
            // Cmd+Click: toggle membership; lead = row; anchor =
            // row when set non-empty, cleared when it just emptied.
            self.toggle_select(id, cx);
        } else {
            // Plain click: replace selection to just this row.
            self.replace_select_one(id, cx);
        }
        // Preview always follows the lead — keeps the right pane
        // and the table in lockstep regardless of which gesture
        // ran. Same cost as the old single-click behavior.
        self.request_preview_for_row(row_ix, cx);
        // Pre-warm the Open With cache so a follow-up right-click
        // builds its submenu without a synchronous shell query.
        self.warm_open_with_for_row(row_ix, cx);
        cx.notify();
    }

    pub(super) fn apply_row_keyboard_gesture(
        &mut self,
        row_ix: usize,
        modifiers: gpui::Modifiers,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.node_id_at_row(row_ix, cx) else {
            return;
        };
        if modifiers.shift {
            self.range_select(id, /* additive */ false, cx);
        } else {
            self.replace_select_one(id, cx);
        }
        self.request_preview_for_row(row_ix, cx);
        self.warm_open_with_for_row(row_ix, cx);
        cx.notify();
    }

    /// Spec §2.4: right-click on a selected row leaves the
    /// selection alone (so "operate on all 12 selected" works);
    /// right-click on an unselected row replaces selection to
    /// that single row before the menu opens. The menu's target
    /// reads `context_row` which is set by the caller before this
    /// runs.
    pub(super) fn apply_row_right_click(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        let Some(id) = self.node_id_at_row(row_ix, cx) else {
            return;
        };
        if !self.active_tab().selection.contains(&id) {
            self.replace_select_one(id, cx);
            cx.notify();
        }
        // Warm Open With for the menu target. Usually a no-op (the
        // click/keyboard gesture already warmed this row); covers
        // right-click-inside-selection where the lead never moved.
        self.warm_open_with_for_row(row_ix, cx);
    }

    /// Programmatic single-row select by row index. Used by the
    /// screenshot driver (`--select-row N`, `--select-name foo`)
    /// to seed a deterministic selection state without simulating
    /// a click. Equivalent to a plain click on the row.
    ///
    /// The screenshot harness runs this BEFORE the streaming load
    /// delivers any batches, so when `entries` is empty we stash
    /// the row index in `pending_select_row` and consume it on the
    /// next batch arrival. This preserves the old row-index-only
    /// semantics the harness depends on while keeping the runtime
    /// selection model NodeId-based.
    pub fn select_row_index(&mut self, row_ix: usize, cx: &mut Context<Self>) {
        if let Some(id) = self.node_id_at_row(row_ix, cx) {
            self.replace_select_one(id, cx);
            cx.notify();
        } else {
            self.active_tab_mut().pending_select_row = Some(row_ix);
        }
    }

    /// Apply a deferred `select_row_index` once entries are
    /// available. Called from `apply_directory_batch_in_tab` after
    /// the delegate has the new rows. Also drains any
    /// `pending_select_rows` (multi-row screenshot seed).
    ///
    /// Only acts when `idx` IS the active tab: the pending-select
    /// seed is a screenshot-harness affordance and the harness only
    /// loads the active tab; the inner select helpers
    /// (`replace_select_one` / `select_row_indices`) are
    /// active-tab-scoped gesture paths. A pending seed on a
    /// background tab stays queued until that tab's next batch
    /// while active.
    pub(super) fn apply_pending_select_row_in_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx != self.active {
            return;
        }
        if let Some(row_ix) = self.active_tab().pending_select_row {
            if let Some(id) = self.node_id_at_row(row_ix, cx) {
                self.active_tab_mut().pending_select_row = None;
                self.replace_select_one(id, cx);
                cx.notify();
            }
        }
        if !self.active_tab().pending_select_rows.is_empty() {
            let rows = std::mem::take(&mut self.active_tab_mut().pending_select_rows);
            self.select_row_indices(&rows, cx);
        }
        self.apply_pending_select_names(cx);
    }

    /// Queue leaf `names` for selection in the active tab, but only when it
    /// is still a plain directory view of `dir` — the "select what I just
    /// pasted/renamed/created" affordance. If the user moved away (navigated
    /// elsewhere, or the tab shows search/dupe results) the operation's
    /// results are not where they're looking, and selecting would be a
    /// surprise: do nothing. The names apply when the post-op reload's rows
    /// land (`apply_pending_select_names`), which also scrolls the first one
    /// into view.
    pub(super) fn queue_select_names_if_current(
        &mut self,
        dir: &std::path::Path,
        names: Vec<String>,
    ) {
        if names.is_empty() {
            return;
        }
        let tab = self.active_tab();
        if tab.tool_result.is_some() || tab.current_dir != dir {
            return;
        }
        self.active_tab_mut().pending_select_names = names;
    }

    /// Resolve queued leaf names (see [`Tab::pending_select_names`]) against
    /// the rows that have now landed and select them.
    ///
    /// Streaming loads deliver in batches, so a name may simply not be here
    /// yet: only the names that resolve are consumed, and the rest stay queued
    /// for a later batch. Names that never arrive — the file was moved or
    /// renamed between the operation and the click, or it's hidden/filtered
    /// in this view — are dropped when the load completes
    /// (`finish_directory_load_in_tab`) or the tab navigates.
    pub(super) fn apply_pending_select_names(&mut self, cx: &mut Context<Self>) {
        if self.active_tab().pending_select_names.is_empty() {
            return;
        }
        let queued = self.active_tab().pending_select_names.clone();
        let (found, missing): (Vec<usize>, Vec<String>) = {
            let table = self.active_tab().table.read(cx);
            let entries = &table.delegate().entries;
            let mut found = Vec::new();
            let mut missing = Vec::new();
            for name in queued {
                match entries.iter().position(|e| e.name == name) {
                    Some(i) => found.push(i),
                    None => missing.push(name),
                }
            }
            (found, missing)
        };
        if found.is_empty() {
            return;
        }
        self.active_tab_mut().pending_select_names = missing;
        let first = found[0];
        self.select_row_indices(&found, cx);
        // Scroll the result into view. Without this the selection can land
        // off-screen in a long listing and the reveal looks like it did
        // nothing — which is exactly how the first version of the extract
        // confirmation failed.
        let grid = matches!(self.active_tab().view_mode, crate::grid::ViewMode::Grid);
        if grid {
            let cols = self.grid_cols(cx).max(1);
            self.active_tab()
                .grid_scroll
                .scroll_to_item(first / cols, gpui::ScrollStrategy::Center);
        } else {
            self.active_tab()
                .table
                .update(cx, |table, cx| table.scroll_to_row(first, cx));
        }
        cx.notify();
    }

    /// Programmatic multi-row select used by the screenshot
    /// harness (`--select-rows`). First index becomes the anchor,
    /// last becomes the lead. If any index is out of range we
    /// stash the whole list for retry on the next batch arrival.
    pub fn select_row_indices(&mut self, rows: &[usize], cx: &mut Context<Self>) {
        let ids: Option<Vec<NodeId>> = rows.iter().map(|r| self.node_id_at_row(*r, cx)).collect();
        let Some(ids) = ids else {
            self.active_tab_mut().pending_select_rows = rows.to_vec();
            return;
        };
        if ids.is_empty() {
            return;
        }
        let anchor = ids.first().copied();
        let lead = ids.last().copied();
        let tab = self.active_tab_mut();
        tab.selection = ids.into_iter().collect();
        tab.anchor = anchor;
        tab.lead = lead;
        self.refresh_file_list_selection(cx);
        cx.notify();
    }

    /// Plain-click semantics: selection = {id}, anchor = lead = id.
    /// Non-range gesture → clears `range_live`.
    pub(super) fn replace_select_one(&mut self, id: NodeId, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.selection.clear();
        tab.selection.insert(id);
        tab.anchor = Some(id);
        tab.lead = Some(id);
        tab.range_live = false;
        self.refresh_file_list_selection(cx);
    }

    /// Cmd+Click semantics: toggle `id` in the set. lead = id.
    /// Empty after toggle → anchor cleared; otherwise anchor = id.
    /// Non-range gesture → clears `range_live`.
    pub(super) fn toggle_select(&mut self, id: NodeId, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        if !tab.selection.remove(&id) {
            tab.selection.insert(id);
        }
        tab.lead = Some(id);
        tab.anchor = if tab.selection.is_empty() {
            None
        } else {
            Some(id)
        };
        tab.range_live = false;
        self.refresh_file_list_selection(cx);
    }

    /// Shift+Click and Cmd+Shift+Click: compute the inclusive
    /// span from anchor to `id` in visible (delegate) order; if
    /// `additive` (Cmd+Shift), union it into the existing set,
    /// otherwise replace. lead = id; anchor unchanged (or seeded
    /// to id when there was none, matching spec's "treat as plain
    /// click").
    pub(super) fn range_select(&mut self, id: NodeId, additive: bool, cx: &mut Context<Self>) {
        let entries: Vec<NodeId> = self
            .active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .iter()
            .map(|e| e.id)
            .collect();
        let Some(target_idx) = entries.iter().position(|x| *x == id) else {
            return;
        };
        let anchor_id = self.active_tab().anchor;
        let anchor_idx = anchor_id.and_then(|a| entries.iter().position(|x| *x == a));
        match anchor_idx {
            None => {
                // No anchor → treat as plain click (spec §2.4).
                self.replace_select_one(id, cx);
            }
            Some(a_idx) => {
                let (lo, hi) = if a_idx <= target_idx {
                    (a_idx, target_idx)
                } else {
                    (target_idx, a_idx)
                };
                let span: HashSet<NodeId> = entries[lo..=hi].iter().copied().collect();
                let tab = self.active_tab_mut();
                if additive {
                    tab.selection.extend(span);
                } else {
                    tab.selection = span;
                }
                tab.lead = Some(id);
                // Range gesture → mark live so the span keeps
                // recomputing as rows stream in (spec §2.6).
                tab.range_live = true;
                // Anchor unchanged.
                self.refresh_file_list_selection(cx);
            }
        }
    }

    /// Spec §2.5: Cmd+A — select every row currently in the
    /// (filtered) model. anchor = first visible, lead = last.
    /// Non-range gesture → clears `range_live`.
    pub(super) fn select_all_visible(&mut self, cx: &mut Context<Self>) {
        let (all, first, last): (HashSet<NodeId>, Option<NodeId>, Option<NodeId>) = {
            let delegate = self.active_tab().table.read(cx).delegate();
            let ids: Vec<NodeId> = delegate.entries.iter().map(|e| e.id).collect();
            let first = ids.first().copied();
            let last = ids.last().copied();
            (ids.into_iter().collect(), first, last)
        };
        let tab = self.active_tab_mut();
        tab.selection = all;
        tab.anchor = first;
        tab.lead = last;
        tab.range_live = false;
        self.refresh_file_list_selection(cx);
        cx.notify();
    }

    /// Spec §2.5 Esc: clear selection, anchor, lead. Also drains
    /// the filter holding set so a subsequent filter-loosen
    /// doesn't restore ghosts.
    pub fn clear_active_selection(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab_mut();
        tab.selection.clear();
        tab.anchor = None;
        tab.lead = None;
        tab.filtered_out.clear();
        tab.range_live = false;
        self.refresh_file_list_selection(cx);
        cx.notify();
    }

    /// Rebuild the delegate's per-row `selected_in_set` + `is_lead`
    /// parallel vecs from the active tab's selection state, and
    /// mirror the lead's row index into the underlying
    /// `TableState::selected_row` so the primitive's focus overlay
    /// matches the keyboard cursor. Called after every selection
    /// mutation, after every streaming batch, and on `Done`.
    ///
    /// We only call `set_selected_row` when the row index actually
    /// differs to avoid redundant redraw/scroll work. Semantic
    /// selection comes from `RowClicked` / `LeadMoved`; `SelectRow`
    /// is now just the table's internal lead mirror.
    pub fn refresh_file_list_selection(&mut self, cx: &mut Context<Self>) {
        let idx = self.active;
        self.refresh_file_list_selection_in_tab(idx, cx);
    }

    /// Tab-explicit variant for the streaming pipeline, which targets
    /// the loading tab by index rather than whatever tab happens to be
    /// active when a batch lands (Phase A+B's active-swap hack is
    /// gone; helpers now address the tab directly).
    pub(super) fn refresh_file_list_selection_in_tab(
        &mut self,
        idx: usize,
        cx: &mut Context<Self>,
    ) {
        // Snapshot the tab's selection state so the table.update
        // closure doesn't need to borrow Shell again.
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        let selection = tab.selection.clone();
        let lead = tab.lead;
        let table = tab.table.clone();
        let lead_row = table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            delegate.selected_set = selection;
            delegate.invalidate_drag_snapshot();
            delegate.lead = lead;
            let lead_row = lead.and_then(|id| delegate.entries.iter().position(|e| e.id == id));
            state.refresh(cx);
            lead_row
        });
        match lead_row {
            Some(row) => {
                let needs_set = table.read(cx).selected_row() != Some(row);
                if needs_set {
                    table.update(cx, |state, cx| {
                        // Lead mirror, not a click: keep the table's
                        // right-clicked row so an open context menu can
                        // still rebuild itself against it.
                        state.mirror_lead_row(row, cx);
                    });
                    // The list view auto-scrolls via `set_selected_row`,
                    // but the grid's `uniform_list` rides its own
                    // `grid_scroll` handle that the table never touches — so
                    // reveal the lead there too. Without this, Back / parent
                    // navigation (which seeds the child folder as the lead)
                    // selects a cell that may sit far below the fold while
                    // the grid stays pinned at the top. Gated on the lead
                    // actually changing (`needs_set`), matching the table, so
                    // streaming batches don't fight the user's scroll.
                    if let Some(tab) = self.tabs.get(idx) {
                        if matches!(tab.view_mode, crate::grid::ViewMode::Grid) {
                            let icon_px = crate::grid::icon_size(cx);
                            let gap = crate::grid::cell_gap(cx);
                            let w = f32::from(tab.grid_pane_width)
                                .max(crate::grid::cell_width(icon_px, gap));
                            let cols = crate::grid::cols_per_row(w, icon_px, gap).max(1);
                            tab.grid_scroll
                                .scroll_to_item(row / cols, gpui::ScrollStrategy::Center);
                        }
                    }
                }
            }
            // No lead → no selection. Clear the primitive's focus overlay so
            // it can't paint a phantom ring on whatever row inherits the
            // stale index after a folder switch (the preview correctly shows
            // "No selection"; the file pane must agree).
            None => {
                if table.read(cx).selected_row().is_some() {
                    table.update(cx, |state, cx| state.clear_selected_row(cx));
                }
            }
        }
    }

    /// Resolve the NodeId at a given row index by reading the
    /// delegate's `entries`. Cheap (one indexed access). Returns
    /// `None` if the row index is out of bounds — possible if the
    /// model changed between an event being queued and dispatched.
    pub(super) fn node_id_at_row(&self, row_ix: usize, cx: &App) -> Option<NodeId> {
        self.active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .get(row_ix)
            .map(|e| e.id)
    }

    /// Spec §2.6 streaming arrival. For each NodeId currently in
    /// the tab's `filtered_out` holding set whose row has now
    /// arrived in the model, move it back into `selection`. Runs
    /// after every batch and at `Done`. Doesn't drop anything —
    /// dropping is `reconcile_done`'s job.
    pub(super) fn restore_filtered_out_against_model_in_tab(
        &mut self,
        idx: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        let visible: HashSet<NodeId> = tab
            .table
            .read(cx)
            .delegate()
            .entries
            .iter()
            .map(|e| e.id)
            .collect();
        let tab = &mut self.tabs[idx];
        if tab.filtered_out.is_empty() {
            return;
        }
        let mut restored = false;
        tab.filtered_out.retain(|id| {
            if visible.contains(id) {
                tab.selection.insert(*id);
                restored = true;
                false
            } else {
                true
            }
        });
        if restored {
            self.refresh_file_list_selection_in_tab(idx, cx);
        }
    }

    /// Spec §2.6 live Shift-range recompute. When the tab's
    /// `range_live` flag is set, the selection is logically the
    /// anchor→lead inclusive span. As rows stream in, the visible
    /// position of the endpoints may change and rows between them
    /// may newly appear. Recompute the span on every batch + at
    /// `Done`. No-op if either endpoint isn't visible yet — we
    /// wait for the row that defines them to land before binding.
    pub(super) fn recompute_live_range_in_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        if !tab.range_live {
            return;
        }
        let (anchor_id, lead_id) = match (tab.anchor, tab.lead) {
            (Some(a), Some(l)) => (a, l),
            _ => return,
        };
        let entries: Vec<NodeId> = tab
            .table
            .read(cx)
            .delegate()
            .entries
            .iter()
            .map(|e| e.id)
            .collect();
        let anchor_idx = entries.iter().position(|x| *x == anchor_id);
        let lead_idx = entries.iter().position(|x| *x == lead_id);
        let (Some(a), Some(l)) = (anchor_idx, lead_idx) else {
            return;
        };
        let (lo, hi) = if a <= l { (a, l) } else { (l, a) };
        let span: HashSet<NodeId> = entries[lo..=hi].iter().copied().collect();
        self.tabs[idx].selection = span;
        self.refresh_file_list_selection_in_tab(idx, cx);
    }

    /// Spec §2.6 `LoadMsg::Done` reconciliation. Drops NodeIds no
    /// longer present in the final model from `selection` (or
    /// holds them in `filtered_out` when a filter is active), and
    /// re-seats `anchor` / `lead` if they vanished. Also runs the
    /// other reconcile passes one last time so a range that just
    /// became valid is bound by the time the load is "done."
    pub(super) fn reconcile_done_in_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        let visible: HashSet<NodeId> = tab
            .table
            .read(cx)
            .delegate()
            .entries
            .iter()
            .map(|e| e.id)
            .collect();
        let filter_active = !tab.filter_text.trim().is_empty();
        let tab = &mut self.tabs[idx];
        let mut moved = false;
        if filter_active {
            // Move members not in the visible set into the filter
            // holding set, preserving them across a future filter
            // loosening (spec §2.6 filter rule).
            tab.selection.retain(|id| {
                if visible.contains(id) {
                    true
                } else {
                    tab.filtered_out.insert(*id);
                    moved = true;
                    false
                }
            });
        } else {
            // Filter is empty → ghost ids are genuinely gone.
            // Drop both selection and filter-holding entries that
            // aren't in the model.
            let before = tab.selection.len();
            tab.selection.retain(|id| visible.contains(id));
            let after = tab.selection.len();
            moved = moved || (before != after);
            let fo_before = tab.filtered_out.len();
            tab.filtered_out.retain(|id| visible.contains(id));
            moved = moved || (fo_before != tab.filtered_out.len());
        }
        // Re-seat anchor / lead if they're gone.
        if let Some(a) = tab.anchor {
            if !visible.contains(&a) {
                tab.anchor = None;
                moved = true;
            }
        }
        if let Some(l) = tab.lead {
            if !visible.contains(&l) {
                tab.lead = None;
                moved = true;
            }
        }
        // If neither endpoint is in the model anymore, the live
        // range can't reconcile — let it freeze.
        if tab.range_live && (tab.anchor.is_none() || tab.lead.is_none()) {
            tab.range_live = false;
            moved = true;
        }
        if moved {
            self.refresh_file_list_selection_in_tab(idx, cx);
        }
        // Final pass: restore any filtered_out members that did
        // make it in and recompute a still-live range.
        self.restore_filtered_out_against_model_in_tab(idx, cx);
        self.recompute_live_range_in_tab(idx, cx);
    }

    /// Keyboard navigation: move the lead by `delta` and, when
    /// `extend` is true (Shift-extend variants), make the
    /// selection the inclusive span from anchor to the new lead.
    /// Plain moves replace the selection with just the new lead
    /// (spec §2.5).
    pub(super) fn move_selection(
        &mut self,
        delta: SelectionDelta,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        let entries: Vec<NodeId> = self
            .active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .iter()
            .map(|e| e.id)
            .collect();
        let len = entries.len();
        if len == 0 {
            self.clear_active_selection(cx);
            return;
        }
        let page = 12i64;
        let last = len as i64 - 1;
        let cur_idx: i64 = self
            .active_tab()
            .lead
            .and_then(|id| entries.iter().position(|x| *x == id))
            .map(|i| i as i64)
            .unwrap_or(0);
        let next: i64 = match delta {
            SelectionDelta::Up => cur_idx - 1,
            SelectionDelta::Down => cur_idx + 1,
            SelectionDelta::PageUp => cur_idx - page,
            SelectionDelta::PageDown => cur_idx + page,
            SelectionDelta::First => 0,
            SelectionDelta::Last => last,
        };
        let clamped = next.clamp(0, last) as usize;
        let new_lead = entries[clamped];
        if extend {
            // Range-extend keeps anchor fixed; if there was no
            // anchor, seed it at the previous lead (or the new
            // one, which collapses to plain navigation).
            let tab = self.active_tab_mut();
            if tab.anchor.is_none() {
                tab.anchor = tab.lead.or(Some(new_lead));
            }
            let anchor_id = tab.anchor.unwrap_or(new_lead);
            let anchor_idx = entries
                .iter()
                .position(|x| *x == anchor_id)
                .unwrap_or(clamped);
            let (lo, hi) = if anchor_idx <= clamped {
                (anchor_idx, clamped)
            } else {
                (clamped, anchor_idx)
            };
            tab.selection = entries[lo..=hi].iter().copied().collect();
            tab.lead = Some(new_lead);
            // Spec §2.6: a Shift-extend keyboard nav keeps the
            // range live so subsequent batches recompute the span.
            tab.range_live = true;
        } else {
            // Plain navigation collapses selection — replace_select_one
            // clears range_live.
            self.replace_select_one(new_lead, cx);
            return;
        }
        self.refresh_file_list_selection(cx);
        // Preview pane follows the lead.
        self.request_preview_for_row(clamped, cx);
        cx.notify();
    }

    /// Icon-grid navigation: move the lead by an arbitrary signed
    /// `step` (±1 for Left/Right, ±columns for Up/Down), with the same
    /// extend/collapse semantics as [`Self::move_selection`]. The linear
    /// anchor→lead span is used for Shift-extend (matching the list).
    pub(super) fn move_grid_selection(&mut self, step: i64, extend: bool, cx: &mut Context<Self>) {
        let entries: Vec<NodeId> = self
            .active_tab()
            .table
            .read(cx)
            .delegate()
            .entries
            .iter()
            .map(|e| e.id)
            .collect();
        let len = entries.len();
        if len == 0 {
            self.clear_active_selection(cx);
            return;
        }
        let last = len as i64 - 1;
        let cur_idx: i64 = self
            .active_tab()
            .lead
            .and_then(|id| entries.iter().position(|x| *x == id))
            .map(|i| i as i64)
            .unwrap_or(0);
        let clamped = (cur_idx + step).clamp(0, last) as usize;
        let new_lead = entries[clamped];
        if extend {
            let tab = self.active_tab_mut();
            if tab.anchor.is_none() {
                tab.anchor = tab.lead.or(Some(new_lead));
            }
            let anchor_id = tab.anchor.unwrap_or(new_lead);
            let anchor_idx = entries
                .iter()
                .position(|x| *x == anchor_id)
                .unwrap_or(clamped);
            let (lo, hi) = if anchor_idx <= clamped {
                (anchor_idx, clamped)
            } else {
                (clamped, anchor_idx)
            };
            tab.selection = entries[lo..=hi].iter().copied().collect();
            tab.lead = Some(new_lead);
            tab.range_live = true;
            self.refresh_file_list_selection(cx);
            self.request_preview_for_row(clamped, cx);
            cx.notify();
        } else {
            self.replace_select_one(new_lead, cx);
        }
    }

    /// Columns-per-row of the active tab's grid, from the cached pane
    /// width and live icon size. At least 1.
    fn grid_cols(&self, cx: &App) -> usize {
        let icon_px = crate::grid::icon_size(cx);
        let gap = crate::grid::cell_gap(cx);
        let w =
            f32::from(self.active_tab().grid_pane_width).max(crate::grid::cell_width(icon_px, gap));
        crate::grid::cols_per_row(w, icon_px, gap)
    }

    pub(super) fn on_grid_left(&mut self, _: &GridLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_grid_selection(-1, false, cx);
    }
    pub(super) fn on_grid_right(&mut self, _: &GridRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_grid_selection(1, false, cx);
    }
    pub(super) fn on_grid_up(&mut self, _: &GridUp, _: &mut Window, cx: &mut Context<Self>) {
        let c = self.grid_cols(cx) as i64;
        self.move_grid_selection(-c, false, cx);
    }
    pub(super) fn on_grid_down(&mut self, _: &GridDown, _: &mut Window, cx: &mut Context<Self>) {
        let c = self.grid_cols(cx) as i64;
        self.move_grid_selection(c, false, cx);
    }
    pub(super) fn on_grid_left_extend(
        &mut self,
        _: &GridLeftExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_grid_selection(-1, true, cx);
    }
    pub(super) fn on_grid_right_extend(
        &mut self,
        _: &GridRightExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_grid_selection(1, true, cx);
    }
    pub(super) fn on_grid_up_extend(
        &mut self,
        _: &GridUpExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let c = self.grid_cols(cx) as i64;
        self.move_grid_selection(-c, true, cx);
    }
    pub(super) fn on_grid_down_extend(
        &mut self,
        _: &GridDownExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let c = self.grid_cols(cx) as i64;
        self.move_grid_selection(c, true, cx);
    }

    // -- Grid marquee / rubber-band selection ----------------------
    //
    // A press on the grid's empty background (not a cell) begins a
    // marquee; dragging sweeps a selection rectangle over the cells. The
    // gesture lives on the grid root div (`grid_body`); these handlers
    // run in window space and map into grid content space via the tab's
    // cached `grid_pane_origin` + live scroll offset. All geometry is
    // analytic (no per-cell bounds are stored) — the same O(n) scan the
    // list's `range_select` already does.

    /// Map a window-space pointer position to the grid entry index under
    /// it, or `None` when the pointer is over empty background (a gap
    /// column, below the last row, or a trailing empty slot). Drives the
    /// marquee's "start only on background" guard so presses on real
    /// cells still reach the cell's own click/drag handlers.
    fn grid_index_at(&self, pos: gpui::Point<Pixels>, cx: &App) -> Option<usize> {
        let tab = self.active_tab();
        let icon_px = crate::grid::icon_size(cx);
        let gap = crate::grid::cell_gap(cx);
        let cell_w = crate::grid::cell_width(icon_px, gap);
        let cell_h = crate::grid::cell_height(icon_px, gap);
        let cols = self.grid_cols(cx);
        let off = tab.grid_scroll.0.borrow().base_handle.offset();
        let o = tab.grid_pane_origin;
        let content_x = f32::from(pos.x) - f32::from(o.x) - f32::from(off.x) - GRID_ROW_PAD;
        let content_y = f32::from(pos.y) - f32::from(o.y) - f32::from(off.y);
        if content_x < 0.0 || content_y < 0.0 {
            return None;
        }
        let col = (content_x / cell_w).floor() as usize;
        if col >= cols {
            return None;
        }
        let row = (content_y / cell_h).floor() as usize;
        let i = row * cols + col;
        let n = tab.table.read(cx).delegate().entries.len();
        (i < n).then_some(i)
    }

    /// Mouse-down on the grid root: begin a marquee only when the press
    /// lands on empty background. Shift/Cmd makes it additive (union with
    /// the existing selection).
    pub(super) fn on_grid_marquee_down(
        &mut self,
        ev: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.grid_index_at(ev.position, cx).is_some() {
            return;
        }
        let additive = ev.modifiers.shift || ev.modifiers.secondary();
        window.focus(&self.active_tab().grid_focus, cx);
        let base = if additive {
            self.active_tab().selection.clone()
        } else {
            HashSet::new()
        };
        self.active_tab_mut().marquee = Some(super::tab::Marquee {
            start: ev.position,
            current: ev.position,
            additive,
            base,
            moved: false,
        });
    }

    /// Mouse-move while a marquee is live: grow the rectangle and, once
    /// the pointer has travelled past the click threshold, recompute the
    /// swept selection.
    pub(super) fn on_grid_marquee_move(
        &mut self,
        ev: &gpui::MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((start, was_moved)) = self
            .active_tab()
            .marquee
            .as_ref()
            .map(|m| (m.start, m.moved))
        else {
            return;
        };
        let dx = f32::from(ev.position.x) - f32::from(start.x);
        let dy = f32::from(ev.position.y) - f32::from(start.y);
        let past = dx.abs() > MARQUEE_THRESHOLD || dy.abs() > MARQUEE_THRESHOLD;
        if let Some(m) = self.active_tab_mut().marquee.as_mut() {
            m.current = ev.position;
            if past {
                m.moved = true;
            }
        }
        if was_moved || past {
            self.apply_marquee_selection(cx);
            cx.notify();
        }
    }

    /// Mouse-up (inside or outside the pane): finish the marquee. A
    /// press that never moved is a plain background click — it clears the
    /// selection (unless it was additive, which is a no-op).
    pub(super) fn on_grid_marquee_up(
        &mut self,
        _ev: &gpui::MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(m) = self.active_tab_mut().marquee.take() else {
            return;
        };
        if !m.moved && !m.additive {
            self.clear_active_selection(cx);
        } else {
            cx.notify();
        }
    }

    /// Recompute the selection swept by the current marquee rectangle,
    /// unioning with the pre-drag snapshot when the gesture is additive.
    fn apply_marquee_selection(&mut self, cx: &mut Context<Self>) {
        let (start, current, base) = {
            let Some(m) = self.active_tab().marquee.as_ref() else {
                return;
            };
            (m.start, m.current, m.base.clone())
        };
        let tab = self.active_tab();
        let icon_px = crate::grid::icon_size(cx);
        let gap = crate::grid::cell_gap(cx);
        let cell_w = crate::grid::cell_width(icon_px, gap);
        let cell_h = crate::grid::cell_height(icon_px, gap);
        let cols = self.grid_cols(cx);
        let off = tab.grid_scroll.0.borrow().base_handle.offset();
        let o = tab.grid_pane_origin;
        let to_content = |p: gpui::Point<Pixels>| {
            (
                f32::from(p.x) - f32::from(o.x) - f32::from(off.x) - GRID_ROW_PAD,
                f32::from(p.y) - f32::from(o.y) - f32::from(off.y),
            )
        };
        let (ax, ay) = to_content(start);
        let (bx, by) = to_content(current);
        let (x0, x1) = (ax.min(bx), ax.max(bx));
        let (y0, y1) = (ay.min(by), ay.max(by));
        let entries: Vec<NodeId> = tab
            .table
            .read(cx)
            .delegate()
            .entries
            .iter()
            .map(|e| e.id)
            .collect();
        let mut hits = base;
        let mut last_hit: Option<NodeId> = None;
        for (i, id) in entries.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let cx0 = col as f32 * cell_w;
            let cy0 = row as f32 * cell_h;
            // Rectangle-vs-rectangle overlap (half-open on the far edges).
            if x0 < cx0 + cell_w && x1 > cx0 && y0 < cy0 + cell_h && y1 > cy0 {
                hits.insert(*id);
                last_hit = Some(*id);
            }
        }
        let tab = self.active_tab_mut();
        tab.selection = hits;
        tab.range_live = false;
        if let Some(l) = last_hit {
            tab.lead = Some(l);
            tab.anchor = Some(l);
        }
        self.refresh_file_list_selection(cx);
    }

    pub(super) fn on_cursor_up(&mut self, _: &CursorUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::Up, false, cx);
    }
    pub(super) fn on_cursor_down(
        &mut self,
        _: &CursorDown,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::Down, false, cx);
    }
    pub(super) fn on_cursor_first(
        &mut self,
        _: &CursorFirst,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::First, false, cx);
    }
    pub(super) fn on_cursor_last(
        &mut self,
        _: &CursorLast,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::Last, false, cx);
    }
    pub(super) fn on_page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::PageUp, false, cx);
    }
    pub(super) fn on_page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(SelectionDelta::PageDown, false, cx);
    }

    pub(super) fn on_cursor_up_extend(
        &mut self,
        _: &CursorUpExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::Up, true, cx);
    }
    pub(super) fn on_cursor_down_extend(
        &mut self,
        _: &CursorDownExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::Down, true, cx);
    }
    pub(super) fn on_cursor_first_extend(
        &mut self,
        _: &CursorFirstExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::First, true, cx);
    }
    pub(super) fn on_cursor_last_extend(
        &mut self,
        _: &CursorLastExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::Last, true, cx);
    }
    pub(super) fn on_page_up_extend(
        &mut self,
        _: &PageUpExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::PageUp, true, cx);
    }
    pub(super) fn on_page_down_extend(
        &mut self,
        _: &PageDownExtend,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(SelectionDelta::PageDown, true, cx);
    }

    pub(super) fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_all_visible(cx);
    }

    pub(super) fn on_clear_selection(
        &mut self,
        _: &ClearSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_active_selection(cx);
    }

    /// Type-to-select. A printable key pressed with the file list or
    /// icon grid focused jumps the selection to the first entry whose
    /// display name starts with the accumulated prefix, scrolling it
    /// into view. Wired as an `on_key_down` on the shell root, so it
    /// covers both views (the grid renders inside the same tree).
    ///
    /// gpui matches keybindings to actions *before* running key
    /// listeners (window.rs `dispatch_key_event`), so arrows, Space
    /// (Quick Look), Cmd/Ctrl chords, etc. are consumed as actions and
    /// never reach here — only unbound printable characters do.
    pub(crate) fn on_typeahead_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let grid = matches!(self.active_tab().view_mode, crate::grid::ViewMode::Grid);
        // `contains_focused`, not `is_focused`: clicking a row moves window
        // focus to the table's own handle (a descendant of the shell root),
        // and typing right after a click must still typeahead. Exact-match
        // checking silently disabled typeahead after any click — found on
        // the AROS port, but latent on every platform.
        let view_focused = if grid {
            self.active_tab().grid_focus.contains_focused(window, cx)
        } else {
            self.focus_handle.contains_focused(window, cx)
        };
        // A focused text input outranks the view: the toolbar filter, or a
        // modal name prompt (New Folder / Rename / favorite label) that
        // renders inside a Root layer `contains_focused` counts as part of
        // the shell subtree. `has_focused_input` (Root's own tracking)
        // catches every gpui-component input wherever it sits in the tree.
        let input_focused = window.has_focused_input(cx);

        // Precedence is a pure function so it's unit-testable without a
        // window — see `typeahead_char` and its tests.
        let Some(ch) = typeahead_char(
            input_focused,
            view_focused,
            &event.keystroke.modifiers,
            event.keystroke.key_char.as_deref(),
        ) else {
            return;
        };

        if self.typeahead_advance(ch, grid, cx) {
            // Consumed — don't let the character fall through to any
            // IME / text-input handling on the way back up.
            cx.stop_propagation();
        }
    }

    /// Append `ch` to the type-to-select buffer (resetting it first if
    /// the idle timeout elapsed) and move the selection to the first
    /// matching entry. Returns whether a keystroke was consumed.
    ///
    /// Behaviour mirrors Finder: accumulating characters narrow the
    /// prefix from the top of the list; pressing the *same* single
    /// character repeatedly cycles through every entry starting with
    /// it, wrapping around.
    fn typeahead_advance(&mut self, ch: char, grid: bool, cx: &mut Context<Self>) -> bool {
        let now = std::time::Instant::now();
        let expired = self
            .typeahead
            .as_ref()
            .is_none_or(|(_, t)| now.duration_since(*t) > TYPEAHEAD_TIMEOUT);
        let prev = if expired {
            String::new()
        } else {
            self.typeahead
                .as_ref()
                .map(|(b, _)| b.clone())
                .unwrap_or_default()
        };
        let candidate = format!("{prev}{ch}");

        // Snapshot the visible entries (display names lowercased) and
        // the current lead's index — same read pattern the arrow-key
        // navigators use.
        let (names, ids): (Vec<String>, Vec<NodeId>) = {
            let d = self.active_tab().table.read(cx).delegate();
            (
                d.entries
                    .iter()
                    .map(|e| e.display_name.to_lowercase())
                    .collect(),
                d.entries.iter().map(|e| e.id).collect(),
            )
        };
        if ids.is_empty() {
            return false;
        }
        let cur = self
            .active_tab()
            .lead
            .and_then(|id| ids.iter().position(|x| *x == id));

        // 1. Extend the prefix: first entry (from the top) matching the
        //    full accumulated candidate.
        let (matched, buffer) =
            if let Some(i) = names.iter().position(|n| n.starts_with(&candidate)) {
                (Some(i), candidate)
            } else if prev.is_empty() || prev.chars().all(|c| c == ch) {
                // 2. Same single character repeated with no longer match →
                //    cycle to the next entry starting with `ch`, wrapping.
                let single = ch.to_string();
                let start = cur.map_or(0, |i| i + 1);
                let next = (0..ids.len())
                    .map(|k| (start + k) % ids.len())
                    .find(|&k| names[k].starts_with(&single));
                (next, single)
            } else {
                // 3. Multi-character prefix that matches nothing: keep the
                //    old buffer and don't move (but stay grouped in time).
                (None, prev)
            };

        self.typeahead = Some((buffer, now));

        let Some(i) = matched else {
            // Still consumed the key (typeahead is active); just no move.
            return true;
        };
        let id = ids[i];
        let cols = if grid { self.grid_cols(cx).max(1) } else { 1 };
        self.replace_select_one(id, cx);
        self.request_preview_for_row(i, cx);
        if grid {
            // The list view auto-scrolls via `set_selected_row`; the
            // grid's uniform_list has its own scroll handle that
            // selection does not touch, so nudge it here.
            self.active_tab()
                .grid_scroll
                .scroll_to_item(i / cols, gpui::ScrollStrategy::Center);
        }
        cx.notify();
        true
    }
}

/// Idle window after which the type-to-select buffer resets: a keystroke
/// this long after the previous one starts a fresh prefix rather than
/// extending the old one (Finder uses roughly this).
const TYPEAHEAD_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(750);

/// Decide whether a printable key-down should drive type-to-select, and
/// with what character — the precedence rules of [`Shell::on_typeahead_key`]
/// as a pure function so they're unit-testable without a window.
///
/// Order matters:
/// 1. **A focused text input wins outright** (`input_focused`). The
///    filter and the modal name prompts (New Folder / Rename / favorite
///    label) own every printable key; if typeahead consumed one it would
///    `stop_propagation` and the character would never reach the input's
///    IME — on macOS that reads as "typing does nothing" in those fields.
///    This is the guard for that regression.
/// 2. The active list/grid must actually hold focus (`view_focused`,
///    descendant-aware) — the shell root also sees keys bubbling from
///    unrelated descendants.
/// 3. Modifier chords (Cmd/Ctrl/Fn) are commands, not text.
/// 4. Only a single, non-control glyph counts; it's lowercased for
///    case-insensitive prefix matching.
///
/// Returns the match character, or `None` to bow out.
fn typeahead_char(
    input_focused: bool,
    view_focused: bool,
    modifiers: &gpui::Modifiers,
    key_char: Option<&str>,
) -> Option<char> {
    if input_focused || !view_focused {
        return None;
    }
    if modifiers.platform || modifiers.control || modifiers.function {
        return None;
    }
    let s = key_char?;
    let ch = s
        .chars()
        .next()
        .filter(|c| !c.is_control() && s.chars().count() == 1)?;
    Some(ch.to_lowercase().next().unwrap_or(ch))
}

#[cfg(test)]
mod typeahead_tests {
    use super::typeahead_char;
    use gpui::Modifiers;

    fn plain() -> Modifiers {
        Modifiers::default()
    }

    #[test]
    fn typeahead_yields_to_focused_input() {
        // THE REGRESSION GUARD. A focused text field (filter or a modal
        // name prompt like New Folder / Rename) must win over the view,
        // even when the view "contains" that field's focus — otherwise
        // typeahead's stop_propagation eats the character and typing into
        // the field silently does nothing on macOS.
        assert_eq!(
            typeahead_char(true, true, &plain(), Some("h")),
            None,
            "a focused input must suppress typeahead even while the view contains focus"
        );
    }

    #[test]
    fn typeahead_fires_when_view_focused_and_no_input() {
        // The AROS-port fix this regression came from: after clicking a
        // row, the view contains focus and typing must still typeahead.
        assert_eq!(typeahead_char(false, true, &plain(), Some("p")), Some('p'));
    }

    #[test]
    fn typeahead_lowercases_for_case_insensitive_match() {
        assert_eq!(typeahead_char(false, true, &plain(), Some("P")), Some('p'));
    }

    #[test]
    fn typeahead_needs_the_view_focused() {
        assert_eq!(typeahead_char(false, false, &plain(), Some("p")), None);
    }

    #[test]
    fn typeahead_ignores_modifier_chords() {
        let cmd = Modifiers {
            platform: true,
            ..Default::default()
        };
        assert_eq!(typeahead_char(false, true, &cmd, Some("p")), None);
        let ctrl = Modifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(typeahead_char(false, true, &ctrl, Some("p")), None);
        let func = Modifiers {
            function: true,
            ..Default::default()
        };
        assert_eq!(typeahead_char(false, true, &func, Some("\u{f700}")), None);
    }

    #[test]
    fn typeahead_ignores_control_and_multichar() {
        // No printable char (arrows, Enter → None key_char).
        assert_eq!(typeahead_char(false, true, &plain(), None), None);
        // A control scalar is not text.
        assert_eq!(typeahead_char(false, true, &plain(), Some("\u{7f}")), None);
        // Multi-scalar IME strings aren't single-key typeahead.
        assert_eq!(typeahead_char(false, true, &plain(), Some("ab")), None);
    }
}
