//! Process-wide, session-only Private Mode.
//!
//! The first public implementation is deliberately fail-closed: every Ferail
//! render root replaces its normal contents with a safe private presentation.
//! This gives the interaction and pixel-safety contract one small chokepoint;
//! individual real surfaces can later opt into structured pseudonymized
//! presentation through `ferail_core::private_presentation`.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use ferail_core::private_presentation::PrivateSession;
use gpui::{
    App, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, SharedString, Styled as _, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Icon,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use crate::text::TextScale as _;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceKind {
    Browser,
    Settings,
    Viewer,
    DiskUsage,
    Archive,
    Information,
    Picker,
    Other,
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
    crate::log_info!(90, "private mode: arming generation {generation}");
    // Native captions are Ferail-owned pixels too (task switcher, Window
    // menu). Replace them synchronously before the protected state can be
    // acknowledged; each view restores its ordinary caption after exit.
    for handle in cx.windows() {
        let _ = handle.update(cx, |_root, window, _cx| {
            window.set_window_title(&tr!("Private — Ferail"));
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
    cx.stop_propagation();
    if event.keystroke.key == "escape" {
        exit(cx);
    }
}

fn exit_button() -> impl IntoElement {
    Button::new("private-mode-exit")
        .ghost()
        .icon(Icon::empty().path("icons/privacy.svg"))
        .label(tr!("Private"))
        .tooltip(tr!("Leave Private Mode (Esc)"))
        .on_click(|_: &ClickEvent, _window, cx| {
            cx.stop_propagation();
            exit(cx);
        })
}

/// Safe replacement for a Ferail-owned window.  It renders no raw model and
/// registers no normal action, drag, wheel or context-menu handlers.
pub fn surface<T>(
    kind: SurfaceKind,
    _window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let arming = matches!(state(), State::Arming { .. });
    let theme = cx.theme();
    let heading = if arming {
        tr!("Preparing private view…")
    } else {
        tr!("Private Mode")
    };
    let description = if arming {
        tr!("Ferail is replacing personal content before the next frame.")
    } else {
        tr!("Personal names, paths, content, and metadata are hidden.")
    };
    let kind_label = match kind {
        SurfaceKind::Browser => tr!("Files"),
        SurfaceKind::Settings => tr!("Settings"),
        SurfaceKind::Viewer => tr!("Viewer"),
        SurfaceKind::DiskUsage => tr!("Disk Usage"),
        SurfaceKind::Archive => tr!("Archive"),
        SurfaceKind::Information => tr!("Information"),
        SurfaceKind::Picker => tr!("Picker"),
        SurfaceKind::Other => tr!("Ferail"),
    };

    let aliases = session();
    let rows = [
        aliases.leaf("capture-document.pdf", false),
        aliases.leaf("reference-photo.jpg", false),
        aliases.leaf("project-folder", true),
        aliases.leaf("notes.txt", false),
        aliases.leaf("archive.tar.gz", false),
    ];

    v_flex()
        .id("private-mode-surface")
        .key_context("FerailPrivate")
        .on_key_down(on_private_key)
        .size_full()
        .min_w_0()
        .min_h_0()
        .bg(theme.background)
        .text_color(theme.foreground)
        .child(
            h_flex()
                .h(px(52.0))
                .w_full()
                .px_4()
                .items_center()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_scale_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Ferail"),
                        )
                        .child(
                            div()
                                .text_scale_xs()
                                .text_color(theme.muted_foreground)
                                .child(env!("CARGO_PKG_VERSION")),
                        ),
                )
                .child(div().flex_1())
                .child(exit_button()),
        )
        .child(
            h_flex()
                .flex_1()
                .min_h_0()
                .child(
                    v_flex()
                        .h_full()
                        .w(px(220.0))
                        .p_4()
                        .gap_3()
                        .border_r_1()
                        .border_color(theme.border)
                        .text_color(theme.muted_foreground)
                        .child(tr!("Locations"))
                        .child(tr!("Home"))
                        .child(tr!("Documents"))
                        .child(tr!("Downloads"))
                        .child(tr!("Pictures")),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(
                            h_flex()
                                .h(px(48.0))
                                .px_4()
                                .items_center()
                                .border_b_1()
                                .border_color(theme.border)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(kind_label),
                        )
                        .child(v_flex().flex_1().min_h_0().p_5().gap_3().children(
                            rows.into_iter().map(|name| {
                                h_flex()
                                    .h(px(38.0))
                                    .items_center()
                                    .gap_3()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(
                                        Icon::empty().path("icons/file/generic.svg").size(px(18.0)),
                                    )
                                    .child(SharedString::from(name))
                            }),
                        )),
                ),
        )
        .child(
            h_flex()
                .h(px(34.0))
                .px_4()
                .items_center()
                .border_t_1()
                .border_color(theme.border)
                .text_scale_xs()
                .text_color(theme.muted_foreground)
                .child(heading)
                .child(div().flex_1())
                .child(description),
        )
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
