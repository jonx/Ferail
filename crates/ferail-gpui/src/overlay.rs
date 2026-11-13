//! The one way Ferail shows a notification or opens a dialog.
//!
//! gpui-component's window Root draws dialogs and notifications itself, above
//! everything the application renders. Two Ferail rules need a hand on them,
//! so every caller goes through [`OverlayWindowExt`] and clippy denies the
//! direct `WindowExt::push_notification` / `open_dialog` calls:
//!
//! - **Private Mode.** The overlays would sit above the Private Mode shield
//!   and show raw names, so while the mode is on they are held and replayed
//!   on exit ([`replay_held`]).
//! - **Escape closes the dialog.** A dialog only hears Escape while focus is
//!   inside it, and focus can be taken back after it opens: closing the macOS
//!   menu bar restores focus to the element it came from, and a toolbar click
//!   can do the same. Each dialog therefore carries a zero-size dispatch
//!   anchor, and [`install`] sends `Cancel` from the newest anchor when Escape
//!   arrives with a dialog open and focus outside every dialog. `Cancel`
//!   takes the dialog's own path, so its `on_cancel` handler still runs.
//!   This is the pattern gpui-component's `DialogClose` uses for its button.

use std::cell::RefCell;

use gpui::{
    AnyWindowHandle, App, FocusHandle, InteractiveElement as _, ParentElement as _, Styled as _,
    WeakFocusHandle, Window, div,
};
use gpui_component::WindowExt as _;
use gpui_component::dialog::{Cancel, Dialog};
use gpui::{ClipboardItem, Context, SharedString};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::notification::Notification as ToastNotification;
use gpui_component::{IconName, Sizable as _};

/// A notification on its way to [`OverlayWindowExt::push_notice`].
///
/// It wraps gpui-component's notification and keeps the text it was built
/// with, which the widget does not expose, so every notice can carry a
/// **Copy message** button. The builder methods mirror the widget's.
pub struct Notification {
    inner: ToastNotification,
    title: Option<SharedString>,
    message: SharedString,
    has_action: bool,
    has_content: bool,
    /// An explicit `autohide` choice, kept because adding the copy button
    /// to the action slot would otherwise pin the notice open.
    autohide: Option<bool>,
}

impl Notification {
    fn wrap(inner: ToastNotification, message: SharedString) -> Self {
        Self {
            inner,
            title: None,
            message,
            has_action: false,
            has_content: false,
            autohide: None,
        }
    }

    pub fn info(message: impl Into<SharedString>) -> Self {
        let message = message.into();
        Self::wrap(ToastNotification::info(message.clone()), message)
    }

    pub fn success(message: impl Into<SharedString>) -> Self {
        let message = message.into();
        Self::wrap(ToastNotification::success(message.clone()), message)
    }

    pub fn warning(message: impl Into<SharedString>) -> Self {
        let message = message.into();
        Self::wrap(ToastNotification::warning(message.clone()), message)
    }

    pub fn error(message: impl Into<SharedString>) -> Self {
        let message = message.into();
        Self::wrap(ToastNotification::error(message.clone()), message)
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        let title = title.into();
        self.inner = self.inner.title(title.clone());
        self.title = Some(title);
        self
    }

    pub fn id<T: Sized + 'static>(mut self) -> Self {
        self.inner = self.inner.id::<T>();
        self
    }

    pub fn id1<T: Sized + 'static>(mut self, key: impl Into<gpui::ElementId>) -> Self {
        self.inner = self.inner.id1::<T>(key);
        self
    }

    pub fn autohide(mut self, autohide: bool) -> Self {
        self.inner = self.inner.autohide(autohide);
        self.autohide = Some(autohide);
        self
    }

    /// The notice's own action button. It keeps the notice open, as in the
    /// widget; the copy button then moves under the message.
    pub fn action<F>(mut self, action: F) -> Self
    where
        F: Fn(&mut ToastNotification, &mut Window, &mut Context<ToastNotification>) -> Button
            + 'static,
    {
        self.inner = self.inner.action(action);
        self.has_action = true;
        self
    }

    pub fn content(
        mut self,
        content: impl Fn(&mut ToastNotification, &mut Window, &mut Context<ToastNotification>) -> gpui::AnyElement
            + 'static,
    ) -> Self {
        self.inner = self.inner.content(content);
        self.has_content = true;
        self
    }

    pub fn on_click(
        mut self,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.inner = self.inner.on_click(on_click);
        self
    }

    pub fn on_close(mut self, on_close: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.inner = self.inner.on_close(on_close);
        self
    }

    /// The widget notification, with the copy button in the first free slot:
    /// the action slot, else under the message, else none.
    fn into_toast(self) -> ToastNotification {
        let text: SharedString = match &self.title {
            Some(title) => format!("{title}\n{}", self.message).into(),
            None => self.message.clone(),
        };
        if text.is_empty() {
            return self.inner;
        }
        let copy_button = move || {
            let text = text.clone();
            Button::new("notice-copy")
                .icon(IconName::Copy)
                .ghost()
                .xsmall()
                .tooltip(tr!("Copy message"))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
                })
        };
        if !self.has_action {
            // The action slot pins a notice open; restore what the caller
            // asked for, or the widget's default.
            self.inner
                .action(move |_, _, _| copy_button())
                .autohide(self.autohide.unwrap_or(true))
        } else if !self.has_content {
            self.inner.content(move |_, _, _| {
                use gpui::IntoElement as _;
                div().flex().pt_1().child(copy_button()).into_any_element()
            })
        } else {
            self.inner
        }
    }
}

impl From<String> for Notification {
    fn from(message: String) -> Self {
        let message: SharedString = message.into();
        Self::wrap(ToastNotification::new().message(message.clone()), message)
    }
}

impl From<SharedString> for Notification {
    fn from(message: SharedString) -> Self {
        Self::wrap(ToastNotification::new().message(message.clone()), message)
    }
}

impl From<&str> for Notification {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}

/// A notification or dialog that arrived while Private Mode was on.
enum HeldOverlay {
    Notice(Box<Notification>),
    Dialog(Box<DialogBuilder>),
}

type DialogBuilder = dyn Fn(Dialog, &mut Window, &mut App) -> Dialog + 'static;

thread_local! {
    /// UI-thread only, like every caller of `OverlayWindowExt`.
    static HELD: RefCell<Vec<(AnyWindowHandle, HeldOverlay)>> = const { RefCell::new(Vec::new()) };
    /// Dispatch anchors of the dialogs Ferail opened, oldest first. The
    /// strong handle lives in the dialog's builder, so a closed dialog's
    /// entry stops upgrading and is pruned.
    static ANCHORS: RefCell<Vec<(AnyWindowHandle, WeakFocusHandle)>> = const { RefCell::new(Vec::new()) };
}

/// Show what Private Mode held back, in arrival order, in the windows it was
/// meant for. A window closed in the meantime simply drops its share.
pub fn replay_held(cx: &mut App) {
    let held = HELD.with(|held| std::mem::take(&mut *held.borrow_mut()));
    for (handle, overlay) in held {
        let _ = handle.update(cx, |_root, window, cx| match overlay {
            HeldOverlay::Notice(note) => window.push_notice(*note, cx),
            HeldOverlay::Dialog(build) => window.open_modal(cx, build),
        });
    }
}

pub trait OverlayWindowExt {
    fn push_notice(&mut self, note: impl Into<Notification>, cx: &mut App);
    fn open_modal<F>(&mut self, cx: &mut App, build: F)
    where
        F: Fn(Dialog, &mut Window, &mut App) -> Dialog + 'static;
}

impl OverlayWindowExt for Window {
    fn push_notice(&mut self, note: impl Into<Notification>, cx: &mut App) {
        let note = note.into();
        if crate::private_mode::enabled() {
            let handle = self.window_handle();
            HELD.with(|held| {
                held.borrow_mut()
                    .push((handle, HeldOverlay::Notice(Box::new(note))))
            });
            return;
        }
        #[allow(clippy::disallowed_methods)]
        self.push_notification(note.into_toast(), cx);
    }

    fn open_modal<F>(&mut self, cx: &mut App, build: F)
    where
        F: Fn(Dialog, &mut Window, &mut App) -> Dialog + 'static,
    {
        let handle = self.window_handle();
        if crate::private_mode::enabled() {
            HELD.with(|held| {
                held.borrow_mut()
                    .push((handle, HeldOverlay::Dialog(Box::new(build))))
            });
            return;
        }
        let anchor: FocusHandle = cx.focus_handle();
        ANCHORS.with(|anchors| {
            let mut anchors = anchors.borrow_mut();
            anchors.retain(|(_, weak)| weak.upgrade().is_some());
            anchors.push((handle, anchor.downgrade()));
        });
        #[allow(clippy::disallowed_methods)]
        self.open_dialog(cx, move |dialog, window, cx| {
            // Zero-size and out of flow: it only gives the anchor a place in
            // this dialog's dispatch tree. Never hovered, so never focused.
            build(dialog, window, cx).child(div().absolute().size_0().track_focus(&anchor))
        });
    }
}

/// Route a stray Escape to the newest open dialog (see the module docs).
/// Installed once at startup; covers every Ferail window.
pub fn install(cx: &mut App) {
    cx.intercept_keystrokes(|event, window, cx| {
        let keystroke = &event.keystroke;
        if keystroke.key != "escape" || keystroke.modifiers.modified() {
            return;
        }
        if !window.has_active_dialog(cx) {
            return;
        }
        // Focus already inside a dialog: its own Escape binding answers.
        if window
            .context_stack()
            .iter()
            .any(|ctx| ctx.contains("Dialog"))
        {
            return;
        }
        if cancel_newest_dialog(window, cx) {
            cx.stop_propagation();
        }
    })
    .detach();
}

/// Dispatch `Cancel` from the newest rendered dialog anchor in `window`.
/// Returns whether one was found.
fn cancel_newest_dialog(window: &mut Window, cx: &mut App) -> bool {
    let this_window = window.window_handle();
    let anchor = ANCHORS.with(|anchors| {
        let mut anchors = anchors.borrow_mut();
        anchors.retain(|(_, weak)| weak.upgrade().is_some());
        anchors
            .iter()
            .rev()
            .filter(|(handle, _)| *handle == this_window)
            .filter_map(|(_, weak)| weak.upgrade())
            .find(|anchor| anchor.contains(anchor, window))
    });
    let Some(anchor) = anchor else {
        return false;
    };
    anchor.dispatch_action(&Cancel, window, cx);
    true
}
