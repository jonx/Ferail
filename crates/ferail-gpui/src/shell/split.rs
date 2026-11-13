//! Split view: two of the window's tabs shown side by side or stacked
//! (docs/features/SPLIT_VIEW.md).
//!
//! A pane is not a new kind of surface. It is a slot that shows one of the
//! tabs the window already owns, so each pane keeps its own table,
//! enumeration, selection, history, view mode and scroll position for free.
//! `Shell::active` stays the single notion of "the tab commands act on": it
//! is the tab of the focused pane. Pointing at the other pane (press, drag
//! hover, drop) focuses it in the capture phase, before any row handler
//! runs, which is why every handler that resolves against the active tab
//! works unchanged in either pane.

use gpui::{
    Axis, Bounds, Context, InteractiveElement as _, IntoElement as _, MouseButton, MouseMoveEvent,
    ParentElement as _, Pixels, StatefulInteractiveElement as _, Styled as _, Window, canvas, div,
    px, relative,
};
use gpui_component::ActiveTheme as _;

use super::tab::TabId;
use super::{
    CopyToOtherPane, MoveToOtherPane, Shell, SplitDown, SplitRight, SwitchPane, TransferMode,
};

/// Smallest share of the split area either pane keeps while the divider is
/// dragged.
const MIN_RATIO: f32 = 0.2;
/// Opacity of the pane that does not have the focus.
const UNFOCUSED_PANE_OPACITY: f32 = 0.62;
/// Hit width of the divider between the panes; the visible rule is 1 px.
const DIVIDER_HIT: f32 = 7.0;

/// The window's split state: which two tabs are shown and how.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneSplit {
    /// The tab in each slot: `[left, right]` or `[top, bottom]`.
    pub panes: [TabId; 2],
    /// `Horizontal` places the panes side by side, `Vertical` stacks them.
    pub axis: Axis,
    /// Share of the split area given to the first pane, `0..1`.
    pub ratio: f32,
    /// Slot that last held the focus. It is the slot a tab chosen from the
    /// tab strip lands in.
    pub focus: usize,
}

/// Outcome of matching a split against the window's tabs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Reconciled {
    Keep(PaneSplit),
    /// The split cannot stand. `activate` names the surviving pane's tab,
    /// when one survives, so closing a pane leaves the user on the other.
    Collapse {
        activate: Option<TabId>,
    },
}

impl PaneSplit {
    pub(crate) fn new(first: TabId, second: TabId, axis: Axis, focus: usize) -> Self {
        Self {
            panes: [first, second],
            axis,
            ratio: 0.5,
            focus: focus.min(1),
        }
    }

    /// Slot showing `tab`, if any.
    pub(crate) fn slot_of(&self, tab: TabId) -> Option<usize> {
        self.panes.iter().position(|id| *id == tab)
    }

    /// Bring the split in line with the tab list and the active tab.
    ///
    /// - A pane whose tab is gone collapses the split onto the survivor.
    /// - An active tab shown in a pane moves the focus to that pane.
    /// - An active tab shown in neither pane (picked in the tab strip,
    ///   opened with New Tab) replaces the focused pane's tab.
    pub(crate) fn reconcile(mut self, tabs: &[TabId], active: TabId) -> Reconciled {
        let alive = |id: &TabId| tabs.contains(id);
        if !alive(&self.panes[0]) || !alive(&self.panes[1]) {
            let activate = self.panes.iter().copied().find(alive);
            return Reconciled::Collapse { activate };
        }
        match self.slot_of(active) {
            Some(slot) => self.focus = slot,
            None => self.panes[self.focus.min(1)] = active,
        }
        if self.panes[0] == self.panes[1] {
            return Reconciled::Collapse {
                activate: Some(active),
            };
        }
        Reconciled::Keep(self)
    }
}

/// Divider position as a share of the split area, from a pointer position.
pub(crate) fn ratio_at(pointer: Pixels, origin: Pixels, extent: Pixels) -> Option<f32> {
    let extent = f32::from(extent);
    if extent <= 1.0 {
        return None;
    }
    let ratio = (f32::from(pointer) - f32::from(origin)) / extent;
    Some(ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO))
}

impl Shell {
    /// Reconcile `split` with the tabs. Cheap; runs at the top of every
    /// render and before any command that reads the split, so the many
    /// places that change `tabs` or `active` need no split bookkeeping.
    pub(crate) fn sync_split(&mut self) {
        let Some(split) = self.split.take() else {
            return;
        };
        let ids: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        match split.reconcile(&ids, self.tabs[self.active].id) {
            Reconciled::Keep(split) => self.split = Some(split),
            Reconciled::Collapse { activate } => {
                self.split_dragging = false;
                if let Some(idx) = activate.and_then(|id| self.tabs.iter().position(|t| t.id == id))
                {
                    self.active = idx;
                }
            }
        }
    }

    /// Index of the tab shown in the pane that does not have the focus.
    pub(crate) fn other_pane_index(&self) -> Option<usize> {
        let split = self.split.as_ref()?;
        let active = self.tabs[self.active].id;
        let other = split.panes[1 - split.slot_of(active)?];
        self.tabs.iter().position(|t| t.id == other)
    }

    /// Whether the tab at `idx` is on screen: the active tab, or the other
    /// pane's tab while the window is split.
    pub(crate) fn tab_is_shown(&self, idx: usize) -> bool {
        idx == self.active || self.other_pane_index() == Some(idx)
    }

    /// Give the focus to the pane showing `tab`. No-op when it already has
    /// it or when the tab is gone.
    pub(crate) fn focus_pane_tab(&mut self, tab: TabId, cx: &mut Context<Self>) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab) else {
            return;
        };
        if idx == self.active {
            return;
        }
        // Row-indexed state belongs to the pane that was active.
        self.spring_load = None;
        self.context_row = None;
        self.select_tab(idx, cx);
        // Mirroring the lead onto the table stops propagation, as a row
        // click would. Here the pointer event is only passing through on
        // its way to the row, so let it continue.
        cx.propagate();
    }

    /// Put keyboard focus where the active tab's keys are handled.
    fn focus_active_surface(&self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab();
        if matches!(tab.view_mode, crate::grid::ViewMode::Grid) && tab.tool_result.is_none() {
            window.focus(&tab.grid_focus, cx);
        } else {
            self.focus_handle.focus(window, cx);
        }
    }

    /// Split along `axis`, turn an existing split to `axis`, or close a
    /// split that already runs along it.
    fn toggle_split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_split();
        match self.split.as_mut() {
            Some(split) if split.axis == axis => self.split = None,
            Some(split) => split.axis = axis,
            None => {
                let first = self.active;
                // Reuse the neighbouring tab when there is one, so a split
                // lines up two tabs the user already opened. A lone tab is
                // duplicated instead.
                let second = if first + 1 < self.tabs.len() {
                    first + 1
                } else if first > 0 {
                    first - 1
                } else {
                    let path = self.active_tab().current_dir.clone();
                    let id = self.process.fs.id_for_path(&path);
                    self.process
                        .node_store
                        .borrow_mut()
                        .get_or_create_path_with_id(path.clone(), id);
                    let tab = self.make_tab(path.clone(), id, window, cx);
                    let tab_id = tab.id;
                    self.tabs.insert(first + 1, tab);
                    self.load_path_for_tab(tab_id, path, cx);
                    first + 1
                };
                let (a, b) = (self.tabs[first].id, self.tabs[second].id);
                // Keep strip order: the tab further left takes the first
                // slot. The focus stays on the tab the user was in.
                self.split = Some(if first < second {
                    PaneSplit::new(a, b, axis, 0)
                } else {
                    PaneSplit::new(b, a, axis, 1)
                });
                self.refresh_file_list_selection_in_tab(second, cx);
                self.warm_loaded_viewport_in_tab(second, cx);
            }
        }
        self.split_dragging = false;
        cx.notify();
    }

    pub(super) fn on_split_right(
        &mut self,
        _: &SplitRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_split(Axis::Horizontal, window, cx);
    }

    pub(super) fn on_split_down(
        &mut self,
        _: &SplitDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_split(Axis::Vertical, window, cx);
    }

    /// The toolbar toggle: close an open split whatever its axis, otherwise
    /// split side by side, or stacked with Shift held.
    pub(super) fn on_split_button(
        &mut self,
        stacked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_split();
        if self.split.is_some() {
            self.split = None;
            self.split_dragging = false;
            cx.notify();
        } else {
            let axis = if stacked {
                Axis::Vertical
            } else {
                Axis::Horizontal
            };
            self.toggle_split(axis, window, cx);
        }
        self.focus_active_surface(window, cx);
    }

    pub(super) fn on_switch_pane(
        &mut self,
        _: &SwitchPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_split();
        let Some(idx) = self.other_pane_index() else {
            return;
        };
        let tab = self.tabs[idx].id;
        self.focus_pane_tab(tab, cx);
        self.focus_active_surface(window, cx);
    }

    pub(super) fn on_copy_to_other_pane(
        &mut self,
        _: &CopyToOtherPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transfer_to_other_pane(TransferMode::Copy, window, cx);
    }

    pub(super) fn on_move_to_other_pane(
        &mut self,
        _: &MoveToOtherPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transfer_to_other_pane(TransferMode::Move, window, cx);
    }

    /// Copy or move the focused pane's selection into the folder the other
    /// pane shows. The transfer itself is the ordinary one: planned and run
    /// off-thread, with collisions, progress, cancellation and undo.
    fn transfer_to_other_pane(
        &mut self,
        mode: TransferMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::overlay::OverlayWindowExt as _;
        use gpui_component::notification::Notification;

        self.sync_split();
        let Some(other) = self.other_pane_index() else {
            window.push_notice(
                Notification::info(tr!(
                    "Split the window first: there is no other pane to send items to."
                )),
                cx,
            );
            return;
        };
        if self.reject_platform_filesystem_command(window, cx) {
            return;
        }
        // Only a plain folder listing names a destination. Search results,
        // an open archive or a provider-owned location do not.
        let destination = &self.tabs[other];
        if destination.tool_result.is_some() || destination.platform_namespace.is_some() {
            window.push_notice(
                Notification::info(tr!("The other pane is not showing a folder.")),
                cx,
            );
            return;
        }
        let dest = destination.current_dir.clone();
        let sources: Vec<std::path::PathBuf> = self
            .action_entries_visible_order(cx)
            .into_iter()
            .map(|(_, _, path)| path)
            .collect();
        if sources.is_empty() {
            return;
        }
        // Both panes on the same folder: a move would be a no-op and a copy
        // is what Duplicate is for.
        if sources.iter().all(|s| s.parent() == Some(dest.as_path())) {
            return;
        }
        self.spawn_transfer_op(sources, dest, mode, window, cx);
    }

    /// Follow the pointer while the divider is held.
    pub(super) fn on_split_drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.split_dragging {
            return;
        }
        if !event.dragging() {
            self.end_split_drag(cx);
            return;
        }
        let bounds = self.split_bounds.get();
        let Some(split) = self.split.as_mut() else {
            self.split_dragging = false;
            return;
        };
        let ratio = match split.axis {
            Axis::Horizontal => ratio_at(event.position.x, bounds.origin.x, bounds.size.width),
            Axis::Vertical => ratio_at(event.position.y, bounds.origin.y, bounds.size.height),
        };
        if let Some(ratio) = ratio {
            if (ratio - split.ratio).abs() > 0.001 {
                split.ratio = ratio;
                cx.notify();
            }
        }
    }

    pub(super) fn end_split_drag(&mut self, cx: &mut Context<Self>) {
        if self.split_dragging {
            self.split_dragging = false;
            cx.notify();
        }
    }

    /// The file area below the tab strip: one pane, or two with a divider.
    pub(super) fn split_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        self.sync_split();
        self.sync_compare(cx);
        let Some(split) = self.split.clone() else {
            return self.pane_column(cx);
        };
        let focused = self.active;
        let horizontal = split.axis == Axis::Horizontal;
        let mut columns = Vec::with_capacity(2);
        for id in split.panes {
            let Some(idx) = self.tabs.iter().position(|t| t.id == id) else {
                continue;
            };
            // Build this pane's elements against its own tab. Everything
            // deferred past this point (list closures, listeners) looks its
            // tab up by `TabId` or runs after the pane took the focus.
            self.active = idx;
            let column = self.pane_column(cx);
            columns.push((id, idx == focused, column));
        }
        self.active = focused;
        if columns.len() != 2 {
            return self.pane_column(cx);
        }

        let bounds_cell = self.split_bounds.clone();
        let measure = canvas(
            move |bounds: Bounds<Pixels>, _, _| bounds_cell.set(bounds),
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let border = cx.theme().border;
        let accent = cx.theme().primary;
        let dragging = self.split_dragging;
        let divider = div()
            .id("split-divider")
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .map(|d| {
                if horizontal {
                    d.w(px(DIVIDER_HIT)).h_full().cursor_col_resize()
                } else {
                    d.h(px(DIVIDER_HIT)).w_full().cursor_row_resize()
                }
            })
            .child(div().bg(if dragging { accent } else { border }).map(|d| {
                if horizontal {
                    d.w(px(1.0)).h_full()
                } else {
                    d.h(px(1.0)).w_full()
                }
            }))
            .hover(|style| style.bg(accent.opacity(0.25)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.split_dragging = true;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            // Double-click evens the panes out.
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                if event.click_count() >= 2 {
                    if let Some(split) = this.split.as_mut() {
                        split.ratio = 0.5;
                        cx.notify();
                    }
                }
            }));

        let mut panes = columns
            .into_iter()
            .enumerate()
            .map(|(slot, (id, is_focused, column))| {
                let pane = div()
                    .id(("split-pane", slot))
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    // Pointing at a pane focuses it before the press, the drag
                    // hover or the drop reaches a row, so the row handler
                    // already resolves against this pane's tab.
                    .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                        this.focus_pane_tab(id, cx);
                    }))
                    .capture_any_mouse_up(cx.listener(move |this, _, _, cx| {
                        if cx.has_active_drag() || crate::file_list::native_archive_drag_active() {
                            this.focus_pane_tab(id, cx);
                        }
                    }))
                    .on_drag_move(cx.listener(
                        move |this, event: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                            if event.bounds.contains(&event.event.position) {
                                this.focus_pane_tab(id, cx);
                            }
                        },
                    ))
                    .on_drag_move(cx.listener(
                        move |this,
                              event: &gpui::DragMoveEvent<crate::file_list::ArchiveEntryDrag>,
                              _,
                              cx| {
                            if event.bounds.contains(&event.event.position) {
                                this.focus_pane_tab(id, cx);
                            }
                        },
                    ))
                    .when(!is_focused, |d| d.opacity(UNFOCUSED_PANE_OPACITY))
                    .child(column);
                if slot == 0 {
                    // The first pane takes its share; the second absorbs the
                    // rest, so a window resize keeps the proportion.
                    if horizontal {
                        pane.flex_none().w(relative(split.ratio)).h_full()
                    } else {
                        pane.flex_none().h(relative(split.ratio)).w_full()
                    }
                } else {
                    pane.flex_1()
                }
            });
        let first = panes.next();
        let second = panes.next();

        let panes = div()
            .relative()
            .flex()
            .map(|d| {
                if horizontal {
                    d.flex_row()
                } else {
                    d.flex_col()
                }
            })
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .child(measure)
            .children(first)
            .child(divider)
            .children(second);
        panes.into_any_element()
    }
}

use gpui::prelude::FluentBuilder as _;

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(raw: &[u64]) -> Vec<TabId> {
        raw.iter().map(|n| TabId(*n)).collect()
    }

    fn split(a: u64, b: u64, focus: usize) -> PaneSplit {
        PaneSplit::new(TabId(a), TabId(b), Axis::Horizontal, focus)
    }

    #[test]
    fn active_tab_in_a_pane_takes_the_focus() {
        let Reconciled::Keep(s) = split(1, 2, 0).reconcile(&ids(&[1, 2, 3]), TabId(2)) else {
            panic!("split should stand");
        };
        assert_eq!(s.focus, 1);
        assert_eq!(s.panes, [TabId(1), TabId(2)]);
    }

    #[test]
    fn tab_picked_in_the_strip_replaces_the_focused_pane() {
        let Reconciled::Keep(s) = split(1, 2, 1).reconcile(&ids(&[1, 2, 3]), TabId(3)) else {
            panic!("split should stand");
        };
        assert_eq!(s.panes, [TabId(1), TabId(3)]);
        assert_eq!(s.focus, 1);
    }

    #[test]
    fn closing_a_pane_collapses_onto_the_other() {
        // Tab 2 was closed and the strip moved the active tab to 3.
        assert_eq!(
            split(1, 2, 1).reconcile(&ids(&[1, 3]), TabId(3)),
            Reconciled::Collapse {
                activate: Some(TabId(1))
            }
        );
    }

    #[test]
    fn closing_both_panes_collapses_without_a_target() {
        assert_eq!(
            split(1, 2, 0).reconcile(&ids(&[3]), TabId(3)),
            Reconciled::Collapse { activate: None }
        );
    }

    #[test]
    fn other_slot_is_the_one_not_holding_the_tab() {
        let s = split(4, 9, 0);
        assert_eq!(s.slot_of(TabId(9)), Some(1));
        assert_eq!(s.slot_of(TabId(5)), None);
    }

    #[test]
    fn divider_ratio_is_clamped_and_needs_a_real_extent() {
        assert_eq!(ratio_at(px(500.0), px(100.0), px(800.0)), Some(0.5));
        assert_eq!(ratio_at(px(0.0), px(100.0), px(800.0)), Some(MIN_RATIO));
        assert_eq!(
            ratio_at(px(5000.0), px(100.0), px(800.0)),
            Some(1.0 - MIN_RATIO)
        );
        assert_eq!(ratio_at(px(10.0), px(0.0), px(0.0)), None);
    }
}
