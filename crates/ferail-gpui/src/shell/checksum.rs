//! SHA-256 generation and optional clipboard comparison.
//!
//! The file is streamed on the background executor. Only the final digest and
//! byte counters cross back into GPUI; paths and file contents are never
//! persisted. The expected digest is copied into dialog-local state, so
//! clearing it never mutates the system clipboard.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use gpui::{
    App, AppContext as _, ClipboardItem, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::Button,
    dialog::{DialogClose, DialogFooter},
    h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    progress::Progress,
    v_flex,
};
use sha2::{Digest, Sha256};

use ferail_core::EntryKind;

use super::{GenerateSha256, Shell};
use crate::tasks::TaskKind;
use crate::text::TextScale as _;

const READ_BUFFER_SIZE: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
enum ParsedChecksum {
    Empty,
    Valid(String),
    Invalid,
}

/// Extract exactly one maximal 64-character hexadecimal run.
///
/// This accepts the common clipboard shapes `hash`, `hash  filename`, and
/// `SHA256(filename) = hash`. Trimming the whole input first is deliberate:
/// browser/download-page copies often carry a leading space or trailing
/// newline. More than one candidate is ambiguous and therefore rejected.
fn parse_sha256(input: &str) -> ParsedChecksum {
    let input = input.trim();
    if input.is_empty() {
        return ParsedChecksum::Empty;
    }

    let bytes = input.as_bytes();
    let mut candidates = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_hexdigit() {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
            end += 1;
        }
        if end - start == 64 {
            candidates.push(input[start..end].to_ascii_lowercase());
        }
        start = end;
    }

    match candidates.as_slice() {
        [checksum] => ParsedChecksum::Valid(checksum.clone()),
        _ => ParsedChecksum::Invalid,
    }
}

fn hash_reader<R: Read>(
    mut reader: R,
    total: u64,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64, u64),
) -> io::Result<Option<String>> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; READ_BUFFER_SIZE];
    let mut done = 0_u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        done = done.saturating_add(read as u64);
        on_progress(done, total);
    }
    Ok(Some(format!("{:x}", hasher.finalize())))
}

fn hash_file(
    path: &Path,
    cancel: &AtomicBool,
    on_progress: impl FnMut(u64, u64),
) -> io::Result<Option<String>> {
    let file = File::open(path)?;
    let total = file.metadata()?.len();
    hash_reader(file, total, cancel, on_progress)
}

#[derive(Clone, Debug)]
enum HashPhase {
    Computing { done: u64, total: u64 },
    Ready(String),
    Failed(SharedString),
    Cancelled,
}

enum HashMsg {
    Progress { done: u64, total: u64 },
    Done(io::Result<Option<String>>),
}

struct ChecksumView {
    file_name: SharedString,
    expected: Entity<InputState>,
    phase: HashPhase,
    _expected_subscription: Subscription,
}

impl ChecksumView {
    fn new(
        file_name: SharedString,
        expected: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let expected = cx.new(|cx| {
            let input = InputState::new(window, cx)
                .placeholder(tr!("Paste an expected SHA-256 (optional)"));
            match expected {
                Some(value) => input.default_value(value),
                None => input,
            }
        });
        let subscription = cx.subscribe(
            &expected,
            |_this: &mut Self, _input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        );
        Self {
            file_name,
            expected,
            phase: HashPhase::Computing { done: 0, total: 0 },
            _expected_subscription: subscription,
        }
    }

    fn comparison(&self, cx: &App) -> ParsedChecksum {
        parse_sha256(self.expected.read(cx).value().as_ref())
    }

    fn comparison_banner(&self, cx: &App) -> impl IntoElement {
        let parsed = self.comparison(cx);
        let (message, color) = match (&self.phase, parsed) {
            (_, ParsedChecksum::Empty) => (
                tr!("Paste an expected SHA-256 to compare."),
                cx.theme().muted_foreground,
            ),
            (_, ParsedChecksum::Invalid) => (
                tr!("Enter exactly one 64-character hexadecimal SHA-256 checksum."),
                cx.theme().warning,
            ),
            (HashPhase::Ready(actual), ParsedChecksum::Valid(expected)) if *actual == expected => (
                tr!("MATCH — the file checksum is identical."),
                cx.theme().success,
            ),
            (HashPhase::Ready(_), ParsedChecksum::Valid(_)) => (
                tr!("DOES NOT MATCH — the checksums are different."),
                cx.theme().danger,
            ),
            (HashPhase::Computing { .. }, ParsedChecksum::Valid(_)) => (
                tr!("The comparison will appear when calculation finishes."),
                cx.theme().muted_foreground,
            ),
            _ => (
                tr!("The comparison is unavailable because calculation did not finish."),
                cx.theme().muted_foreground,
            ),
        };
        div()
            .w_full()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .bg(color.opacity(0.10))
            .border_1()
            .border_color(color.opacity(0.45))
            .text_scale_sm()
            .text_color(color)
            .child(message)
    }
}

impl Render for ChecksumView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let expected_is_empty = self.expected.read(cx).value().trim().is_empty();

        let generated =
            match &self.phase {
                HashPhase::Computing { done, total } => {
                    let fraction = if *total == 0 {
                        0.0
                    } else {
                        (*done as f32 / *total as f32).clamp(0.0, 1.0)
                    };
                    v_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .child(tr!("Calculating…"))
                                .child(div().text_scale_xs().text_color(muted).child(
                                    if *total == 0 {
                                        tr!("Reading file…")
                                    } else {
                                        format!("{:.0}%", fraction * 100.0).into()
                                    },
                                )),
                        )
                        .child(
                            Progress::new("sha256-progress")
                                .small()
                                .loading(*total == 0)
                                .value(fraction * 100.0),
                        )
                        .into_any_element()
                }
                HashPhase::Ready(hash) => {
                    let copy = hash.clone();
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .px_3()
                                .py_2()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().secondary.opacity(0.5))
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_scale_xs()
                                .whitespace_nowrap()
                                .child(hash.clone()),
                        )
                        .child(
                            Button::new("copy-sha256")
                                .label(tr!("Copy"))
                                .small()
                                .on_click(move |_, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                    window.push_notification(
                                        Notification::success(tr!("SHA-256 copied")),
                                        cx,
                                    );
                                }),
                        )
                        .into_any_element()
                }
                HashPhase::Failed(message) => div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().danger.opacity(0.08))
                    .text_scale_sm()
                    .text_color(cx.theme().danger)
                    .child(message.clone())
                    .into_any_element(),
                HashPhase::Cancelled => div()
                    .text_scale_sm()
                    .text_color(muted)
                    .child(tr!("Calculation cancelled."))
                    .into_any_element(),
            };

        let expected = self.expected.clone();
        let view = cx.entity();
        v_flex()
            .w_full()
            .gap_4()
            .py_2()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_scale_xs()
                            .text_color(muted)
                            .child(tr!("File")),
                    )
                    .child(div().text_scale_sm().child(self.file_name.clone())),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_scale_sm().child(tr!("Generated SHA-256")))
                    .child(generated),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_scale_sm().child(tr!("Expected SHA-256")))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&self.expected).small()),
                            )
                            .child(
                                Button::new("clear-expected-sha256")
                                    .label(tr!("Clear"))
                                    .outline()
                                    .small()
                                    .disabled(expected_is_empty)
                                    .on_click(move |_, window, cx| {
                                        expected.update(cx, |input, cx| {
                                            input.set_value(String::new(), window, cx);
                                        });
                                        view.update(cx, |_view, cx| cx.notify());
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .text_scale_xs()
                            .text_color(muted)
                            .child(tr!(
                                "A checksum found in the clipboard is filled in automatically. Clear only removes it from this dialog."
                            )),
                    ),
            )
            .child(self.comparison_banner(cx))
    }
}

impl Shell {
    pub(crate) fn on_generate_sha256(
        &mut self,
        _: &GenerateSha256,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::trail::command("Generate SHA-256");
        let targets = self.action_entries_visible_order(cx);
        if targets.len() != 1 {
            window.push_notification(
                Notification::info(tr!("Select one file to generate its SHA-256.")),
                cx,
            );
            return;
        }
        let Some((_, entry, path)) = targets.into_iter().next() else {
            window.push_notification(
                Notification::info(tr!("Select one file to generate its SHA-256.")),
                cx,
            );
            return;
        };
        if matches!(entry.kind, EntryKind::Directory) {
            window.push_notification(
                Notification::info(tr!("Select one file to generate its SHA-256.")),
                cx,
            );
            return;
        }

        let clipboard_checksum = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .and_then(|text| match parse_sha256(&text) {
                ParsedChecksum::Valid(value) => Some(value),
                ParsedChecksum::Empty | ParsedChecksum::Invalid => None,
            });
        let cancel = Arc::new(AtomicBool::new(false));
        let state = cx.new(|cx| {
            ChecksumView::new(
                entry.display_name.clone().into(),
                clipboard_checksum,
                window,
                cx,
            )
        });

        let state_for_dialog = state.clone();
        let cancel_for_dialog = cancel.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title(tr!("SHA-256 checksum"))
                .w(px(680.))
                .child(state_for_dialog.clone())
                .footer(
                    DialogFooter::new().child(
                        div().w(px(96.)).child(
                            DialogClose::new()
                                .child(Button::new("sha256-close").label(tr!("Close")).small()),
                        ),
                    ),
                )
                .on_cancel({
                    let cancel = cancel_for_dialog.clone();
                    move |_, _, _| {
                        cancel.store(true, Ordering::Relaxed);
                        true
                    }
                })
        });

        self.start_sha256_worker(path, state, cancel, cx);
    }

    fn start_sha256_worker(
        &mut self,
        path: PathBuf,
        state: Entity<ChecksumView>,
        cancel: Arc<AtomicBool>,
        cx: &mut Context<Self>,
    ) {
        let task_id = self.process.tasks.borrow_mut().begin_with_cancel(
            TaskKind::Checksum,
            tr!("Calculating SHA-256…"),
            cancel.clone(),
        );
        // One queued progress sample is enough. A bounded channel keeps a
        // very large file from accumulating one message per MiB if rendering
        // falls behind the disk; the final result waits for that slot.
        let (tx, rx) = async_channel::bounded(1);
        let worker_cancel = cancel.clone();
        cx.background_executor()
            .spawn(async move {
                let result = hash_file(&path, &worker_cancel, |done, total| {
                    let _ = tx.try_send(HashMsg::Progress { done, total });
                });
                let _ = tx.send(HashMsg::Done(result)).await;
            })
            .detach();

        cx.spawn(async move |shell, cx| {
            while let Ok(message) = rx.recv().await {
                let finished = matches!(message, HashMsg::Done(_));
                let _ = shell.update(cx, |shell, cx| {
                    match message {
                        HashMsg::Progress { done, total } => {
                            let fraction = if total == 0 {
                                0.0
                            } else {
                                done as f32 / total as f32
                            };
                            shell.process.tasks.borrow_mut().update(task_id, fraction);
                            state.update(cx, |state, cx| {
                                state.phase = HashPhase::Computing { done, total };
                                cx.notify();
                            });
                        }
                        HashMsg::Done(Ok(Some(hash))) => {
                            shell.process.tasks.borrow_mut().end(task_id);
                            state.update(cx, |state, cx| {
                                state.phase = HashPhase::Ready(hash);
                                cx.notify();
                            });
                        }
                        HashMsg::Done(Ok(None)) => {
                            shell.process.tasks.borrow_mut().end(task_id);
                            state.update(cx, |state, cx| {
                                state.phase = HashPhase::Cancelled;
                                cx.notify();
                            });
                        }
                        HashMsg::Done(Err(error)) => {
                            let message =
                                tr!("Could not calculate SHA-256: {detail}", detail = error);
                            shell
                                .process
                                .tasks
                                .borrow_mut()
                                .end_failed(task_id, message.clone());
                            state.update(cx, |state, cx| {
                                state.phase = HashPhase::Failed(message);
                                cx.notify();
                            });
                        }
                    }
                    cx.notify();
                });
                if finished {
                    break;
                }
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::AtomicBool;

    use super::{ParsedChecksum, hash_reader, parse_sha256};

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn clipboard_checksum_trims_and_normalizes() {
        assert_eq!(
            parse_sha256(&format!("  \n{}\t ", HASH.to_ascii_uppercase())),
            ParsedChecksum::Valid(HASH.to_string())
        );
    }

    #[test]
    fn clipboard_checksum_accepts_common_formats() {
        assert_eq!(
            parse_sha256(&format!("{HASH}  ferail.dmg")),
            ParsedChecksum::Valid(HASH.to_string())
        );
        assert_eq!(
            parse_sha256(&format!("SHA256(ferail.dmg) = {HASH}")),
            ParsedChecksum::Valid(HASH.to_string())
        );
        assert_eq!(
            parse_sha256(&format!("sha256:{HASH}")),
            ParsedChecksum::Valid(HASH.to_string())
        );
    }

    #[test]
    fn clipboard_checksum_rejects_wrong_length_or_ambiguity() {
        assert_eq!(parse_sha256("   \n"), ParsedChecksum::Empty);
        assert_eq!(parse_sha256(&HASH[..63]), ParsedChecksum::Invalid);
        assert_eq!(parse_sha256(&format!("{HASH}0")), ParsedChecksum::Invalid);
        assert_eq!(
            parse_sha256(&format!("{HASH} {HASH}")),
            ParsedChecksum::Invalid
        );
    }

    #[test]
    fn streaming_sha256_matches_known_digest() {
        let result = hash_reader(Cursor::new(b"abc"), 3, &AtomicBool::new(false), |_, _| {})
            .expect("hashing should succeed");
        assert_eq!(
            result.as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn streaming_sha256_honors_cancellation() {
        let cancel = AtomicBool::new(true);
        assert_eq!(
            hash_reader(Cursor::new(b"abc"), 3, &cancel, |_, _| {})
                .expect("cancellation is not an I/O error"),
            None
        );
    }
}
