//! Filter-syntax cheat sheet — the (?) button beside the filter field.
//!
//! A stopgap until the filter box grows chips / a richer query UI:
//! a gpui-component `Dialog` listing every structured token the
//! filter understands, built from `ferail_core::filter_expr::TOKEN_HELP`
//! (the same table the parser's tests round-trip and the autocomplete
//! menu reads), plus the handful of grammar rules that don't fit a
//! token row. Static content, no I/O, no state — safe to open from
//! any click handler.

use crate::text::TextScale as _;
use ferail_core::filter_expr::TOKEN_HELP;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, WindowExt as _,
    dialog::{Dialog, DialogButtonProps},
    h_flex, v_flex,
};

/// Open the cheat sheet as a modal in `window`. Esc, the close button,
/// and an overlay click dismiss it.
pub fn open_filter_help_dialog(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, _window, cx| build_dialog(dialog, cx));
}

fn build_dialog(dialog: Dialog, cx: &App) -> Dialog {
    dialog
        .title("Filter syntax")
        .w(px(560.0))
        .overlay_closable(true)
        .keyboard(true)
        .close_button(true)
        .button_props(DialogButtonProps::default().show_cancel(false))
        .child(body(cx))
}

fn body(cx: &App) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;

    let mut rows = v_flex().gap_1p5();
    for help in TOKEN_HELP {
        let examples = if help.values.is_empty() {
            String::new()
        } else {
            help.values
                .iter()
                .map(|v| format!("{}{v}", help.key))
                .collect::<Vec<_>>()
                .join("   ")
        };
        rows = rows.child(token_row(help.key, help.detail, examples, muted));
    }
    rows = rows.child(token_row(
        "\"…\"",
        "quoted phrase — match the words together, spaces included",
        "\"final report\"".to_string(),
        muted,
    ));

    v_flex()
        .gap_3()
        .py_1()
        .child(
            div()
                .text_scale_sm()
                .child("Type words to match names and formats. Add tokens to filter by value. Everything you type must match (AND)."),
        )
        .child(rows)
        .child(div().h(px(1.0)).bg(border))
        .child(
            v_flex()
                .gap_1()
                .text_scale_xs()
                .text_color(muted)
                .child("Dates: YYYY-MM-DD, with >, >=, <, <= or a range a..b — or today, yesterday, week, month, year.")
                .child("Sizes: b, kb, mb, gb, tb (1024-based, like the Size column); >, <, >=, <= or a range a..b.")
                .child("Anything unrecognised is matched as plain text. Press \u{23CE} to run the same query as a search of subfolders."),
        )
}

fn token_row(
    key: &'static str,
    detail: &'static str,
    examples: String,
    muted: Hsla,
) -> impl IntoElement {
    h_flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .flex_shrink_0()
                .w(px(72.0))
                .text_scale_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(key),
        )
        .child(
            v_flex()
                // flex_1 + min_w(0) so long detail/example lines wrap
                // inside the dialog instead of overflowing its edge.
                .flex_1()
                .min_w(px(0.0))
                .gap_0p5()
                .child(div().text_scale_sm().child(detail))
                .when(!examples.is_empty(), |this| {
                    this.child(div().text_scale_xs().text_color(muted).child(examples))
                }),
        )
}
