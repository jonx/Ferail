//! Shared inline-editing presentation and lifecycle.
//!
//! Zed's project panel gets inline rename right by keeping one persistent
//! editor and swapping it into the row being edited.  Ferail uses the same
//! shape, but keeps the lifecycle independent from filenames so later fields
//! (dates, labels, numeric values) can reuse it without inheriting filesystem
//! rules.

use std::{cell::RefCell, rc::Rc};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ElementId, Entity, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    input::{Input, InputState},
    tooltip::Tooltip,
};

/// Semantic identity for the first concrete inline field Ferail ships.
/// Keeping the tab in the key prevents a stale editor from appearing over a
/// coincidentally equal `NodeId` after a tab switch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileNameEditTarget {
    pub tab_id: u64,
    pub node_id: ferail_core::NodeId,
}

/// The generic lifecycle shared by every inline editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineEditPhase {
    Editing,
    Committing,
}

/// Validation belongs to the value adapter, not the editor chrome.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum InlineEditValidation {
    #[default]
    Valid,
    Error(SharedString),
}

/// One active edit. `K` identifies the semantic target (a file row, a path
/// bar, eventually a date field); the component itself never interprets it.
#[derive(Clone, Debug)]
pub struct InlineEditSession<K> {
    pub target: K,
    pub original: SharedString,
    pub phase: InlineEditPhase,
    pub validation: InlineEditValidation,
}

/// Cheap cloneable handle shared by the Shell and virtualized renderers.
///
/// Only one session can exist for a given field family, so a repeated F2 can
/// refocus the existing editor instead of allocating another prompt.  The
/// model stores no filesystem data and performs no I/O.
#[derive(Clone)]
pub struct InlineEditModel<K> {
    session: Rc<RefCell<Option<InlineEditSession<K>>>>,
}

impl<K> Default for InlineEditModel<K> {
    fn default() -> Self {
        Self {
            session: Rc::new(RefCell::new(None)),
        }
    }
}

impl<K: Clone + Eq> InlineEditModel<K> {
    pub fn begin(&self, target: K, original: impl Into<SharedString>) {
        *self.session.borrow_mut() = Some(InlineEditSession {
            target,
            original: original.into(),
            phase: InlineEditPhase::Editing,
            validation: InlineEditValidation::Valid,
        });
    }

    pub fn clear(&self) -> bool {
        self.session.borrow_mut().take().is_some()
    }

    pub fn is_active(&self) -> bool {
        self.session.borrow().is_some()
    }

    pub fn is_target(&self, target: &K) -> bool {
        self.session
            .borrow()
            .as_ref()
            .is_some_and(|session| &session.target == target)
    }

    pub fn snapshot(&self) -> Option<InlineEditSession<K>> {
        self.session.borrow().clone()
    }

    pub fn update(&self, target: &K, f: impl FnOnce(&mut InlineEditSession<K>)) -> bool {
        let mut session = self.session.borrow_mut();
        let Some(session) = session.as_mut().filter(|session| &session.target == target) else {
            return false;
        };
        f(session);
        true
    }
}

/// The input engine used by Ferail's inline surfaces. Inline values are all
/// single-line fields; full document editors belong in text previews/editors,
/// not in compact toolbar or row chrome.
#[derive(Clone)]
pub enum InlineEditInput {
    Text(Entity<InputState>),
}

/// Layout presets are deliberately presentation-only.  A future date editor
/// can choose Row/Grid without adding another lifecycle implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineEditLayout {
    Row,
    Grid,
    AddressBar,
}

/// Explorer-style rename gesture: the item was already selected, then its
/// label received a later plain single click.
///
/// This decides only that the gesture *qualifies*, never that the editor
/// should mount now. The first click of a double-click is reported with
/// count 1 too, so the caller arms the rename and waits out the
/// double-click interval (`Shell::arm_click_rename`); a count >= 2 event
/// cancels it and Open wins. Mounting the editor on the count-1 click made
/// double-clicking a selected folder open the name editor instead, because
/// the freshly mounted input swallowed the second click.
pub fn should_begin_click_rename(
    already_selected: bool,
    editing: bool,
    click_count: usize,
    modified: bool,
) -> bool {
    already_selected && !editing && click_count == 1 && !modified
}

/// Whether one of the Shell's single-line surfaces owns Escape. Keeping this
/// policy explicit avoids depending on action propagation across entity
/// boundaries.
pub fn should_capture_inline_escape(
    name_focused: bool,
    path_focused: bool,
    filter_focused: bool,
) -> bool {
    name_focused || path_focused || filter_focused
}

/// Reusable inline editor chrome: a quiet one-pixel focus outline, no shadow,
/// and a validation outline/tooltip.  The input entity is persistent; this
/// element is only the place where it is mounted for the current frame.
#[derive(IntoElement)]
pub struct InlineEditor {
    id: ElementId,
    input: InlineEditInput,
    layout: InlineEditLayout,
    phase: InlineEditPhase,
    validation: InlineEditValidation,
    aria_label: SharedString,
}

impl InlineEditor {
    pub fn new(
        id: impl Into<ElementId>,
        input: InlineEditInput,
        layout: InlineEditLayout,
        session: &InlineEditSession<impl Clone>,
        aria_label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            input,
            layout,
            phase: session.phase,
            validation: session.validation.clone(),
            aria_label: aria_label.into(),
        }
    }
}

impl RenderOnce for InlineEditor {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let error = match &self.validation {
            InlineEditValidation::Valid => None,
            InlineEditValidation::Error(message) => Some(message.clone()),
        };
        let border = if error.is_some() {
            cx.theme().danger
        } else {
            crate::selection_colors::strong(cx)
        };
        let committing = self.phase == InlineEditPhase::Committing;
        let input = match self.input {
            InlineEditInput::Text(state) => Input::new(&state)
                .xsmall()
                .appearance(false)
                .bordered(false)
                .readonly(committing)
                .aria_label(self.aria_label)
                .into_any_element(),
        };

        div()
            .id(self.id)
            .flex()
            .items_center()
            .min_w_0()
            .when(self.layout != InlineEditLayout::Grid, |this| this.flex_1())
            .when(self.layout == InlineEditLayout::Grid, |this| this.w_full())
            .h(px(match self.layout {
                InlineEditLayout::Row => 24.0,
                InlineEditLayout::Grid => 28.0,
                InlineEditLayout::AddressBar => 28.0,
            }))
            // Input::xsmall owns the compact 4-DIP horizontal inset. Adding
            // another frame inset here was the source of the address-field
            // text drifting down and right.
            .px_0()
            .rounded(px(4.0))
            .border_1()
            .border_color(border)
            .bg(cx.theme().input_background())
            .when(committing, |this| this.opacity(0.72))
            // Text selection belongs to the input, not the file-row/grid
            // gesture sitting above it.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            // gpui-component emits PressEnter to the Input subscriber, but
            // the matching Enter action can continue through the entity
            // boundary afterwards.  Stop it here so accepting an inline
            // rename cannot also reach Shell::OpenSelected with the old path.
            .on_action(|_: &gpui_component::input::Enter, _, cx| cx.stop_propagation())
            .child(input)
            .when_some(error, |this, message| {
                this.tooltip(move |window, cx| Tooltip::new(message.clone()).build(window, cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_session_replaces_the_previous_target() {
        let model = InlineEditModel::<u64>::default();
        model.begin(1, "one");
        model.begin(2, "two");
        let session = model.snapshot().expect("active session");
        assert_eq!(session.target, 2);
        assert_eq!(session.original.as_ref(), "two");
    }

    #[test]
    fn updates_are_target_scoped() {
        let model = InlineEditModel::<u64>::default();
        model.begin(7, "name");
        assert!(!model.update(&8, |session| session.phase = InlineEditPhase::Committing));
        assert_eq!(model.snapshot().unwrap().phase, InlineEditPhase::Editing);
        assert!(model.update(&7, |session| session.phase = InlineEditPhase::Committing));
        assert_eq!(model.snapshot().unwrap().phase, InlineEditPhase::Committing);
    }

    #[test]
    fn rename_click_requires_a_plain_reclick_on_the_selected_label() {
        assert!(should_begin_click_rename(true, false, 1, false));
        assert!(!should_begin_click_rename(false, false, 1, false));
        assert!(!should_begin_click_rename(true, true, 1, false));
        // A real double-click never qualifies: Open wins outright.
        assert!(!should_begin_click_rename(true, false, 2, false));
        assert!(!should_begin_click_rename(true, false, 3, false));
        assert!(!should_begin_click_rename(true, false, 1, true));
    }

    #[test]
    fn escape_is_captured_only_for_an_active_inline_surface() {
        assert!(should_capture_inline_escape(true, false, false));
        assert!(should_capture_inline_escape(false, true, false));
        assert!(should_capture_inline_escape(false, false, true));
        assert!(!should_capture_inline_escape(false, false, false));
    }
}
