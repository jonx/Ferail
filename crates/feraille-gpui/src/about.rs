//! About dialog — a modal popup hosted by gpui-component's `Dialog`
//! primitive. The Dialog ships ESC-to-dismiss, click-outside-to-
//! dismiss (overlay click), a close button, and an open/close
//! animation — wiring all of that by hand on a stand-alone OS
//! window would mean reimplementing focus traps + activation
//! observers from scratch. The earlier separate-window version
//! is gone for that reason; if About should ever be detachable
//! we can promote `AboutBody` to a stand-alone view.
//!
//! Surface: [`open_about_dialog`] — call from any App-level handler
//! (menu / accelerator). It defers the actual open until the next app
//! tick so app-menu dispatch has finished unwinding, then resolves a
//! host window and routes `window.open_dialog(...)`.
//!
//! Singleton: a `Global` boolean guards against stacked Abouts when
//! the menu item is clicked twice in a row. The flag is cleared in
//! the dialog's `on_close` callback.
//!
//! Content:
//!  - 96x96 app icon (decoded from the embedded PNG, BGRA-swapped)
//!  - "Feraille" wordmark + version
//!  - Tagline
//!  - Platform (OS · arch), Author, clickable Website
//!  - Copyright

use crate::text::TextScale as _;
use std::sync::Arc;

use gpui::*;
use gpui_component::{
    WindowExt as _,
    dialog::{Dialog, DialogButtonProps},
    h_flex, v_flex,
};
use image::{Frame, RgbaImage};
use smallvec::SmallVec;

/// Process-wide flag: is an About dialog currently visible? Cheap
/// guard against the menu item being clicked twice in quick
/// succession (gpui-component allows stacking dialogs by default —
/// for About that would look like a bug). Stored as a Global so the
/// open and close call sites read from the same source of truth.
#[derive(Default)]
struct AboutOpenFlag(bool);
impl Global for AboutOpenFlag {}

/// Show the About dialog. Routes through a hosting window so the
/// dialog inherits its focus stack + overlay; multiple invocations
/// no-op while one is already open or queued to open.
///
/// Host-window resolution falls back through three sources:
///  1. `cx.active_window()` — normal case (key-equivalent fired,
///     or menu raised from a focused window on Mac).
///  2. First open window — on Windows, the menu's action dispatch
///     can leave `active_window()` returning `None` mid-flight;
///     any open window has a Root with the dialog layer, so this
///     covers the menu-click path.
///  3. None of those exist (zero-window state) → fall back to the
///     platform About panel instead of silently doing nothing.
///
/// The app-menu path defers the open to the next app tick; trying to
/// mutate the window immediately from the menu callback can get lost
/// while the native menu is still dismissing.
pub fn open_about_dialog(cx: &mut App) {
    if cx
        .try_global::<AboutOpenFlag>()
        .map(|f| f.0)
        .unwrap_or(false)
    {
        return;
    }
    cx.set_global(AboutOpenFlag(true));
    cx.defer(|cx| {
        if !open_about_dialog_now(cx) {
            cx.set_global(AboutOpenFlag(false));
            crate::platform_shell::show_about_panel();
        }
    });
}

fn open_about_dialog_now(cx: &mut App) -> bool {
    let active = cx.active_window();
    let all = cx.windows();
    let Some(host) = active.or_else(|| all.into_iter().next()) else {
        return false;
    };
    host.update(cx, |_, window, cx| {
        window.open_dialog(cx, move |dialog, _window, _cx| build_dialog(dialog));
    })
    .is_ok()
}

fn build_dialog(dialog: Dialog) -> Dialog {
    dialog
        .title("About Feraille")
        .w(px(380.0))
        .overlay_closable(true)
        .keyboard(true)
        .close_button(true)
        .button_props(
            // No OK/Cancel buttons in the footer — Close button in the
            // corner + ESC + overlay click cover dismissal. An empty
            // `show_cancel(false)` keeps both buttons hidden.
            DialogButtonProps::default().show_cancel(false),
        )
        .child(about_body())
        // Clear the singleton flag once the dialog has closed (covers
        // ESC, overlay click, close button, all three).
        .on_close(|_, _window, cx: &mut App| {
            cx.set_global(AboutOpenFlag(false));
        })
}

/// The dialog's body, rendered fresh on each open. Static content,
/// so we don't need a View — a plain `IntoElement` is enough.
fn about_body() -> impl IntoElement {
    let os_label: &str = match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    };
    let arch = std::env::consts::ARCH;
    let version = env!("CARGO_PKG_VERSION");

    let icon = decode_icon(crate::app_icon::PNG);

    v_flex()
        .items_center()
        .gap_3()
        .py_2()
        .child(icon_element(&icon))
        .child(
            v_flex()
                .items_center()
                .gap_1()
                .child(WithTheme::wordmark())
                .child(WithTheme::muted(format!("Version {version}")))
                .child(WithTheme::tagline("A fast, calm file explorer.")),
        )
        .child(
            v_flex()
                .gap_1()
                .items_center()
                .child(meta_row("Platform", format!("{os_label} \u{00B7} {arch}")))
                .child(meta_row("Author", "John Knipper".to_string()))
                .child(website_row("github.com/jonx/Feraille")),
        )
        .child(WithTheme::copyright("Copyright \u{00A9} 2026 John Knipper"))
}

fn icon_element(icon: &Option<Arc<RenderImage>>) -> AnyElement {
    match icon {
        Some(handle) => img(handle.clone())
            .w(px(96.0))
            .h(px(96.0))
            .into_any_element(),
        // Decode failure: keep the slot so the rest of the layout
        // doesn't reflow. Rare enough that a blank box is fine.
        None => div().w(px(96.0)).h(px(96.0)).into_any_element(),
    }
}

/// Theme-coloured text helpers. Wrapped in a struct purely to keep
/// the call sites in `about_body` readable — the parent uses
/// `cx.theme()`, but we don't have a `Context` here, so each helper
/// closes over the renderer at draw time via `div().text_color(...)`
/// resolved against `cx.theme()` in the parent. Concretely: we use
/// the inline closure `|cx| ...` style accepted by gpui's text
/// builders. Here it's simpler — read theme inline from `cx` at
/// render time by capturing in `div().text_color(cx.theme().foreground)`.
struct WithTheme;
impl WithTheme {
    fn wordmark() -> impl IntoElement {
        div()
            .text_scale_xl()
            .font_weight(FontWeight::BOLD)
            .child("Feraille")
    }
    fn muted(s: impl Into<SharedString>) -> impl IntoElement {
        div().text_scale_xs().opacity(0.65).child(s.into())
    }
    fn tagline(s: impl Into<SharedString>) -> impl IntoElement {
        div().text_scale_sm().child(s.into())
    }
    fn copyright(s: impl Into<SharedString>) -> impl IntoElement {
        div().text_scale_xs().opacity(0.55).child(s.into())
    }
}

/// "Label  value" row. Label is muted (lowered opacity), value full-
/// strength. We avoid pulling `theme.muted_foreground` here because
/// the helper doesn't have a `&App`; opacity gets us the same visual
/// weight on both light and dark themes.
fn meta_row(label: &'static str, value: String) -> impl IntoElement {
    h_flex()
        .gap_2()
        .child(div().text_scale_xs().opacity(0.65).child(label))
        .child(div().text_scale_xs().child(value))
}

/// Website row. The value is clickable and routes through
/// `platform_shell::open_url` so the system browser owns it.
fn website_row(url: &'static str) -> impl IntoElement {
    h_flex()
        .gap_2()
        .child(div().text_scale_xs().opacity(0.65).child("Website"))
        .child(
            div()
                .id(ElementId::Name("about-website-link".into()))
                .cursor_pointer()
                .text_scale_xs()
                .underline()
                .child(url)
                .on_click(move |_: &ClickEvent, _window, _cx| {
                    let target = format!("https://{url}");
                    crate::platform_shell::open_url(&target);
                }),
        )
}

/// Decode the embedded PNG into a `RenderImage`. Returns `None` on
/// any decode failure so the caller can fall back to a blank box —
/// a missing About icon shouldn't crash the app.
///
/// Same channel swap as `preview::build_render_image`: gpui's
/// `RenderImage` wants BGRA on its wire even though the wrapping
/// type is named `RgbaImage`.
fn decode_icon(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut rgba = img.into_raw();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let buf = RgbaImage::from_raw(w, h, rgba)?;
    let frame = Frame::new(buf);
    Some(Arc::new(RenderImage::new(SmallVec::from_elem(frame, 1))))
}
