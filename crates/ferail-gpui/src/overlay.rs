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
use gpui_component::notification::Notification;

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
        self.push_notification(note, cx);
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
