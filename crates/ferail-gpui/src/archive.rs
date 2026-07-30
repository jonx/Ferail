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
    v_flex, ActiveTheme, Disableable, ElementExt as _, Sizable,
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
const FILTER_MIN_WIDTH: f32 = 620.0;

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
    /// Rounded pane width, captured at prepaint. Lets the header drop its
    /// lower-priority controls before they crush the archive's name.
    host_width: Option<f32>,
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
            |this: &mut Self, table, event: &TableEvent, _window, cx| match event {
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
                    cx.notify();
                }
                TableEvent::LeadMoved { row_ix, modifiers } => {
                    let modifiers = *modifiers;
                    let row_ix = *row_ix;
                    table.update(cx, |t, cx| {
                        t.delegate_mut().apply_click_gesture(row_ix, modifiers);
                        cx.notify();
                    });
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
                    .min_w_0()
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
            .child(body)
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
        ..Default::default()
    };
    let handle =
        cx.open_window(opts, |window, cx| cx.new(|cx| gpui_component::Root::new(view, window, cx)))?;
    Ok(handle)
}
