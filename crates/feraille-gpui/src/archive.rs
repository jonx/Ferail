//! Archive workbench — an embedded tool-result view (peer to Disk Usage and
//! the Duplicate panel) for browsing an archive's contents and extracting from
//! it. Opened via the "Open as Archive" context action; docked into the tab's
//! `tool_result` seam (docs/features/TOOL_RESULTS.md) and closed the same way
//! as the other tool results.
//!
//! Phase B foundation: browse (read-only) + Extract All / Extract Selected.
//! The table of contents is read **off the UI thread** (Prime Directive) via
//! `feraille_fs_native::read_archive_toc`; render only reads the cached result.
//! Editing (add/remove entries), the create dialog, password entry, and
//! drag-in land in later slices.

use std::collections::BTreeSet;
use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    button::Button,
    h_flex,
    input::{Input, InputState},
    v_flex, ActiveTheme, Disableable, Sizable,
};

use feraille_archive::{ArchiveEntry, Capabilities, Format, Toc};
use feraille_fs_native::ArchiveError;

use crate::shell::Shell;
use crate::text::{IconScale as _, TextScale as _};

/// Key context for the archive pane (keymap bindings hang off this).
pub const ARCHIVE_CONTEXT: &str = "Archive";

/// The off-thread load state of the archive's table of contents.
enum ArchiveLoad {
    Loading,
    Loaded(Toc),
    /// The archive's *listing itself* is encrypted (7z header encryption), so
    /// nothing can be shown until a password is supplied. `error` is set after
    /// a rejected attempt.
    NeedsPassword { error: Option<String> },
    Failed(String),
}

pub struct ArchiveView {
    archive_path: PathBuf,
    format: Format,
    caps: Capabilities,
    load: ArchiveLoad,
    /// Indices into `Toc::entries` the user has selected for cherry-pick
    /// extraction.
    selected: BTreeSet<usize>,
    /// Weak handle to the owning shell — used to run extraction through
    /// `Shell::spawn_archive_op` (tasks / progress / cancel / toast / undo /
    /// reload) rather than duplicating that machinery here.
    shell: Option<WeakEntity<Shell>>,
    /// Password entry for encrypted archives, created eagerly so `render`
    /// never has to build state.
    password_input: Entity<InputState>,
    /// The password the user supplied, once accepted. Threaded into every
    /// extraction from this view.
    password: Option<String>,
    focus_handle: FocusHandle,
}

impl ArchiveView {
    pub fn new(
        archive_path: PathBuf,
        format: Format,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let password_input =
            cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Password"));
        let mut view = Self {
            archive_path,
            format,
            caps: format.capabilities(),
            load: ArchiveLoad::Loading,
            selected: BTreeSet::new(),
            shell: None,
            password_input,
            password: None,
            focus_handle: cx.focus_handle(),
        };
        view.start_load(None, cx);
        view
    }

    pub fn set_shell(&mut self, shell: WeakEntity<Shell>) {
        self.shell = Some(shell);
    }

    /// Schedule the TOC read on the background executor and apply the result
    /// through an entity update (the only place view state mutates).
    fn start_load(&mut self, password: Option<String>, cx: &mut Context<Self>) {
        let path = self.archive_path.clone();
        let attempt = password.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    feraille_fs_native::read_archive_toc(&path, attempt.as_deref())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(toc) => {
                        // Keep the accepted password for extraction.
                        this.password = password;
                        this.load = ArchiveLoad::Loaded(toc);
                    }
                    Err(ArchiveError::PasswordRequired) => {
                        this.load = ArchiveLoad::NeedsPassword { error: None }
                    }
                    Err(ArchiveError::WrongPassword) => {
                        this.load = ArchiveLoad::NeedsPassword {
                            error: Some("Incorrect password — try again.".to_string()),
                        }
                    }
                    Err(e) => this.load = ArchiveLoad::Failed(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Whether the archive's *contents* are encrypted while its listing was
    /// readable (the zip case: entry names are public, data is not). Extraction
    /// is gated until a password is supplied.
    fn locked_contents(&self) -> bool {
        self.password.is_none()
            && matches!(&self.load, ArchiveLoad::Loaded(toc) if toc.needs_password)
    }

    /// Apply the typed password. When the listing itself was encrypted we
    /// re-read the table of contents (which validates it immediately);
    /// otherwise we just record it and let extraction use it.
    fn submit_password(&mut self, cx: &mut Context<Self>) {
        let typed = self.password_input.read(cx).value().to_string();
        if typed.is_empty() {
            return;
        }
        if matches!(self.load, ArchiveLoad::NeedsPassword { .. }) {
            self.load = ArchiveLoad::Loading;
            cx.notify();
            self.start_load(Some(typed), cx);
        } else {
            self.password = Some(typed);
            cx.notify();
        }
    }

    fn toggle_row(&mut self, i: usize, cx: &mut Context<Self>) {
        if !self.selected.remove(&i) {
            self.selected.insert(i);
        }
        cx.notify();
    }

    /// Destination for extraction: the folder the archive lives in.
    fn dest_parent(&self) -> Option<PathBuf> {
        self.archive_path.parent().map(|p| p.to_path_buf())
    }

    fn extract_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(shell), Some(parent)) = (self.shell.clone(), self.dest_parent()) else {
            return;
        };
        let archive = self.archive_path.clone();
        let password = self.password.clone();
        let _ = shell.update(cx, |shell, scx| {
            shell.spawn_extract_into(vec![archive], parent, password, window, scx);
        });
    }

    fn extract_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ArchiveLoad::Loaded(toc) = &self.load else {
            return;
        };
        if self.selected.is_empty() {
            return;
        }
        let entries: Vec<String> = self
            .selected
            .iter()
            .filter_map(|&i| toc.entries.get(i))
            .map(|e| e.path.clone())
            .collect();
        let (Some(shell), Some(parent)) = (self.shell.clone(), self.dest_parent()) else {
            return;
        };
        let archive = self.archive_path.clone();
        let password = self.password.clone();
        let _ = shell.update(cx, |shell, scx| {
            shell.extract_archive_entries_into(archive, entries, parent, password, window, scx);
        });
    }

    // -- rendering ----------------------------------------------------------

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let name = self
            .archive_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        let subtitle = match &self.load {
            ArchiveLoad::Loading => "Reading…".to_string(),
            ArchiveLoad::NeedsPassword { .. } => "Encrypted".to_string(),
            ArchiveLoad::Failed(_) => "Unreadable".to_string(),
            ArchiveLoad::Loaded(toc) => {
                let files = toc.file_count();
                let size = toc
                    .total_uncompressed()
                    .map(|b| format!(" · {}", feraille_fs_native::humanize_bytes(b)))
                    .unwrap_or_default();
                let encrypted = if toc.needs_password { " · encrypted" } else { "" };
                format!(
                    "{} · {files} file{}{size}{encrypted}",
                    self.format.label(),
                    if files == 1 { "" } else { "s" }
                )
            }
        };

        let selected_count = self.selected.len();
        // Extraction stays disabled while the archive's contents are still
        // locked — better than letting the op fail on every entry.
        let can_extract = matches!(self.load, ArchiveLoad::Loaded(_))
            && self.caps.can_extract
            && !self.locked_contents();
        let read_only = self.caps.is_read_only();

        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_scale_md().child(name))
                    .child(
                        div()
                            .text_scale_xs()
                            .text_color(theme.muted_foreground)
                            .child(subtitle),
                    ),
            )
            .when(read_only, |this| {
                this.child(
                    div()
                        .px_1p5()
                        .py_0p5()
                        .rounded_md()
                        .bg(theme.muted)
                        .text_scale_xs()
                        .text_color(theme.muted_foreground)
                        .child("Read-only"),
                )
            })
            .child(
                Button::new("archive-extract-selected")
                    .label(if selected_count > 0 {
                        format!("Extract {selected_count} Selected")
                    } else {
                        "Extract Selected".to_string()
                    })
                    .small()
                    .disabled(!can_extract || selected_count == 0)
                    .on_click(cx.listener(|this, _, window, cx| this.extract_selected(window, cx))),
            )
            .child(
                Button::new("archive-extract-all")
                    .label("Extract All")
                    .small()
                    .disabled(!can_extract)
                    .on_click(cx.listener(|this, _, window, cx| this.extract_all(window, cx))),
            )
            .into_any_element()
    }

    /// Password entry row: masked field + Unlock. Used full-pane when the
    /// listing itself is encrypted, and as a strip under the header when only
    /// the contents are.
    fn password_form(
        &self,
        prompt: &str,
        error: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_scale_sm()
                    .text_color(theme.muted_foreground)
                    .child(prompt.to_string()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(220.0)).child(Input::new(&self.password_input).small()))
                    .child(
                        Button::new("archive-unlock")
                            .label("Unlock")
                            .small()
                            .on_click(cx.listener(|this, _, _window, cx| this.submit_password(cx))),
                    ),
            )
            .when_some(error, |this, err| {
                this.child(
                    div()
                        .text_scale_xs()
                        .text_color(theme.danger)
                        .child(err.to_string()),
                )
            })
            .into_any_element()
    }

    fn centered_message(&self, message: impl Into<SharedString>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .flex_1()
            .size_full()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_scale_sm()
                    .text_color(theme.muted_foreground)
                    .child(message.into()),
            )
            .into_any_element()
    }

    fn entry_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let ArchiveLoad::Loaded(toc) = &self.load else {
            return div().into_any_element();
        };
        let count = toc.entries.len();
        let theme = cx.theme();
        let selected_bg = theme.accent;
        let hover_bg = theme.muted;
        let muted = theme.muted_foreground;
        let weak = cx.weak_entity();

        uniform_list("archive-entries", count, move |range, _window, app| {
            let _guard = feraille_core::path_guard::enter_render();
            let Some(ent) = weak.upgrade() else {
                return Vec::new();
            };
            let this = ent.read(app);
            let ArchiveLoad::Loaded(toc) = &this.load else {
                return Vec::new();
            };
            range
                .filter_map(|i| toc.entries.get(i).map(|e| (i, e)))
                .map(|(i, entry)| {
                    let selected = this.selected.contains(&i);
                    row(i, entry, selected, selected_bg, hover_bg, muted, weak.clone())
                })
                .collect()
        })
        .flex_1()
        .into_any_element()
    }
}

/// One archive-entry row: indented by depth, leaf name + size, click toggles
/// selection.
fn row(
    i: usize,
    entry: &ArchiveEntry,
    selected: bool,
    selected_bg: Hsla,
    hover_bg: Hsla,
    muted: Hsla,
    weak: WeakEntity<ArchiveView>,
) -> AnyElement {
    let indent = entry.depth() as f32 * 14.0;
    let size = if entry.is_dir {
        String::new()
    } else {
        entry
            .uncompressed_size
            .map(feraille_fs_native::humanize_bytes)
            .unwrap_or_default()
    };

    h_flex()
        .id(("archive-row", i))
        .w_full()
        .px_3()
        .py_1()
        .gap_2()
        .items_center()
        .cursor_pointer()
        .when(selected, |d| d.bg(selected_bg.opacity(0.22)))
        .hover(|d| d.bg(hover_bg))
        .on_click(move |_, _, app| {
            let _ = weak.update(app, |this, cx| this.toggle_row(i, cx));
        })
        .child(div().w(px(indent)).flex_none())
        // Fixed-width type slot: folder glyph for directories, blank for files
        // so the name column stays aligned.
        .child(div().w(px(16.0)).flex_none().when(entry.is_dir, |d| {
            d.child(
                // Directory-kind icon (upstream Lucide `folder`), matching the
                // file list — NOT the local nav/folder.svg New Folder glyph.
                svg()
                    .path("icons/folder.svg")
                    .icon_px(14.0)
                    .text_color(muted),
            )
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_scale_sm()
                .child(entry.leaf().to_string()),
        )
        .child(
            div()
                .text_scale_xs()
                .text_color(muted)
                .child(size),
        )
        .into_any_element()
}

impl Render for ArchiveView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let bg = theme.background;
        let border = theme.border;
        let header = self.header(cx);

        // Contents-only encryption (zip): the listing renders, but a password
        // strip sits between the header and the rows until it is supplied.
        let locked_strip = self.locked_contents().then(|| {
            let form = self.password_form(
                "This archive's contents are encrypted. Enter its password to extract.",
                None,
                cx,
            );
            div()
                .w_full()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(border)
                .child(form)
        });

        let body = match &self.load {
            ArchiveLoad::Loading => self.centered_message("Reading archive…", cx),
            ArchiveLoad::NeedsPassword { error } => {
                let error = error.clone();
                let form = self.password_form(
                    "This archive is encrypted. Enter its password to view the contents.",
                    error.as_deref(),
                    cx,
                );
                v_flex()
                    .flex_1()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child(form)
                    .into_any_element()
            }
            ArchiveLoad::Failed(e) => {
                self.centered_message(format!("Couldn't read this archive: {e}"), cx)
            }
            ArchiveLoad::Loaded(_) => self.entry_list(cx),
        };

        v_flex()
            .track_focus(&self.focus_handle)
            .key_context(ARCHIVE_CONTEXT)
            .size_full()
            .bg(bg)
            .child(header)
            .children(locked_strip)
            .child(body)
    }
}
