//! Local fork of `gpui-component`'s `resizable` splitter.
//!
//! Forked to fix live window-resize behavior. Upstream keeps every
//! panel `flex_grow_1` with a px `flex_basis` and, on each container
//! prepaint, rescales all panels **proportionally** and `cx.notify()`s.
//! Two consequences:
//!
//! 1. **Resize jiggle.** The frame that carries a new window size still
//!    lays out with last frame's basis values — flex distributes the
//!    width delta evenly across panels — then the proportional rescale
//!    lands one frame later and snaps them elsewhere. During a live
//!    resize, right-anchored content visibly jumps left↔right and
//!    wrapping text reflows every frame (most obvious in the settings
//!    window).
//! 2. **Idle repaint churn.** `update_panel_size` notified on every
//!    panel prepaint, so windows using `use_keyed_state` re-rendered
//!    forever.
//!
//! This fork gives panels two explicit roles instead:
//!
//! - **Fixed** panels (built with `.size(px)`) keep their width:
//!   `flex_none` + `flex_basis(size)`. A window resize never moves
//!   them (Finder-style sidebars).
//! - **Flex** panels (no `.size(...)`) absorb the container delta via
//!   `flex_grow`/`flex_shrink` — in the *same* frame, handled by the
//!   layout engine, with no correction pass and no notify loop.
//!
//! Drag-resize still works: a handle drag resizes the nearest fixed
//! panel (translating drags on a flex panel's trailing handle into a
//! resize of the fixed sibling to its right), clamped so every other
//! panel keeps its minimum. State only notifies on real size changes.

use std::ops::Range;

use gpui::{
    Along, Axis, Bounds, Context, ElementId, EventEmitter, IsZero as _, Pixels, Window, px,
};

mod panel;
mod resize_handle;
pub use panel::*;
pub(crate) use resize_handle::*;

pub(crate) const PANEL_MIN_SIZE: Pixels = px(100.);

/// Create a [`ResizablePanelGroup`] with horizontal resizing.
pub fn h_resizable(id: impl Into<ElementId>) -> ResizablePanelGroup {
    ResizablePanelGroup::new(id).axis(Axis::Horizontal)
}

/// Create a [`ResizablePanelGroup`] with vertical resizing.
#[allow(dead_code)]
pub fn v_resizable(id: impl Into<ElementId>) -> ResizablePanelGroup {
    ResizablePanelGroup::new(id).axis(Axis::Vertical)
}

/// Create a [`ResizablePanel`].
pub fn resizable_panel() -> ResizablePanel {
    ResizablePanel::new()
}

/// State for a [`ResizablePanelGroup`].
#[derive(Debug, Clone)]
pub struct ResizableState {
    /// The `axis` will sync to the actual axis of the group in use.
    axis: Axis,
    panels: Vec<ResizablePanelState>,
    sizes: Vec<Pixels>,
    pub(crate) resizing_panel_ix: Option<usize>,
    bounds: Bounds<Pixels>,
}

impl Default for ResizableState {
    fn default() -> Self {
        Self {
            axis: Axis::Horizontal,
            panels: vec![],
            sizes: vec![],
            resizing_panel_ix: None,
            bounds: Bounds::default(),
        }
    }
}

impl ResizableState {
    /// The last laid-out size of each panel, in panel order.
    pub fn sizes(&self) -> &Vec<Pixels> {
        &self.sizes
    }

    /// Programmatically resize the panel at `ix` to `size`, with the
    /// same clamping as a drag. Emits `ResizablePanelEvent::Resized`
    /// so subscribers (e.g. preference persistence) see the change as
    /// if the user had dragged a handle. Out-of-range indices no-op.
    pub fn resize_panel(
        &mut self,
        ix: usize,
        size: Pixels,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ix >= self.sizes.len() {
            return;
        }
        self.sync_real_panel_sizes();
        self.resize_panel_clamped(ix, size, cx);
        self.done_resizing(cx);
    }

    pub(crate) fn sync_panels_count(
        &mut self,
        axis: Axis,
        panels_count: usize,
        cx: &mut Context<Self>,
    ) {
        let mut changed = self.axis != axis;
        self.axis = axis;

        if panels_count > self.panels.len() {
            let diff = panels_count - self.panels.len();
            self.panels
                .extend(vec![ResizablePanelState::default(); diff]);
            self.sizes.extend(vec![PANEL_MIN_SIZE; diff]);
            changed = true;
        }

        if panels_count < self.panels.len() {
            self.panels.truncate(panels_count);
            self.sizes.truncate(panels_count);
            changed = true;
        }

        if changed {
            cx.notify();
        }
    }

    /// Record a panel's laid-out bounds and constraints. Pure
    /// bookkeeping — deliberately no `cx.notify()`: this runs on every
    /// prepaint, and notifying from here is what kept upstream's
    /// windows repainting forever.
    pub(crate) fn update_panel_size(
        &mut self,
        panel_ix: usize,
        bounds: Bounds<Pixels>,
        size_range: Range<Pixels>,
        fixed: bool,
    ) {
        let Some(panel) = self.panels.get_mut(panel_ix) else {
            return;
        };
        let size = bounds.size.along(self.axis);
        panel.bounds = bounds;
        panel.size_range = size_range;
        panel.fixed = fixed;
        if fixed && panel.size.is_none() && !size.is_zero() {
            // First layout of a fixed panel: adopt the laid-out size
            // (its initial `.size(...)`) as the persistent basis.
            panel.size = Some(size);
        }
        if !size.is_zero() {
            self.sizes[panel_ix] = size;
        }
    }

    pub(crate) fn set_bounds(&mut self, bounds: Bounds<Pixels>) {
        // No correction pass on container resize: fixed panels hold
        // their basis and flex panels absorb the delta natively in the
        // same frame. (Upstream rescaled everything proportionally
        // here, one frame late — the source of the resize jiggle.)
        self.bounds = bounds;
    }

    #[allow(dead_code)]
    pub(crate) fn clear(&mut self) {
        self.panels.clear();
        self.sizes.clear();
    }

    #[inline]
    pub(crate) fn container_size(&self) -> Pixels {
        self.bounds.size.along(self.axis)
    }

    pub(crate) fn done_resizing(&mut self, cx: &mut Context<Self>) {
        self.resizing_panel_ix = None;
        cx.emit(ResizablePanelEvent::Resized);
    }

    fn panel_size_range(&self, ix: usize) -> Range<Pixels> {
        let Some(panel) = self.panels.get(ix) else {
            return PANEL_MIN_SIZE..Pixels::MAX;
        };
        panel.size_range.clone()
    }

    fn sync_real_panel_sizes(&mut self) {
        for (i, panel) in self.panels.iter().enumerate() {
            let size = panel.bounds.size.along(self.axis);
            if !size.is_zero() {
                self.sizes[i] = size;
            }
        }
    }

    /// Drag worker: the handle at `ix` sits between panel `ix` and
    /// panel `ix + 1`; `size` is the requested width of panel `ix`.
    pub(crate) fn resize_panel_at_handle(
        &mut self,
        ix: usize,
        size: Pixels,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ix + 1 >= self.panels.len() {
            return;
        }
        self.sync_real_panel_sizes();
        if self.panels[ix].fixed {
            self.resize_panel_clamped(ix, size, cx);
            return;
        }
        // Dragging the trailing edge of a flex panel: the flex panel
        // has no basis to set — translate into resizing the nearest
        // fixed panel to the right (e.g. the handle between the center
        // pane and the preview moves the preview's left edge).
        let Some(right_ix) = (ix + 1..self.panels.len()).find(|&i| self.panels[i].fixed) else {
            return;
        };
        let delta = size - self.sizes[ix];
        let requested = self.sizes[right_ix] - delta;
        self.resize_panel_clamped(right_ix, requested, cx);
    }

    /// Set a fixed panel's basis, clamped to its own range and to the
    /// container minus every other panel's minimum footprint (fixed
    /// siblings keep their current width; flex siblings are entitled
    /// to at least their range minimum).
    fn resize_panel_clamped(&mut self, ix: usize, requested: Pixels, cx: &mut Context<Self>) {
        let range = self.panel_size_range(ix);
        let container = self.container_size();
        let mut budget = container;
        for (j, panel) in self.panels.iter().enumerate() {
            if j == ix {
                continue;
            }
            budget -= if panel.fixed {
                self.sizes[j].max(panel.size_range.start)
            } else {
                panel.size_range.start
            };
        }
        let new_size = requested
            .clamp(range.start, range.end)
            .min(budget.max(range.start));
        if (new_size.as_f32() - self.sizes[ix].as_f32()).abs() < 0.5 {
            return;
        }
        self.sizes[ix] = new_size;
        self.panels[ix].size = Some(new_size);
        cx.notify();
    }
}

impl EventEmitter<ResizablePanelEvent> for ResizableState {}

#[derive(Debug, Clone)]
pub(crate) struct ResizablePanelState {
    pub size: Option<Pixels>,
    pub size_range: Range<Pixels>,
    bounds: Bounds<Pixels>,
    /// True when the panel was built with an explicit `.size(...)`:
    /// it holds its width (`flex_none`) and drags/`resize_panel` move
    /// it. Flex panels absorb the leftover container space.
    pub fixed: bool,
}

impl Default for ResizablePanelState {
    fn default() -> Self {
        Self {
            size: None,
            size_range: PANEL_MIN_SIZE..Pixels::MAX,
            bounds: Bounds::default(),
            fixed: false,
        }
    }
}
