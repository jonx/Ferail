//! Panel group + panel elements for the local splitter fork. See
//! [`super`] (mod.rs) for why this exists and how it differs from
//! upstream `gpui-component`'s `resizable`.

use std::{
    ops::{Deref, Range},
    rc::Rc,
};

use gpui::{
    AnyElement, App, AppContext, Axis, Bounds, Context, Element, ElementId, Empty, Entity,
    InteractiveElement as _, IntoElement, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels,
    Render, RenderOnce, Style, StyleRefinement, Styled, Window, div, prelude::FluentBuilder,
};

use gpui_component::{AxisExt, ElementExt as _, StyledExt as _, h_flex, v_flex};

use super::{PANEL_MIN_SIZE, ResizableState, resizable_panel, resize_handle};

pub enum ResizablePanelEvent {
    Resized,
}

#[derive(Clone)]
pub(crate) struct DragPanel;
impl Render for DragPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
        Empty
    }
}

/// Resize callback shared by the group element and its builder.
type ResizeHandler = Rc<dyn Fn(&Entity<ResizableState>, &mut Window, &mut App)>;

/// A group of resizable panels.
#[derive(IntoElement)]
pub struct ResizablePanelGroup {
    id: ElementId,
    state: Option<Entity<ResizableState>>,
    axis: Axis,
    children: Vec<ResizablePanel>,
    on_resize: ResizeHandler,
}

impl ResizablePanelGroup {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            axis: Axis::Horizontal,
            children: vec![],
            state: None,
            on_resize: Rc::new(|_, _, _| {}),
        }
    }

    /// Bind an externally-owned state entity. If not provided, the
    /// group keeps its own state via `use_keyed_state`.
    pub fn with_state(mut self, state: &Entity<ResizableState>) -> Self {
        self.state = Some(state.clone());
        self
    }

    /// Set the axis of the group, default horizontal.
    pub fn axis(mut self, axis: Axis) -> Self {
        self.axis = axis;
        self
    }

    /// Add a panel to the group.
    pub fn child(mut self, panel: impl Into<ResizablePanel>) -> Self {
        self.children.push(panel.into());
        self
    }

    /// Add multiple panels to the group.
    #[allow(dead_code)]
    pub fn children<I>(mut self, panels: impl IntoIterator<Item = I>) -> Self
    where
        I: Into<ResizablePanel>,
    {
        self.children = panels.into_iter().map(|panel| panel.into()).collect();
        self
    }

    /// Callback fired when a drag ends (mouse-up after a handle drag)
    /// or `ResizableState::resize_panel` runs.
    pub fn on_resize(
        mut self,
        on_resize: impl Fn(&Entity<ResizableState>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_resize = Rc::new(on_resize);
        self
    }
}

impl<T> From<T> for ResizablePanel
where
    T: Into<AnyElement>,
{
    fn from(value: T) -> Self {
        resizable_panel().child(value.into())
    }
}

impl From<ResizablePanelGroup> for ResizablePanel {
    fn from(value: ResizablePanelGroup) -> Self {
        resizable_panel().child(value)
    }
}

impl RenderOnce for ResizablePanelGroup {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = self.state.clone().unwrap_or_else(|| {
            window.use_keyed_state(self.id.clone(), cx, |_, _| ResizableState::default())
        });
        let container = if self.axis.is_horizontal() {
            h_flex()
        } else {
            v_flex()
        };

        // Sync panels to the state.
        let panels_count = self.children.len();
        state.update(cx, |state, cx| {
            state.sync_panels_count(self.axis, panels_count, cx);
        });

        container
            .id(self.id)
            .size_full()
            .children(
                self.children
                    .into_iter()
                    .enumerate()
                    .map(|(ix, mut panel)| {
                        panel.panel_ix = ix;
                        panel.axis = self.axis;
                        panel.state = Some(state.clone());
                        panel
                    }),
            )
            .on_prepaint({
                let state = state.clone();
                move |bounds, _, cx| {
                    // Bookkeeping only, no correction pass, no notify.
                    // Fixed panels hold their basis; flex panels absorb
                    // the container delta in this same frame via the
                    // layout engine.
                    state.update(cx, |state, _| state.set_bounds(bounds))
                }
            })
            .child(ResizePanelGroupElement {
                state: state.clone(),
                axis: self.axis,
                on_resize: self.on_resize.clone(),
            })
    }
}

/// A resizable panel inside a [`ResizablePanelGroup`].
///
/// Panels built with `.size(px)` are **fixed**: they keep that width
/// (`flex_none`), the user changes it by dragging the handle, and a
/// window resize never moves it. Panels without a size are **flex**:
/// they absorb whatever the fixed siblings leave over.
///
/// Implements [`Styled`], so call sites can add padding / colors /
/// borders. **Reserved styles** (fight the panel's layout management):
/// `flex_basis`, `absolute`, `overflow_hidden` (would clip the resize
/// handle at `left: -4px`).
#[derive(IntoElement)]
pub struct ResizablePanel {
    axis: Axis,
    panel_ix: usize,
    state: Option<Entity<ResizableState>>,
    /// `Some` makes this a fixed panel with that initial size.
    initial_size: Option<Pixels>,
    /// Size range limit of this panel.
    size_range: Range<Pixels>,
    children: Vec<AnyElement>,
    visible: bool,
    style: StyleRefinement,
}

impl ResizablePanel {
    pub(super) fn new() -> Self {
        Self {
            panel_ix: 0,
            initial_size: None,
            state: None,
            size_range: (PANEL_MIN_SIZE..Pixels::MAX),
            axis: Axis::Horizontal,
            children: vec![],
            visible: true,
            style: StyleRefinement::default(),
        }
    }

    /// Set the visibility of the panel, default is true.
    #[allow(dead_code)]
    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Give the panel a fixed size (see type docs).
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.initial_size = Some(size.into());
        self
    }

    /// Limit the panel's size. Default [`PANEL_MIN_SIZE`]..MAX.
    pub fn size_range(mut self, range: impl Into<Range<Pixels>>) -> Self {
        self.size_range = range.into();
        self
    }
}

impl Styled for ResizablePanel {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for ResizablePanel {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ResizablePanel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        if !self.visible {
            return div().id(("resizable-panel", self.panel_ix));
        }

        let state = self
            .state
            .expect("BUG: The `state` in ResizablePanel should be present.");
        let panel_size = state
            .read(cx)
            .panels
            .get(self.panel_ix)
            .and_then(|p| p.size);
        let size_range = self.size_range.clone();
        let is_fixed = self.initial_size.is_some();
        // A fixed panel's basis: the persisted drag size when there is
        // one, else the initial size: clamped to the *current* range
        // so a squeezed range (collapsed sidebar) pins it without
        // destroying the remembered width.
        let basis = panel_size
            .or(self.initial_size)
            .map(|size| size.clamp(size_range.start, size_range.end));

        div()
            .id(("resizable-panel", self.panel_ix))
            .flex()
            .size_full()
            .relative()
            .refine_style(&self.style)
            .when(self.axis.is_vertical(), |this| {
                this.min_h(size_range.start).max_h(size_range.end)
            })
            .when(self.axis.is_horizontal(), |this| {
                this.min_w(size_range.start).max_w(size_range.end)
            })
            .map(|this| match (is_fixed, basis) {
                (true, Some(basis)) => this.flex_none().flex_basis(basis),
                _ => this.flex_grow_1().flex_shrink_1(),
            })
            .on_prepaint({
                let state = state.clone();
                move |bounds, _, cx| {
                    state.update(cx, |state, _| {
                        state.update_panel_size(self.panel_ix, bounds, self.size_range, is_fixed)
                    })
                }
            })
            .children(self.children)
            .when(self.panel_ix > 0, |this| {
                let ix = self.panel_ix - 1;
                this.child(resize_handle(("resizable-handle", ix), self.axis).on_drag(
                    DragPanel,
                    move |drag_panel, _, _, cx| {
                        cx.stop_propagation();
                        // Set current resizing panel ix
                        state.update(cx, |state, _| {
                            state.resizing_panel_ix = Some(ix);
                        });
                        cx.new(|_| drag_panel.deref().clone())
                    },
                ))
            })
    }
}

struct ResizePanelGroupElement {
    state: Entity<ResizableState>,
    on_resize: ResizeHandler,
    axis: Axis,
}

impl IntoElement for ResizePanelGroupElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ResizePanelGroupElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        (window.request_layout(Style::default(), None, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.on_mouse_event({
            let state = self.state.clone();
            let axis = self.axis;
            let current_ix = state.read(cx).resizing_panel_ix;
            move |e: &MouseMoveEvent, phase, window, cx| {
                if !phase.bubble() {
                    return;
                }
                let Some(ix) = current_ix else { return };

                state.update(cx, |state, cx| {
                    let Some(panel) = state.panels.get(ix) else {
                        return;
                    };
                    match axis {
                        Axis::Horizontal => state.resize_panel_at_handle(
                            ix,
                            e.position.x - panel.bounds.left(),
                            window,
                            cx,
                        ),
                        Axis::Vertical => state.resize_panel_at_handle(
                            ix,
                            e.position.y - panel.bounds.top(),
                            window,
                            cx,
                        ),
                    }
                })
            }
        });

        // When any mouse up, stop dragging.
        window.on_mouse_event({
            let state = self.state.clone();
            let current_ix = state.read(cx).resizing_panel_ix;
            let on_resize = self.on_resize.clone();
            move |_: &MouseUpEvent, phase, window, cx| {
                if current_ix.is_none() {
                    return;
                }
                if phase.bubble() {
                    state.update(cx, |state, cx| state.done_resizing(cx));
                    on_resize(&state, window, cx);
                }
            }
        })
    }
}
