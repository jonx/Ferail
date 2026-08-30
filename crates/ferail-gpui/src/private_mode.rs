//! Process-wide, session-only Private Mode.
//!
//! Private Mode preserves the prepared Ferail interface and projects only
//! sensitive values at render time. Raw models never change. A transparent,
//! process-wide interaction shield freezes the prepared view while a semantic
//! presenter supplies stable session aliases for names and paths.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use ferail_core::private_presentation::{PrivateSession, PrivateValue};
use gpui::{
    AnyElement, App, Bounds, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    ParentElement as _, Pixels, SharedString, Styled as _, Window, div,
};

gpui::actions!(private_mode, [TogglePrivateMode, ExitPrivateMode]);

const OFF: u8 = 0;
const ARMING: u8 = 1;
const ACTIVE: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(OFF);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static SESSION: OnceLock<Arc<PrivateSession>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Off,
    Arming { generation: u64 },
    Active { generation: u64 },
}

pub fn state() -> State {
    let generation = GENERATION.load(Ordering::Acquire);
    match STATE.load(Ordering::Acquire) {
        ARMING => State::Arming { generation },
        ACTIVE => State::Active { generation },
        _ => State::Off,
    }
}

#[inline]
pub fn enabled() -> bool {
    STATE.load(Ordering::Acquire) != OFF
}

pub fn session() -> Arc<PrivateSession> {
    SESSION
        .get_or_init(|| Arc::new(PrivateSession::new()))
        .clone()
}

pub fn toggle(cx: &mut App) {
    if enabled() {
        exit(cx);
    } else {
        enter(cx);
    }
}

/// Install the lock before repainting.  The first protected frame is the
/// opaque Arming presentation; Active follows on the next deferred UI turn.
pub fn enter(cx: &mut App) {
    if enabled() {
        return;
    }
    let generation = GENERATION.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
    let _ = session();
    STATE.store(ARMING, Ordering::Release);
    crate::boot::install_private_menus(cx);
    crate::log_info!(90, "private mode: arming generation {generation}");
    // Native captions are Ferail-owned pixels too (task switcher, Window
    // menu). Replace them synchronously before the protected state can be
    // acknowledged; each view restores its ordinary caption after exit.
    for handle in cx.windows() {
        let _ = handle.update(cx, |_root, window, _cx| {
            window.set_window_title(&tr!("Private - Ferail"));
        });
    }
    cx.refresh_windows();
    cx.defer(move |cx| {
        if GENERATION.load(Ordering::Acquire) == generation
            && STATE.load(Ordering::Acquire) == ARMING
        {
            STATE.store(ACTIVE, Ordering::Release);
            crate::log_info!(90, "private mode: active generation {generation}");
            cx.refresh_windows();
        }
    });
}

pub fn exit(cx: &mut App) {
    if !enabled() {
        return;
    }
    STATE.store(OFF, Ordering::Release);
    crate::boot::install_app_menus(cx);
    crate::log_info!(90, "private mode: off");
    cx.refresh_windows();
}

/// Normal app-level commands call this before doing work.  Root-level Shell
/// and secondary-window handlers are also absent while their private surface
/// is rendered, making the default policy deny-by-construction.
#[inline]
pub fn blocks_normal_actions() -> bool {
    enabled()
}

fn on_private_key(event: &KeyDownEvent, _window: &mut Window, cx: &mut App) {
    let toggle_shortcut = event.keystroke.key.eq_ignore_ascii_case("k")
        && event.keystroke.modifiers.platform
        && event.keystroke.modifiers.shift;
    if event.keystroke.key == "escape" || toggle_shortcut {
        exit(cx);
    }
    // Capture-phase listener on the protection wrapper: no focused child can
    // turn this keystroke into navigation, editing, or a command first.
    cx.stop_propagation();
}

/// Present a filesystem leaf through the shared semantic interface. The off
/// path is a cheap `SharedString` clone; entering Private Mode therefore never
/// walks or rewrites a million-row model, only visible controls ask for an
/// alias as they render.
pub fn present_leaf(raw: &SharedString, is_dir: bool) -> SharedString {
    if enabled() {
        session()
            .present(PrivateValue::Leaf {
                raw: raw.as_ref(),
                is_dir,
            })
            .into()
    } else {
        raw.clone()
    }
}

pub fn present_leaf_str(raw: &str, is_dir: bool) -> String {
    if enabled() {
        session().present(PrivateValue::Leaf { raw, is_dir })
    } else {
        raw.to_owned()
    }
}

pub fn present_path(raw: &std::path::Path) -> String {
    if enabled() {
        session().present(PrivateValue::Path(raw))
    } else {
        ferail_fs_native::paths::display_path(raw)
    }
}

pub fn present_label(raw: &str) -> String {
    if enabled() {
        session().present(PrivateValue::Label(raw))
    } else {
        raw.to_owned()
    }
}

pub fn present_bytes(identity: u64, raw: u64) -> u64 {
    if enabled() {
        session().bytes(identity, raw)
    } else {
        raw
    }
}

pub fn present_timestamp(identity: u64, raw: i64, now: i64) -> i64 {
    if enabled() {
        session().timestamp(identity, raw, now)
    } else {
        raw
    }
}

pub fn present_dimensions(identity: u64, raw: (u32, u32)) -> (u32, u32) {
    if enabled() {
        session().dimensions(identity, raw)
    } else {
        raw
    }
}

pub fn present_digest(raw: &str, width: usize) -> String {
    if enabled() {
        session().present(PrivateValue::Digest { raw, width })
    } else {
        raw.to_owned()
    }
}

/// Preserve the real prepared UI and place an invisible input shield above it.
/// Window close/quit remain OS-level and therefore continue to work.
pub fn protect(content: impl IntoElement, cx: &App) -> AnyElement {
    protect_with_toggle(content, cx, None)
}

/// Shell variant of [`protect`]. The invisible shield still blocks the whole
/// window, but a click landing on the already-painted title-bar shield toggles
/// Private Mode off. No duplicate exit control is painted above the UI.
pub fn protect_with_toggle(
    content: impl IntoElement,
    _cx: &App,
    toggle_bounds: Option<Bounds<Pixels>>,
) -> AnyElement {
    if !enabled() {
        return content.into_any_element();
    }
    let shield = div()
        .id("private-mode-interaction-shield")
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .left_0()
        // One almost-transparent pixel layer gives the overlay a hitbox while
        // leaving the prepared application visually unchanged.
        .bg(gpui::rgba(0x00000001))
        .occlude()
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            if toggle_bounds.is_some_and(|bounds| bounds.contains(&event.position)) {
                exit(cx);
            }
            cx.stop_propagation();
        })
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation());
    div()
        .id("private-mode-protected-root")
        .relative()
        .size_full()
        .capture_key_down(on_private_key)
        .child(content)
        .child(shield)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state_is_off() {
        STATE.store(OFF, Ordering::Release);
        assert_eq!(state(), State::Off);
        assert!(!blocks_normal_actions());
    }
}
