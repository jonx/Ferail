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
use std::rc::Rc;

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
    /// The caller's own content (buttons under the message), drawn below
    /// the structured parts.
    content: Option<Rc<ContentBuilder>>,
    /// An explicit `autohide` choice, kept because adding the copy button
    /// to the action slot would otherwise pin the notice open.
    autohide: Option<bool>,
    /// What the message is about, how to cope, and the raw detail, each
    /// on its own line under the message ([`Self::report`]).
    parts: Parts,
}

type ContentBuilder = dyn Fn(&mut ToastNotification, &mut Window, &mut Context<ToastNotification>) -> gpui::AnyElement;

/// The structured lines of a notice, under its one-line message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parts {
    /// What it applies to: an item, a path. May carry
    /// [`crate::emphasis`] markers around a name.
    pub subjects: Vec<String>,
    /// The practical next step.
    pub advice: Option<String>,
    /// The raw technical detail (OS error text), small and muted.
    pub detail: Option<String>,
}

impl Parts {
    fn is_empty(&self) -> bool {
        self.subjects.is_empty() && self.advice.is_none() && self.detail.is_none()
    }
}

/// Split a flat error message into the problem and its [`Parts`]:
///
/// - the first line is the problem; a path inside it (`Copying failed:
///   /a/b.pdf: No such file`) becomes the subject, and what follows the
///   path becomes the detail;
/// - `• ` lines are subjects, other lines are advice;
/// - anything after a blank line is detail.
pub fn split_error(message: &str) -> (String, Parts) {
    let (head, tail) = match message.split_once("\n\n") {
        Some((head, tail)) => (head, Some(tail.trim())),
        None => (message, None),
    };
    let mut lines = head.lines();
    let mut problem = lines.next().unwrap_or("").trim().to_string();
    let mut parts = Parts::default();
    let mut advice: Vec<&str> = Vec::new();
    for line in lines {
        let line = line.trim();
        if let Some(item) = line.strip_prefix("\u{2022} ") {
            // "name: reason" (French "name : reason"): the name in italics.
            // The last colon, since a name may hold one and a reason does not.
            let subject = match item.rsplit_once(':') {
                Some((name, reason)) if !name.trim().is_empty() && reason.starts_with(' ') => {
                    format!(
                        "{}{}:{reason}",
                        crate::emphasis::name(name.trim_end()),
                        &name[name.trim_end().len()..]
                    )
                }
                _ => item.to_string(),
            };
            parts.subjects.push(subject);
        } else if !line.is_empty() {
            advice.push(line);
        }
    }
    let mut detail: Vec<String> = Vec::new();
    if let Some((before, path, rest)) = split_path(&problem) {
        let path = std::path::Path::new(&path);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let subject = match path.parent().filter(|p| !p.as_os_str().is_empty()) {
            Some(parent) => format!(
                "{} \u{00b7} {}",
                crate::emphasis::name(name),
                parent.display()
            ),
            None => crate::emphasis::name(name),
        };
        parts.subjects.insert(0, subject);
        if let Some(rest) = rest {
            detail.push(rest);
        }
        problem = before;
    }
    if let Some(tail) = tail.filter(|t| !t.is_empty()) {
        detail.push(tail.to_string());
    }
    parts.advice = (!advice.is_empty()).then(|| advice.join(" "));
    parts.detail = (!detail.is_empty()).then(|| detail.join("\n"));
    (problem, parts)
}

/// `"<problem>: <path>[: <rest>]"` → its three pieces. A path starts with
/// `/`, `~/`, a drive letter or `\\`, right after a `": "`.
fn split_path(line: &str) -> Option<(String, String, Option<String>)> {
    let looks_like_path = |s: &str| {
        let b = s.as_bytes();
        s.starts_with('/')
            || s.starts_with("~/")
            || s.starts_with("\\\\")
            || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
    };
    let mut search = 0;
    while let Some(found) = line[search..].find(": ") {
        let at = search + found;
        let after = &line[at + 2..];
        if looks_like_path(after) {
            let before = line[..at].trim_end().to_string();
            return Some(match after.find(": ") {
                Some(end) => (
                    before,
                    after[..end].to_string(),
                    Some(after[end + 2..].trim().to_string()).filter(|r| !r.is_empty()),
                ),
                None => (before, after.trim_end_matches('.').to_string(), None),
            });
        }
        search = at + 2;
    }
    None
}

impl Notification {
    fn wrap(inner: ToastNotification, message: SharedString) -> Self {
        Self {
            inner,
            title: None,
            message,
            has_action: false,
            content: None,
            autohide: None,
            parts: Parts::default(),
        }
    }

    /// An error laid out for reading at a glance: the problem on the first
    /// line, then what it applies to, what to do, and the technical detail,
    /// each on its own line ([`split_error`] finds them in a flat message).
    /// When the detail names a known failure (a missing file, a permission
    /// denial) and the message offers no advice, the plain reason joins the
    /// problem and its advice is added.
    pub fn report(message: &str) -> Self {
        let (mut problem, mut parts) = split_error(message);
        if parts.advice.is_none()
            && let Some(detail) = &parts.detail
        {
            let kind = crate::shell::classify_error_text(detail);
            if kind != ferail_fs_native::file_ops::FileOpErrorKind::Other {
                problem = tr!(
                    "{label}: {detail}",
                    label = problem,
                    detail = crate::i18n::tr_static(kind.summary())
                )
                .to_string();
                parts.advice = Some(crate::i18n::tr_static(kind.advice()).to_string());
            }
        }
        let mut note = Self::error(problem);
        note.parts = parts;
        note
    }

    /// Add the raw technical detail under the message.
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        self.parts.detail = Some(match self.parts.detail.take() {
            Some(existing) => format!("{existing}\n{detail}"),
            None => detail,
        });
        self
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
        self.content = Some(Rc::new(content));
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

    /// The widget notification: the structured parts and the caller's
    /// content under the message, and the copy button in the action slot,
    /// or at the end of the content when the caller took that slot.
    fn into_toast(self) -> ToastNotification {
        let mut lines: Vec<String> = Vec::new();
        if let Some(title) = &self.title {
            lines.push(title.to_string());
        }
        lines.push(self.message.to_string());
        for subject in &self.parts.subjects {
            lines.push(crate::emphasis::plain(subject));
        }
        lines.extend(self.parts.advice.clone());
        lines.extend(self.parts.detail.clone());
        let text: SharedString = lines
            .into_iter()
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
            .into();
        let parts = self.parts.clone();
        let content = self.content.clone();
        let has_action = self.has_action;
        let inner = if parts.is_empty() && content.is_none() {
            self.inner
        } else {
            let text = text.clone();
            self.inner.content(move |note, window, cx| {
                use gpui::IntoElement as _;
                let mut col = gpui_component::v_flex().gap_1p5().pt_1().min_w_0();
                col = col.children(render_parts(&parts, cx));
                if let Some(content) = &content {
                    col = col.child(content(note, window, cx));
                }
                if has_action && !text.is_empty() {
                    col = col.child(div().flex().child(copy_button(text.clone())));
                }
                col.into_any_element()
            })
        };
        if text.is_empty() {
            return inner;
        }
        if !has_action {
            // The action slot pins a notice open; restore what the caller
            // asked for, or the widget's default.
            inner
                .action(move |_, _, _| copy_button(text.clone()))
                .autohide(self.autohide.unwrap_or(true))
        } else if self.parts.is_empty() && self.content.is_none() {
            inner.content(move |_, _, _| {
                use gpui::IntoElement as _;
                div().flex().pt_1().child(copy_button(text.clone())).into_any_element()
            })
        } else {
            inner
        }
    }
}

/// The icon that puts the whole notice on the clipboard.
fn copy_button(text: SharedString) -> Button {
    Button::new("notice-copy")
        .icon(IconName::Copy)
        .ghost()
        .xsmall()
        .tooltip(tr!("Copy message"))
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        })
}

/// Subjects, advice, then the detail in a small scrolling well.
fn render_parts(parts: &Parts, cx: &App) -> Vec<gpui::AnyElement> {
    use crate::text::TextScale as _;
    use gpui::{IntoElement as _, StatefulInteractiveElement as _, px};
    use gpui_component::ActiveTheme as _;
    let muted = cx.theme().muted_foreground;
    let mut out = Vec::new();
    if !parts.subjects.is_empty() {
        out.push(
            gpui_component::v_flex()
                .gap_0p5()
                .min_w_0()
                .children(parts.subjects.iter().map(|subject| {
                    div()
                        .min_w_0()
                        .text_scale_sm()
                        .child(crate::emphasis::styled(subject))
                }))
                .into_any_element(),
        );
    }
    if let Some(advice) = &parts.advice {
        out.push(
            div()
                .text_scale_sm()
                .text_color(muted)
                .child(SharedString::from(advice.clone()))
                .into_any_element(),
        );
    }
    if let Some(detail) = &parts.detail {
        out.push(
            div()
                .id("notice-detail")
                .max_h(px(96.))
                .overflow_y_scroll()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(cx.theme().muted)
                .text_scale_xs()
                .text_color(muted)
                .child(SharedString::from(detail.clone()))
                .into_any_element(),
        );
    }
    out
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

#[cfg(test)]
mod tests {
    use super::{Parts, split_error};

    #[test]
    fn a_path_in_the_problem_becomes_subject_and_detail() {
        let (problem, parts) = split_error(
            "Échec de la copie : /Users/demo/Documents/Rapport annuel.pdf: No such file or directory (os error 2)",
        );
        assert_eq!(problem, "Échec de la copie");
        assert_eq!(
            crate::emphasis::plain(&parts.subjects[0]),
            "Rapport annuel.pdf \u{00b7} /Users/demo/Documents"
        );
        assert_eq!(parts.detail.as_deref(), Some("No such file or directory (os error 2)"));
        assert_eq!(parts.advice, None);
    }

    #[test]
    fn bullets_advice_and_detail_after_a_blank_line() {
        let (problem, parts) = split_error(
            "Move: 1 of 3 done \u{00b7} 2 failed\n\u{2022} a.txt: permission denied\n\u{2022} b.txt: in use\nRetry as administrator.\n\n/x/a.txt: EACCES",
        );
        assert_eq!(problem, "Move: 1 of 3 done \u{00b7} 2 failed");
        let plain: Vec<String> = parts.subjects.iter().map(|s| crate::emphasis::plain(s)).collect();
        assert_eq!(plain, vec!["a.txt: permission denied", "b.txt: in use"]);
        assert_eq!(parts.subjects[0], format!("{}: permission denied", crate::emphasis::name("a.txt")));
        assert_eq!(parts.advice.as_deref(), Some("Retry as administrator."));
        assert_eq!(parts.detail.as_deref(), Some("/x/a.txt: EACCES"));
    }

    #[test]
    fn plain_messages_stay_whole() {
        let (problem, parts) = split_error("Could not compare: the folders are the same");
        assert_eq!(problem, "Could not compare: the folders are the same");
        assert_eq!(parts, Parts::default());
        // A Windows path is recognised everywhere; it splits into name and
        // folder where `Path` knows backslashes.
        let (problem, parts) = split_error("Open failed: C:\\Users\\a.txt");
        assert_eq!(problem, "Open failed");
        assert_eq!(parts.subjects.len(), 1);
    }
}
