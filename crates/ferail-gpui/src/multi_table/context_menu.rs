//! Context-menu element that **rebuilds itself when its content changes**.
//!
//! Forked from `gpui_component::menu::ContextMenu` (see
//! [docs/GPUI-UPSTREAM.md](../../../../docs/GPUI-UPSTREAM.md)) with one
//! divergence: the caller supplies a `revision` closure alongside the menu
//! builder, and whenever that revision changes while the menu is open, the
//! menu is rebuilt in place — same position, still open.
//!
//! Why we need it: a file manager's menu contents depend on data that is
//! illegal to fetch on the UI thread (LaunchServices "Open With" candidates,
//! for one — see the Prime Directive). Upstream builds the menu exactly once,
//! on mouse-down, so a cache miss there is *permanent for that open*: the
//! menu shows a placeholder and never fills in, and the user has to close and
//! reopen it. That makes a warm cache load-bearing for correctness, when it
//! should only ever be load-bearing for speed. With a revision hook, the
//! off-thread fetch reports back through the normal entity/notify boundary,
//! the revision ticks, and the open menu fills in — a miss costs latency, not
//! content.
//!
//! Everything else — the deferred first build, dismiss handling, positioning,
//! snapping — is upstream's, kept deliberately close so the fork stays cheap
//! to re-sync.

use std::{cell::RefCell, rc::Rc};

use gpui::{
    Anchor, AnyElement, App, Context, DismissEvent, Element, ElementId, Entity, Focusable,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, StyleRefinement, Styled,
    Subscription, Window, anchored, deferred, div, prelude::FluentBuilder, px,
};
use gpui_component::menu::PopupMenu;

type MenuBuilder = Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;
type RevisionFn = Rc<dyn Fn(&App) -> u64>;

/// Attach a self-refreshing context menu to an element.
pub trait LiveContextMenuExt: InteractiveElement + ParentElement + Styled {
    /// `revision` is polled every frame the menu is open; when its value
    /// changes, `builder` re-runs and the open menu is replaced with the
    /// result. Return a constant from `revision` for a menu whose content
    /// can't change while it is open.
    fn live_context_menu<R, F>(mut self, revision: R, builder: F) -> LiveContextMenu<Self>
    where
        Self: Sized,
        R: Fn(&App) -> u64 + 'static,
        F: Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    {
        // Same identity scheme as upstream: prefer the element's own id so
        // each menu keeps its own element state.
        let id = self
            .interactivity()
            .element_id
            .clone()
            .map(|id| format!("live-context-menu-{id:?}"))
            .unwrap_or_else(|| format!("live-context-menu-{:p}", &self as *const _));
        LiveContextMenu {
            id: id.into(),
            element: Some(self),
            builder: Some(Rc::new(builder)),
            revision: Rc::new(revision),
            anchor: Anchor::TopLeft,
            _ignore_style: StyleRefinement::default(),
        }
    }
}

impl<E: InteractiveElement + ParentElement + Styled> LiveContextMenuExt for E {}

pub struct LiveContextMenu<E: ParentElement + Styled + Sized> {
    id: ElementId,
    element: Option<E>,
    builder: Option<MenuBuilder>,
    revision: RevisionFn,
    anchor: Anchor,
    // Not used; present so style refinements forward to the wrapped element.
    _ignore_style: StyleRefinement,
}

impl<E: ParentElement + Styled> LiveContextMenu<E> {
    fn with_element_state<R>(
        &mut self,
        id: &GlobalElementId,
        window: &mut Window,
        cx: &mut App,
        f: impl FnOnce(&mut Self, &mut LiveContextMenuState, &mut Window, &mut App) -> R,
    ) -> R {
        window.with_optional_element_state::<LiveContextMenuState, _>(
            Some(id),
            |element_state, window| {
                let mut element_state = element_state.unwrap().unwrap_or_default();
                let result = f(self, &mut element_state, window, cx);
                (result, Some(element_state))
            },
        )
    }
}

impl<E: ParentElement + Styled> ParentElement for LiveContextMenu<E> {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        if let Some(element) = &mut self.element {
            element.extend(elements);
        }
    }
}

impl<E: ParentElement + Styled> Styled for LiveContextMenu<E> {
    fn style(&mut self) -> &mut StyleRefinement {
        if let Some(element) = &mut self.element {
            element.style()
        } else {
            &mut self._ignore_style
        }
    }
}

impl<E: ParentElement + Styled + IntoElement + 'static> IntoElement for LiveContextMenu<E> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct SharedState {
    menu_view: Option<Entity<PopupMenu>>,
    open: bool,
    position: Point<Pixels>,
    /// Revision the live `menu_view` was built from. `None` while a build is
    /// pending, which is also what suppresses duplicate rebuild scheduling.
    built_revision: Option<u64>,
    _subscription: Option<Subscription>,
}

pub struct LiveContextMenuState {
    element: Option<AnyElement>,
    shared_state: Rc<RefCell<SharedState>>,
}

impl Default for LiveContextMenuState {
    fn default() -> Self {
        Self {
            element: None,
            shared_state: Rc::new(RefCell::new(SharedState {
                menu_view: None,
                open: false,
                position: Default::default(),
                built_revision: None,
                _subscription: None,
            })),
        }
    }
}

/// Build (or rebuild) the popup on the next effect cycle and install it.
///
/// Deferred for the same reason upstream defers the first build: the click
/// that opens the menu is still being dispatched, so the entities the builder
/// wants to read are on the stack.
fn schedule_build(
    shared_state: Rc<RefCell<SharedState>>,
    builder: Option<MenuBuilder>,
    revision: u64,
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
                    state.built_revision = None;
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
            state.built_revision = Some(revision);
        }
        window.refresh();
    });
}

impl<E: ParentElement + Styled + IntoElement + 'static> Element for LiveContextMenu<E> {
    type RequestLayoutState = LiveContextMenuState;
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
            |this, state: &mut LiveContextMenuState, window, cx| {
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
                    LiveContextMenuState {
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
        let revision_fn = self.revision.clone();

        // Fork divergence: an open menu whose content revision moved on
        // rebuilds itself. `built_revision: None` means a build is already
        // in flight, so a slow builder can't be scheduled twice.
        let current_revision = (revision_fn)(cx);
        {
            let shared_state = request_layout.shared_state.clone();
            let stale = {
                let s = shared_state.borrow();
                s.open
                    && s.built_revision
                        .is_some_and(|built| built != current_revision)
            };
            if stale {
                shared_state.borrow_mut().built_revision = None;
                schedule_build(shared_state, builder.clone(), current_revision, window, cx);
            }
        }

        self.with_element_state(
            id.unwrap(),
            window,
            cx,
            |_view, state: &mut LiveContextMenuState, window, _| {
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
                            shared_state.built_revision = None;
                            shared_state.position = event.position;
                            shared_state.open = true;
                        }

                        let revision = (revision_fn)(cx);
                        schedule_build(shared_state.clone(), builder.clone(), revision, window, cx);
                    }
                });
            },
        );
    }
}
