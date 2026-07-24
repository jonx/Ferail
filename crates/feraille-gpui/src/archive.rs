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
use gpui_component::{button::Button, h_flex, v_flex, ActiveTheme, Disableable, Sizable};

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
    /// Encrypted archive whose listing needs a password (prompt is a later
    /// slice — for now the view explains the situation).
    NeedsPassword,
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
    /// `Shell::spawn_file_op` (tasks / toast / undo / reload) rather than
    /// duplicating that machinery here.
    shell: Option<WeakEntity<Shell>>,
    focus_handle: FocusHandle,
}

impl ArchiveView {
    pub fn new(archive_path: PathBuf, format: Format, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            archive_path,
            format,
            caps: format.capabilities(),
            load: ArchiveLoad::Loading,
            selected: BTreeSet::new(),
            shell: None,
            focus_handle: cx.focus_handle(),
        };
        view.start_load(cx);
        view
    }

    pub fn set_shell(&mut self, shell: WeakEntity<Shell>) {
        self.shell = Some(shell);
    }

    /// Schedule the TOC read on the background executor and apply the result
    /// through an entity update (the only place view state mutates).
    fn start_load(&mut self, cx: &mut Context<Self>) {
        let path = self.archive_path.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { feraille_fs_native::read_archive_toc(&path, None) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.load = match result {
                    Ok(toc) => ArchiveLoad::Loaded(toc),
                    Err(ArchiveError::PasswordRequired) => ArchiveLoad::NeedsPassword,
                    Err(e) => ArchiveLoad::Failed(e.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
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
        let _ = shell.update(cx, |shell, scx| {
            shell.spawn_extract_into(vec![archive], parent, window, scx);
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
        let _ = shell.update(cx, |shell, scx| {
            shell.extract_archive_entries_into(archive, entries, parent, window, scx);
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
            ArchiveLoad::NeedsPassword => "Encrypted".to_string(),
            ArchiveLoad::Failed(_) => "Unreadable".to_string(),
            ArchiveLoad::Loaded(toc) => {
                let files = toc.file_count();
                let size = toc
                    .total_uncompressed()
                    .map(|b| format!(" · {}", feraille_fs_native::humanize_bytes(b)))
                    .unwrap_or_default();
                format!("{} · {files} file{}{size}", self.format.label(), if files == 1 { "" } else { "s" })
            }
        };

        let selected_count = self.selected.len();
        let can_extract = matches!(self.load, ArchiveLoad::Loaded(_)) && self.caps.can_extract;
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
        let header = self.header(cx);
        let body = match &self.load {
            ArchiveLoad::Loading => self.centered_message("Reading archive…", cx),
            ArchiveLoad::NeedsPassword => self.centered_message(
                "This archive is encrypted — password support is coming soon.",
                cx,
            ),
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
            .child(body)
    }
}
