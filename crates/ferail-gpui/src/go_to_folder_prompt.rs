//! Compact "Go to Folder" prompt.
//!
//! This is deliberately built on the ordinary single-line `InputState`.
//! Filesystem completion is a small adjacent model populated on a worker; a
//! document/code editor is neither necessary nor visually appropriate here.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _,
    Subscription, WeakEntity, Window, deferred, div, px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, WindowExt as _, h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    v_flex,
};

use crate::shell::Shell;
use crate::single_line_complete::{SingleLineSuggestions, apply_suggestion};
use crate::text::TextScale as _;

pub struct GoToFolderPrompt {
    input: Entity<InputState>,
    suggestions: SingleLineSuggestions,
    generation: u64,
    shell: WeakEntity<Shell>,
    in_new_tab: bool,
    #[allow(dead_code)]
    subscription: Subscription,
}

impl GoToFolderPrompt {
    pub fn new(
        shell: WeakEntity<Shell>,
        current: String,
        in_new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .submit_on_enter(true)
                .placeholder(tr!("/path/to/folder"))
        });
        input.update(cx, |state, cx| {
            state.set_value(current.clone(), window, cx);
            state.set_selected_range(0..current.len(), cx);
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let value = input.read(cx).value().to_string();
                    let cursor = input.read(cx).selected_range().end;
                    this.generation = this.generation.wrapping_add(1);
                    let generation = this.generation;
                    cx.spawn(async move |weak, cx| {
                        let worker_value = value.clone();
                        let items = cx
                            .background_executor()
                            .spawn(async move {
                                crate::path_complete::single_line_suggestions(&worker_value, cursor)
                            })
                            .await;
                        let _ = weak.update(cx, |this, cx| {
                            if this.generation == generation
                                && this.input.read(cx).value().as_ref() == value
                            {
                                this.suggestions.replace(items);
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
                InputEvent::PressEnter { .. } if !this.accept(None, window, cx) => {
                    this.commit(window, cx);
                }
                _ => {}
            },
        );
        Self {
            input,
            suggestions: Default::default(),
            generation: 0,
            shell,
            in_new_tab,
            subscription,
        }
    }

    pub fn focus_and_select_all(&self, window: &mut Window, cx: &mut App) {
        self.input.read(cx).focus_handle(cx).focus(window, cx);
        self.input
            .update(cx, |state, cx| state.select_all(window, cx));
    }

    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.input.read(cx).value().to_string();
        if raw.trim().is_empty() {
            window.close_dialog(cx);
            return;
        }
        window.close_dialog(cx);
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        shell.update(cx, |this, cx| {
            this.go_to_pasted_path(raw, self.in_new_tab, window, cx);
        });
    }

    fn accept(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let suggestion = index
            .and_then(|index| self.suggestions.items().get(index))
            .or_else(|| self.suggestions.selected())
            .cloned();
        let Some(suggestion) = suggestion else {
            return false;
        };
        self.suggestions.clear();
        apply_suggestion(&self.input, &suggestion, window, cx);
        true
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        if !self.suggestions.is_open() {
            return false;
        }
        self.suggestions.move_by(delta);
        cx.notify();
        true
    }
}

impl Render for GoToFolderPrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let completion_menu: Option<AnyElement> = if self.suggestions.is_open() {
            let mut menu = v_flex()
                .w_full()
                .max_h(px(240.0))
                .overflow_y_scrollbar()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .shadow_md()
                .p_1();
            for (index, suggestion) in self.suggestions.items().iter().take(10).enumerate() {
                let weak = cx.weak_entity();
                let selected = index == self.suggestions.selected_index();
                menu = menu.child(
                    h_flex()
                        .id(("go-to-folder-completion", index))
                        .w_full()
                        .px_2()
                        .py_1()
                        .rounded(cx.theme().radius)
                        .text_scale_sm()
                        .when(selected, |this| this.bg(cx.theme().accent.opacity(0.18)))
                        .hover(|this| this.bg(cx.theme().accent.opacity(0.12)))
                        .child(suggestion.label.clone())
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            let _ = weak.update(cx, |this, cx| {
                                this.accept(Some(index), window, cx);
                            });
                        }),
                );
            }
            Some(
                deferred(
                    div()
                        .absolute()
                        .top(px(30.0))
                        .left_0()
                        .w_full()
                        .occlude()
                        .child(menu),
                )
                .with_priority(20)
                .into_any_element(),
            )
        } else {
            None
        };

        div()
            .relative()
            .w_full()
            .on_action({
                let weak = cx.weak_entity();
                move |_: &gpui_component::input::MoveUp, _window, cx| {
                    let handled = weak
                        .update(cx, |this, cx| this.move_selection(-1, cx))
                        .unwrap_or(false);
                    if handled {
                        cx.stop_propagation();
                    }
                }
            })
            .on_action({
                let weak = cx.weak_entity();
                move |_: &gpui_component::input::MoveDown, _window, cx| {
                    let handled = weak
                        .update(cx, |this, cx| this.move_selection(1, cx))
                        .unwrap_or(false);
                    if handled {
                        cx.stop_propagation();
                    }
                }
            })
            .child(Input::new(&self.input).small())
            .when_some(completion_menu, |this, menu| this.child(menu))
    }
}
