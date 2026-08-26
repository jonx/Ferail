//! "Create Checksum File…" dialog and background generation orchestration.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ferail_fs_native::verify::{GenerateFormat, generate_manifest};
use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement, Render,
    Styled, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonGroup, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    h_flex,
    input::{Input, InputState},
    notification::Notification,
    v_flex,
};

use crate::shell::Shell;
use crate::text::TextScale as _;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scope {
    Selection,
    CurrentFolder,
}

pub struct CreateChecksumView {
    root: PathBuf,
    selected: Vec<PathBuf>,
    name_input: Entity<InputState>,
    format: GenerateFormat,
    scope: Scope,
}

impl CreateChecksumView {
    fn new(
        root: PathBuf,
        selected: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let scope = if selected.is_empty() {
            Scope::CurrentFolder
        } else {
            Scope::Selection
        };
        Self {
            root,
            selected,
            name_input: cx
                .new(|cx| InputState::new(window, cx).default_value("checksums.sfv".to_string())),
            format: GenerateFormat::Sfv,
            scope,
        }
    }

    fn output(&self, cx: &App) -> Option<PathBuf> {
        let name = self.name_input.read(cx).value().trim().to_string();
        if name.is_empty() || Path::new(&name).file_name().is_none() || name.contains(['/', '\\']) {
            return None;
        }
        Some(self.root.join(name))
    }
}

impl Render for CreateChecksumView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut formats = ButtonGroup::new("checksum-format").small();
        formats = formats
            .child(
                Button::new("checksum-format-sfv")
                    .label(tr!("SFV (CRC32)"))
                    .selected(self.format == GenerateFormat::Sfv),
            )
            .child(
                Button::new("checksum-format-sha")
                    .label(tr!("SHA256SUMS"))
                    .selected(self.format == GenerateFormat::Sha256Sums),
            );
        let formats = formats.on_click(cx.listener(|this, selected: &Vec<usize>, window, cx| {
            let Some(index) = selected.first() else {
                return;
            };
            this.format = if *index == 0 {
                GenerateFormat::Sfv
            } else {
                GenerateFormat::Sha256Sums
            };
            let current = this.name_input.read(cx).value().to_string();
            if matches!(current.as_str(), "checksums.sfv" | "SHA256SUMS") {
                let value = if this.format == GenerateFormat::Sfv {
                    "checksums.sfv"
                } else {
                    "SHA256SUMS"
                };
                this.name_input.update(cx, |input, cx| {
                    input.set_value(value, window, cx);
                });
            }
            cx.notify();
        }));

        let mut scopes = ButtonGroup::new("checksum-scope").small();
        scopes = scopes
            .child(
                Button::new("checksum-scope-selection")
                    .label(tr!("Selection"))
                    .selected(self.scope == Scope::Selection)
                    .disabled(self.selected.is_empty()),
            )
            .child(
                Button::new("checksum-scope-folder")
                    .label(tr!("Current folder"))
                    .selected(self.scope == Scope::CurrentFolder),
            );
        let scopes = scopes.on_click(cx.listener(|this, selected: &Vec<usize>, _, cx| {
            let Some(index) = selected.first() else {
                return;
            };
            this.scope = if *index == 0 && !this.selected.is_empty() {
                Scope::Selection
            } else {
                Scope::CurrentFolder
            };
            cx.notify();
        }));

        let row = |label, child: gpui::AnyElement| {
            h_flex()
                .w_full()
                .gap_3()
                .items_center()
                .child(
                    div()
                        .w(px(96.))
                        .text_scale_sm()
                        .text_color(muted)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(child))
        };
        v_flex()
            .w_full()
            .gap_3()
            .py_2()
            .child(row(
                tr!("Name"),
                Input::new(&self.name_input).small().into_any_element(),
            ))
            .child(row(tr!("Format"), formats.into_any_element()))
            .child(row(tr!("Scope"), scopes.into_any_element()))
            .child(
                div().text_scale_xs().text_color(muted).child(tr!(
                    "Folders are scanned recursively without following links, entering packages, or downloading cloud placeholders."
                )),
            )
            .child(
                div().text_scale_xs().text_color(muted).child(tr!(
                    "CRC32 is a legacy integrity check. A matching checksum does not prove authenticity."
                )),
            )
    }
}

pub fn open_dialog(
    root: PathBuf,
    selected: Vec<PathBuf>,
    window: &mut Window,
    cx: &mut Context<Shell>,
) {
    let state = cx.new(|cx| CreateChecksumView::new(root, selected, window, cx));
    let shell = cx.entity();
    let state_for_dialog = state.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let state = state_for_dialog.clone();
        let shell = shell.clone();
        dialog
            .title(tr!("Create Checksum File"))
            .w(px(590.))
            .child(state.clone())
            .footer(
                DialogFooter::new()
                    .child(
                        div().w(px(96.)).child(
                            DialogClose::new().child(
                                Button::new("checksum-create-cancel")
                                    .label(tr!("Cancel"))
                                    .small(),
                            ),
                        ),
                    )
                    .child(
                        div().w(px(96.)).child(
                            DialogAction::new().child(
                                Button::new("checksum-create-ok")
                                    .label(tr!("Create"))
                                    .primary()
                                    .small(),
                            ),
                        ),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let plan = state.read(cx);
                let Some(output) = plan.output(cx) else {
                    return false;
                };
                let sources = if plan.scope == Scope::Selection {
                    plan.selected.clone()
                } else {
                    vec![plan.root.clone()]
                };
                let root = plan.root.clone();
                let format = plan.format;
                shell.update(cx, |shell, cx| {
                    shell.start_checksum_generation(root, sources, output, format, window, cx);
                });
                true
            })
    });
    window.on_next_frame(move |window, cx| {
        state
            .read(cx)
            .name_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
    });
}

impl Shell {
    pub(crate) fn start_checksum_generation(
        &mut self,
        root: PathBuf,
        sources: Vec<PathBuf>,
        output: PathBuf,
        format: GenerateFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cancel = Arc::new(AtomicBool::new(false));
        let task_id = self.process.tasks.borrow_mut().begin_with_cancel(
            crate::tasks::TaskKind::Checksum,
            tr!("Creating checksum file…"),
            cancel.clone(),
        );
        let process = self.process.clone();
        let win = window.window_handle();
        let (tx, rx) = async_channel::bounded(1);
        let worker_cancel = cancel.clone();
        let worker_root = root.clone();
        let worker_output = output.clone();
        cx.background_executor()
            .spawn(async move {
                let result = collect_files(&worker_root, &sources, &worker_output, &worker_cancel)
                    .and_then(|files| {
                        generate_manifest(
                            &worker_root,
                            &worker_output,
                            &files,
                            format,
                            &worker_cancel,
                            |progress| {
                                let fraction = if progress.file_bytes_total == 0 {
                                    0.0
                                } else {
                                    (progress.entry_index as f32
                                        + progress.file_bytes_done as f32
                                            / progress.file_bytes_total as f32)
                                        / progress.entry_count.max(1) as f32
                                };
                                let _ = tx.try_send(GenerateMessage::Progress(fraction));
                            },
                        )
                    });
                let _ = tx.send(GenerateMessage::Done(result)).await;
            })
            .detach();

        cx.spawn(async move |_shell, cx| {
            while let Ok(message) = rx.recv().await {
                match message {
                    GenerateMessage::Progress(fraction) => {
                        process.tasks.borrow_mut().update(task_id, fraction);
                    }
                    GenerateMessage::Done(Ok(report)) => {
                        process.tasks.borrow_mut().end(task_id);
                        if !report.cancelled {
                            Shell::broadcast_reload_for_process(
                                &process,
                                vec![output.parent().unwrap_or(&root).to_path_buf()],
                                cx,
                            );
                            let _ = win.update(cx, |_, window, cx| {
                                window.push_notification(
                                    Notification::success(trn!(
                                        "Created checksum file with {n} entry",
                                        "Created checksum file with {n} entries",
                                        report.entries_written as usize
                                    )),
                                    cx,
                                );
                            });
                        }
                        break;
                    }
                    GenerateMessage::Done(Err(error)) => {
                        let message =
                            tr!("Could not create checksum file: {detail}", detail = error);
                        process
                            .tasks
                            .borrow_mut()
                            .end_failed(task_id, message.clone());
                        let _ = win.update(cx, |_, window, cx| {
                            window.push_notification(
                                crate::shell::error_notification(message.to_string()),
                                cx,
                            );
                        });
                        break;
                    }
                }
            }
        })
        .detach();
    }
}

enum GenerateMessage {
    Progress(f32),
    Done(std::io::Result<ferail_fs_native::verify::GenerateReport>),
}

fn collect_files(
    root: &Path,
    sources: &[PathBuf],
    output: &Path,
    cancel: &AtomicBool,
) -> std::io::Result<Vec<PathBuf>> {
    let canonical_root = crate::shell::canonicalize_for_identity(root.to_path_buf());
    let root_metadata = std::fs::symlink_metadata(&canonical_root)?;
    let mut stack = sources.to_vec();
    let mut files = Vec::new();
    while let Some(path) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink()
            || !same_filesystem(&metadata, &root_metadata)
            || is_windows_reparse_point(&metadata)
        {
            continue;
        }
        if metadata.is_dir() {
            if path != canonical_root && is_package(&path) {
                continue;
            }
            for entry in std::fs::read_dir(&path)? {
                stack.push(entry?.path());
            }
            continue;
        }
        if !metadata.is_file() || path == output {
            continue;
        }
        if ferail_fs_native::is_cloud_placeholder(&path) {
            continue;
        }
        let canonical = crate::shell::canonicalize_for_identity(path.clone());
        let relative = canonical.strip_prefix(&canonical_root).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "selected file is outside the checksum root",
            )
        })?;
        files.push(relative.to_path_buf());
    }
    files.sort();
    files.dedup();
    Ok(files)
}

#[cfg(unix)]
fn same_filesystem(metadata: &std::fs::Metadata, root_metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    metadata.dev() == root_metadata.dev()
}

#[cfg(not(unix))]
fn same_filesystem(_: &std::fs::Metadata, _: &std::fs::Metadata) -> bool {
    true
}

#[cfg(windows)]
fn is_windows_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_windows_reparse_point(_: &std::fs::Metadata) -> bool {
    false
}

fn is_package(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "app" | "bundle" | "framework" | "pkg" | "plugin"
            )
        })
}
