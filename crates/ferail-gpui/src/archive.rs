//! Archive workbench: an embedded tool-result view (peer to Disk Usage and
//! the Duplicate panel) for browsing an archive's contents and extracting from
//! it. Opened via the "Open as Archive" context action; docked into the tab's
//! `tool_result` seam (docs/features/TOOL_RESULTS.md) and closed the same way
//! as the other tool results.
//!
//! # Rows are the real file list
//!
//! The contents render through the tab's ordinary [`FileListDelegate`] +
//! `DataTable`, not a bespoke list, so archive rows inherit the app's columns,
//! selection model, sort, striping and row lines for free. What makes them
//! archive rows is that the delegate's `paths` map is left **empty** (their
//! `path_for_entry` is `None`, so every path-dependent affordance degrades to
//! its "unknown" branch) and a parallel `archive_rows` vector carries the tree
//! metadata the Name cell draws.
//!
//! # Tree, not a flat dump
//!
//! A real archive is deep: a macOS app bundle runs to thousands of entries,
//! so [`ferail_archive::ArchiveTree`] indexes the flat table of contents and
//! this view projects only the levels the user has expanded.
//!
//! All I/O (format probe, table-of-contents read) happens on the background
//! executor; render only reads cached state (Prime Directive).

use std::collections::HashSet;
use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, ElementExt as _, Selectable as _, Sizable, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex,
};

use ferail_archive::{ArchiveEntry, ArchiveTree, Capabilities, Format, Toc, TreeRow};
use ferail_core::{EntryKind, FileEntry, NodeId};
use ferail_fs_native::ArchiveError;

use crate::file_list::FileListDelegate;
use crate::multi_table::{DataTable, TableEvent, TableState};
use crate::shell::{OpenSelected, Shell};
use crate::text::{IconScale as _, TextScale as _, TruncateMiddle as _};
use crate::tool_results::{ToolHostContext, ToolHostEvent};

/// Callback a standalone archive window uses to dock itself back into a tab.
/// Mirrors `disk_usage::DockOwner`.
pub type ArchiveDockOwner = std::rc::Rc<dyn Fn(PathBuf, Entity<ArchiveView>, &mut App)>;

/// Narrower than this and the header hides its filter box so the archive's
/// name keeps a usable share of the row.
const FILTER_MIN_WIDTH: f32 = 1180.0;

/// Above this, previewing asks first. An archive entry has to be written out
/// before Quick Look can read it (it is an OS service that takes a file URL),
/// and the table of contents gives us the uncompressed size *before* we spend
/// anything, so the check is free and also caps decompression bombs.
const PREVIEW_CONFIRM_BYTES: u64 = 100 * 1024 * 1024;

/// Ceiling for decoding an entry in memory. Above this we stage to disk
/// instead of holding the whole thing: text and images worth previewing are
/// far below it.
const PREVIEW_INMEMORY_CAP: u64 = 16 * 1024 * 1024;

/// Key context for the archive pane (keymap bindings hang off this).
pub const ARCHIVE_CONTEXT: &str = "Archive";

actions!(archive, [ArchiveDismiss]);

#[derive(Clone)]
enum ArchiveCloseTarget {
    Window(AnyWindowHandle),
    Dock(WeakEntity<Shell>),
}

fn close_archive_target(target: ArchiveCloseTarget, cx: &mut App) {
    match target {
        ArchiveCloseTarget::Window(handle) => {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        ArchiveCloseTarget::Dock(shell) => {
            let _ = shell.update(cx, |shell, cx| shell.close_active_tool_result(cx));
        }
    }
}

/// The off-thread load state of the archive's table of contents.
enum ArchiveLoad {
    Loading,
    Loaded(Box<Toc>),
    /// The archive's *listing itself* is encrypted (7z header encryption), so
    /// nothing can be shown until a password is supplied. `error` is set after
    /// a rejected attempt.
    NeedsPassword {
        error: Option<String>,
    },
    Failed(String),
}

pub struct ArchiveView {
    archive_path: PathBuf,
    /// Resolved off-thread, by extension when it names a format, otherwise by
    /// content, so a `.docx` / `.jar` / `.apk` opens as the zip it is.
    format: Option<Format>,
    load: ArchiveLoad,
    /// Folder index over the flat table of contents.
    tree: ArchiveTree,
    /// Archive paths of the folders the user has opened.
    expanded: HashSet<String>,
    /// Stable synthetic ids so selection survives expand/collapse.
    ids: std::collections::HashMap<String, NodeId>,
    next_id: u64,
    /// Filter text typed into the workbench's own search box.
    filter_input: Entity<InputState>,
    filter: String,
    /// This view's **own** table. Owning it (rather than borrowing the tab's)
    /// is what lets the workbench move between a docked tab and a standalone
    /// window without either host losing its listing.
    table: Entity<TableState<FileListDelegate>>,
    /// Kept alive for the lifetime of the view, dropping it stops selection.
    _table_sub: Subscription,
    /// Where this view currently lives, and how to dock it back when windowed.
    host: ToolHostContext,
    dock_owner: Option<ArchiveDockOwner>,
    shell: Option<WeakEntity<Shell>>,
    password_input: Entity<InputState>,
    password: Option<String>,
    /// Identity of the archive at the moment `load` was read. A transactional
    /// save refuses to overwrite the file if this no longer matches.
    stamp: Option<ferail_fs_native::ArchiveStamp>,
    /// Unsaved operations plus the worker-expanded virtual rows for additions.
    edits: ferail_fs_native::ArchiveEditPlan,
    pending_entries: Vec<ArchiveEntry>,
    saving: bool,
    /// Avoid stacking multiple close-confirmation dialogs when the platform
    /// sends repeated close requests while one is already visible.
    close_prompt_open: bool,
    /// Hover explanation shown while an external file drag is over the pane.
    drop_feedback: Option<SharedString>,
    /// Item count used to build the current external-drop message. Mouse-move
    /// events can arrive hundreds of times per second, but the localized text
    /// only changes when this count or the accepted/rejected state changes.
    drop_feedback_count: Option<usize>,
    /// Whether the current hover explanation describes an accepted drop.
    /// Archive-entry drags over their source workbench are deliberately
    /// rejected: releasing there must never bubble into the docked folder
    /// pane and accidentally extract into its current directory.
    drop_feedback_allowed: bool,
    /// Own preview panel, used when this workbench lives in its own window,
    /// there is no Shell pane there to borrow. `None` while docked, where the
    /// Shell's pane shows the entry instead.
    preview_panel: Option<Entity<crate::preview_panel::PreviewPanel>>,
    /// Rounded pane width, captured at prepaint. Lets the header drop its
    /// lower-priority controls before they crush the archive's name.
    host_width: Option<f32>,
    /// Preview is opt-in: entries must be written to a scratch file before the
    /// preview providers (Quick Look) can read them, so nothing is extracted
    /// until the user asks for it.
    preview_enabled: bool,
    /// Scratch directory holding entries written out for preview. Created
    /// lazily on the first preview and swept at the next startup if the view
    /// closes while no further background callback can remove it safely.
    scratch: Option<PathBuf>,
    /// Archive path of the entry currently staged for preview, so re-selecting
    /// the same row doesn't extract it twice.
    previewed: Option<String>,
    /// The scratch file backing the current preview. At most one exists at a
    /// time: staging a new entry or closing the preview removes the prior file;
    /// crash/startup cleanup covers a window closed during background work.
    staged_file: Option<PathBuf>,
    focus_handle: FocusHandle,
    _escape_subscription: Option<Subscription>,
}

impl ArchiveView {
    pub fn new(
        archive_path: PathBuf,
        process: std::rc::Rc<crate::process_state::ProcessState>,
        shell_focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The workbench builds its own table so it can be hosted either way.
        let delegate = FileListDelegate::new(
            process.fs.clone(),
            process.icons.clone(),
            process.thumbnails.clone(),
            process.tasks.clone(),
            process.cut_marker.clone(),
            process.list_sort.clone(),
            shell_focus,
        );
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .col_selectable(false)
                .col_movable(true)
                .col_resizable(true)
        });
        // No Shell to route gestures through when windowed, so handle the
        // table's own events here.
        let table_sub = cx.subscribe_in(
            &table,
            window,
            |this: &mut Self, table, event: &TableEvent, _window: &mut Window, cx| match event {
                TableEvent::RowClicked {
                    row_ix,
                    modifiers,
                    click_count,
                } => {
                    let modifiers = *modifiers;
                    let row_ix = *row_ix;
                    table.update(cx, |t, cx| {
                        t.delegate_mut().apply_click_gesture(row_ix, modifiers);
                        cx.notify();
                    });
                    if *click_count >= 2 {
                        this.activate_row(row_ix, _window, cx);
                    } else {
                        this.preview_selection(_window, cx);
                    }
                    cx.notify();
                }
                TableEvent::LeadMoved { row_ix, modifiers } => {
                    let modifiers = *modifiers;
                    let row_ix = *row_ix;
                    table.update(cx, |t, cx| {
                        t.delegate_mut().apply_click_gesture(row_ix, modifiers);
                        cx.notify();
                    });
                    this.preview_selection(_window, cx);
                    cx.notify();
                }
                TableEvent::ExternalDrop { row_ix, paths } => {
                    let destination = table
                        .read(cx)
                        .delegate()
                        .archive_row(*row_ix)
                        .filter(|row| row.is_dir)
                        .map(|row| row.path.clone());
                    if let Some(destination) = destination {
                        this.add_dropped_at(paths.clone(), destination, _window, cx);
                    }
                }
                _ => {}
            },
        );
        let password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(tr!("Password"))
        });
        let filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("Filter contents\u{2026}")));
        // Re-project on every keystroke in the filter box.
        cx.subscribe(&filter_input, |this: &mut Self, input, ev, cx| {
            if matches!(ev, gpui_component::input::InputEvent::Change) {
                this.filter = input.read(cx).value().to_string();
                this.project_rows(cx);
                cx.notify();
            }
        })
        .detach();

        let mut view = Self {
            archive_path,
            format: None,
            load: ArchiveLoad::Loading,
            tree: ArchiveTree::default(),
            expanded: HashSet::new(),
            ids: std::collections::HashMap::new(),
            next_id: 1,
            filter_input,
            filter: String::new(),
            table,
            _table_sub: table_sub,
            host: ToolHostContext::Docked,
            dock_owner: None,
            shell: None,
            password_input,
            password: None,
            stamp: None,
            edits: ferail_fs_native::ArchiveEditPlan::default(),
            pending_entries: Vec::new(),
            saving: false,
            close_prompt_open: false,
            drop_feedback: None,
            drop_feedback_count: None,
            drop_feedback_allowed: false,
            host_width: None,
            preview_panel: None,
            preview_enabled: false,
            scratch: None,
            previewed: None,
            staged_file: None,
            focus_handle: cx.focus_handle(),
            _escape_subscription: None,
        };
        view.start_load(None, cx);
        view
    }

    pub fn set_shell(&mut self, shell: WeakEntity<Shell>) {
        self.shell = Some(shell);
    }

    pub fn set_dock_owner(&mut self, dock_owner: Option<ArchiveDockOwner>, cx: &mut Context<Self>) {
        self.dock_owner = dock_owner;
        cx.notify();
    }

    pub fn handle_host_event(&mut self, event: ToolHostEvent, cx: &mut Context<Self>) {
        match event {
            ToolHostEvent::HostChanged(context) => self.host = context,
        }
        // Windowed: nothing else is drawing a preview pane, so host one.
        // Docked: hand the job back to the Shell's pane.
        match self.host {
            ToolHostContext::Windowed => {
                if self.preview_panel.is_none() {
                    if let Some(shell) = self.shell.clone() {
                        let process = crate::process_state::process_state(cx).clone();
                        let panel = cx.new(|_| {
                            crate::preview_panel::PreviewPanel::new(process, shell, 200.0)
                        });
                        cx.subscribe(
                            &panel,
                            |this: &mut Self,
                             _p,
                             _: &crate::preview_panel::PreviewCloseRequested,
                             cx| {
                                this.set_preview_enabled(false, cx);
                            },
                        )
                        .detach();
                        self.preview_panel = Some(panel);
                    }
                }
            }
            ToolHostContext::Docked => {
                self.preview_panel = None;
            }
        }
        cx.notify();
    }

    /// Record the pane width. Guarded so an unchanged width doesn't notify:
    /// prepaint runs every frame and an unconditional notify would loop.
    fn update_host_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let next = width.round();
        if self.host_width == Some(next) {
            return;
        }
        self.host_width = Some(next);
        cx.notify();
    }

    /// Toggle the preview pane for archive entries. Turning it on previews the
    /// current selection immediately; turning it off leaves the staged file
    /// alone (the scratch dir is cleaned when the view closes).
    fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.preview_enabled = !self.preview_enabled;
        if !self.preview_enabled {
            self.previewed = None;
            self.discard_staged(cx);
            if let Some(shell) = self.shell.clone() {
                let _ = shell.update(cx, |s, cx| {
                    s.preview_override = None;
                    cx.notify();
                });
            }
        }
        if self.preview_enabled {
            // Reveal the shell's pane too, so the button never appears to do
            // nothing when the pane happens to be hidden.
            if let Some(shell) = self.shell.clone() {
                let _ = shell.update(cx, |s, cx| {
                    s.preview_visible = true;
                    cx.notify();
                });
            }
            self.preview_selection(window, cx);
        }
        cx.notify();
    }

    /// Remove the currently staged scratch file, if any.
    fn discard_staged(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.staged_file.take() {
            cx.background_executor()
                .spawn(async move { ferail_fs_native::scratch::remove_staged(&path) })
                .detach();
        }
    }

    /// Force the preview toggle off: used when the preview pane is closed
    /// from its own header, so the two controls can't disagree.
    pub fn set_preview_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.preview_enabled == enabled {
            return;
        }
        self.preview_enabled = enabled;
        if !enabled {
            self.previewed = None;
            self.discard_staged(cx);
        }
        cx.notify();
    }

    /// Stage the selected entry for preview, if it is a single file and preview
    /// is enabled. Big entries ask first.
    fn preview_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.preview_enabled {
            return;
        }
        let Some(row) = self.single_selected_file(cx) else {
            return;
        };
        self.preview_row(row, window, cx);
    }

    /// Activate one archive row without performing a permanent extraction.
    /// Folders expand in place; files open the existing privacy-preserving
    /// preview path (memory when possible, one private scratch file otherwise).
    fn activate_row(&mut self, row_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let row = self.table.read(cx).delegate().archive_row(row_ix).cloned();
        let Some(row) = row else {
            return;
        };
        if row.is_dir {
            self.toggle_expanded(&row.path, cx);
            return;
        }
        self.preview_enabled = true;
        if let Some(shell) = self.shell.clone() {
            let _ = shell.update(cx, |shell, cx| {
                shell.preview_visible = true;
                cx.notify();
            });
        }
        self.preview_row(row, window, cx);
        cx.notify();
    }

    fn activate_lead(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row_ix = {
            let table = self.table.read(cx);
            let delegate = table.delegate();
            delegate
                .lead
                .and_then(|lead| delegate.entries.iter().position(|entry| entry.id == lead))
        };
        if let Some(row_ix) = row_ix {
            self.activate_row(row_ix, window, cx);
        }
    }

    fn preview_row(&mut self, row: TreeRow, window: &mut Window, cx: &mut Context<Self>) {
        if self.previewed.as_deref() == Some(row.path.as_str()) {
            return; // already staged
        }
        let size = row.size.unwrap_or(0);
        if size > PREVIEW_CONFIRM_BYTES {
            self.confirm_large_preview(row, size, window, cx);
        } else if !self.preview_in_memory(&row.path.clone(), cx) {
            self.stage_preview(row.path, cx);
        }
    }

    /// The selected row when it is exactly one non-directory entry: preview is
    /// meaningless for a folder or a multi-selection.
    fn single_selected_file(&self, cx: &App) -> Option<TreeRow> {
        let table = self.table.read(cx);
        let del = table.delegate();
        let mut hits = del
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| del.selected_set.contains(&e.id));
        let (row_ix, _) = hits.next()?;
        if hits.next().is_some() {
            return None; // multi-selection
        }
        let row = del.archive_row(row_ix)?.clone();
        (!row.is_dir).then_some(row)
    }

    fn confirm_large_preview(
        &mut self,
        row: TreeRow,
        size: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let human = ferail_fs_native::humanize_bytes(size);
        let name = row.name.clone();
        let path = row.path.clone();
        let this = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let (path, this) = (path.clone(), this.clone());
            dialog
                .title(tr!("Preview this file?"))
                .child(
                    div()
                        .text_scale_sm()
                        .child(tr!(
                            "\u{201c}{name}\u{201d} is {human}. Previewing it writes a temporary copy out of the archive first.",
                            name = name,
                            human = human
                        )),
                )
                .on_ok(move |_, _window, cx: &mut App| {
                    let _ = this.update(cx, |this: &mut ArchiveView, cx| {
                        this.stage_preview(path.clone(), cx);
                    });
                    true
                })
        });
    }

    /// Decode an entry in memory when one of our own renderers can draw it:
    /// text and images, which is most of what anyone peeks at. Nothing touches
    /// disk on this path. Returns false when the entry needs Quick Look, which
    /// only reads files.
    fn preview_in_memory(&mut self, entry: &str, cx: &mut Context<Self>) -> bool {
        let leaf = entry.rsplit('/').next().unwrap_or(entry).to_string();
        let ext = leaf
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        // Only claim what we can actually draw; everything else falls through
        // to staging so the preview stays as rich as it was.
        const IMAGE_EXT: &[&str] = &[
            "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tiff", "tif",
        ];
        let is_image = IMAGE_EXT.contains(&ext.as_str());
        let archive = self.archive_path.clone();
        let password = self.password.clone();
        let entry_owned = entry.to_string();
        let entry_for_fallback = entry_owned.clone();
        let this = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            let decoded = cx
                .background_executor()
                .spawn(async move {
                    let bytes = ferail_fs_native::read_archive_entry_bytes(
                        &archive,
                        &entry_owned,
                        password.as_deref(),
                        PREVIEW_INMEMORY_CAP,
                    )
                    .ok()
                    .flatten()?;
                    let size = bytes.len() as u64;
                    if is_image {
                        let decoded = image::load_from_memory(&bytes).ok()?;
                        let rgba = decoded.to_rgba8();
                        let (w, h) = (rgba.width(), rgba.height());
                        let img = crate::icons::build_render_image(rgba.into_raw(), w, h);
                        return Some((
                            size,
                            crate::preview_panel::PreviewContent::Image(std::sync::Arc::new(img)),
                        ));
                    }
                    let text = crate::text_preview::decode_text_preview(bytes)?;
                    Some((
                        size,
                        crate::preview_panel::PreviewContent::Text(text.into()),
                    ))
                })
                .await;
            let Some((size, content)) = decoded else {
                // Not something we can draw: stage it for Quick Look.
                let _ = this.update(cx, |this: &mut ArchiveView, cx| {
                    this.stage_preview(entry_for_fallback.clone(), cx);
                });
                return;
            };
            let _ = this.update(cx, |this: &mut ArchiveView, cx| {
                this.set_preview_target(
                    crate::preview_panel::PreviewTarget::InMemory {
                        name: leaf.clone(),
                        size,
                        content,
                    },
                    cx,
                );
            });
        })
        .detach();
        true
    }

    /// Point whichever panel is hosting us at `target`.
    fn set_preview_target(
        &mut self,
        target: crate::preview_panel::PreviewTarget,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = &self.preview_panel {
            panel.update(cx, |p, cx| p.set_target(target, cx));
            cx.notify();
            return;
        }
        if let Some(shell) = self.shell.clone() {
            let _ = shell.update(cx, |s, cx| {
                s.preview_override = Some(target);
                cx.notify();
            });
        }
    }

    /// Extract one entry into the scratch dir off-thread, then hand the real
    /// path to the shell's existing preview pipeline.
    fn stage_preview(&mut self, entry: String, cx: &mut Context<Self>) {
        let Some(shell) = self.shell.clone() else {
            return;
        };
        let scratch = self.scratch.clone();
        // Supersede the previous one immediately, never two at once.
        self.discard_staged(cx);
        self.previewed = Some(entry.clone());
        let display_name = entry
            .rsplit('/')
            .next()
            .unwrap_or(entry.as_str())
            .to_string();
        let archive = self.archive_path.clone();
        let password = self.password.clone();
        cx.spawn(async move |this, cx| {
            let staged = cx
                .background_executor()
                .spawn(async move {
                    let scratch = match scratch {
                        Some(dir) => dir,
                        None => ferail_fs_native::scratch::scratch_dir()?,
                    };
                    // Wipe first: one staged file exists at a time, and the
                    // wipe must not run after extraction or it would delete
                    // the entry we just wrote.
                    ferail_fs_native::scratch::clear_staged_dir(&scratch);
                    let progress = ferail_fs_native::file_ops::TransferProgress::new();
                    let cancel = std::sync::atomic::AtomicBool::new(false);
                    let opts = ferail_fs_native::ExtractOptions {
                        password: password.as_deref(),
                        overwrite: true,
                    };
                    ferail_fs_native::extract_archive_entries(
                        &archive,
                        &scratch,
                        &[entry.as_str()],
                        opts,
                        &progress,
                        &cancel,
                    )
                    .ok()?;
                    let extracted = scratch.join(&entry);
                    if !extracted.is_file() {
                        return None;
                    }
                    // Rename to an opaque, extension-preserving name: the
                    // extension is what lets Quick Look pick a renderer, but
                    // the original name would otherwise sit in plain sight.
                    let staged = ferail_fs_native::scratch::staged_path(&scratch, &entry);
                    if std::fs::rename(&extracted, &staged).is_err() {
                        return None;
                    }
                    ferail_fs_native::scratch::set_private_permissions(&staged, 0o600);
                    // The entry's own directory chain is now empty: drop it so
                    // the folder names don't linger either.
                    if let Some(top) = entry.split('/').next() {
                        if top != entry {
                            let _ = std::fs::remove_dir_all(scratch.join(top));
                        }
                    }
                    Some((staged, scratch))
                })
                .await;
            if let Some((staged, scratch)) = staged {
                let _ = this.update(cx, |this: &mut ArchiveView, cx| {
                    this.scratch = Some(scratch);
                    this.staged_file = Some(staged.clone());
                    cx.notify();
                });
                let _ = shell.update(cx, |s, cx| {
                    crate::preview::request(s, staged.clone(), cx);
                    cx.notify();
                });
                let _ = this.update(cx, |this: &mut ArchiveView, cx| {
                    let target = crate::preview_panel::PreviewTarget::File {
                        path: staged.clone(),
                        entry: Box::new(crate::preview_panel::synthetic_entry(
                            &staged,
                            &display_name,
                        )),
                    };
                    this.set_preview_target(target, cx);
                });
            }
        })
        .detach();
    }

    /// Screenshot harness: select `row_ix` and turn the preview on, so the
    /// staged-preview state can be captured headlessly.
    pub fn preview_row_for_capture(
        &mut self,
        row_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.table.update(cx, |t, cx| {
            t.delegate_mut()
                .apply_click_gesture(row_ix, gpui::Modifiers::default());
            cx.notify();
        });
        if !self.preview_enabled {
            self.toggle_preview(window, cx);
        } else {
            self.preview_selection(window, cx);
        }
    }

    /// Password to carry in a drag payload, so a drop can extract encrypted
    /// entries without re-prompting.
    pub fn password_for_drag(&self) -> Option<String> {
        self.password.clone()
    }

    /// The workbench's table, so the shell can read counts for the status bar.
    pub fn table(&self) -> &Entity<TableState<FileListDelegate>> {
        &self.table
    }

    /// The archive this view is showing: the shell needs it to re-dock.
    pub fn archive_path(&self) -> &std::path::Path {
        &self.archive_path
    }

    fn caps(&self) -> Option<Capabilities> {
        self.format.map(|f| f.capabilities())
    }

    /// Probe the format and read the table of contents on the background
    /// executor, then apply the result through an entity update.
    fn start_load(&mut self, password: Option<String>, cx: &mut Context<Self>) {
        let path = self.archive_path.clone();
        let attempt = password.clone();
        cx.spawn(async move |this, cx| {
            let probe = {
                let path = path.clone();
                cx.background_executor()
                    .spawn(async move { ferail_fs_native::probe_archive_format(&path) })
                    .await
            };
            let Some(format) = probe else {
                let _ = this.update(cx, |this, cx| {
                    this.load = ArchiveLoad::Failed(
                        tr!("This file isn't an archive Ferail can open.").to_string(),
                    );
                    cx.notify();
                });
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    let toc = ferail_fs_native::read_archive_toc(&path, attempt.as_deref())?;
                    let stamp = ferail_fs_native::archive_stamp(&path)?;
                    Ok::<_, ArchiveError>((toc, stamp))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.format = Some(format);
                match result {
                    Ok((toc, stamp)) => {
                        this.password = password;
                        this.stamp = Some(stamp);
                        this.edits = ferail_fs_native::ArchiveEditPlan::default();
                        this.pending_entries.clear();
                        this.saving = false;
                        this.tree = ArchiveTree::build(&toc);
                        // Open the single root folder straight away: an
                        // archive with one wrapper directory should not greet
                        // the user with a single collapsed row.
                        if let Some(root) = toc.single_root() {
                            if this.tree.is_dir(root) {
                                this.expanded.insert(root.to_string());
                            }
                        }
                        this.load = ArchiveLoad::Loaded(Box::new(toc));
                        this.project_rows(cx);
                    }
                    Err(ArchiveError::PasswordRequired) => {
                        this.load = ArchiveLoad::NeedsPassword { error: None }
                    }
                    Err(ArchiveError::WrongPassword) => {
                        this.load = ArchiveLoad::NeedsPassword {
                            error: Some(tr!("Incorrect password: try again.").to_string()),
                        }
                    }
                    Err(e) => this.load = ArchiveLoad::Failed(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Stable id per archive path, so the selection set survives re-projection.
    fn id_for(&mut self, path: &str) -> NodeId {
        if let Some(id) = self.ids.get(path) {
            return *id;
        }
        let id = NodeId::from_raw(self.next_id).expect("nonzero");
        self.next_id += 1;
        self.ids.insert(path.to_string(), id);
        id
    }

    /// The table of contents after applying the unsaved journal. Removed rows
    /// disappear, renamed subtrees move as one, and worker-inspected additions
    /// appear without touching the archive itself.
    fn projected_toc(&self) -> Option<Toc> {
        let ArchiveLoad::Loaded(base) = &self.load else {
            return None;
        };
        let mut entries = Vec::with_capacity(base.entries.len() + self.pending_entries.len());
        for entry in &base.entries {
            if archive_path_matches_any(&entry.path, &self.edits.removals) {
                continue;
            }
            let mut entry = entry.clone();
            entry.path = project_archive_path(&entry.path, &self.edits.renames);
            entries.push(entry);
        }
        entries.extend(self.pending_entries.iter().cloned().map(|mut entry| {
            entry.path = project_archive_path(&entry.path, &self.edits.renames);
            entry
        }));
        Some(Toc {
            entries,
            needs_password: base.needs_password,
        })
    }

    fn refresh_edit_projection(&mut self, cx: &mut Context<Self>) {
        if let Some(toc) = self.projected_toc() {
            self.tree = ArchiveTree::build(&toc);
            self.project_rows(cx);
        }
        cx.notify();
    }

    fn row_description(&self, row: &TreeRow) -> String {
        if row.is_dir {
            return String::new();
        }
        if self
            .pending_entries
            .iter()
            .any(|entry| normalized_archive_path(&entry.path) == normalized_archive_path(&row.path))
        {
            return tr!("Pending addition").to_string();
        }
        let mut parts = Vec::new();
        if let Some(packed) = row.compressed_size {
            parts.push(
                tr!(
                    "Packed {size}",
                    size = ferail_fs_native::humanize_bytes(packed)
                )
                .to_string(),
            );
            if let Some(original) = row.size.filter(|size| *size > 0) {
                let saved = 100i128 - (packed as i128 * 100 / original as i128);
                parts.push(tr!("{percent}% saved", percent = saved).to_string());
            }
        }
        if let Some(method) = &row.compression_method {
            parts.push(method.clone());
        }
        if let Some(checksum) = &row.checksum {
            parts.push(checksum.clone());
        }
        if let Some(mode) = row.unix_mode {
            parts.push(tr!("mode {mode}", mode = format!("{:04o}", mode & 0o7777)).to_string());
        }
        if row.encrypted {
            parts.push(tr!("encrypted").to_string());
        }
        if let Some(comment) = &row.comment {
            parts.push(comment.clone());
        }
        parts.join(" \u{00b7} ")
    }

    /// Push the currently visible tree rows into the tab's file-list delegate.
    fn project_rows(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.load, ArchiveLoad::Loaded(_)) {
            return;
        }
        let rows: Vec<TreeRow> = if self.filter.trim().is_empty() {
            self.tree.visible_rows(&self.expanded)
        } else {
            self.tree.matching_rows(self.filter.trim())
        };
        let entries: Vec<FileEntry> = rows
            .iter()
            .map(|row| {
                let id = self.id_for(&row.path);
                let size = row.size.unwrap_or(0);
                FileEntry {
                    id,
                    name: row.name.clone().into(),
                    display_name: row.name.clone().into(),
                    // Archive names are untrusted display text too. Feed the
                    // literal stored leaf through the same deceptive-name
                    // detector as normal filesystem rows; rendering exposes
                    // bidi controls, zero-width characters and mixed-script
                    // look-alikes without changing the extraction path.
                    name_has_hazards: ferail_core::name_hazards::has_hazards(&row.name),
                    kind: if row.is_dir {
                        EntryKind::Directory
                    } else {
                        EntryKind::File
                    },
                    size,
                    mtime_unix: row.mtime_unix.unwrap_or(0),
                    display_size: if row.is_dir {
                        ferail_core::empty_entry_text()
                    } else {
                        ferail_fs_native::humanize_bytes(size).into()
                    },
                    display_kind: ferail_fs_native::describe_kind(
                        if row.is_dir {
                            EntryKind::Directory
                        } else {
                            EntryKind::File
                        },
                        &row.name,
                    )
                    .into(),
                    display_magic: ferail_core::empty_entry_text(),
                    display_description: self.row_description(row).into(),
                    details_loaded: true,
                    is_quarantined: false,
                    quarantine: None,
                    hidden: false,
                    created_unix: None,
                    locked: false,
                }
            })
            .collect();

        let weak = cx.entity().downgrade();
        self.table.update(cx, |table, cx| {
            table
                .delegate_mut()
                .replace_archive_entries(entries, rows, weak);
            cx.notify();
        });
    }

    /// Open / close a folder row.
    pub fn toggle_expanded(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_string());
        }
        self.project_rows(cx);
        cx.notify();
    }

    /// Whether the archive's *contents* are encrypted while its listing was
    /// readable (the zip case). Extraction is gated until a password is given.
    fn locked_contents(&self) -> bool {
        self.password.is_none()
            && matches!(&self.load, ArchiveLoad::Loaded(toc) if toc.needs_password)
    }

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

    /// Extract the rows selected in the file list. A selected folder brings its
    /// whole subtree (the engine's selection matches directory prefixes), so a
    /// collapsed folder extracts entirely without expanding it first.
    fn extract_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.selected_archive_paths(cx);
        if paths.is_empty() {
            return;
        }
        let (Some(shell), Some(parent)) = (self.shell.clone(), self.dest_parent()) else {
            return;
        };
        let archive = self.archive_path.clone();
        let password = self.password.clone();
        let _ = shell.update(cx, |shell, scx| {
            shell.extract_archive_entries_into(archive, paths, parent, password, window, scx);
        });
    }

    /// Extract to a folder the user picks, rather than next to the archive.
    ///
    /// Extract All / Extract Selected both write into the archive's own
    /// folder, which fails outright when that volume is read-only or full,
    /// on AROS the shared host folder is mounted twice, `MacRO:` read-only
    /// and `MacRW:` writable, so an archive opened through `MacRO:` can never
    /// extract in place. This is the way out, and it honours the current
    /// selection: selected rows if there are any, otherwise the whole
    /// archive, matching what the two buttons beside it would have done.
    fn extract_to(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shell) = self.shell.clone() else {
            return;
        };
        let selected = self.selected_archive_paths(cx);
        let archive = self.archive_path.clone();
        let password = self.password.clone();
        cx.spawn_in(window, async move |_this, cx| {
            let Some(dest) = crate::shell::pick_destination_folder(cx).await else {
                return;
            };
            let _ = shell.update_in(cx, |shell, window, scx| {
                if selected.is_empty() {
                    shell.spawn_extract_into(vec![archive], dest, password, window, scx);
                } else {
                    shell.extract_archive_entries_into(
                        archive, selected, dest, password, window, scx,
                    );
                }
            });
        })
        .detach();
    }

    /// Archive paths behind the delegate's current selection.
    fn selected_archive_paths(&self, cx: &App) -> Vec<String> {
        let table = self.table.read(cx);
        let del = table.delegate();
        del.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| del.selected_set.contains(&e.id))
            .filter_map(|(i, _)| del.archive_path_for_row(i).map(str::to_string))
            .collect()
    }

    fn selection_count(&self, cx: &App) -> usize {
        let table = self.table.read(cx);
        let del = table.delegate();
        del.entries
            .iter()
            .filter(|e| del.selected_set.contains(&e.id))
            .count()
    }

    /// Only an explicitly named `.zip` is editable. Content-probed OOXML,
    /// JAR, APK and similar package files deliberately stay browse-only:
    /// changing their zip members can invalidate signatures or structure.
    fn is_plain_editable_zip(&self) -> bool {
        self.format == Some(Format::Zip)
            && Format::from_path(&self.archive_path.to_string_lossy()) == Some(Format::Zip)
    }

    fn edit_rejection(&self) -> Option<SharedString> {
        if self.saving {
            return Some(tr!("Wait for the current archive save to finish."));
        }
        if !matches!(self.load, ArchiveLoad::Loaded(_)) {
            return Some(tr!("Wait for the archive to finish loading."));
        }
        if self.format == Some(Format::Zip) && !self.is_plain_editable_zip() {
            return Some(tr!(
                "This ZIP-based package is browse-only to avoid damaging its structure or signature."
            ));
        }
        if !self.is_plain_editable_zip() {
            let format = self
                .format
                .map(|format| format.label().to_string())
                .unwrap_or_else(|| tr!("This archive").to_string());
            return Some(tr!(
                "{format} archives are read-only here.",
                format = format
            ));
        }
        if self.locked_contents() {
            return Some(tr!("Unlock this archive before editing it."));
        }
        None
    }

    pub(crate) fn can_stage_edits(&self) -> bool {
        self.edit_rejection().is_none()
    }

    fn show_edit_rejection(&self, window: &mut Window, cx: &mut App) {
        if let Some(reason) = self.edit_rejection() {
            window.push_notification(
                gpui_component::notification::Notification::warning(reason),
                cx,
            );
        }
    }

    /// Inspect dropped sources on a worker and add them to the unsaved journal.
    /// `destination` is an internal archive folder, not a filesystem path.
    fn add_dropped_at(
        &mut self,
        paths: Vec<PathBuf>,
        destination: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        if !self.can_stage_edits() {
            self.show_edit_rejection(window, cx);
            return;
        }
        let additions: Vec<ferail_fs_native::ArchiveAddition> = paths
            .into_iter()
            .map(|source| ferail_fs_native::ArchiveAddition {
                source,
                destination: destination.clone(),
            })
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let inspect_plan = additions.clone();
            let inspected = cx
                .background_executor()
                .spawn(async move {
                    ferail_fs_native::inspect_archive_additions(&inspect_plan)
                        .map_err(|error| error.to_string())
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match inspected {
                Ok(entries) => {
                    let existing: HashSet<String> = this
                        .projected_toc()
                        .into_iter()
                        .flat_map(|toc| toc.entries)
                        .map(|entry| normalized_archive_path(&entry.path).to_string())
                        .collect();
                    let collision = entries.iter().find(|entry| {
                        existing.contains(normalized_archive_path(&entry.path))
                    });
                    if let Some(entry) = collision {
                        window.push_notification(
                            gpui_component::notification::Notification::warning(tr!(
                                "Can't add “{name}” because an entry with that name already exists.",
                                name = entry.path
                            )),
                            cx,
                        );
                        return;
                    }
                    this.edits.additions.extend(additions);
                    this.pending_entries.extend(entries);
                    this.refresh_edit_projection(cx);
                }
                Err(message) => window.push_notification(
                    gpui_component::notification::Notification::error(tr!(
                        "Couldn't prepare these items for the archive: {message}",
                        message = message
                    )),
                    cx,
                ),
            });
        })
        .detach();
    }

    fn add_dropped(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.add_dropped_at(paths, String::new(), window, cx);
    }

    pub fn stage_remove_selected(&mut self, cx: &mut Context<Self>) {
        if !self.can_stage_edits() {
            return;
        }
        let selected = self.selected_archive_paths(cx);
        if selected.is_empty() {
            return;
        }

        // A pending source is one journal entry even when it expanded to a
        // whole directory tree. Removing any selected root inside it removes
        // that staged addition and its virtual rows.
        let discarded_addition_roots: Vec<String> = self
            .edits
            .additions
            .iter()
            .map(archive_addition_root)
            .filter(|root| {
                selected.iter().any(|path| {
                    archive_path_at_or_below(path, root) || archive_path_at_or_below(root, path)
                })
            })
            .collect();
        self.edits.additions.retain(|addition| {
            let root = archive_addition_root(addition);
            !discarded_addition_roots.iter().any(|discarded| {
                normalized_archive_path(discarded) == normalized_archive_path(&root)
            })
        });
        let live_roots: Vec<String> = self
            .edits
            .additions
            .iter()
            .map(archive_addition_root)
            .collect();
        self.pending_entries.retain(|entry| {
            live_roots
                .iter()
                .any(|root| archive_path_at_or_below(&entry.path, root))
        });

        for path in selected {
            if discarded_addition_roots.iter().any(|root| {
                archive_path_at_or_below(&path, root) || archive_path_at_or_below(root, &path)
            }) {
                continue;
            }
            let original = unproject_archive_path(&path, &self.edits.renames);
            if !self
                .edits
                .removals
                .iter()
                .any(|root| archive_path_at_or_below(&original, root))
            {
                self.edits
                    .removals
                    .retain(|root| !archive_path_at_or_below(root, &original));
                self.edits.removals.push(original);
            }
        }
        self.refresh_edit_projection(cx);
    }

    fn revert_edits(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.edits = ferail_fs_native::ArchiveEditPlan::default();
        self.pending_entries.clear();
        self.refresh_edit_projection(cx);
    }

    fn confirm_close(
        &mut self,
        target: ArchiveCloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.edits.is_empty() || self.close_prompt_open || self.saving {
            return;
        }
        self.close_prompt_open = true;
        let count = self.edits.change_count();
        let view = cx.entity().downgrade();
        let cancel_view = view.clone();
        let discard_view = view.clone();
        let save_view = view.clone();
        let discard_target = target.clone();
        let save_target = target;
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let cancel_view = cancel_view.clone();
            let discard_view = discard_view.clone();
            let save_view = save_view.clone();
            let discard_target = discard_target.clone();
            let save_target = save_target.clone();
            let dismissed_view = view.clone();
            dialog
                .title(tr!("Save changes before closing?"))
                .child(div().text_scale_sm().child(trn!(
                    "Your {n} unsaved change will be lost if you discard it.",
                    "Your {n} unsaved changes will be lost if you discard them.",
                    count
                )))
                .footer(
                    gpui_component::dialog::DialogFooter::new()
                        .child(
                            Button::new("archive-close-keep")
                                .label(tr!("Keep Editing"))
                                .small()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = cancel_view.update(cx, |this, cx| {
                                        this.close_prompt_open = false;
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            Button::new("archive-close-discard")
                                .label(tr!("Discard Changes"))
                                .danger()
                                .small()
                                .on_click(move |_, window, cx| {
                                    let _ = discard_view.update(cx, |this, _| {
                                        this.close_prompt_open = false;
                                        this.edits = ferail_fs_native::ArchiveEditPlan::default();
                                        this.pending_entries.clear();
                                    });
                                    window.close_dialog(cx);
                                    close_archive_target(discard_target.clone(), cx);
                                }),
                        )
                        .child(
                            Button::new("archive-close-save")
                                .label(tr!("Save Changes"))
                                .primary()
                                .small()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = save_view.update(cx, |this, cx| {
                                        this.close_prompt_open = false;
                                        this.save_edits_with_close(
                                            Some(save_target.clone()),
                                            window,
                                            cx,
                                        );
                                    });
                                }),
                        ),
                )
                .on_cancel(move |_, _, cx| {
                    let _ = dismissed_view.update(cx, |this, cx| {
                        this.close_prompt_open = false;
                        cx.notify();
                    });
                    true
                })
        });
    }

    pub(crate) fn request_dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_prompt_open || self.saving || window.has_active_dialog(cx) {
            return;
        }
        let target = match self.host {
            ToolHostContext::Docked => self.shell.clone().map(ArchiveCloseTarget::Dock),
            ToolHostContext::Windowed => Some(ArchiveCloseTarget::Window(window.window_handle())),
        };
        let Some(target) = target else { return };
        if self.edits.is_empty() {
            match target {
                ArchiveCloseTarget::Window(_) => window.remove_window(),
                ArchiveCloseTarget::Dock(shell) => {
                    let _ = shell.update(cx, |shell, cx| shell.close_active_tool_result(cx));
                }
            }
        } else {
            self.confirm_close(target, window, cx);
        }
    }

    fn on_dismiss(&mut self, _: &ArchiveDismiss, window: &mut Window, cx: &mut Context<Self>) {
        self.request_dismiss(window, cx);
    }

    pub fn rename_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_stage_edits() {
            self.show_edit_rejection(window, cx);
            return;
        }
        let selected = self.selected_archive_paths(cx);
        if selected.len() != 1 {
            return;
        }
        let current = selected[0].clone();
        let leaf = normalized_archive_path(&current)
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(leaf.clone()));
        let view = cx.entity().downgrade();
        let input_for_dialog = input.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let input = input_for_dialog.clone();
            let current = current.clone();
            let view = view.clone();
            let original_leaf = leaf.clone();
            dialog
                .title(tr!("Rename Archive Entry"))
                .child(Input::new(&input).small())
                .on_ok(move |_, window, cx: &mut App| {
                    let name = input.read(cx).value().trim().to_string();
                    if name.is_empty() || name == original_leaf {
                        return true;
                    }
                    if name == "."
                        || name == ".."
                        || name.chars().any(|ch| matches!(ch, '/' | '\\' | '\0'))
                    {
                        window.push_notification(
                            gpui_component::notification::Notification::warning(tr!(
                                "Archive entry names can't contain slashes or be “.” or “..”."
                            )),
                            cx,
                        );
                        return false;
                    }
                    let parent = normalized_archive_path(&current)
                        .rsplit_once('/')
                        .map(|(parent, _)| parent);
                    let to = parent
                        .map(|parent| format!("{parent}/{name}"))
                        .unwrap_or(name);
                    let staged = view
                        .update(cx, |this, cx| {
                            if this.rename_would_collide(&current, &to) {
                                return false;
                            }
                            this.stage_rename(current.clone(), to, cx);
                            true
                        })
                        .unwrap_or(false);
                    if !staged {
                        window.push_notification(
                            gpui_component::notification::Notification::warning(tr!(
                                "An archive entry with that name already exists."
                            )),
                            cx,
                        );
                    }
                    staged
                })
        });
        window.on_next_frame(move |window, cx| {
            input.read(cx).focus_handle(cx).focus(window, cx);
            window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        });
    }

    fn rename_would_collide(&self, current: &str, to: &str) -> bool {
        if archive_path_at_or_below(to, current) {
            return true;
        }
        let Some(toc) = self.projected_toc() else {
            return true;
        };
        let stationary: HashSet<String> = toc
            .entries
            .iter()
            .filter(|entry| !archive_path_at_or_below(&entry.path, current))
            .map(|entry| normalized_archive_path(&entry.path).to_string())
            .collect();
        toc.entries
            .iter()
            .filter(|entry| archive_path_at_or_below(&entry.path, current))
            .map(|entry| replace_archive_root(&entry.path, current, to))
            .any(|target| stationary.contains(normalized_archive_path(&target)))
    }

    fn stage_rename(&mut self, current: String, to: String, cx: &mut Context<Self>) {
        let original = unproject_archive_path(&current, &self.edits.renames);
        let base_from = normalized_archive_path(&original).to_string();
        if let Some(existing) = self
            .edits
            .renames
            .iter_mut()
            .find(|rename| normalized_archive_path(&rename.from) == base_from)
        {
            existing.to = to;
        } else {
            self.edits.renames.push(ferail_fs_native::ArchiveRename {
                from: base_from,
                to,
            });
        }
        self.refresh_edit_projection(cx);
    }

    fn save_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_edits_with_close(None, window, cx);
    }

    fn save_edits_with_close(
        &mut self,
        close_target: Option<ArchiveCloseTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.edits.is_empty() || self.saving {
            return;
        }
        if let Some(reason) = self.edit_rejection() {
            window.push_notification(
                gpui_component::notification::Notification::warning(reason),
                cx,
            );
            return;
        }
        let (Some(shell), Some(stamp)) = (self.shell.clone(), self.stamp) else {
            return;
        };
        self.saving = true;
        let archive = self.archive_path.clone();
        let plan = self.edits.clone();
        let password = self.password.clone();
        let this = cx.entity().downgrade();
        let settled: crate::shell::ArchiveOpSettled = Box::new(move |success, _shell, cx| {
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                if success {
                    let password = this.password.clone();
                    this.start_load(password, cx);
                } else {
                    cx.notify();
                }
            });
            if success {
                if let Some(close_target) = close_target {
                    match close_target {
                        ArchiveCloseTarget::Window(close_window) => {
                            let _ = close_window.update(cx, |_, window, _| window.remove_window());
                        }
                        ArchiveCloseTarget::Dock(_) => _shell.close_active_tool_result(cx),
                    }
                }
            }
        });
        let _ = shell.update(cx, |shell, scx| {
            shell.save_archive_edits(
                crate::shell::ArchiveSaveRequest {
                    archive,
                    stamp,
                    plan,
                    password,
                },
                Some(settled),
                window,
                scx,
            );
        });
        cx.notify();
    }

    fn convert_archive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.load, ArchiveLoad::Loaded(_)) || !self.edits.is_empty() || self.saving {
            return;
        }
        let Some(shell) = self.shell.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        let source = self.archive_path.clone();
        let source_format = self.format;
        let known_password = self.password.clone();
        let app: &mut App = std::borrow::BorrowMut::borrow_mut(cx);
        crate::archive_convert::open_dialog(
            source,
            source_format,
            known_password,
            shell,
            window,
            app,
        );
    }

    // -- rendering ----------------------------------------------------------

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let name = self
            .archive_path
            .file_name()
            .map(|s| crate::private_mode::present_leaf_str(&s.to_string_lossy(), false))
            .unwrap_or_default();
        let label: SharedString = self
            .format
            .map(|f| SharedString::new_static(f.label()))
            .unwrap_or_else(|| tr!("Archive"));

        let subtitle = match &self.load {
            ArchiveLoad::Loading => tr!("Reading\u{2026}").to_string(),
            ArchiveLoad::NeedsPassword { .. } => {
                tr!("{label} \u{00b7} encrypted", label = label).to_string()
            }
            ArchiveLoad::Failed(_) => tr!("Unreadable").to_string(),
            ArchiveLoad::Loaded(_) => {
                let toc = self.projected_toc().unwrap_or_default();
                // `label · N files · size · encrypted`, each piece a whole
                // phrase so translations stay word-order independent.
                let mut parts = vec![
                    label.to_string(),
                    trn!("{n} file", "{n} files", toc.file_count()).to_string(),
                ];
                if let (Some(stamp), Some(unpacked)) = (self.stamp, toc.total_uncompressed()) {
                    parts.push(
                        tr!(
                            "{packed} archive → {unpacked} unpacked",
                            packed = ferail_fs_native::humanize_bytes(
                                crate::private_mode::present_bytes(0x4152_504b, stamp.byte_len())
                            ),
                            unpacked = ferail_fs_native::humanize_bytes(
                                crate::private_mode::present_bytes(0x4152_554e, unpacked)
                            )
                        )
                        .to_string(),
                    );
                } else if let Some(stamp) = self.stamp {
                    parts.push(ferail_fs_native::humanize_bytes(
                        crate::private_mode::present_bytes(0x4152_504b, stamp.byte_len()),
                    ));
                }
                if toc.needs_password {
                    parts.push(tr!("encrypted").to_string());
                }
                parts.join(" \u{00b7} ")
            }
        };

        let selected = self.selection_count(cx);
        let can_extract = matches!(self.load, ArchiveLoad::Loaded(_))
            && self.caps().is_some_and(|c| c.can_extract)
            && !self.locked_contents();
        let read_only = self.format.is_some() && !self.is_plain_editable_zip();
        let loaded = matches!(self.load, ArchiveLoad::Loaded(_));
        let changes = self.edits.change_count();
        let editable = self.can_stage_edits();
        let extract_selected_label = if selected > 0 {
            tr!("Extract {selected} Selected", selected = selected)
        } else {
            tr!("Extract Selected")
        };

        let remove_button = Button::new("archive-remove-selected")
            .small()
            .icon(gpui_component::Icon::empty().path("icons/trash.svg"))
            .tooltip(tr!("Remove the selection when changes are saved"));

        let extract_selected_button = Button::new("archive-extract-selected")
            .small()
            .icon(gpui_component::Icon::empty().path("icons/file/archive.svg"))
            .tooltip(extract_selected_label);

        let extract_to_button = Button::new("archive-extract-to")
            .small()
            .icon(gpui_component::Icon::empty().path("icons/nav/folder.svg"))
            .tooltip(tr!("Extract to a folder you choose (selection, or all)"));

        let extract_all_button = Button::new("archive-extract-all")
            .small()
            .icon(gpui_component::Icon::empty().path("icons/nav/downloads.svg"))
            .tooltip(tr!("Extract All"));

        let convert_tooltip = if changes > 0 {
            tr!("Save or revert archive changes before converting.")
        } else {
            tr!("Convert this archive to another format")
        };
        let convert_button = Button::new("archive-convert")
            .small()
            .icon(gpui_component::Icon::empty().path("icons/redo.svg"))
            .tooltip(convert_tooltip.clone());

        h_flex()
            .w_full()
            .px_2()
            .py_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .flex_1()
                    // Floor the name's share: with four controls to its right
                    // an unbounded flex child collapses to a couple of glyphs.
                    .min_w(px(112.0))
                    // Filenames truncate in the middle (house style: keeps the
                    // start and the extension); the subtitle is free-form, so
                    // its tail is expendable. Without these the text wrapped a
                    // character per line once the pane got narrow.
                    .child(div().w_full().truncate_middle().text_scale_md().child(name))
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_scale_xs()
                            .text_color(theme.muted_foreground)
                            .child(subtitle),
                    ),
            )
            .when(read_only, |this| {
                this.child(
                    h_flex()
                        .gap_0p5()
                        .items_center()
                        .px_1p5()
                        .py_0p5()
                        .rounded_md()
                        .bg(theme.muted)
                        .text_scale_xs()
                        .text_color(theme.muted_foreground)
                        .child(
                            svg()
                                .path("icons/lock.svg")
                                .icon_px(11.0)
                                .text_color(theme.muted_foreground),
                        )
                        .child(tr!("Read-only")),
                )
            })
            .when(changes > 0, |this| {
                this.child(
                    div()
                        .px_1p5()
                        .py_0p5()
                        .rounded_md()
                        .bg(theme.accent.opacity(0.10))
                        .text_scale_xs()
                        .text_color(theme.accent)
                        .child(if self.saving {
                            tr!("Saving changes…")
                        } else {
                            trn!("{n} unsaved change", "{n} unsaved changes", changes)
                        }),
                )
            })
            // Below this the filter box would leave the name a few pixels, so
            // drop it: the name and the extract verbs matter more, and the
            // pane can always be widened (or popped out) to filter.
            .when(
                loaded && self.host_width.unwrap_or(f32::MAX) >= FILTER_MIN_WIDTH,
                |this| {
                    this.child(div().w(px(180.0)).flex_shrink_0().child(
                        if crate::private_mode::enabled() {
                            div()
                                .h(px(28.0))
                                .w_full()
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.border)
                                .text_scale_sm()
                                .text_color(theme.muted_foreground)
                                .child(tr!("Private"))
                                .into_any_element()
                        } else {
                            Input::new(&self.filter_input).small().into_any_element()
                        },
                    ))
                },
            )
            .child(
                convert_button
                    .disabled(!loaded || changes > 0 || self.saving)
                    .tooltip(convert_tooltip)
                    .on_click(cx.listener(|this, _, window, cx| this.convert_archive(window, cx))),
            )
            .child(
                Button::new("archive-preview")
                    .small()
                    .icon(gpui_component::Icon::empty().path("icons/eye.svg"))
                    .tooltip(if self.preview_enabled {
                        tr!("Hide preview")
                    } else {
                        tr!("Preview selected file")
                    })
                    .selected(self.preview_enabled)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_preview(window, cx))),
            )
            .when(self.is_plain_editable_zip() && loaded, |this| {
                this.child(
                    remove_button
                        .disabled(!editable || selected == 0)
                        .tooltip(tr!("Remove the selection when changes are saved"))
                        .on_click(
                            cx.listener(|this, _, _window, cx| this.stage_remove_selected(cx)),
                        ),
                )
            })
            .when(changes > 0, |this| {
                this.child(
                    Button::new("archive-revert-edits")
                        .label(tr!("Revert"))
                        .small()
                        .disabled(self.saving)
                        .on_click(cx.listener(|this, _, _window, cx| this.revert_edits(cx))),
                )
                .child(
                    Button::new("archive-save-edits")
                        .label(tr!("Save Changes"))
                        .small()
                        .primary()
                        .disabled(self.saving)
                        .on_click(cx.listener(|this, _, window, cx| this.save_edits(window, cx))),
                )
            })
            .child(
                extract_selected_button
                    .disabled(!can_extract || selected == 0)
                    .on_click(cx.listener(|this, _, window, cx| this.extract_selected(window, cx))),
            )
            .when(
                self.host == ToolHostContext::Windowed && self.dock_owner.is_some(),
                |this| {
                    let dock = self.dock_owner.clone();
                    let path = self.archive_path.clone();
                    this.child(
                        Button::new("archive-dock")
                            .small()
                            .icon(gpui_component::Icon::empty().path("icons/minimize.svg"))
                            .tooltip(tr!("Dock in tab"))
                            .on_click(cx.listener(move |_, _, window, cx| {
                                let view = cx.entity().clone();
                                let app: &mut App = std::borrow::BorrowMut::borrow_mut(cx);
                                let (dock, path) = (dock.clone(), path.clone());
                                if let Some(dock) = dock {
                                    app.defer(move |cx| dock(path, view, cx));
                                }
                                window.remove_window();
                            })),
                    )
                },
            )
            .child(
                extract_to_button
                    .disabled(!can_extract)
                    .on_click(cx.listener(|this, _, window, cx| this.extract_to(window, cx))),
            )
            .child(
                extract_all_button
                    .disabled(!can_extract)
                    .on_click(cx.listener(|this, _, window, cx| this.extract_all(window, cx))),
            )
            .into_any_element()
    }

    fn password_form(
        &self,
        prompt: SharedString,
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
                    .child(prompt),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(px(220.0))
                            .child(Input::new(&self.password_input).small()),
                    )
                    .child(
                        Button::new("archive-unlock")
                            .label(tr!("Unlock"))
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

    fn centered_message(
        &self,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
}

pub(crate) fn normalized_archive_path(path: &str) -> &str {
    path.trim_matches('/')
}

pub(crate) fn archive_path_at_or_below(path: &str, root: &str) -> bool {
    let path = normalized_archive_path(path);
    let root = normalized_archive_path(root);
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|tail| tail.starts_with('/'))
}

fn archive_path_matches_any(path: &str, roots: &[String]) -> bool {
    roots
        .iter()
        .any(|root| archive_path_at_or_below(path, root))
}

fn replace_archive_root(path: &str, from: &str, to: &str) -> String {
    let trailing = path.ends_with('/');
    let path = normalized_archive_path(path);
    let from = normalized_archive_path(from);
    let suffix = path.strip_prefix(from).unwrap_or_default();
    let mut result = format!("{}{suffix}", normalized_archive_path(to));
    if trailing {
        result.push('/');
    }
    result
}

pub(crate) fn project_archive_path(
    original: &str,
    renames: &[ferail_fs_native::ArchiveRename],
) -> String {
    let path = normalized_archive_path(original);
    let best = renames
        .iter()
        .filter(|rename| archive_path_at_or_below(path, &rename.from))
        .max_by_key(|rename| normalized_archive_path(&rename.from).len());
    let Some(rename) = best else {
        return original.to_string();
    };
    replace_archive_root(original, &rename.from, &rename.to)
}

pub(crate) fn unproject_archive_path(
    current: &str,
    renames: &[ferail_fs_native::ArchiveRename],
) -> String {
    let trailing = current.ends_with('/');
    let path = normalized_archive_path(current);
    let best = renames
        .iter()
        .filter(|rename| archive_path_at_or_below(path, &rename.to))
        .max_by_key(|rename| normalized_archive_path(&rename.to).len());
    let Some(rename) = best else {
        return current.to_string();
    };
    let to = normalized_archive_path(&rename.to);
    let suffix = path.strip_prefix(to).unwrap_or_default();
    let mut result = format!("{}{suffix}", normalized_archive_path(&rename.from));
    if trailing {
        result.push('/');
    }
    result
}

pub(crate) fn archive_addition_root(addition: &ferail_fs_native::ArchiveAddition) -> String {
    let leaf = addition
        .source
        .file_name()
        .map(|name| name.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let destination = normalized_archive_path(&addition.destination);
    if destination.is_empty() {
        leaf
    } else {
        format!("{destination}/{leaf}")
    }
}

impl Render for ArchiveView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.host == ToolHostContext::Windowed {
            let raw_name = self
                .archive_path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            window.set_window_title(&tr!(
                "Archive: {name}",
                name = crate::private_mode::present_leaf_str(&raw_name, false)
            ));
        }
        let bg = cx.theme().background;
        let border = cx.theme().border;
        let accent = cx.theme().accent;
        let danger = cx.theme().danger;
        let header = self.header(cx);
        let drop_allowed = self.can_stage_edits();
        let native_archive_dragging = crate::file_list::native_archive_drag_active();
        let feedback_allowed = self.drop_feedback_allowed;
        let drag_message = if cx.has_active_drag() || native_archive_dragging {
            self.drop_feedback.clone()
        } else {
            None
        };

        let locked_strip = self.locked_contents().then(|| {
            let form = self.password_form(
                tr!("This archive's contents are encrypted. Enter its password to extract."),
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
            ArchiveLoad::Loading => self.centered_message(tr!("Reading archive\u{2026}"), cx),
            ArchiveLoad::NeedsPassword { error } => {
                let error = error.clone();
                let form = self.password_form(
                    tr!("This archive is encrypted. Enter its password to view the contents."),
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
            ArchiveLoad::Failed(e) => self.centered_message(e.clone(), cx),
            // The contents ARE the app's normal file table: columns,
            // selection, sort and row lines all come from it.
            ArchiveLoad::Loaded(_) => DataTable::new(&self.table)
                .bordered(false)
                .stripe(true)
                .small()
                .into_any_element(),
        };

        let view = cx.entity().clone();
        let content = v_flex()
            .track_focus(&self.focus_handle)
            .key_context(ARCHIVE_CONTEXT)
            .on_action(cx.listener(|this, _: &OpenSelected, window, cx| {
                this.activate_lead(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(Self::on_dismiss))
            .size_full()
            .bg(bg)
            .on_prepaint(move |bounds, _, cx| {
                view.update(cx, |this, cx| {
                    this.update_host_width(f32::from(bounds.size.width), cx);
                });
            })
            .drag_over::<ExternalPaths>(move |style, _, _, cx| {
                if drop_allowed {
                    style
                        .cursor_copy()
                        .border_2()
                        .border_color(cx.theme().accent)
                        .bg(cx.theme().accent.opacity(0.08))
                } else {
                    style
                        .cursor_not_allowed()
                        .border_2()
                        .border_color(cx.theme().danger)
                        .bg(cx.theme().danger.opacity(0.06))
                }
            })
            .on_drag_move(cx.listener(
                |this, event: &gpui::DragMoveEvent<ExternalPaths>, window, cx| {
                    if !event.bounds.contains(&event.event.position) {
                        return;
                    }
                    let allowed = this.can_stage_edits();
                    let count = event.drag(cx).paths().len();
                    if this.drop_feedback_count != Some(count)
                        || this.drop_feedback_allowed != allowed
                    {
                        let message = this.edit_rejection().unwrap_or_else(|| {
                            trn!(
                                "Drop to add {n} item to the archive",
                                "Drop to add {n} items to the archive",
                                count
                            )
                        });
                        this.drop_feedback = Some(message);
                        this.drop_feedback_count = Some(count);
                        this.drop_feedback_allowed = allowed;
                        cx.notify();
                    }
                    let cursor = if allowed {
                        gpui::CursorStyle::DragCopy
                    } else {
                        gpui::CursorStyle::OperationNotAllowed
                    };
                    if cx.active_drag_cursor_style() != Some(cursor) {
                        cx.set_active_drag_cursor_style(cursor, window);
                    }
                },
            ))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.add_dropped(paths.paths().to_vec(), window, cx);
            }))
            // An archive entry dropped back on its own workbench is a
            // cancelled drag, not an extraction request. In the docked form
            // the Shell's ordinary folder target is an ancestor of this
            // element, so stopping propagation here is what keeps a release
            // over empty table space from extracting into `current_dir`.
            .drag_over::<crate::file_list::ArchiveEntryDrag>(|style, _, _, cx| {
                style
                    .cursor_not_allowed()
                    .border_2()
                    .border_color(cx.theme().danger)
                    .bg(cx.theme().danger.opacity(0.06))
            })
            .on_drag_move(cx.listener(
                |this,
                 event: &gpui::DragMoveEvent<crate::file_list::ArchiveEntryDrag>,
                 window,
                 cx| {
                    if !event.bounds.contains(&event.event.position) {
                        return;
                    }
                    let message = tr!("Drop outside the archive to extract");
                    if this.drop_feedback.as_ref() != Some(&message) || this.drop_feedback_allowed {
                        this.drop_feedback = Some(message);
                        this.drop_feedback_count = None;
                        this.drop_feedback_allowed = false;
                        cx.notify();
                    }
                    if cx.active_drag_cursor_style() != Some(gpui::CursorStyle::OperationNotAllowed)
                    {
                        cx.set_active_drag_cursor_style(
                            gpui::CursorStyle::OperationNotAllowed,
                            window,
                        );
                    }
                },
            ))
            .on_drop(cx.listener(
                |_this, _drag: &crate::file_list::ArchiveEntryDrag, _window, cx| {
                    cx.stop_propagation();
                },
            ))
            // Cross-window native file promises no longer carry GPUI's
            // custom payload. The platform still delivers MouseMove/MouseUp;
            // explicitly reject those here so the docked pane's parent can
            // never interpret the release as extraction into current_dir.
            .on_mouse_move(cx.listener(|this, _event, _window, cx| {
                if !crate::file_list::native_archive_drag_active() {
                    return;
                }
                let message = tr!("Drop outside the archive to extract");
                if this.drop_feedback.as_ref() != Some(&message) || this.drop_feedback_allowed {
                    this.drop_feedback = Some(message);
                    this.drop_feedback_count = None;
                    this.drop_feedback_allowed = false;
                    cx.notify();
                }
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|_this, _event, _window, cx| {
                    if cx.has_active_drag() {
                        return;
                    }
                    if crate::file_list::take_native_archive_drag().is_some() {
                        crate::log_info!(100, "archive-drag: rejected by archive workbench");
                        cx.stop_propagation();
                    }
                }),
            )
            .when(native_archive_dragging, move |style| {
                style.cursor_not_allowed().border_2().border_color(danger)
            })
            .child(header)
            .children(locked_strip)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(div().flex_1().min_w_0().child(body))
                    // Windowed only: docked, the Shell's pane shows it.
                    .when_some(
                        self.preview_enabled
                            .then(|| self.preview_panel.clone())
                            .flatten(),
                        |this, panel| {
                            this.child(
                                div()
                                    .w(px(360.0))
                                    .flex_none()
                                    .border_l_1()
                                    .border_color(border)
                                    .child(panel),
                            )
                        },
                    ),
            )
            // Drag feedback is a footer, not a banner: appearing above the
            // table would push every row down mid-drag, moving the drop
            // target out from under the pointer. Below the `flex_1` body the
            // rows keep their position and the list gives up the height.
            .children(drag_message.map(|message| {
                let color = if feedback_allowed { accent } else { danger };
                div()
                    .w_full()
                    .flex_none()
                    .px_3()
                    .py_1p5()
                    .border_t_1()
                    .border_color(color)
                    .bg(color.opacity(0.08))
                    .text_scale_sm()
                    .text_color(color)
                    .child(message)
            }))
            .into_any_element();
        crate::private_mode::protect(content, cx)
    }
}

/// Open `view` in its own window (the pop-out half of dock/undock).
///
/// A separate window is what makes dragging *into* an archive practical:
/// Finder or a second Ferail window can sit alongside it. Mirrors
/// [`crate::disk_usage::open_existing_window`].
pub fn open_existing_window(
    archive: PathBuf,
    view: Entity<ArchiveView>,
    dock_owner: Option<ArchiveDockOwner>,
    cx: &mut App,
) -> Result<WindowHandle<gpui_component::Root>, anyhow::Error> {
    view.update(cx, |view, cx| view.set_dock_owner(dock_owner, cx));
    let raw_name = archive
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let menu_label = tr!(
        "Archive: {name}",
        name = crate::private_mode::present_leaf_str(&raw_name, false)
    );
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(900.0), px(640.0)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(menu_label.clone()),
            ..Default::default()
        }),
        ..crate::base_window_options()
    };
    let close_view = view.downgrade();
    let handle = cx.open_window(opts, move |window, cx| {
        window.on_window_should_close(cx, {
            let close_view = close_view.clone();
            move |window, cx| {
                let Some(view) = close_view.upgrade() else {
                    return true;
                };
                if view.read(cx).edits.is_empty() {
                    return true;
                }
                let target = ArchiveCloseTarget::Window(window.window_handle());
                view.update(cx, |view, cx| view.confirm_close(target, window, cx));
                false
            }
        });
        let target_window = window.window_handle();
        let escape_view = view.downgrade();
        let escape_subscription = cx.intercept_keystrokes(move |event, window, app| {
            if event.keystroke.key != "escape"
                || window.window_handle() != target_window
                || window.has_active_dialog(app)
            {
                return;
            }
            let _ = escape_view.update(app, |view, cx| view.request_dismiss(window, cx));
            app.stop_propagation();
        });
        view.update(cx, |view, _| {
            view._escape_subscription = Some(escape_subscription)
        });
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    })?;
    crate::process_state::process_state(cx)
        .register_aux_window(handle.into(), menu_label.to_string());
    crate::boot::refresh_window_menu(cx);
    Ok(handle)
}
