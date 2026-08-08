//! Archive workbench — an embedded tool-result view (peer to Disk Usage and
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
//! A real archive is deep — a macOS app bundle runs to thousands of entries —
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
    button::Button,
    h_flex,
    input::{Input, InputState},
    v_flex, ActiveTheme, Disableable, ElementExt as _, Selectable as _, Sizable, WindowExt as _,
};

use ferail_archive::{ArchiveTree, Capabilities, Format, Toc, TreeRow};
use ferail_core::{EntryKind, FileEntry, NodeId};
use ferail_fs_native::ArchiveError;

use crate::file_list::FileListDelegate;
use crate::multi_table::{DataTable, TableEvent, TableState};
use crate::shell::Shell;
use crate::text::{TextScale as _, TruncateMiddle as _};
use crate::tool_results::{ToolHostContext, ToolHostEvent};

/// Callback a standalone archive window uses to dock itself back into a tab.
/// Mirrors `disk_usage::DockOwner`.
pub type ArchiveDockOwner = std::rc::Rc<dyn Fn(PathBuf, Entity<ArchiveView>, &mut App)>;

/// Narrower than this and the header hides its filter box so the archive's
/// name keeps a usable share of the row.
const FILTER_MIN_WIDTH: f32 = 820.0;

/// Above this, previewing asks first. An archive entry has to be written out
/// before Quick Look can read it (it is an OS service that takes a file URL),
/// and the table of contents gives us the uncompressed size *before* we spend
/// anything — so the check is free and also caps decompression bombs.
const PREVIEW_CONFIRM_BYTES: u64 = 100 * 1024 * 1024;

/// Ceiling for decoding an entry in memory. Above this we stage to disk
/// instead of holding the whole thing — text and images worth previewing are
/// far below it.
const PREVIEW_INMEMORY_CAP: u64 = 16 * 1024 * 1024;

/// Key context for the archive pane (keymap bindings hang off this).
pub const ARCHIVE_CONTEXT: &str = "Archive";

/// The off-thread load state of the archive's table of contents.
enum ArchiveLoad {
    Loading,
    Loaded(Box<Toc>),
    /// The archive's *listing itself* is encrypted (7z header encryption), so
    /// nothing can be shown until a password is supplied. `error` is set after
    /// a rejected attempt.
    NeedsPassword { error: Option<String> },
    Failed(String),
}

pub struct ArchiveView {
    archive_path: PathBuf,
    /// Resolved off-thread — by extension when it names a format, otherwise by
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
    /// Kept alive for the lifetime of the view — dropping it stops selection.
    _table_sub: Subscription,
    /// Where this view currently lives, and how to dock it back when windowed.
    host: ToolHostContext,
    dock_owner: Option<ArchiveDockOwner>,
    shell: Option<WeakEntity<Shell>>,
    password_input: Entity<InputState>,
    password: Option<String>,
    /// Own preview panel, used when this workbench lives in its own window —
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
    /// Scratch directory holding entries written out for preview, removed when
    /// this view is dropped. Created lazily on the first preview.
    scratch: Option<PathBuf>,
    /// Archive path of the entry currently staged for preview, so re-selecting
    /// the same row doesn't extract it twice.
    previewed: Option<String>,
    /// The scratch file backing the current preview. At most one exists at a
    /// time: staging a new entry, closing the preview, or dropping the view
    /// removes it, so archive contents are never in the clear for longer than
    /// they are on screen.
    staged_file: Option<PathBuf>,
    focus_handle: FocusHandle,
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
                    if *click_count >= 2 {
                        // Double-click opens a folder rather than "opening" a
                        // file that has no path on disk.
                        let path = table
                            .read(cx)
                            .delegate()
                            .archive_path_for_row(*row_ix)
                            .map(str::to_string);
                        if let Some(path) = path {
                            this.toggle_expanded(&path, cx);
                            return;
                        }
                    }
                    let modifiers = *modifiers;
                    let row_ix = *row_ix;
                    table.update(cx, |t, cx| {
                        t.delegate_mut().apply_click_gesture(row_ix, modifiers);
                        cx.notify();
                    });
                    this.preview_selection(_window, cx);
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
                _ => {}
            },
        );
        let password_input =
            cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Password"));
        let filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter contents\u{2026}"));
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
            host_width: None,
            preview_panel: None,
            preview_enabled: false,
            scratch: None,
            previewed: None,
            staged_file: None,
            focus_handle: cx.focus_handle(),
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

    /// Record the pane width. Guarded so an unchanged width doesn't notify —
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
            self.discard_staged();
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
    fn discard_staged(&mut self) {
        if let Some(path) = self.staged_file.take() {
            ferail_fs_native::scratch::remove_staged(&path);
        }
    }

    /// Force the preview toggle off — used when the preview pane is closed
    /// from its own header, so the two controls can't disagree.
    pub fn set_preview_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.preview_enabled == enabled {
            return;
        }
        self.preview_enabled = enabled;
        if !enabled {
            self.previewed = None;
            self.discard_staged();
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

    /// The selected row when it is exactly one non-directory entry — preview is
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
                .title(SharedString::from("Preview this file?"))
                .child(
                    div()
                        .text_scale_sm()
                        .child(format!(
                            "\u{201c}{name}\u{201d} is {human}. Previewing it writes a temporary copy out of the archive first."
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

    /// Decode an entry in memory when one of our own renderers can draw it —
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
                // Not something we can draw — stage it for Quick Look.
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
        let scratch = match self.scratch_dir() {
            Some(dir) => dir,
            None => return,
        };
        // Supersede the previous one immediately — never two at once.
        self.discard_staged();
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
                    // The entry's own directory chain is now empty — drop it so
                    // the folder names don't linger either.
                    if let Some(top) = entry.split('/').next() {
                        if top != entry {
                            let _ = std::fs::remove_dir_all(scratch.join(top));
                        }
                    }
                    Some(staged)
                })
                .await;
            if let Some(staged) = staged {
                let _ = this.update(cx, |this: &mut ArchiveView, cx| {
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

    /// Per-view scratch directory, created on first use.
    ///
    /// # Privacy
    ///
    /// Previewing an archive entry means writing it out in the clear: Quick
    /// Look is a separate OS process that reads a file URL, so an encrypted or
    /// hashed *payload* could not be rendered at all. What we can control is
    /// everything around it:
    ///
    /// - It lives under `std::env::temp_dir()`, which on macOS is the
    ///   per-user `$TMPDIR` (`/var/folders/…/T`, mode 0700) rather than the
    ///   world-readable `/tmp`.
    /// - The directory is created 0700 and each staged file 0600, so on a
    ///   shared machine no other user can read them.
    /// - Staged files are named by a hash of the entry path, so a leftover
    ///   file leaks nothing through its *name* — "salary-review.pdf" is
    ///   metadata even when the bytes are unreadable.
    /// - The directory carries our PID, and
    ///   `ferail_fs_native::scratch::sweep_stale_scratch` deletes the dirs of
    ///   dead processes at startup. `Drop` covers a clean exit; the sweep
    ///   covers crashes and kills, which no in-process cleanup can.
    ///
    /// The mechanics live in `ferail_fs_native::scratch` — filesystem work
    /// belongs behind that boundary, and it is where they can be tested.
    fn scratch_dir(&mut self) -> Option<PathBuf> {
        if let Some(dir) = &self.scratch {
            return Some(dir.clone());
        }
        let dir = ferail_fs_native::scratch::scratch_dir()?;
        self.scratch = Some(dir.clone());
        Some(dir)
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

    /// The archive this view is showing — the shell needs it to re-dock.
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
                        "This file isn't an archive Ferail can open.".to_string(),
                    );
                    cx.notify();
                });
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    ferail_fs_native::read_archive_toc(&path, attempt.as_deref())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.format = Some(format);
                match result {
                    Ok(toc) => {
                        this.password = password;
                        this.tree = ArchiveTree::build(&toc);
                        // Open the single root folder straight away — an
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
                    name: row.name.clone(),
                    display_name: row.name.clone(),
                    name_has_hazards: false,
                    kind: if row.is_dir {
                        EntryKind::Directory
                    } else {
                        EntryKind::File
                    },
                    size,
                    mtime_unix: row.mtime_unix.unwrap_or(0),
                    display_size: if row.is_dir {
                        String::new()
                    } else {
                        ferail_fs_native::humanize_bytes(size)
                    },
                    display_kind: ferail_fs_native::describe_kind(
                        if row.is_dir { EntryKind::Directory } else { EntryKind::File },
                        &row.name,
                    ),
                    display_magic: String::new(),
                    display_description: String::new(),
                    is_quarantined: false,
                    quarantine: None,
                    hidden: false,
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
    /// folder, which fails outright when that volume is read-only or full —
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

    /// Add files dropped from Finder or the file list into this archive.
    fn add_dropped(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if paths.is_empty() || !self.caps().is_some_and(|c| c.can_edit_in_place) {
            return;
        }
        let Some(shell) = self.shell.clone() else {
            return;
        };
        let archive = self.archive_path.clone();
        let password = self.password.clone();
        let this = cx.entity().downgrade();
        let refresh: crate::shell::ArchiveOpDone = Box::new(move |_shell, cx| {
            let _ = this.update(cx, |this: &mut ArchiveView, cx| {
                let password = this.password.clone();
                this.start_load(password, cx);
            });
        });
        let _ = shell.update(cx, |shell, scx| {
            shell.add_to_archive_from(archive, paths, password, Some(refresh), window, scx);
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
        let label = self.format.map(|f| f.label()).unwrap_or("Archive");

        let subtitle = match &self.load {
            ArchiveLoad::Loading => "Reading\u{2026}".to_string(),
            ArchiveLoad::NeedsPassword { .. } => format!("{label} \u{00b7} encrypted"),
            ArchiveLoad::Failed(_) => "Unreadable".to_string(),
            ArchiveLoad::Loaded(toc) => {
                let files = toc.file_count();
                let size = toc
                    .total_uncompressed()
                    .map(|b| format!(" \u{00b7} {}", ferail_fs_native::humanize_bytes(b)))
                    .unwrap_or_default();
                let encrypted = if toc.needs_password {
                    " \u{00b7} encrypted"
                } else {
                    ""
                };
                format!(
                    "{label} \u{00b7} {files} file{}{size}{encrypted}",
                    if files == 1 { "" } else { "s" }
                )
            }
        };

        let selected = self.selection_count(cx);
        let can_extract = matches!(self.load, ArchiveLoad::Loaded(_))
            && self.caps().is_some_and(|c| c.can_extract)
            && !self.locked_contents();
        let read_only = self.caps().is_some_and(|c| c.is_read_only());
        let loaded = matches!(self.load, ArchiveLoad::Loaded(_));

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
                    // Floor the name's share: with four controls to its right
                    // an unbounded flex child collapses to a couple of glyphs.
                    .min_w(px(140.0))
                    // Filenames truncate in the middle (house style — keeps the
                    // start and the extension); the subtitle is free-form, so
                    // its tail is expendable. Without these the text wrapped a
                    // character per line once the pane got narrow.
                    .child(
                        div()
                            .w_full()
                            .truncate_middle()
                            .text_scale_md()
                            .child(name),
                    )
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
            // Below this the filter box would leave the name a few pixels, so
            // drop it: the name and the extract verbs matter more, and the
            // pane can always be widened (or popped out) to filter.
            .when(loaded && self.host_width.unwrap_or(f32::MAX) >= FILTER_MIN_WIDTH, |this| {
                this.child(
                    div()
                        .w(px(180.0))
                        .flex_shrink_0()
                        .child(Input::new(&self.filter_input).small()),
                )
            })
            .child(
                Button::new("archive-preview")
                    .small()
                    .icon(gpui_component::Icon::empty().path("icons/eye.svg"))
                    .tooltip(if self.preview_enabled {
                        "Hide preview"
                    } else {
                        "Preview selected file"
                    })
                    .selected(self.preview_enabled)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_preview(window, cx))),
            )
            .child(
                Button::new("archive-extract-selected")
                    .label(if selected > 0 {
                        format!("Extract {selected} Selected")
                    } else {
                        "Extract Selected".to_string()
                    })
                    .small()
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
                            .tooltip("Dock in tab")
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
                Button::new("archive-extract-to")
                    .label("Extract To\u{2026}")
                    .small()
                    .tooltip("Extract to a folder you choose (selection, or all)")
                    .disabled(!can_extract)
                    .on_click(cx.listener(|this, _, window, cx| this.extract_to(window, cx))),
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

    fn password_form(&self, prompt: &str, error: Option<&str>, cx: &mut Context<Self>) -> AnyElement {
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
}

impl Drop for ArchiveView {
    fn drop(&mut self) {
        // Entries written out for preview are ours alone — take them with us.
        if let Some(dir) = &self.scratch {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl Render for ArchiveView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let bg = theme.background;
        let border = theme.border;
        let header = self.header(cx);
        let editable = self.caps().is_some_and(|c| c.can_edit_in_place);

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
            ArchiveLoad::Loading => self.centered_message("Reading archive\u{2026}", cx),
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
            ArchiveLoad::Failed(e) => self.centered_message(e.clone(), cx),
            // The contents ARE the app's normal file table — columns,
            // selection, sort and row lines all come from it.
            ArchiveLoad::Loaded(_) => DataTable::new(&self.table)
                .bordered(false)
                .stripe(true)
                .small()
                .into_any_element(),
        };

        let view = cx.entity().clone();
        v_flex()
            .track_focus(&self.focus_handle)
            .key_context(ARCHIVE_CONTEXT)
            .size_full()
            .bg(bg)
            .on_prepaint(move |bounds, _, cx| {
                view.update(cx, |this, cx| {
                    this.update_host_width(f32::from(bounds.size.width), cx);
                });
            })
            .when(editable, |this| {
                this.drag_over::<ExternalPaths>(|style, _, _, cx| {
                    style
                        .border_2()
                        .border_color(cx.theme().accent)
                        .bg(cx.theme().accent.opacity(0.08))
                })
                .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                    this.add_dropped(paths.paths().to_vec(), window, cx);
                }))
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
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(900.0), px(640.0)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(SharedString::from(format!(
                "Archive \u{2014} {}",
                archive
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ))),
            ..Default::default()
        }),
        ..crate::base_window_options()
    };
    let handle =
        cx.open_window(opts, |window, cx| cx.new(|cx| gpui_component::Root::new(view, window, cx)))?;
    Ok(handle)
}

