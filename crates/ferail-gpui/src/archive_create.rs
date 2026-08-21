//! "New Archive…" dialog — create an archive from the current selection with
//! the options that suit most people: format, compression level, and an
//! optional password.
//!
//! Deliberately a small, fixed option set rather than every knob each codec
//! exposes. Which options are even *offered* comes from the format's row in
//! `ferail_archive::Capabilities`, so a format that cannot carry a password
//! (tar-family) shows the field disabled instead of silently ignoring it.
//!
//! The dialog only collects intent; the work runs through
//! `Shell::spawn_archive_op` like every other archive operation (worker
//! thread, progress bar, cancel button, undo).

use std::path::PathBuf;

use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonGroup, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    h_flex,
    input::{Input, InputState},
    v_flex,
};

use ferail_archive::{CompressionLevel, Format};

use crate::shell::Shell;
use crate::text::TextScale as _;

/// Formats offered in the picker, in menu order.
const FORMATS: &[Format] = &[
    Format::Zip,
    Format::SevenZ,
    Format::TarGz,
    Format::TarBz2,
    Format::TarXz,
    Format::Tar,
];

const LEVELS: &[CompressionLevel] = &[
    CompressionLevel::Store,
    CompressionLevel::Fast,
    CompressionLevel::Normal,
    CompressionLevel::Maximum,
];

pub struct NewArchiveView {
    /// Items to put in the archive (the resolved selection).
    sources: Vec<PathBuf>,
    name_input: Entity<InputState>,
    password_input: Entity<InputState>,
    format: Format,
    level: CompressionLevel,
}

impl NewArchiveView {
    fn new(sources: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Default name mirrors the one-click Compress convention: the item's
        // own name for a single source, "Archive" for several.
        let default_name = if sources.len() == 1 {
            sources[0]
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Archive".to_string())
        } else {
            "Archive".to_string()
        };
        let name_input = cx.new(|cx| InputState::new(window, cx).default_value(default_name));
        let password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(tr!("Optional"))
        });
        Self {
            sources,
            name_input,
            password_input,
            format: Format::Zip,
            level: CompressionLevel::Normal,
        }
    }

    /// The archive path this dialog would create: `<parent>/<name>.<ext>`,
    /// deduped with a `" 2"` suffix if taken. `None` when the name is empty or
    /// the sources have no parent.
    fn output_path(&self, cx: &App) -> Option<PathBuf> {
        let name = self.name_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            return None;
        }
        let parent = self.sources.first()?.parent()?.to_path_buf();
        let ext = self.format.canonical_extension();
        let mut candidate = parent.join(format!("{name}.{ext}"));
        let mut n = 2;
        while candidate.exists() {
            candidate = parent.join(format!("{name} {n}.{ext}"));
            n += 1;
        }
        Some(candidate)
    }

    fn password(&self, cx: &App) -> Option<String> {
        if !self.format.capabilities().supports_password {
            return None;
        }
        let pw = self.password_input.read(cx).value().to_string();
        (!pw.is_empty()).then_some(pw)
    }

    fn row(label: SharedString, control: impl IntoElement, cx: &App) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_3()
            .items_center()
            .child(
                div()
                    .w(px(96.))
                    .flex_none()
                    .text_scale_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(control))
    }
}

impl Render for NewArchiveView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let caps = self.format.capabilities();
        let ext: SharedString = format!(".{}", self.format.canonical_extension()).into();
        let muted = cx.theme().muted_foreground;

        // Format picker.
        let mut formats = ButtonGroup::new("new-archive-format").small();
        for (i, f) in FORMATS.iter().enumerate() {
            formats = formats.child(
                Button::new(("new-archive-format", i))
                    .label(f.label())
                    .selected(*f == self.format),
            );
        }
        let formats = formats.on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
            if let Some(f) = clicks.first().and_then(|i| FORMATS.get(*i)) {
                this.format = *f;
                cx.notify();
            }
        }));

        // Compression level.
        let mut levels = ButtonGroup::new("new-archive-level").small();
        for (i, l) in LEVELS.iter().enumerate() {
            levels = levels.child(
                Button::new(("new-archive-level", i))
                    .label(match l {
                        CompressionLevel::Store => tr!("Store"),
                        CompressionLevel::Fast => tr!("Fast"),
                        CompressionLevel::Normal => tr!("Normal"),
                        CompressionLevel::Maximum => tr!("Maximum"),
                    })
                    .selected(*l == self.level),
            );
        }
        let levels = levels
            .disabled(!caps.supports_levels)
            .on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
                if let Some(l) = clicks.first().and_then(|i| LEVELS.get(*i)) {
                    this.level = *l;
                    cx.notify();
                }
            }));

        let count = self.sources.len();

        v_flex()
            .w_full()
            .gap_3()
            .py_2()
            .child(Self::row(
                tr!("Name"),
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.name_input).small()))
                    .child(div().text_scale_sm().text_color(muted).child(ext)),
                cx,
            ))
            .child(Self::row(tr!("Format"), formats, cx))
            .child(Self::row(tr!("Compression"), levels, cx))
            // A format that can't carry a password gets an explanation rather
            // than a dead-but-typable field.
            .child(Self::row(
                tr!("Password"),
                if caps.supports_password {
                    div()
                        .child(Input::new(&self.password_input).small())
                        .into_any_element()
                } else {
                    div()
                        .text_scale_sm()
                        .text_color(muted)
                        .child(tr!(
                            "Not supported by {format} archives.",
                            format = self.format.label()
                        ))
                        .into_any_element()
                },
                cx,
            ))
            .child(
                div()
                    .text_scale_xs()
                    .text_color(muted)
                    .child(trn!(
                        "{n} item will be added.",
                        "{n} items will be added.",
                        count
                    )),
            )
    }
}

/// Open the New Archive dialog over `sources`.
pub fn open_dialog(
    sources: Vec<PathBuf>,
    window: &mut Window,
    cx: &mut Context<Shell>,
) {
    if sources.is_empty() {
        return;
    }
    let state = cx.new(|cx| NewArchiveView::new(sources, window, cx));
    let shell_entity = cx.entity();
    let state_for_dialog = state.clone();
    window.open_dialog(cx, move |dialog, _window, _cx| {
        let state = state_for_dialog.clone();
        let shell = shell_entity.clone();
        dialog
            .title(tr!("New Archive"))
            .w(px(560.))
            .child(state.clone())
            .footer(
                DialogFooter::new()
                    .child(
                        div().w(px(96.)).child(
                            DialogClose::new()
                                .child(Button::new("new-archive-cancel").label(tr!("Cancel")).small()),
                        ),
                    )
                    .child(
                        div().w(px(96.)).child(
                            DialogAction::new().child(
                                Button::new("new-archive-ok")
                                    .label(tr!("Create"))
                                    .primary()
                                    .small(),
                            ),
                        ),
                    ),
            )
            .on_ok(move |_, window, cx: &mut App| {
                let plan = state.read(cx);
                let Some(output) = plan.output_path(cx) else {
                    // Empty name — keep the dialog open so it can be fixed.
                    return false;
                };
                let sources = plan.sources.clone();
                let format = plan.format;
                let level = plan.level;
                let password = plan.password(cx);
                shell.update(cx, |shell, cx| {
                    shell.create_archive_from(sources, output, format, level, password, window, cx);
                });
                true
            })
    });
    // Focus the name field once the dialog has mounted (same next-frame trick
    // as the rename prompt).
    window.on_next_frame(move |window, cx| {
        let input = state.read(cx).name_input.clone();
        input.read(cx).focus_handle(cx).focus(window, cx);
    });
}
