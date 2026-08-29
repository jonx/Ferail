//! Context-menu wrapper for Ferail's Windows extended-menu exception.
//!
//! Forked from `gpui_component::menu::ContextMenu` (see
//! [docs/GPUI-UPSTREAM.md](../../../../docs/GPUI-UPSTREAM.md)) with one narrow
//! divergence: Shift+right-click on Windows must not also open Ferail's menu,
//! because that gesture is reserved for the native extended Shell menu.
//!
//! Everything else — the deferred first build, dismiss handling, positioning,
//! and snapping — stays deliberately close to upstream. Dynamic content is
//! not handled here: async submenus retain their identity and update through
//! `PopupMenu::rebuild`, so the root menu never needs polling or replacement.

use std::{cell::RefCell, rc::Rc};

use gpui::{
    Anchor, AnyElement, App, Context, DismissEvent, Element, ElementId, Entity, Focusable,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, StyleRefinement, Styled,
    Subscription, Window, anchored, deferred, div, prelude::FluentBuilder, px,
};
use gpui_component::menu::PopupMenu;

type MenuBuilder = Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

/// Attach a context menu that respects the native Windows extended-menu
/// gesture. Async children should update themselves with `PopupMenu::rebuild`.
pub trait PlatformContextMenuExt: InteractiveElement + ParentElement + Styled {
    fn platform_context_menu<F>(mut self, builder: F) -> PlatformContextMenu<Self>
    where
        Self: Sized,
        F: Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    {
        // Same identity scheme as upstream: prefer the element's own id so
        // each menu keeps its own element state.
        let id = self
            .interactivity()
            .element_id
            .clone()
            .map(|id| format!("platform-context-menu-{id:?}"))
            .unwrap_or_else(|| format!("platform-context-menu-{:p}", &self as *const _));
        PlatformContextMenu {
            id: id.into(),
            element: Some(self),
            builder: Some(Rc::new(builder)),
            anchor: Anchor::TopLeft,
            _ignore_style: StyleRefinement::default(),
        }
    }
}

impl<E: InteractiveElement + ParentElement + Styled> PlatformContextMenuExt for E {}

pub struct PlatformContextMenu<E: ParentElement + Styled + Sized> {
    id: ElementId,
    element: Option<E>,
    builder: Option<MenuBuilder>,
    anchor: Anchor,
    // Not used; present so style refinements forward to the wrapped element.
    _ignore_style: StyleRefinement,
}

impl<E: ParentElement + Styled> PlatformContextMenu<E> {
    fn with_element_state<R>(
        &mut self,
        id: &GlobalElementId,
        window: &mut Window,
        cx: &mut App,
        f: impl FnOnce(&mut Self, &mut PlatformContextMenuState, &mut Window, &mut App) -> R,
    ) -> R {
        window.with_optional_element_state::<PlatformContextMenuState, _>(
            Some(id),
            |element_state, window| {
                let mut element_state = element_state.unwrap().unwrap_or_default();
                let result = f(self, &mut element_state, window, cx);
                (result, Some(element_state))
            },
        )
    }
}

impl<E: ParentElement + Styled> ParentElement for PlatformContextMenu<E> {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        if let Some(element) = &mut self.element {
            element.extend(elements);
        }
    }
}

impl<E: ParentElement + Styled> Styled for PlatformContextMenu<E> {
    fn style(&mut self) -> &mut StyleRefinement {
        if let Some(element) = &mut self.element {
            element.style()
        } else {
            &mut self._ignore_style
        }
    }
}

impl<E: ParentElement + Styled + IntoElement + 'static> IntoElement for PlatformContextMenu<E> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct SharedState {
    menu_view: Option<Entity<PopupMenu>>,
    open: bool,
    position: Point<Pixels>,
    _subscription: Option<Subscription>,
}

pub struct PlatformContextMenuState {
    element: Option<AnyElement>,
    shared_state: Rc<RefCell<SharedState>>,
}

impl Default for PlatformContextMenuState {
    fn default() -> Self {
        Self {
            element: None,
            shared_state: Rc::new(RefCell::new(SharedState {
                menu_view: None,
                open: false,
                position: Default::default(),
                _subscription: None,
            })),
        }
    }
}

/// Build the popup on the next effect cycle and install it.
///
/// Deferred for the same reason upstream defers the first build: the click
/// that opens the menu is still being dispatched, so the entities the builder
/// wants to read are on the stack.
fn schedule_build(
    shared_state: Rc<RefCell<SharedState>>,
    builder: Option<MenuBuilder>,
    window: &mut Window,
    cx: &mut App,
) {
    window.defer(cx, move |window, cx| {
        let menu = PopupMenu::build(window, cx, move |menu, window, cx| {
            let Some(build) = &builder else {
                return menu;
            };
            build(menu, window, cx)
        });

        // Weak capture, deliberately: the App holds this closure for as long
        // as the PopupMenu entity lives, and `SharedState.menu_view` holds
        // that entity — a strong `shared_state` here is therefore a cycle
        // (App → closure → SharedState → Entity → App) that leaked the menu
        // past app quit (GPUI's "Exited with leaked handles" assertion, seen
        // on Windows 0.6.5). Dropping the menu handle on dismiss also
        // releases the last menu's contents right away instead of retaining
        // them until the next right-click.
        let subscription = window.subscribe(&menu, cx, {
            let shared_state = Rc::downgrade(&shared_state);
            move |_, _: &DismissEvent, window, _cx| {
                if let Some(shared_state) = shared_state.upgrade() {
                    let mut state = shared_state.borrow_mut();
                    state.open = false;
                    state.menu_view = None;
                }
                window.refresh();
            }
        });

        {
            let mut state = shared_state.borrow_mut();
            // A dismiss (or a fresh right-click elsewhere) may have landed
            // while this build was in flight — don't resurrect the menu.
            if !state.open {
                return;
            }
            state.menu_view = Some(menu);
            state._subscription = Some(subscription);
        }
        window.refresh();
    });
}

impl<E: ParentElement + Styled + IntoElement + 'static> Element for PlatformContextMenu<E> {
    type RequestLayoutState = PlatformContextMenuState;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let anchor = self.anchor;

        self.with_element_state(
            id.unwrap(),
            window,
            cx,
            |this, state: &mut PlatformContextMenuState, window, cx| {
                let (position, open) = {
                    let shared_state = state.shared_state.borrow();
                    (shared_state.position, shared_state.open)
                };
                let menu_view = state.shared_state.borrow().menu_view.clone();
                let mut menu_element = None;
                if open {
                    let has_menu_item = menu_view
                        .as_ref()
                        .map(|menu| !menu.read(cx).is_empty())
                        .unwrap_or(false);

                    if has_menu_item {
                        menu_element = Some(
                            deferred(
                                anchored().child(
                                    div()
                                        .w(window.bounds().size.width)
                                        .h(window.bounds().size.height)
                                        .on_scroll_wheel(|_, _, cx| {
                                            cx.stop_propagation();
                                        })
                                        .child(
                                            anchored()
                                                .position(position)
                                                .snap_to_window_with_margin(px(8.))
                                                .anchor(anchor)
                                                .when_some(menu_view, |this, menu| {
                                                    // Focus the menu so it can handle actions.
                                                    if !menu
                                                        .focus_handle(cx)
                                                        .contains_focused(window, cx)
                                                    {
                                                        menu.focus_handle(cx).focus(window, cx);
                                                    }

                                                    this.child(menu.clone())
                                                }),
                                        ),
                                ),
                            )
                            .with_priority(1)
                            .into_any(),
                        );
                    }
                }

                let mut element = this
                    .element
                    .take()
                    .expect("Element should exists.")
                    .children(menu_element)
                    .into_any_element();

                let layout_id = element.request_layout(window, cx);

                (
                    layout_id,
                    PlatformContextMenuState {
                        element: Some(element),
                        shared_state: state.shared_state.clone(),
                    },
                )
            },
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: gpui::Bounds<gpui::Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if let Some(element) = &mut request_layout.element {
            element.prepaint(window, cx);
        }
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(element) = &mut request_layout.element {
            element.paint(window, cx);
        }

        // Take the builder before setting up element state to avoid borrow issues
        let builder = self.builder.clone();
        self.with_element_state(
            id.unwrap(),
            window,
            cx,
            |_view, state: &mut PlatformContextMenuState, window, _| {
                let shared_state = state.shared_state.clone();

                let hitbox = hitbox.clone();
                // On right mouse click, build the menu and show it at the
                // mouse position.
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase.bubble()
                        && event.button == MouseButton::Right
                        && hitbox.is_hovered(window)
                        // On Windows, Shift+right-click is reserved for the
                        // native extended Shell menu. Row/cell handlers launch
                        // that menu, but GPUI window listeners are independent
                        // of element propagation, so stop_propagation alone
                        // cannot prevent this listener from opening Ferail's
                        // menu as well.
                        && !(cfg!(windows) && event.modifiers.shift)
                    {
                        {
                            let mut shared_state = shared_state.borrow_mut();
                            // Clear any existing menu view to allow immediate replacement
                            // Set the new position and open the menu
                            shared_state.menu_view = None;
                            shared_state._subscription = None;
                            shared_state.position = event.position;
                            shared_state.open = true;
                        }

                        schedule_build(shared_state.clone(), builder.clone(), window, cx);
                    }
                });
            },
        );
    }
}
