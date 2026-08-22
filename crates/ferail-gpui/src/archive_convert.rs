//! Capability-driven Convert Archive dialog.
//!
//! The view collects intent only. Source probing, guarded extraction, archive
//! creation, validation, collision handling, and cleanup all happen through
//! `ferail_fs_native::convert_archive` on the background executor.

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Entity, Focusable as _, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, px,
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

const LEVELS: &[CompressionLevel] = &[
    CompressionLevel::Store,
    CompressionLevel::Fast,
    CompressionLevel::Normal,
    CompressionLevel::Maximum,
];

pub(crate) struct ArchiveConversionRequest {
    pub source: PathBuf,
    pub output_stem: String,
    pub target: Format,
    pub level: CompressionLevel,
    pub input_password: Option<String>,
    pub output_password: Option<String>,
}

struct ConvertArchiveView {
    source: PathBuf,
    source_format: Option<Format>,
    name_input: Entity<InputState>,
    input_password: Entity<InputState>,
    output_password: Entity<InputState>,
    target: Format,
    level: CompressionLevel,
}

impl ConvertArchiveView {
    fn new(
        source: PathBuf,
        source_format: Option<Format>,
        known_password: Option<String>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let name = archive_stem(&source, source_format);
        let name_input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let input_password = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .masked(true)
                .placeholder(tr!("Only needed for an encrypted source"));
            if let Some(password) = known_password {
                input = input.default_value(password);
            }
            input
        });
        let output_password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(tr!("Optional"))
        });
        Self {
            source,
            source_format,
            name_input,
            input_password,
            output_password,
            target: Format::Zip,
            level: CompressionLevel::Normal,
        }
    }

    fn output_stem(&self, cx: &App) -> Option<String> {
        let name = self.name_input.read(cx).value().trim().to_string();
        (!name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains('/')
            && !name.contains('\\'))
        .then_some(name)
    }

    fn password(input: &Entity<InputState>, cx: &App) -> Option<String> {
        let value = input.read(cx).value().to_string();
        (!value.is_empty()).then_some(value)
    }

    fn row(label: SharedString, control: impl IntoElement, cx: &App) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_3()
            .items_center()
            .child(
                div()
                    .w(px(112.))
                    .flex_none()
                    .text_scale_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(control))
    }

    fn is_package(&self) -> bool {
        self.source_format == Some(Format::Zip)
            && Format::from_path(&self.source.to_string_lossy()) != Some(Format::Zip)
    }
}

impl Render for ConvertArchiveView {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let caps = self.target.capabilities();
        let muted = cx.theme().muted_foreground;
        let ext: SharedString = format!(".{}", self.target.canonical_extension()).into();
        let source_name = self
            .source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let source_format: SharedString = self
            .source_format
            .map(|format| SharedString::new_static(format.label()))
            .unwrap_or_else(|| tr!("Detected when conversion starts"));

        let mut formats = ButtonGroup::new("convert-archive-format").small();
        for (index, format) in Format::creatable_multi_file().iter().enumerate() {
            formats = formats.child(
                Button::new(("convert-archive-format", index))
                    .label(format.label())
                    .selected(*format == self.target),
            );
        }
        let formats = formats.on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
            if let Some(format) = clicks
                .first()
                .and_then(|index| Format::creatable_multi_file().get(*index))
            {
                this.target = *format;
                cx.notify();
            }
        }));

        let mut levels = ButtonGroup::new("convert-archive-level").small();
        for (index, level) in LEVELS.iter().enumerate() {
            levels = levels.child(
                Button::new(("convert-archive-level", index))
                    .label(match level {
                        CompressionLevel::Store => tr!("Store"),
                        CompressionLevel::Fast => tr!("Fast"),
                        CompressionLevel::Normal => tr!("Normal"),
                        CompressionLevel::Maximum => tr!("Maximum"),
                    })
                    .selected(*level == self.level),
            );
        }
        let levels = levels.disabled(!caps.supports_levels).on_click(cx.listener(
            |this, clicks: &Vec<usize>, _window, cx| {
                if let Some(level) = clicks.first().and_then(|index| LEVELS.get(*index)) {
                    this.level = *level;
                    cx.notify();
                }
            },
        ));

        v_flex()
            .w_full()
            .gap_3()
            .py_2()
            .child(Self::row(
                tr!("Source"),
                h_flex()
                    .gap_2()
                    .child(div().text_scale_sm().child(source_name))
                    .child(div().text_scale_xs().text_color(muted).child(source_format)),
                cx,
            ))
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
            .child(Self::row(
                tr!("Source password"),
                Input::new(&self.input_password).small(),
                cx,
            ))
            .child(Self::row(
                tr!("New password"),
                if caps.supports_create_password {
                    div()
                        .child(Input::new(&self.output_password).small())
                        .into_any_element()
                } else {
                    div()
                        .text_scale_sm()
                        .text_color(muted)
                        .child(tr!(
                            "Not supported by {format} archives.",
                            format = self.target.label()
                        ))
                        .into_any_element()
                },
                cx,
            ))
            .child(
                div()
                    .text_scale_xs()
                    .text_color(muted)
                    .child(tr!(
                        "The source stays unchanged. The result uses a new unused filename."
                    )),
            )
            .child(
                div()
                    .text_scale_xs()
                    .text_color(muted)
                    .child(tr!(
                        "Some archive-specific metadata may not survive conversion."
                    )),
            )
            .when(self.output_stem(cx).is_none(), |this| {
                this.child(
                    div()
                        .text_scale_xs()
                        .text_color(cx.theme().danger)
                        .child(tr!("The name must be a single non-empty filename.")),
                )
            })
            .when(self.is_package(), |this| {
                this.child(
                    div()
                        .text_scale_xs()
                        .text_color(cx.theme().danger)
                        .child(tr!(
                            "This is a structured ZIP package. The converted copy is a plain archive and may no longer work as an app or document."
                        )),
                )
            })
    }
}

pub fn open_dialog(
    source: PathBuf,
    source_format: Option<Format>,
    known_password: Option<String>,
    shell: Entity<Shell>,
    window: &mut Window,
    cx: &mut App,
) {
    let state =
        cx.new(|cx| ConvertArchiveView::new(source, source_format, known_password, window, cx));
    let state_for_dialog = state.clone();
    window.open_dialog(cx, move |dialog, _window, _cx| {
        let state = state_for_dialog.clone();
        let shell = shell.clone();
        dialog
            .title(tr!("Convert Archive"))
            .w(px(650.))
            .child(state.clone())
            .footer(
                DialogFooter::new()
                    .child(
                        div().w(px(104.)).child(
                            DialogClose::new().child(
                                Button::new("convert-archive-cancel")
                                    .label(tr!("Cancel"))
                                    .small(),
                            ),
                        ),
                    )
                    .child(
                        div().w(px(104.)).child(
                            DialogAction::new().child(
                                Button::new("convert-archive-ok")
                                    .label(tr!("Convert"))
                                    .primary()
                                    .small(),
                            ),
                        ),
                    ),
            )
            .on_ok(move |_, window, cx: &mut App| {
                let view = state.read(cx);
                let Some(output_stem) = view.output_stem(cx) else {
                    return false;
                };
                let request = ArchiveConversionRequest {
                    source: view.source.clone(),
                    output_stem,
                    target: view.target,
                    level: view.level,
                    input_password: ConvertArchiveView::password(&view.input_password, cx),
                    output_password: view
                        .target
                        .capabilities()
                        .supports_create_password
                        .then(|| ConvertArchiveView::password(&view.output_password, cx))
                        .flatten(),
                };
                shell.update(cx, |shell, cx| {
                    shell.convert_archive_from(request, window, cx);
                });
                true
            })
    });
    window.on_next_frame(move |window, cx| {
        let input = state.read(cx).name_input.clone();
        input.read(cx).focus_handle(cx).focus(window, cx);
    });
}

fn archive_stem(source: &std::path::Path, source_format: Option<Format>) -> String {
    let leaf = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Archive".into());
    let format = source_format.or_else(|| Format::from_path(&leaf));
    if let Some(format) = format {
        let suffix = format!(".{}", format.canonical_extension());
        if leaf.to_ascii_lowercase().ends_with(&suffix) {
            return leaf[..leaf.len() - suffix.len()].to_string();
        }
    }
    source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "Archive".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_stem_removes_the_complete_archive_suffix() {
        assert_eq!(
            archive_stem(std::path::Path::new("photos.tar.gz"), Some(Format::TarGz)),
            "photos"
        );
        assert_eq!(
            archive_stem(std::path::Path::new("report.docx"), Some(Format::Zip)),
            "report"
        );
    }
}
