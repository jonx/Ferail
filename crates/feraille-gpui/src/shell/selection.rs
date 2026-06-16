use super::*;

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
                        state.set_selected_row(row, cx);
                    });
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
}
