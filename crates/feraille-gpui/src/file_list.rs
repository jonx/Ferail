//! File-list table delegate — Phase 4.c.
//!
//! Wraps `feraille-fs-native` enumeration in a `TableDelegate` so
//! `gpui-component`'s virtualized `Table` renders the entries
//! efficiently even for directories with thousands of files. Columns
//! are Name / Size / Kind / Modified. Size/Kind are pre-formatted on the
//! domain side per the UI_NONBLOCKING contract; Modified is the exception,
//! rendered live from `mtime_unix` so its relative label keeps counting
//! (pure arithmetic, bounded to visible rows — still nonblocking).

use crate::text::{IconScale as _, TextScale as _, TruncateMiddle as _};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use feraille_core::{EntryKind, FileEntry, FormatFlag, NodeId};
use feraille_fs_native::NativeFs;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Div, ExternalPaths, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Pixels, Point, Render, RenderImage, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled, Window, div, img, px, svg,
};
use gpui_component::{
    ActiveTheme,
    menu::{PopupMenu, PopupMenuItem},
    tooltip::Tooltip,
};
use smallvec::{SmallVec, smallvec};

use crate::icons::{IconCache, file_type_icon, tint_color};
use crate::multi_table::{Column, ColumnSort, TableDelegate, TableEvent, TableState};
use crate::tasks::{TaskKind, TaskRegistry};
use crate::thumbnails::{THUMB_PX, ThumbnailCache, is_thumbnailable, show_thumbnails};

/// The floating ghost shown under the cursor while dragging rows out of
/// the list (or to an in-app drop target) — gpui's `on_drag` needs an
/// `Entity<impl Render>` for the drag image. A single-row drag shows the
/// item's icon/thumbnail + name as a labelled chip; a multi-row drag
/// renders the *actual* item images as a loose Finder-style stack
/// (capped at [`GHOST_STACK_CAP`]) with a red count badge — no "N items"
/// string. The images come straight from the already-warm thumbnail/icon
/// caches, so building the ghost never touches the filesystem.
///
/// gpui paints the drag view at `mouse − cursor_offset`, and
/// `cursor_offset` is the grab point within the *dragged element* — for
/// a full-width row that lands the ghost at the row's left edge. So we
/// re-anchor the chip back under the cursor by absolutely positioning it
/// at `offset` (= the `cursor_offset` gpui hands the constructor), plus
/// a small down-right nudge so it trails the pointer like Finder.
pub struct DragBadge {
    /// Item file names, lead-first, capped at [`GHOST_STACK_CAP`]. The
    /// single-item ghost labels its chip with `names[0]`; the multi-item
    /// ghost lists the first [`GHOST_NAME_CAP`] beside the stack with a
    /// "+N more" overflow line.
    pub names: SmallVec<[SharedString; GHOST_STACK_CAP]>,
    /// Actual item images (Quick Look thumbnail when warm, else the
    /// workspace type icon), lead-first, capped at [`GHOST_STACK_CAP`].
    pub icons: SmallVec<[Arc<RenderImage>; GHOST_STACK_CAP]>,
    pub count: usize,
    pub offset: Point<Pixels>,
}

/// Max real images to render in a multi-item drag stack. Finder shows a
/// small fan regardless of selection size; more than this just adds
/// cache lookups and visual mush.
pub const GHOST_STACK_CAP: usize = 4;

/// Max file names listed beside a multi-item drag stack before the rest
/// collapse into a "+N more" line.
pub const GHOST_NAME_CAP: usize = 3;

impl Render for DragBadge {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let content = if self.count <= 1 {
            // Single item: labelled chip with the file's icon/thumbnail.
            const ICON: f32 = 22.0;
            let chip = div()
                .flex()
                .items_center()
                .gap_2()
                .pl_1p5()
                .pr_2()
                .py_1()
                .rounded(theme.radius)
                .bg(theme.background)
                .border_1()
                .border_color(theme.border)
                .shadow_lg()
                .text_scale_sm()
                .text_color(theme.foreground)
                .when_some(self.icons.first().cloned(), |this, image| {
                    this.child(img(image).w(px(ICON)).h(px(ICON)).flex_shrink_0())
                })
                .child(
                    div()
                        .max_w(px(260.0))
                        .truncate()
                        .child(self.names.first().cloned().unwrap_or_default()),
                );
            div().child(chip)
        } else {
            // Multiple items: a loose stack of the real item images (drawn
            // back-to-front so the lead lands on top) with a red count
            // badge, beside a short list of the names — the Finder stack,
            // plus the "which files" the user asked for.
            const ICON: f32 = 40.0;
            const SPREAD: f32 = 7.0;
            let n = self.icons.len().clamp(1, GHOST_STACK_CAP);
            let span = ICON + (n as f32 - 1.0) * SPREAD;
            let mut stack = div().relative().flex_shrink_0().w(px(span)).h(px(span));
            for (i, image) in self.icons.iter().take(GHOST_STACK_CAP).enumerate().rev() {
                let off = px(i as f32 * SPREAD);
                stack = stack.child(
                    img(image.clone())
                        .absolute()
                        .left(off)
                        .top(off)
                        .w(px(ICON))
                        .h(px(ICON))
                        .rounded(px(6.0))
                        .border_2()
                        .border_color(theme.background)
                        .shadow_md(),
                );
            }
            let stack = stack.child(
                div()
                    .absolute()
                    .top(px(-6.0))
                    .right(px(-6.0))
                    .min_w(px(20.0))
                    .h(px(20.0))
                    .px_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(gpui::rgb(0xff3b30))
                    .border_2()
                    .border_color(theme.background)
                    .text_color(gpui::white())
                    .text_scale_xs()
                    .font_weight(FontWeight::BOLD)
                    .child(format!("{}", self.count)),
            );
            // Name list: the first GHOST_NAME_CAP, then a "+N more" line.
            let shown = self.names.len().min(GHOST_NAME_CAP);
            let mut names = div()
                .flex()
                .flex_col()
                .gap_0p5()
                .text_scale_sm()
                .text_color(theme.foreground);
            for name in self.names.iter().take(GHOST_NAME_CAP) {
                names = names.child(div().max_w(px(220.0)).truncate().child(name.clone()));
            }
            if self.count > shown {
                names = names.child(
                    div()
                        .text_scale_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("+{} more", self.count - shown)),
                );
            }
            let chip = div()
                .flex()
                .items_center()
                .gap_3()
                .pl_2()
                .pr_3()
                .py_2()
                .rounded(theme.radius)
                .bg(theme.background)
                .border_1()
                .border_color(theme.border)
                .shadow_lg()
                .child(stack)
                .child(names);
            div().child(chip)
        };
        // gpui paints this root at `mouse − cursor_offset`. Push the
        // content back under the cursor (plus a Finder-like down-right
        // nudge) with padding, which also sizes the root to contain it so
        // it's never clipped.
        div()
            .pl(self.offset.x + px(12.0))
            .pt(self.offset.y + px(8.0))
            .child(content)
    }
}

/// One target row's capability snapshot for context-menu gating,
/// projected from the cached `FileEntry` at right-click time — no I/O,
/// no path resolution. Add a field here when a command needs to gate on
/// a new per-file capability (e.g. `is_symlink`, `is_app`).
#[derive(Clone, Copy)]
pub struct TargetCap {
    pub kind: EntryKind,
    pub is_quarantined: bool,
    /// Whether this row is a file whose name looks like a supported archive.
    /// Lexical (extension-only) and precomputed at right-click staging time,
    /// so the menu decides whether to offer Extract without any I/O on
    /// menu-open (Prime Directive).
    pub is_archive: bool,
}

impl From<&FileEntry> for TargetCap {
    fn from(e: &FileEntry) -> Self {
        TargetCap {
            kind: e.kind,
            is_quarantined: e.is_quarantined,
            is_archive: matches!(e.kind, EntryKind::File)
                && feraille_archive::Format::is_archive_path(&e.name),
        }
    }
}

/// Capabilities of the rows a context command will act on, resolved once
/// at menu-open time with the SAME logic as
/// `Shell::action_entries_visible_order` so the menu the user sees
/// matches the set the handler touches (see docs/features/CONTEXT_MENU.md).
///
/// `caps` drives **bulk** commands (Clear Quarantine, Trash, …) through
/// the `any`/`all` quantifiers. `anchor` drives **anchor** commands (Open
/// Terminal Here, Slideshow from Here) that act on the single clicked row.
#[derive(Clone, Default)]
pub struct MenuTargets {
    pub caps: Vec<TargetCap>,
    pub anchor: Option<TargetCap>,
}

impl MenuTargets {
    pub fn len(&self) -> usize {
        self.caps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.caps.is_empty()
    }

    /// Exactly one target — the gate for commands that only make sense
    /// per single file (Copy Path, Rename, Open With).
    pub fn is_single(&self) -> bool {
        self.caps.len() == 1
    }

    /// More than one target.
    pub fn is_multi(&self) -> bool {
        self.caps.len() > 1
    }

    /// "Any" quantifier: at least one target satisfies `pred`. Show the
    /// item and let the handler act on the qualifying subset.
    pub fn any(&self, pred: impl Fn(&TargetCap) -> bool) -> bool {
        self.caps.iter().any(pred)
    }

    /// "All" quantifier: the set is non-empty and every target satisfies
    /// `pred`. Show only when the command is valid for the whole set.
    pub fn all(&self, pred: impl Fn(&TargetCap) -> bool) -> bool {
        !self.caps.is_empty() && self.caps.iter().all(pred)
    }
}

/// Availability rule for a context command whose visibility depends on
/// the resolved selection (docs/features/CONTEXT_MENU.md). Commands that
/// always apply to a group — whether as one batch op (Compress, Trash)
/// or fanned out per file (Open, Quick Look, Get Info) — need no rule
/// and are added unconditionally; only the two cases below gate.
pub enum Availability {
    /// Meaningful for exactly one file; hidden once more than one row is
    /// targeted (Copy Path, Rename, Open With).
    SingleOnly,
    /// Per-command callback over the resolved targets — the escape hatch
    /// for capability and anchor rules (Clear Quarantine = any
    /// quarantined; Open Terminal Here = anchor is a folder; Slideshow =
    /// anchor is a file).
    When(fn(&MenuTargets) -> bool),
}

impl Availability {
    pub fn allows(&self, t: &MenuTargets) -> bool {
        match self {
            Availability::SingleOnly => t.is_single(),
            Availability::When(f) => f(t),
        }
    }
}

/// Capability: at least one target carries the Mark-of-the-Web. The
/// handler (`Shell::on_clear_quarantine`) strips it from the quarantined
/// subset, so "any" is the right quantifier.
fn avail_any_quarantined(t: &MenuTargets) -> bool {
    t.any(|c| c.is_quarantined)
}

/// Bulk rule: at least one target is an archive file — offer Extract, which
/// acts on the archive subset (mixed selections extract only their archives),
/// mirroring how Clear Quarantine acts on the quarantined subset.
fn avail_any_archive(t: &MenuTargets) -> bool {
    t.any(|c| c.is_archive)
}

/// Anchor rule: the right-clicked (else lead) row is a folder — for
/// commands that act on one directory (Open Terminal Here, Favorites).
fn avail_anchor_dir(t: &MenuTargets) -> bool {
    matches!(t.anchor.map(|c| c.kind), Some(EntryKind::Directory))
}

/// Anchor rule: the right-clicked (else lead) row is a non-directory —
/// for file-anchored commands (Slideshow from Here).
fn avail_anchor_file(t: &MenuTargets) -> bool {
    t.anchor
        .map(|c| !matches!(c.kind, EntryKind::Directory))
        .unwrap_or(false)
}

/// Delegate that vends the current directory's entries to the
/// Table. Holds the live `Vec<FileEntry>`; the Shell rotates it on
/// every `navigate()`. The Vec is already filtered by both
/// `show_hidden` and `filter_text` at `load()` time — the Table
/// always sees the user-visible subset, no per-cell skipping.
pub struct FileListDelegate {
    pub entries: Vec<FileEntry>,
    pub columns: Vec<Column>,
    /// Columns the user has hidden (header right-click → uncheck). Kept
    /// out of `columns` — the table only ever sees the visible set, so
    /// its index-based reorder/sort/resize logic stays untouched — but
    /// retained here (with identity + width) so re-showing restores them
    /// and they persist across launches. See [`split_persisted_columns`].
    pub hidden_columns: Vec<Column>,
    pub fs: Arc<NativeFs>,
    /// Snapshot of entry paths captured during enumeration/application.
    /// Rendering may read this cache, but must not call back into the
    /// filesystem resolver.
    pub paths: HashMap<NodeId, PathBuf>,
    /// Shared icon cache. Lookup-or-fetch via NSWorkspace; subsequent
    /// renders for the same kind are a HashMap hit. Wrapped in
    /// Rc<RefCell> so render_td's `&mut self` can borrow without
    /// fighting the cache.
    pub icons: Rc<RefCell<IconCache>>,
    /// Shared Quick Look thumbnail cache (real photo/video/PDF
    /// content), keyed by path. Same `Rc<RefCell>` the process holds,
    /// so every tab warms the same cache. `render_td` reads it
    /// allocation-free; [`Self::visible_rows_changed`] fills it
    /// viewport-only off the UI thread.
    pub thumbnails: Rc<RefCell<ThumbnailCache>>,
    /// Shared task registry (same `Rc` the process + status bar hold).
    /// Thumbnail warm passes register an ambient `ThumbnailPrefetch`
    /// task here so a slow batch is visible in the status bar / panel.
    pub tasks: Rc<RefCell<TaskRegistry>>,
    /// Cut-marked paths (Cmd+X), shared `Rc` with `ProcessState` so the
    /// set is always current; `render_tr` dims any row whose path is in
    /// it. (docs/features/FILE_OPS.md)
    pub cut_marker: Rc<RefCell<Vec<PathBuf>>>,
    /// Ant Trail heat per row, parallel to `entries`. Populated by
    /// `Shell::load_path` after each enumerate. 0.0 = never visited
    /// (no tint); 1.0 = the most-visited folder. Renderer maps to
    /// a low-opacity accent background.
    pub heats: Vec<f32>,
    /// Finder colour tags per row, parallel to `entries`. Populated
    /// lazily by `load()` (synchronous bulk read via
    /// `crate::platform_shell::read_canonical_tags`, capped at the first
    /// N rows so a 50k-file folder doesn't pay the per-row xattr
    /// lookup synchronously on the UI thread). Renderer pairs each
    /// row's slot with the name cell to draw small coloured dots.
    pub tags: Vec<Vec<feraille_core::commands::TagColor>>,
    /// `is_favorited[row]` is `true` when the entry's path is currently
    /// in the user-curated Favorites list — drives the §5 star indicator
    /// on the Name cell. Recomputed by `Shell::refresh_file_list_favorited`
    /// on every load and whenever the Favorites entity changes (the
    /// `cx.observe` subscription in `Shell::new`).
    pub is_favorited: Vec<bool>,
    /// NodeId-keyed selection set (spec §2.2), mirrored from the
    /// active tab on every selection mutation, every streaming
    /// batch, and `Done`. `render_tr` looks up `entries[row].id` to
    /// decide whether to paint the selection fill. NodeId-keyed
    /// (not row-indexed) so sort/filter/streaming changes can
    /// reorder rows without desyncing the visual.
    pub selected_set: HashSet<NodeId>,
    /// The keyboard-cursor / range-lead, mirrored from the active
    /// tab. At most one. Cosmetic only — the Table primitive's
    /// `selected_row` overlay is the visible focus ring.
    pub lead: Option<NodeId>,
    /// Warm cache for the right-click "Open With" submenu: the most
    /// recently fetched `(path, LaunchServices candidates)` pair.
    /// Populated off the UI thread by [`spawn_open_with_warm`] —
    /// triggered on selection-lead changes and on a cache-miss menu
    /// build. The menu builder reads ONLY this cache (prime
    /// directive: no shell queries at menu-open time); a miss shows
    /// a disabled placeholder for that one open, exactly like
    /// Finder's "Fetching…" when LaunchServices is slow.
    ///
    /// Dispatch handlers (`Shell::open_with_slot`) resolve slot
    /// indices against this same cache so the app at slot N when
    /// the menu was BUILT is the app that opens — re-fetching at
    /// dispatch could reorder candidates and launch the wrong app.
    pub open_with_warm: Option<(PathBuf, Vec<crate::platform_shell::OpenWithCandidate>)>,
    /// Capabilities of the rows the next context-menu command will
    /// target, pushed by `Shell::push_menu_targets` on every row
    /// right-click before `context_menu` builds. Lets bulk commands gate
    /// on the WHOLE resolved target set — e.g. Clear Quarantine shows
    /// when ANY selected row is quarantined — instead of just the
    /// physically-clicked row. Reset on `clear`/`replace_entries` so a
    /// stale set can't gate a freshly loaded folder.
    pub menu_targets: MenuTargets,
    /// The user's active column sort, recorded by `perform_sort` /
    /// `apply_sort`. `None` = natural (name-ascending) order. Read
    /// by the folder-size worker so late-arriving sizes can re-apply
    /// a live Size sort instead of leaving rows in stale positions.
    pub current_sort: Option<(SortColumn, bool)>,
    /// Shared process-wide sort preference. New folder loads and new tabs
    /// seed from this so navigation doesn't silently fall back to raw
    /// filesystem enumeration order.
    sort_state: Rc<Cell<Option<(SortColumn, bool)>>>,
    /// The Shell's focus handle, carrying the `SHELL_CONTEXT` key
    /// context. Handed to the row right-click menu as its
    /// `action_context` so gpui-component resolves each item's
    /// keyboard-shortcut hint against the shell's (stable, always-
    /// painted) dispatch path instead of the focus-sensitive
    /// previous-frame fallback — which left shortcuts blank for the
    /// first frame or two after the menu opened.
    pub shell_focus: gpui::FocusHandle,
    /// Lazily-built drag payload for the CURRENT selection, shared by
    /// every selected row's `on_drag`. Built once on the first
    /// selected-row render after a selection/model change; without it
    /// each selected visible row re-walked ALL entries per render
    /// (Select All in a 10k folder ≈ 400k HashSet probes + PathBuf
    /// clones per pass). Invalidated by every selection write and
    /// every structural entries change.
    drag_snapshot: Option<DragSnapshot>,
    /// Status-bar totals, computed lazily once per model/selection
    /// change instead of O(N) sums on every render pass (`Cell` so the
    /// read-only render path can fill them). Invalidated together with
    /// `drag_snapshot`, plus when folder sizes stream in.
    pub cached_total_size: std::cell::Cell<Option<u64>>,
    pub cached_selected_size: std::cell::Cell<Option<u64>>,
    /// `Some(folder name)` while a *slow* directory load is in flight —
    /// set by `Shell`'s slow-load timer when the first enumeration batch
    /// hasn't landed after `SLOW_LOAD_INDICATOR_DELAY` (a spun-down
    /// external drive, a cold network mount). Flips the table into its
    /// skeleton loading view with a "Reading '<name>'…" line, replacing
    /// the previous directory's stale (and still-clickable) rows.
    /// Cleared by `clear()` / `replace_entries()`, which every load
    /// exit path funnels through. Deliberately NOT set at load start:
    /// fast local navigations would flash a skeleton every click.
    pub slow_load: Option<SharedString>,
}

/// See [`FileListDelegate::drag_snapshot`].
struct DragSnapshot {
    /// Visible-order paths of the whole selection — the real OS drag
    /// payload. Rows still clone this into their `ExternalPaths`
    /// value (gpui's mac backend needs it by value), but the walk,
    /// membership probes, and ghost assembly happen once.
    paths: Vec<PathBuf>,
    names: SmallVec<[SharedString; GHOST_STACK_CAP]>,
    icons: SmallVec<[Arc<RenderImage>; GHOST_STACK_CAP]>,
}

impl FileListDelegate {
    pub fn new(
        fs: Arc<NativeFs>,
        icons: Rc<RefCell<IconCache>>,
        thumbnails: Rc<RefCell<ThumbnailCache>>,
        tasks: Rc<RefCell<TaskRegistry>>,
        cut_marker: Rc<RefCell<Vec<PathBuf>>>,
        sort_state: Rc<Cell<Option<(SortColumn, bool)>>>,
        shell_focus: gpui::FocusHandle,
    ) -> Self {
        let current_sort = sort_state.get();
        // Column order + widths + visibility survive across launches
        // (drag-reorder, drag-resize, and header show/hide all write
        // through the shell's table-event bridge). app_state::load() is
        // the in-memory cache — no I/O.
        let (columns, hidden_columns) =
            split_persisted_columns(crate::app_state::load().list_columns.as_deref());
        Self {
            entries: Vec::new(),
            // Next-level Phase 1: Magic-driven `Format` column
            // replaces the duplicate Kind + Magic columns. The Format
            // cell prefers magic-detected text, falls back to the
            // extension-derived kind, and renders a small mismatch
            // indicator when the two genuinely disagree.
            //
            // Each column is marked `.sortable()` so clicking the
            // header runs `perform_sort` below. The Table primitive
            // also handles resizing + reorder when its TableState
            // has col_resizable / col_movable enabled (both default
            // true in our pinned version).
            columns,
            hidden_columns,
            fs,
            paths: HashMap::new(),
            icons,
            thumbnails,
            tasks,
            cut_marker,
            heats: Vec::new(),
            tags: Vec::new(),
            is_favorited: Vec::new(),
            selected_set: HashSet::new(),
            lead: None,
            menu_targets: MenuTargets::default(),
            open_with_warm: None,
            current_sort,
            sort_state,
            shell_focus,
            drag_snapshot: None,
            cached_total_size: std::cell::Cell::new(None),
            cached_selected_size: std::cell::Cell::new(None),
            slow_load: None,
        }
    }

    /// Whether column `key` is currently shown (present in `columns`).
    pub fn is_column_visible(&self, key: &str) -> bool {
        self.columns.iter().any(|c| c.key.as_ref() == key)
    }

    /// Show or hide column `key` (header right-click menu). Hiding moves
    /// it into `hidden_columns` retaining its width; showing appends it
    /// back to the visible set. The primary `name` column can't be
    /// hidden, nor can the last remaining visible column. Returns whether
    /// the set changed — the caller then `refresh`es and persists.
    pub fn toggle_column(&mut self, key: &str) -> bool {
        if let Some(pos) = self.columns.iter().position(|c| c.key.as_ref() == key) {
            if key == "name" || self.columns.len() <= 1 {
                return false;
            }
            let col = self.columns.remove(pos);
            self.hidden_columns.push(col);
            true
        } else if let Some(pos) = self
            .hidden_columns
            .iter()
            .position(|c| c.key.as_ref() == key)
        {
            let col = self.hidden_columns.remove(pos);
            self.columns.push(col);
            true
        } else {
            false
        }
    }

    /// Restore the default column set (order + widths), un-hiding all
    /// columns. Backs the header menu's "Reset Columns".
    pub fn reset_columns(&mut self) {
        self.columns = default_columns();
        self.hidden_columns.clear();
    }

    /// Drop the cached selection drag payload. Call on every
    /// selection write and structural entries change.
    pub fn invalidate_drag_snapshot(&mut self) {
        self.drag_snapshot = None;
        self.cached_total_size.set(None);
        self.cached_selected_size.set(None);
    }

    /// Build the shared drag payload for the current selection —
    /// visible-order paths plus the capped ghost images/names. Ghost
    /// images come only from already-warm caches (thumbnail when
    /// cached, else the workspace type icon), per the UI_NONBLOCKING
    /// contract.
    fn build_drag_snapshot(&self, cx: &gpui::App) -> DragSnapshot {
        let want_thumb = show_thumbnails(cx);
        let mut paths: Vec<PathBuf> = Vec::with_capacity(self.selected_set.len());
        let mut icons: SmallVec<[Arc<RenderImage>; GHOST_STACK_CAP]> = smallvec![];
        for entry in &self.entries {
            if !self.selected_set.contains(&entry.id) {
                continue;
            }
            let Some(path) = self.path_for_entry(entry.id) else {
                continue;
            };
            self.push_ghost_icon(entry, &path, want_thumb, &mut icons);
            paths.push(path);
        }
        // Names shown on the ghost, lead-first and capped — the
        // single chip uses the first; the multi list shows up to
        // GHOST_NAME_CAP with a "+N more" overflow.
        let names: SmallVec<[SharedString; GHOST_STACK_CAP]> = paths
            .iter()
            .take(GHOST_STACK_CAP)
            .map(|p| ghost_name(p))
            .collect();
        DragSnapshot {
            paths,
            names,
            icons,
        }
    }

    /// Push one warm-cache ghost image (thumbnail if cached, else the
    /// type icon), capped at [`GHOST_STACK_CAP`].
    fn push_ghost_icon(
        &self,
        entry: &FileEntry,
        path: &PathBuf,
        want_thumb: bool,
        out: &mut SmallVec<[Arc<RenderImage>; GHOST_STACK_CAP]>,
    ) {
        if out.len() >= GHOST_STACK_CAP {
            return;
        }
        if want_thumb {
            if let Some(t) = self.thumbnails.borrow().get(path, THUMB_PX) {
                out.push(t);
                return;
            }
        }
        out.push(self.icons.borrow_mut().icon_for(entry, path));
    }


    fn effective_sort(&self) -> (SortColumn, bool) {
        self.current_sort.unwrap_or((SortColumn::Name, true))
    }

    pub fn sync_sort_from_process(&mut self) {
        self.current_sort = self.sort_state.get();
    }

    fn set_current_sort(&mut self, sort: Option<(SortColumn, bool)>) {
        self.current_sort = sort;
        self.sort_state.set(sort);
    }

    /// Sort the visible row model and all row-parallel decoration vectors
    /// together. Sorting only `entries` makes heat tint, tag dots, and favorite
    /// stars drift onto the wrong rows.
    pub fn apply_effective_sort(&mut self) {
        self.sync_sort_from_process();
        let (col, asc) = self.effective_sort();
        self.sort_model(col, asc);
    }

    pub fn apply_sort(&mut self, col: SortColumn, asc: bool) {
        self.set_current_sort(Some((col, asc)));
        self.sort_model(col, asc);
    }

    pub fn reset_sort(&mut self) {
        self.set_current_sort(None);
        self.sort_model(SortColumn::Name, true);
    }

    fn sort_model(&mut self, col: SortColumn, asc: bool) {
        if self.entries.len() <= 1 {
            return;
        }
        // Row order changes: the drag snapshot's visible-order paths
        // are stale (totals unchanged by a re-order, but cheap to
        // recompute and one invalidation path is simpler).
        self.invalidate_drag_snapshot();

        let mut row_state: HashMap<NodeId, (f32, Vec<feraille_core::commands::TagColor>, bool)> =
            self.entries
                .iter()
                .enumerate()
                .map(|(ix, entry)| {
                    (
                        entry.id,
                        (
                            self.heats.get(ix).copied().unwrap_or(0.0),
                            self.tags.get(ix).cloned().unwrap_or_default(),
                            self.is_favorited.get(ix).copied().unwrap_or(false),
                        ),
                    )
                })
                .collect();

        sort_in_place(&mut self.entries, col, asc);

        self.heats.clear();
        self.tags.clear();
        self.is_favorited.clear();
        self.heats.reserve(self.entries.len());
        self.tags.reserve(self.entries.len());
        self.is_favorited.reserve(self.entries.len());
        for entry in &self.entries {
            let (heat, tags, favorited) =
                row_state
                    .remove(&entry.id)
                    .unwrap_or((0.0, Vec::new(), false));
            self.heats.push(heat);
            self.tags.push(tags);
            self.is_favorited.push(favorited);
        }
    }

    // (The old synchronous `load()` — enumerate + up-to-200 inline xattr
    // tag reads on the UI thread — was dead code; the streaming pipeline in
    // `Shell::load_path_for_tab` is the only listing path. Deleted so nobody
    // resurrects a Prime Directive violation.)

    pub fn clear(&mut self) {
        self.invalidate_drag_snapshot();
        self.slow_load = None;
        self.entries.clear();
        self.paths.clear();
        self.heats.clear();
        self.tags.clear();
        self.is_favorited.clear();
        self.menu_targets = MenuTargets::default();
        // selected_set / lead are NodeId-keyed and reconciled by
        // Shell against the new model; not cleared here.
    }

    pub fn replace_entries(
        &mut self,
        entries: Vec<FileEntry>,
        paths: HashMap<NodeId, PathBuf>,
        heats: Vec<f32>,
    ) {
        self.invalidate_drag_snapshot();
        self.slow_load = None;
        self.entries = entries;
        self.paths = paths;
        self.heats = heats;
        self.tags = vec![Vec::new(); self.entries.len()];
        self.is_favorited = vec![false; self.entries.len()];
        self.menu_targets = MenuTargets::default();
        self.apply_effective_sort();
        // selected_set / lead are NodeId-keyed; reconciliation is
        // Shell's job (see `refresh_file_list_selection`).
    }

    pub fn append_entries(
        &mut self,
        entries: Vec<FileEntry>,
        paths: HashMap<NodeId, PathBuf>,
        heats: Vec<f32>,
    ) {
        self.invalidate_drag_snapshot();
        self.paths.extend(paths);
        let n = entries.len();
        self.entries.extend(entries);
        self.heats.extend(heats);
        self.tags.extend((0..n).map(|_| Vec::new()));
        self.is_favorited.extend((0..n).map(|_| false));
        // selected_set / lead untouched — NodeId-keyed, not row-keyed.
    }

    pub fn append_entries_sorted(
        &mut self,
        entries: Vec<FileEntry>,
        paths: HashMap<NodeId, PathBuf>,
        heats: Vec<f32>,
    ) {
        self.append_entries(entries, paths, heats);
        self.apply_effective_sort();
    }

    pub fn path_for_entry(&self, id: NodeId) -> Option<PathBuf> {
        self.paths.get(&id).cloned()
    }

    /// Fetch Quick Look thumbnails for the thumbnailable rows in
    /// `visible_range` (plus a little overscan), off the UI thread,
    /// inserting each into the shared cache and repainting as it lands.
    /// Idempotent and cheap to re-call: rows already cached or in
    /// flight are skipped, so the table's `visible_rows_changed` hook
    /// and the Shell's settings-toggle observer can both drive it.
    pub fn warm_thumbnails(
        &mut self,
        visible_range: Range<usize>,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.warm_thumbnails_sized(visible_range, THUMB_PX, cx);
    }

    /// Warm thumbnails for `visible_range` at a specific physical fetch
    /// size. The table calls this at [`THUMB_PX`]; the icon grid calls
    /// it at its bucketed display size. Same off-thread, dedup, repaint
    /// contract regardless of size.
    pub fn warm_thumbnails_sized(
        &mut self,
        visible_range: Range<usize>,
        size_px: u32,
        cx: &mut Context<TableState<Self>>,
    ) {
        if !show_thumbnails(cx) {
            return;
        }
        // A little overscan so a nudge of the wheel doesn't expose a
        // blank slot before its fetch is even scheduled.
        const OVERSCAN: usize = 8;
        let start = visible_range.start.saturating_sub(OVERSCAN);
        let end = (visible_range.end + OVERSCAN).min(self.entries.len());

        // Visible thumbnailable rows that aren't cached or in flight.
        let mut todo: Vec<PathBuf> = Vec::new();
        {
            let cache = self.thumbnails.borrow();
            for row in start..end {
                let Some(entry) = self.entries.get(row) else {
                    continue;
                };
                if !is_thumbnailable(entry) {
                    continue;
                }
                let Some(path) = self.path_for_entry(entry.id) else {
                    continue;
                };
                if cache.needs_fetch(&path, size_px) {
                    todo.push(path);
                }
            }
        }
        if todo.is_empty() {
            return;
        }
        // Reserve the slots up front so overlapping scroll events don't
        // queue the same path twice.
        {
            let mut cache = self.thumbnails.borrow_mut();
            for path in &todo {
                cache.mark_in_flight(path.clone(), size_px);
            }
        }

        // Ambient task so a slow batch shows in the status bar / panel.
        // Sub-perceptual batches finish inside SURFACE_DELAY and never
        // flicker a row in. (docs/features/FILE_OPS.md)
        let task_id = self.tasks.borrow_mut().begin(
            TaskKind::ThumbnailPrefetch,
            format!("Loading {} thumbnails\u{2026}", todo.len()),
            false,
        );
        let thumbnails = self.thumbnails.clone();
        let tasks = self.tasks.clone();
        cx.spawn(async move |table, cx| {
            for path in todo {
                // Quick Look runs on a worker thread; only Send data
                // crosses the boundary (path in, RGBA bytes out). The
                // `RenderImage` is built back on the UI thread below.
                let fetch_path = path.clone();
                let rgba = cx
                    .background_executor()
                    .spawn(async move {
                        match crate::video_poster::fetch_content_thumbnail(&fetch_path, size_px) {
                            crate::video_poster::Fetched::Done(r) => r,
                            // Awaiting yields this pool thread; the decode
                            // runs on the dedicated poster worker.
                            crate::video_poster::Fetched::NeedsPoster => {
                                crate::video_poster::fetch_poster(fetch_path, size_px).await
                            }
                        }
                    })
                    .await;
                if table
                    .update(cx, |_table, cx| {
                        thumbnails.borrow_mut().insert(path, size_px, rgba);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
            // Always retire the task. Borrow the shared registry directly
            // so a gone table entity still drops the row; notify through
            // the entity when it is still alive.
            if table
                .update(cx, |_table, cx| {
                    tasks.borrow_mut().end(task_id);
                    cx.notify();
                })
                .is_err()
            {
                tasks.borrow_mut().end(task_id);
            }
        })
        .detach();
    }
}

impl TableDelegate for FileListDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.entries.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let _path_guard = feraille_core::path_guard::enter_render();
        // Ant Trail heat tint (Stage 9.b). Renders only on directory
        // rows — files aren't tracked in the trail. 0.0 → no tint;
        // up to ~0.30 warm-orange opacity at full heat. The warm
        // hue matches the "heat" metaphor (frequently-visited
        // folders glow brighter); accent / primary blend too far
        // into hover/selection territory.
        let heat = self.heats.get(row_ix).copied().unwrap_or(0.0);
        let kind_is_dir = self
            .entries
            .get(row_ix)
            .map(|e| matches!(e.kind, EntryKind::Directory))
            .unwrap_or(false);
        let entry_id = self.entries.get(row_ix).map(|e| e.id);
        let in_set = entry_id
            .map(|id| self.selected_set.contains(&id))
            .unwrap_or(false);
        let is_lead = entry_id == self.lead && entry_id.is_some();
        let mut row = div().id(("file-row", row_ix));
        // Cut (Cmd+X) rows render dimmed until the move pastes (or the
        // mark is cleared by a fresh Copy/Cut), mirroring Explorer.
        let is_cut = entry_id
            .and_then(|id| self.paths.get(&id))
            .map(|p| self.cut_marker.borrow().iter().any(|c| c == p))
            .unwrap_or(false);
        if is_cut {
            row = row.opacity(0.45);
        }
        // Folder rows are drop targets for OS file drags (dnd-spec
        // §3.5): accent ring on hover, drop surfaces as a TableEvent
        // for the shell to run the transfer into this folder. Stop
        // propagation so the pane-background target underneath
        // doesn't also fire.
        if kind_is_dir {
            row = row
                .drag_over::<ExternalPaths>(|style, _, _, cx| {
                    style
                        .border_1()
                        .border_color(cx.theme().accent)
                        .bg(cx.theme().accent.opacity(0.10))
                })
                // Spring-load: while a drag hovers this folder row, tell
                // the shell (which times the dwell and drills in).
                .on_drag_move(cx.listener(
                    move |_state, e: &gpui::DragMoveEvent<ExternalPaths>, _window, cx| {
                        if e.bounds.contains(&e.event.position) {
                            cx.emit(TableEvent::DragHover { row_ix });
                        }
                    },
                ))
                .on_drop(
                    cx.listener(move |_state, paths: &ExternalPaths, _window, cx| {
                        cx.stop_propagation();
                        cx.emit(TableEvent::ExternalDrop {
                            row_ix,
                            paths: paths.paths().to_vec(),
                        });
                    }),
                );
        }
        if kind_is_dir && heat > 0.0 && crate::ant_trail::enabled(cx) {
            // Customizable base tint, scaled by heat. Stable hue across
            // light/dark themes (it's a solid color, not a theme color
            // whose alpha would compound in dark mode). Suppressed when
            // the Ant Trail is disabled. See `crate::ant_trail`.
            row = row.bg(crate::ant_trail::tint(crate::ant_trail::base(cx), heat));
        }
        // Spec §2 multi-select fill. Painted for every set member
        // EXCEPT the lead — the Table primitive draws its own
        // `selected_row` overlay on the lead, which serves as the
        // distinct focus ring spec §2.3 calls for.
        //
        // Uses the shared `selection_colors` wash (the saturated blue the
        // grid uses) rather than the theme's `table_active`, which is a
        // desaturated gray the theme hard-caps at alpha ≤ 0.2 and read as
        // "too faint" next to the grid. Members get the fill only; the
        // lead's full-strength focus ring is the contiguous-block divider.
        if in_set && !is_lead {
            row = row.bg(crate::selection_colors::fill(cx));
        }
        // OS drag-out: GPUI's macOS backend recognises ExternalPaths
        // and uses NSFilePromise / NSPasteboard, so dragging rows to
        // Finder / other apps drops the actual files. Spec §3.1:
        // pressing a selected row drags the full visible-order
        // selection; pressing an unselected row drags just that row.
        if let Some(entry) = self.entries.get(row_ix) {
            let row_is_selected = self.selected_set.contains(&entry.id);
            if row_is_selected {
                // Shared snapshot for the whole selection: built once
                // per selection/model change, reused by every selected
                // row. The old per-row walk over ALL entries made a
                // big selection quadratic per render pass.
                if self.drag_snapshot.is_none() {
                    self.drag_snapshot = Some(self.build_drag_snapshot(cx));
                }
                if let Some(snapshot) = self.drag_snapshot.as_ref() {
                    if !snapshot.paths.is_empty() {
                        let count = snapshot.paths.len();
                        let names = snapshot.names.clone();
                        let ghost_icons = snapshot.icons.clone();
                        return row.on_drag(
                            ExternalPaths(snapshot.paths.clone().into()),
                            move |_paths, offset, _window, cx| {
                                cx.new(|_| DragBadge {
                                    names: names.clone(),
                                    icons: ghost_icons.clone(),
                                    count,
                                    offset,
                                })
                            },
                        );
                    }
                }
            } else if let Some(path) = self.path_for_entry(entry.id) {
                // Unselected row: drags just itself — cheap, no snapshot.
                let mut ghost_icons: SmallVec<[Arc<RenderImage>; GHOST_STACK_CAP]> = smallvec![];
                self.push_ghost_icon(entry, &path, show_thumbnails(cx), &mut ghost_icons);
                let names: SmallVec<[SharedString; GHOST_STACK_CAP]> =
                    smallvec![ghost_name(&path)];
                return row.on_drag(
                    ExternalPaths(vec![path].into()),
                    move |_paths, offset, _window, cx| {
                        cx.new(|_| DragBadge {
                            names: names.clone(),
                            icons: ghost_icons.clone(),
                            count: 1,
                            offset,
                        })
                    },
                );
            }
        }
        row
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let _path_guard = feraille_core::path_guard::enter_render();
        let Some(entry) = self.entries.get(row_ix) else {
            return div().into_any_element();
        };

        let col_key = self
            .columns
            .get(col_ix)
            .map(|col| col.key.as_ref())
            .unwrap_or("");

        match col_key {
            // Name — Lucide line-art icon tinted by category (files +
            // symlinks); macOS NSWorkspace bitmap for folders so
            // user-customised folder icons and cloud-sync overlays
            // still render. Optional quarantine badge in the top-right
            // corner. Tooltip carries the full filename so truncation
            // is recoverable. (Next-level Phase 1.)
            "name" => {
                use feraille_core::EntryKind;
                let path = self.path_for_entry(entry.id).unwrap_or_default();
                let quarantined = entry.is_quarantined;
                // When thumbnails are enabled the whole listing uses a
                // slightly larger, uniform icon slot so real-content
                // thumbnails read at a glance and folder icons / icon
                // fallbacks stay vertically aligned with them.
                let thumbs_on = show_thumbnails(cx);
                let slot = if thumbs_on { 24.0 } else { 18.0 };
                let icon_wrapper: gpui::AnyElement = match entry.kind {
                    EntryKind::Directory => {
                        let icon = self.icons.borrow_mut().icon_for(entry, &path);
                        // Platforms whose icon bridge is still a stub (Linux
                        // scaffold, AROS) yield the blank placeholder — show
                        // the Lucide folder glyph instead of an empty slot.
                        let inner: gpui::AnyElement = if self.icons.borrow().is_blank(&icon) {
                            let fi = file_type_icon(entry);
                            let tint = tint_color(fi.tint, cx);
                            svg()
                                .path(fi.path)
                                .w(px(slot))
                                .h(px(slot))
                                .text_color(tint)
                                .into_any_element()
                        } else {
                            img(icon).w(px(slot)).h(px(slot)).into_any_element()
                        };
                        div()
                            .relative()
                            .flex_shrink_0()
                            .w(px(slot))
                            .h(px(slot))
                            .child(inner)
                            .when(quarantined, badge_overlay)
                            .into_any_element()
                    }
                    EntryKind::File | EntryKind::Symlink => {
                        // Real Quick Look thumbnail if one is ready in
                        // the cache; otherwise the generic type icon.
                        // `get` is a non-mutating HashMap read — the
                        // fetch itself happens off the render path in
                        // `visible_rows_changed`.
                        let thumb = if thumbs_on {
                            self.thumbnails.borrow().get(&path, THUMB_PX)
                        } else {
                            None
                        };
                        let inner = if let Some(image) = thumb {
                            // `img` defaults to ObjectFit::Contain, so a
                            // non-square photo fits the square slot
                            // without distortion.
                            img(image).w(px(slot)).h(px(slot)).into_any_element()
                        } else {
                            let icon = file_type_icon(entry);
                            let tint = tint_color(icon.tint, cx);
                            svg()
                                .path(icon.path)
                                .w(px(slot))
                                .h(px(slot))
                                .text_color(tint)
                                .into_any_element()
                        };
                        div()
                            .relative()
                            .flex_shrink_0()
                            .w(px(slot))
                            .h(px(slot))
                            .child(inner)
                            .when(quarantined, badge_overlay)
                            .into_any_element()
                    }
                };
                // Render the *display* leaf (macOS shows an on-disk `:` as
                // `/`, Finder-style); when the name hides deceptive characters
                // draw the same highlighted treatment the preview pane uses, so
                // the list — scanned first — never shows an invisible-char name
                // as innocuous. `name_has_hazards` is precomputed at enumerate
                // time, so the row paint just reads a bool.
                let display_name = entry.display_name.clone();
                let tooltip_name = display_name.clone();
                let name_child = if entry.name_has_hazards {
                    div().flex_1().min_w_0().child(crate::entry_info::name_hazard_element(
                        &display_name,
                        SharedString::from(format!("file-row-name-{row_ix}")),
                    ))
                } else {
                    div()
                        .flex_1()
                        .min_w_0()
                        // Finder-style middle ellipsis: keep the name's start
                        // AND its extension when the column is too narrow.
                        .truncate_middle()
                        .child(SharedString::from(display_name.clone()))
                };
                // Inline tag chips — 6-DIP coloured dots after the
                // filename, one per applied Finder tag (max 7). Read
                // synchronously at load() time and stored in the
                // delegate; render only consumes the cached Vec.
                let row_tags = self.tags.get(row_ix).cloned().unwrap_or_default();
                let mut chips = gpui_component::h_flex().gap_1().flex_shrink_0();
                for color in row_tags.iter().take(7) {
                    chips = chips.child(
                        div()
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded_full()
                            .bg(tag_color_rgba(*color)),
                    );
                }
                // §5 favorited indicator: small accent star trailing
                // the name. Only painted for folder rows (files can't
                // be favorited) where the row's path is in the favorites
                // index. The parallel vec is refreshed by Shell on every
                // load + every favorites mutation.
                let is_favorited = self.is_favorited.get(row_ix).copied().unwrap_or(false);
                let star_color = cx.theme().primary;
                let star = if is_favorited && matches!(entry.kind, EntryKind::Directory) {
                    svg()
                        .path("icons/nav/star.svg")
                        .icon_px(12.0)
                        .text_color(star_color)
                        .into_any_element()
                } else {
                    div().w(px(0.0)).h(px(12.0)).into_any_element()
                };
                div()
                    .id(("file-row-name", row_ix))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_scale_sm()
                    .text_color(cx.theme().foreground)
                    .child(icon_wrapper)
                    .child(name_child)
                    .child(chips)
                    .child(star)
                    .tooltip(move |window, cx| Tooltip::new(tooltip_name.clone()).build(window, cx))
                    .into_any_element()
            }
            "size" => div()
                .text_scale_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(entry.display_size.clone()))
                .into_any_element(),
            // Unified Format column: replaces the old Kind + Magic
            // duplication. The trailing indicator grades how the
            // extension and the content-detected type relate — a red
            // danger triangle only for genuine disguises (dangerous
            // content under an innocent extension), a quiet neutral cue
            // for benign renamed/resaved files. See
            // `FileEntry::format_label` / `FormatFlag`.
            "format" => {
                let (label, flag) = entry.format_label();
                if label.is_empty() {
                    return div().into_any_element();
                }
                let tip_kind = entry.display_kind.clone();
                let tip_magic = entry.display_magic.clone();
                let mut row = div()
                    .id(("file-row-format", row_ix))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_scale_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(label.clone())),
                    );
                match flag {
                    FormatFlag::Alert => {
                        let alert_color = cx.theme().danger;
                        row = row
                            .child(
                                svg()
                                    .path("icons/triangle-alert.svg")
                                    .icon_px(12.0)
                                    .text_color(alert_color),
                            )
                            .tooltip(move |window, cx| {
                                Tooltip::new(SharedString::from(format!(
                                    "Extension says \u{201C}{}\u{201D} but the content is \u{201C}{}\u{201D} — possible disguised file.",
                                    tip_kind, tip_magic
                                )))
                                .build(window, cx)
                            });
                    }
                    FormatFlag::Notice => {
                        let cue_color = cx.theme().muted_foreground;
                        row = row
                            .child(
                                svg()
                                    .path("icons/circle-help.svg")
                                    .icon_px(12.0)
                                    .text_color(cue_color),
                            )
                            .tooltip(move |window, cx| {
                                Tooltip::new(SharedString::from(format!(
                                    "Extension says \u{201C}{}\u{201D} but the content looks like \u{201C}{}\u{201D}.",
                                    tip_kind, tip_magic
                                )))
                                .build(window, cx)
                            });
                    }
                    FormatFlag::None => {}
                }
                row.into_any_element()
            }
            // Relative time is recomputed from `mtime_unix` every paint so
            // the label ("4 seconds ago") stays live; the shell's relative-
            // time tick repaints on a cadence so it counts up even while the
            // user is idle.
            "modified" => div()
                .text_scale_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(feraille_core::humanize_mtime(
                    entry.mtime_unix,
                    feraille_core::now_unix(),
                )))
                .into_any_element(),
            // Description: rich facts from the magic-byte parse,
            // populated lazily by the prefetch worker. Empty string
            // renders as an empty cell — no skeleton shimmer in v1.
            "description" => div()
                .text_scale_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(entry.display_description.clone()))
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }

    /// Plain text of a cell — the same strings `render_td` paints,
    /// minus decorations (icon, tag chips, star, mismatch badge). Powers
    /// double-click-to-fit column sizing (and is the export hook). Must
    /// stay in sync with `render_td`'s per-column text.
    fn cell_text(&self, row_ix: usize, col_ix: usize, _cx: &App) -> String {
        let Some(entry) = self.entries.get(row_ix) else {
            return String::new();
        };
        let col_key = self
            .columns
            .get(col_ix)
            .map(|col| col.key.as_ref())
            .unwrap_or("");
        match col_key {
            // Mirror render_td: the Name cell shows the display leaf.
            "name" => entry.display_name.clone(),
            "size" => entry.display_size.clone(),
            "format" => entry.format_label().0.to_string(),
            "modified" => feraille_core::humanize_mtime(entry.mtime_unix, feraille_core::now_unix()),
            "description" => entry.display_description.clone(),
            _ => String::new(),
        }
    }

    /// Viewport-driven thumbnail warming (prime directive: expensive
    /// work scheduled from a semantic event, off the UI thread, dropped
    /// if it lands late). Called by the table whenever the visible row
    /// range changes — first layout and every scroll. We fetch Quick
    /// Look thumbnails for the *visible* thumbnailable rows only, never
    /// the whole (possibly thousands-deep) folder.
    fn visible_rows_changed(
        &mut self,
        visible_range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.warm_thumbnails(visible_range, cx);
    }

    /// Width the Name cell spends on its leading type icon + gap and
    /// trailing star that `cell_text` can't see, so double-click-to-fit
    /// reserves room for them instead of clipping the filename. Other
    /// columns are pure text.
    fn autofit_extra(&self, col_ix: usize, _cx: &App) -> Pixels {
        let is_name = self
            .columns
            .get(col_ix)
            .map(|col| col.key.as_ref() == "name")
            .unwrap_or(false);
        // icon (18) + icon→text gap (8) + trailing star (12) + its gap (8).
        if is_name { px(46.0) } else { px(0.0) }
    }

    fn header_has_menu(&self, _cx: &App) -> bool {
        true
    }

    fn header_context_menu(
        &mut self,
        mut menu: PopupMenu,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        // Show/hide toggles for every hideable column, plus a reset.
        // Closure items mutate the table's delegate directly through its
        // entity, then refresh + emit `ColumnWidthsChanged` so the shell's
        // existing persistence subscription writes the new layout. `Name`
        // is the primary column and is never offered for hiding.
        let state = cx.entity();
        for col in default_columns() {
            if col.key.as_ref() == "name" {
                continue;
            }
            let key = col.key.to_string();
            let visible = self.is_column_visible(&key);
            // A leading check marks the shown columns (Finder-style).
            let label = if visible {
                format!("\u{2713} {}", col.name)
            } else {
                format!("\u{2007}\u{2007}{}", col.name)
            };
            let state_toggle = state.clone();
            menu = menu.item(PopupMenuItem::new(label).on_click(move |_ev, _w, cx| {
                let key = key.clone();
                state_toggle.update(cx, |s, cx| {
                    if s.delegate_mut().toggle_column(&key) {
                        s.refresh(cx);
                        let widths = s.col_widths();
                        cx.emit(TableEvent::ColumnWidthsChanged(widths));
                    }
                });
            }));
        }
        let state_reset = state.clone();
        menu.separator()
            .item(
                PopupMenuItem::new("Reset Columns").on_click(move |_ev, _w, cx| {
                    state_reset.update(cx, |s, cx| {
                        s.delegate_mut().reset_columns();
                        s.refresh(cx);
                        let widths = s.col_widths();
                        cx.emit(TableEvent::ColumnWidthsChanged(widths));
                    });
                }),
            )
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        use crate::shell::{
            BulkRenameSelected, ClearQuarantine, Compress, CompressTarBz2, CompressTarGz,
            CompressTarXz, CopyPath, DeleteImmediately, Duplicate, Extract, GetInfo, MakeAlias,
            MoveToTrash,
            OpenInNewTab, OpenSelected, OpenTerminalHere, OpenWithSlot0, OpenWithSlot1,
            OpenWithSlot2, OpenWithSlot3, OpenWithSlot4, OpenWithSlot5, OpenWithSlot6,
            OpenWithSlot7, OpenWithSlot8, OpenWithSlot9, OpenWithSlot10, OpenWithSlot11, QuickLook,
            RenameSelected, RevealInFinder, SlideshowFromHere, ToggleFavoriteForTarget,
            ToggleTagBlue, ToggleTagGray, ToggleTagGreen, ToggleTagOrange, ToggleTagPurple,
            ToggleTagRed, ToggleTagYellow,
        };

        // Anchor keyboard-shortcut resolution to the shell's stable
        // dispatch path (carries SHELL_CONTEXT, always painted) so the
        // item hints render from the first frame instead of popping in
        // a frame or two later. See `shell_focus`'s doc comment.
        let menu = menu.action_context(self.shell_focus.clone());

        // Prime directive: menu building is read-only — no shell or
        // filesystem queries at menu-open time.
        //
        // Tags come from the per-row `self.tags` slots the bulk load
        // already populated; the checkmarks therefore always agree
        // with the row's visible tag dots (rows past the load cap
        // show no dots and no checkmarks — consistent).
        //
        // Open With candidates come from the `open_with_warm` cache,
        // populated off-thread on selection-lead changes (see
        // `Shell::warm_open_with_for_row`). On a cache miss — e.g.
        // a direct right-click on a row that was never selected —
        // we show a disabled placeholder this one time, kick the
        // warm fetch, and the next open has the real submenu. Same
        // UX as Finder's "Fetching…" under a slow LaunchServices.
        let target_path = self
            .entries
            .get(row_ix)
            .and_then(|entry| self.path_for_entry(entry.id));
        let warmed_candidates: Option<Vec<crate::platform_shell::OpenWithCandidate>> =
            match (&target_path, &self.open_with_warm) {
                (Some(p), Some((warm_path, cands))) if warm_path == p => Some(cands.clone()),
                _ => None,
            };
        if warmed_candidates.is_none() {
            if let Some(p) = &target_path {
                spawn_open_with_warm(cx.entity().clone(), p.clone(), cx);
            }
        }
        let applied_tags: Vec<feraille_core::commands::TagColor> =
            self.tags.get(row_ix).cloned().unwrap_or_default();

        // Tags submenu — built as a nested PopupMenu Entity via
        // PopupMenu::build. Each colour is a `menu_with_check` so
        // applied tags render with a leading checkmark. Click
        // toggles via the ToggleTagX action.
        let tag_red_on = applied_tags.contains(&feraille_core::commands::TagColor::Red);
        let tag_orange_on = applied_tags.contains(&feraille_core::commands::TagColor::Orange);
        let tag_yellow_on = applied_tags.contains(&feraille_core::commands::TagColor::Yellow);
        let tag_green_on = applied_tags.contains(&feraille_core::commands::TagColor::Green);
        let tag_blue_on = applied_tags.contains(&feraille_core::commands::TagColor::Blue);
        let tag_purple_on = applied_tags.contains(&feraille_core::commands::TagColor::Purple);
        let tag_gray_on = applied_tags.contains(&feraille_core::commands::TagColor::Gray);

        // Availability over the resolved target set (the same set the
        // handler will act on), routed through `Availability`. Anchor
        // rules use the clicked/lead row; `SingleOnly` and capability
        // rules use the whole set. See docs/features/CONTEXT_MENU.md.
        let t = &self.menu_targets;
        let show_slideshow = Availability::When(avail_anchor_file).allows(t);
        let show_terminal = Availability::When(avail_anchor_dir).allows(t);
        let show_favorites = Availability::When(avail_anchor_dir).allows(t);
        let show_clear_quarantine = Availability::When(avail_any_quarantined).allows(t);
        let show_extract = Availability::When(avail_any_archive).allows(t);
        let show_single_only = Availability::SingleOnly.allows(t);
        // Bulk complement of the SingleOnly Rename: pattern rename over
        // the whole resolved set (docs/features/BULK_RENAME.md).
        let bulk_rename_count = t.len();

        let already_favorited = self.is_favorited.get(row_ix).copied().unwrap_or(false);
        let favorite_label = if already_favorited {
            "Remove from Favorites"
        } else {
            "Add to Favorites"
        };

        let mut menu = menu
            .menu("Open", Box::new(OpenSelected))
            .menu("Open in New Tab", Box::new(OpenInNewTab))
            .separator()
            .menu("Get Info", Box::new(GetInfo))
            .menu("Quick Look", Box::new(QuickLook));
        if show_slideshow {
            // Anchor command: start the viewer slideshow anchored to the
            // clicked file (docs/features/VIEWER.md). Folder anchors can't
            // start a slideshow, so the item is file-anchored.
            menu = menu.menu("Slideshow from Here", Box::new(SlideshowFromHere));
        }
        let mut menu = menu.separator().menu(
            feraille_core::commands::REVEAL_LABEL,
            Box::new(RevealInFinder),
        );
        if show_single_only {
            // SingleOnly: copying one path is the row action; copying many
            // joined paths is a deliberate, separate gesture, so this hides
            // past a single target rather than silently concatenating.
            menu = menu.menu("Copy Path", Box::new(CopyPath));
        }
        if show_terminal {
            // Anchor command: open a terminal at the clicked directory,
            // grouped with the path-oriented actions above.
            menu = menu.menu("Open Terminal Here", Box::new(OpenTerminalHere));
        }
        let mut menu = menu.separator();
        if show_single_only {
            // SingleOnly: Rename targets one file (single-target, like
            // Finder's inline rename); hidden on a multi-selection.
            menu = menu.menu("Rename\u{2026}", Box::new(RenameSelected));
        }
        if bulk_rename_count >= 2 {
            // Multi-selection twin of Rename: the pattern-rule modal
            // over every resolved target (docs/features/BULK_RENAME.md).
            menu = menu.menu(
                format!("Rename {bulk_rename_count} Items\u{2026}"),
                Box::new(BulkRenameSelected),
            );
        }
        // "Compress As" submenu — the alternative formats to the one-click
        // ZIP. Built here (deref cx → &mut App, like the Tags/Open-With
        // submenus below) so it can be attached in menu order.
        let compress_as_submenu = PopupMenu::build(window, cx, |m, _w, _c| {
            m.menu("TAR.GZ", Box::new(CompressTarGz))
                .menu("TAR.BZ2", Box::new(CompressTarBz2))
                .menu("TAR.XZ", Box::new(CompressTarXz))
        });
        let mut menu = menu
            .menu("Duplicate", Box::new(Duplicate))
            .menu("Make Alias", Box::new(MakeAlias))
            .menu("Compress", Box::new(Compress))
            .item(PopupMenuItem::submenu("Compress As", compress_as_submenu));
        if show_extract {
            // Capability command: shown when any target is an archive
            // (docs/features/CONTEXT_MENU.md). `Shell::on_extract` acts on
            // the archive subset and chooses a smart destination per archive.
            menu = menu.menu("Extract", Box::new(Extract));
        }
        if show_clear_quarantine {
            // Capability command (docs/features/CONTEXT_MENU.md): show when
            // ANY row in the resolved target set carries the
            // Mark-of-the-Web, matching `Shell::on_clear_quarantine`, which
            // strips it from the quarantined subset. Reads the caps
            // `push_menu_targets` staged at right-click time — no xattr
            // query at menu-open time. Right-clicking the clean file in a
            // mixed selection now offers the command too, instead of hiding
            // it based on the single clicked row.
            menu = menu.separator().menu(
                feraille_core::commands::CLEAR_QUARANTINE_LABEL,
                Box::new(ClearQuarantine),
            );
        }
        if show_favorites {
            // Anchor command: toggle the clicked folder's path against the
            // user's Favorites (docs/features/FAVORITES.md §2.1).
            // `resolve_favorite_target` reads the row from `context_row`.
            menu = menu
                .separator()
                .menu(favorite_label, Box::new(ToggleFavoriteForTarget));
        }

        // Build submenu Entities via `PopupMenu::build`, which only
        // needs `&mut App` (which we have via Context<TableState>'s
        // deref). The parent menu accepts pre-built submenu entries
        // through `PopupMenuItem::submenu(label, entity)`.
        let app_cx: &mut gpui::App = cx;

        // SingleOnly: "Open With" resolves one warmed path; on a
        // multi-selection the slot indices wouldn't map to a single app,
        // so the submenu is hidden rather than acting on just the anchor.
        if show_single_only {
            match &warmed_candidates {
                Some(candidates) if !candidates.is_empty() => {
                    let candidates_for_build = candidates.clone();
                    let open_with_submenu =
                        PopupMenu::build(window, app_cx, move |mut m, _w, _c| {
                            for (i, cand) in candidates_for_build.iter().take(12).enumerate() {
                                let label = if cand.is_default {
                                    SharedString::from(format!("{} (default)", cand.name))
                                } else {
                                    SharedString::from(cand.name.clone())
                                };
                                let action: Box<dyn gpui::Action> = match i {
                                    0 => Box::new(OpenWithSlot0),
                                    1 => Box::new(OpenWithSlot1),
                                    2 => Box::new(OpenWithSlot2),
                                    3 => Box::new(OpenWithSlot3),
                                    4 => Box::new(OpenWithSlot4),
                                    5 => Box::new(OpenWithSlot5),
                                    6 => Box::new(OpenWithSlot6),
                                    7 => Box::new(OpenWithSlot7),
                                    8 => Box::new(OpenWithSlot8),
                                    9 => Box::new(OpenWithSlot9),
                                    10 => Box::new(OpenWithSlot10),
                                    _ => Box::new(OpenWithSlot11),
                                };
                                m = m.menu(label, action);
                            }
                            m
                        });
                    menu = menu.item(PopupMenuItem::submenu("Open With", open_with_submenu));
                }
                // Cache warm but LaunchServices offered nothing: omit the
                // submenu entirely (pre-existing behavior for empty sets).
                Some(_) => {}
                // Cache miss — the warm fetch was kicked above; show a
                // disabled placeholder for this one open.
                None => {
                    menu = menu
                        .item(PopupMenuItem::new("Open With (indexing\u{2026})").disabled(true));
                }
            }
        }

        // Names of the tags applied to the clicked row — offered for
        // pinning to the sidebar as Tag favorites (§9).
        let applied_tag_names: Vec<String> =
            applied_tags.iter().map(|c| c.name().to_string()).collect();
        let tags_submenu = PopupMenu::build(window, app_cx, move |m, _w, _c| {
            let mut m = m
                .menu_with_check("Red", tag_red_on, Box::new(ToggleTagRed))
                .menu_with_check("Orange", tag_orange_on, Box::new(ToggleTagOrange))
                .menu_with_check("Yellow", tag_yellow_on, Box::new(ToggleTagYellow))
                .menu_with_check("Green", tag_green_on, Box::new(ToggleTagGreen))
                .menu_with_check("Blue", tag_blue_on, Box::new(ToggleTagBlue))
                .menu_with_check("Purple", tag_purple_on, Box::new(ToggleTagPurple))
                .menu_with_check("Gray", tag_gray_on, Box::new(ToggleTagGray));
            // Pin each applied tag to the sidebar. Closure items add the
            // Tag favorite directly through the process-global entity —
            // no per-tag action needed (writes are off the paint path).
            if !applied_tag_names.is_empty() {
                m = m.separator();
                for name in &applied_tag_names {
                    let name = name.clone();
                    let label = format!("Pin \u{201c}{name}\u{201d} to Sidebar");
                    m = m.item(PopupMenuItem::new(label).on_click(move |_ev, _w, cx| {
                        let favs = crate::process_state::process_state(cx).favorites.clone();
                        let name = name.clone();
                        favs.update(cx, |f, cx| {
                            f.add_tag(name, cx);
                        });
                    }));
                }
            }
            m
        });
        menu = menu.item(PopupMenuItem::submenu("Tags", tags_submenu));

        menu.separator()
            .menu(feraille_core::commands::TRASH_LABEL, Box::new(MoveToTrash))
            .menu("Delete Immediately\u{2026}", Box::new(DeleteImmediately))
    }

    fn move_column(
        &mut self,
        col_ix: usize,
        to_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        if col_ix == to_ix || col_ix >= self.columns.len() || to_ix >= self.columns.len() {
            return;
        }
        let column = self.columns.remove(col_ix);
        self.columns.insert(to_ix, column);
    }

    /// Click on a header runs this — we delegate to the existing
    /// `sort_in_place` helper, mapping the column index back to a
    /// `SortColumn` via the `columns` vec's index → key lookup. The
    /// Table's column moves shift indices around, which is why we
    /// resolve via key rather than hard-coding indices.
    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) {
        let Some(col) = self.columns.get(col_ix) else {
            return;
        };
        let Ok(sort_col) = col.key.parse::<SortColumn>() else {
            return;
        };
        match sort {
            ColumnSort::Default => {
                // "Reset to natural order" — sort by name ascending
                // (Finder convention) as a deterministic fallback,
                // since we don't retain the load-time order.
                self.reset_sort();
            }
            ColumnSort::Ascending => {
                self.apply_sort(sort_col, true);
            }
            ColumnSort::Descending => {
                self.apply_sort(sort_col, false);
            }
        }
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        // Phase 10 polish: a centred, two-line empty state with the
        // Lucide inbox glyph above the copy reads "considered" rather
        // than "we forgot to handle this case."
        gpui_component::v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                gpui::svg()
                    .path("icons/inbox.svg")
                    .icon_px(48.0)
                    .text_color(cx.theme().muted_foreground.opacity(0.5)),
            )
            .child(
                div()
                    .text_scale_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("This folder is empty."),
            )
    }

    /// Swap the whole table body for the loading view while a slow
    /// device (spun-down external drive, cold network mount) is waking
    /// up. Also removes the previous directory's stale rows from the
    /// screen — they belong to the path the breadcrumb no longer shows
    /// and must not stay clickable.
    fn loading(&self, _cx: &App) -> bool {
        self.slow_load.is_some()
    }

    /// The built-in pulsing skeleton rows (the in-pane "still working"
    /// signal — the status bar runs its indeterminate stripe in
    /// parallel) topped with a line naming what we're waiting on.
    fn render_loading(
        &mut self,
        size: gpui_component::Size,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let label = self
            .slow_load
            .clone()
            .unwrap_or_else(|| SharedString::from("…"));
        gpui_component::v_flex()
            .size_full()
            .child(crate::multi_table::Loading::new().size(size))
            .child(
                gpui_component::h_flex()
                    .w_full()
                    .justify_center()
                    .py_4()
                    .text_scale_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Reading \u{201c}{label}\u{201d}\u{2026}")),
            )
    }
}

/// Fetch Open With candidates for `path` on the background executor
/// and store them in the delegate's [`FileListDelegate::open_with_warm`]
/// cache. The fetch (`open_with_candidates`, ~10–50 ms of
/// LaunchServices / IAssocHandler work) never runs on the UI thread.
/// Last-writer-wins by design: the cache holds one entry — the most
/// recently warmed path — which is always the row the user is about
/// to right-click.
pub fn spawn_open_with_warm(
    table: gpui::Entity<TableState<FileListDelegate>>,
    path: PathBuf,
    cx: &mut gpui::App,
) {
    let weak = table.downgrade();
    let fetch_path = path.clone();
    cx.spawn(async move |cx| {
        let candidates = cx
            .background_executor()
            .spawn(async move { crate::platform_shell::open_with_candidates(&fetch_path) })
            .await;
        let _ = weak.update(cx, |state, cx| {
            state.delegate_mut().open_with_warm = Some((path, candidates));
            cx.notify();
        });
    })
    .detach();
}

/// Lookup helper for double-click open / Enter key — turn a row
/// selection into the path that should be navigated to (for a folder)
/// or opened with the default app (for a file).
pub fn entry_at(delegate: &FileListDelegate, row_ix: usize) -> Option<&FileEntry> {
    delegate.entries.get(row_ix)
}

/// Resolve a file entry's NodeId to an absolute path via the FS.
pub fn path_for(fs: &NativeFs, id: NodeId) -> Option<PathBuf> {
    fs.path_for(id)
}

/// Map a Finder colour tag to its render colour. Values mirror the
/// stock macOS palette (NSColor systemRed/orange/etc. with a slight
/// saturation bump so the 6-DIP dots stay readable on tinted row
/// backgrounds).
pub(crate) fn tag_color_rgba(c: feraille_core::commands::TagColor) -> gpui::Rgba {
    use feraille_core::commands::TagColor;
    match c {
        TagColor::Red => gpui::Rgba {
            r: 1.0,
            g: 0.23,
            b: 0.19,
            a: 1.0,
        },
        TagColor::Orange => gpui::Rgba {
            r: 1.0,
            g: 0.58,
            b: 0.0,
            a: 1.0,
        },
        TagColor::Yellow => gpui::Rgba {
            r: 1.0,
            g: 0.80,
            b: 0.0,
            a: 1.0,
        },
        TagColor::Green => gpui::Rgba {
            r: 0.30,
            g: 0.85,
            b: 0.39,
            a: 1.0,
        },
        TagColor::Blue => gpui::Rgba {
            r: 0.0,
            g: 0.48,
            b: 1.0,
            a: 1.0,
        },
        TagColor::Purple => gpui::Rgba {
            r: 0.69,
            g: 0.32,
            b: 0.87,
            a: 1.0,
        },
        TagColor::Gray => gpui::Rgba {
            r: 0.56,
            g: 0.56,
            b: 0.58,
            a: 1.0,
        },
    }
}

/// Mark-of-the-Web quarantine badge — small red dot in the icon's
/// top-right corner. Pulled out of `render_td` so the file-icon and
/// folder-icon paths share one stylesheet.
pub(crate) fn badge_overlay(this: Div) -> Div {
    this.child(
        div()
            .absolute()
            .top(px(-1.0))
            .right(px(-1.0))
            .w(px(7.0))
            .h(px(7.0))
            .rounded_full()
            .bg(gpui::rgb(0xFF3B30)),
    )
}

/// Sort columns supported by `apply_sort`. Pure logic, easy to extend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Size,
    /// Unified Format column (next-level Phase 1) — sorts by the
    /// magic-detected description, falling back to the extension-
    /// derived kind. Replaces the old `Kind` + `Magic` sort options.
    Format,
    Modified,
}

impl std::str::FromStr for SortColumn {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "name" => Ok(Self::Name),
            "size" => Ok(Self::Size),
            "format" | "kind" | "magic" => Ok(Self::Format),
            "modified" | "mtime" => Ok(Self::Modified),
            _ => Err(()),
        }
    }
}

/// In-place sort with folders-first grouping (Finder convention).
/// Pure logic, easy to extend.

/// The built-in column set, in default order.
fn default_columns() -> Vec<Column> {
    vec![
        Column::new("name", "Name").width(360.0).sortable(),
        Column::new("size", "Size").width(100.0).sortable(),
        Column::new("format", "Format").width(220.0).sortable(),
        Column::new("modified", "Modified").width(160.0).sortable(),
        // Description column: rich ` · `-joined facts derived
        // from the magic-byte parse (bitness/arch/subsystem
        // for binaries, w×h for images, channels/kHz/duration
        // for audio, etc.). Populated by the prefetch worker —
        // empty until the worker batch lands, then never
        // touched by paint. Not sortable in v1: lex sort of
        // description strings groups MP3s near MP4s but
        // separates 32-bit from 64-bit binaries, which is
        // confusing. Revisit if users ask.
        Column::new("description", "Description").width(320.0),
    ]
}

/// Column width clamp for persisted values — a corrupt entry can't
/// collapse a column to 0 or blow the layout out.
const COLUMN_WIDTH_MIN: f32 = 60.0;
const COLUMN_WIDTH_MAX: f32 = 1200.0;

/// Split a persisted `key:width:vis,…` spec (see
/// `app_state::AppState::list_columns`) into the visible column set (in
/// the spec's order, with its widths) and the hidden set. `vis` is
/// `1` (visible) or `0` (hidden); a legacy `key:width` token with no
/// flag is treated as visible. Unknown keys are ignored; default
/// columns the spec never mentions (new in this build) trail as
/// visible. The primary `name` column can never be hidden, and the
/// visible set is never allowed to go empty. Forward- and
/// backward-compatible by construction.
pub fn split_persisted_columns(spec: Option<&str>) -> (Vec<Column>, Vec<Column>) {
    let mut pool = default_columns();
    let Some(spec) = spec else {
        return (pool, Vec::new());
    };
    let mut visible: Vec<Column> = Vec::with_capacity(pool.len());
    let mut hidden: Vec<Column> = Vec::new();
    for entry in spec.split(',') {
        let mut parts = entry.splitn(3, ':');
        let key = parts.next().unwrap_or("").trim();
        if key.is_empty() {
            continue;
        }
        let Some(pos) = pool.iter().position(|c| c.key.as_ref() == key) else {
            continue; // unknown key (older/newer build) — skip
        };
        let mut col = pool.remove(pos);
        // `"NaN".parse::<f32>()` succeeds — filter non-finite values
        // or the clamp propagates NaN into the layout.
        if let Some(w) = parts
            .next()
            .and_then(|w| w.trim().parse::<f32>().ok())
            .filter(|w| w.is_finite())
        {
            col.width = px(w.clamp(COLUMN_WIDTH_MIN, COLUMN_WIDTH_MAX));
        }
        let vis = parts.next().map(|v| v.trim() != "0").unwrap_or(true);
        if vis || col.key.as_ref() == "name" {
            visible.push(col);
        } else {
            hidden.push(col);
        }
    }
    // Columns this build's default set has that the spec didn't mention
    // (new since the spec was written) trail as visible.
    visible.append(&mut pool);
    // Never leave the table with no columns.
    if visible.is_empty() && !hidden.is_empty() {
        let name_pos = hidden.iter().position(|c| c.key.as_ref() == "name");
        visible.push(hidden.remove(name_pos.unwrap_or(0)));
    }
    (visible, hidden)
}

/// Serialize the current column layout into the persisted
/// `key:width:vis` spec: visible columns first (in order, with live
/// widths when the table provides them), then hidden columns. `vis` is
/// `1` for visible, `0` for hidden.
pub fn columns_spec(
    visible: &[Column],
    hidden: &[Column],
    live_widths: Option<&[Pixels]>,
) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(visible.len() + hidden.len());
    for (ix, col) in visible.iter().enumerate() {
        let width = live_widths
            .and_then(|w| w.get(ix).copied())
            .unwrap_or(col.width);
        parts.push(format!("{}:{:.0}:1", col.key, f32::from(width)));
    }
    for col in hidden {
        parts.push(format!("{}:{:.0}:0", col.key, f32::from(col.width)));
    }
    parts.join(",")
}

/// Display-leaf name for the drag chip (macOS `:` → `/`).
fn ghost_name(path: &std::path::Path) -> SharedString {
    path.file_name()
        .map(|n| feraille_fs_native::paths::display_leaf(n.to_string_lossy().as_ref()).into_owned())
        .unwrap_or_default()
        .into()
}

/// Sort with `sort_by_cached_key`: each element's key (its casefolded
/// name, plus the format label for the Format column) is built ONCE
/// per element, not per comparison. The previous comparator allocated
/// 2–4 lowercase Strings per COMPARISON — and the streaming pipeline
/// re-sorts the whole accumulated listing per 256-entry batch, so a
/// 100k-entry directory paid billions of allocations on the UI thread
/// while loading. (Next step if profiling still shows this hot: merge
/// each sorted batch into the sorted body instead of re-sorting.)
///
/// Ordering (unchanged): folders first regardless of direction; the
/// column key ascending/descending; casefolded display-name (the
/// display leaf, so ordering matches what the user reads on macOS
/// where an on-disk `:` shows as `/`) and NodeId as stable tiebreaks
/// (never reversed).
pub fn sort_in_place(entries: &mut [feraille_core::FileEntry], col: SortColumn, asc: bool) {
    use std::cmp::Reverse;
    fn non_dir(e: &feraille_core::FileEntry) -> bool {
        !matches!(e.kind, feraille_core::EntryKind::Directory)
    }
    fn name_key(e: &feraille_core::FileEntry) -> String {
        e.display_name.to_lowercase()
    }
    match (col, asc) {
        (SortColumn::Name, true) => {
            entries.sort_by_cached_key(|e| (non_dir(e), name_key(e), e.id.as_raw()));
        }
        (SortColumn::Name, false) => {
            entries.sort_by_cached_key(|e| (non_dir(e), Reverse(name_key(e)), e.id.as_raw()));
        }
        (SortColumn::Size, true) => {
            entries.sort_by_cached_key(|e| (non_dir(e), e.size, name_key(e), e.id.as_raw()));
        }
        (SortColumn::Size, false) => {
            entries
                .sort_by_cached_key(|e| (non_dir(e), Reverse(e.size), name_key(e), e.id.as_raw()));
        }
        (SortColumn::Format, true) => {
            entries.sort_by_cached_key(|e| {
                (
                    non_dir(e),
                    e.format_label().0.to_lowercase(),
                    name_key(e),
                    e.id.as_raw(),
                )
            });
        }
        (SortColumn::Format, false) => {
            entries.sort_by_cached_key(|e| {
                (
                    non_dir(e),
                    Reverse(e.format_label().0.to_lowercase()),
                    name_key(e),
                    e.id.as_raw(),
                )
            });
        }
        (SortColumn::Modified, true) => {
            entries.sort_by_cached_key(|e| (non_dir(e), e.mtime_unix, name_key(e), e.id.as_raw()));
        }
        (SortColumn::Modified, false) => {
            entries.sort_by_cached_key(|e| {
                (non_dir(e), Reverse(e.mtime_unix), name_key(e), e.id.as_raw())
            });
        }
    }
}

/// Apply a column sort to the live Table by enum. The toolbar sort
/// menu calls this directly; [`apply_sort`] is the string-keyed
/// wrapper for the `--sort` CLI flag.
pub fn apply_sort_column<C: gpui::AppContext>(
    table: &gpui::Entity<TableState<FileListDelegate>>,
    col: SortColumn,
    ascending: bool,
    cx: &mut C,
) {
    table.update(cx, |state, cx| {
        state.delegate_mut().apply_sort(col, ascending);
        state.refresh(cx);
    });
}

/// Apply a column sort to the live Table. Used by the
/// `--sort <col[-desc]>` CLI flag and (eventually) by clicks on
/// the column header row.
pub fn apply_sort<C: gpui::AppContext>(
    table: &gpui::Entity<TableState<FileListDelegate>>,
    column_name: &str,
    ascending: bool,
    cx: &mut C,
) {
    let Ok(col) = column_name.parse::<SortColumn>() else {
        crate::log_warn!(90, "unknown sort column: {column_name}");
        return;
    };
    apply_sort_column(table, col, ascending, cx);
}

// Compile-time sanity check that FontWeight stays in scope — used
// transitively by render_td when we add bold-name styling for
// directories in a future polish pass.
#[allow(dead_code)]
fn _font_weight_check() -> FontWeight {
    FontWeight::MEDIUM
}

#[cfg(test)]
mod column_persist_tests {
    use super::{columns_spec, default_columns, split_persisted_columns};
    use gpui::px;

    #[test]
    fn spec_round_trips_order_and_widths() {
        let (cols, hidden) = split_persisted_columns(Some("modified:200,name:420,size:90"));
        assert!(hidden.is_empty());
        let keys: Vec<&str> = cols.iter().map(|c| c.key.as_ref()).collect();
        // Listed keys in spec order; unlisted (format, description)
        // trail in default relative order.
        assert_eq!(keys, ["modified", "name", "size", "format", "description"]);
        assert_eq!(cols[0].width, px(200.0));
        assert_eq!(cols[1].width, px(420.0));
        assert_eq!(cols[2].width, px(90.0));
        // Serialize → re-split gives the same layout.
        let spec = columns_spec(&cols, &hidden, None);
        let (again, _) = split_persisted_columns(Some(&spec));
        assert_eq!(
            again.iter().map(|c| c.key.clone()).collect::<Vec<_>>(),
            cols.iter().map(|c| c.key.clone()).collect::<Vec<_>>()
        );
        assert_eq!(again[0].width, cols[0].width);
    }

    #[test]
    fn hidden_columns_round_trip() {
        // A `:0` flag hides a column; it lands in the hidden set and
        // survives a serialize → re-split cycle.
        let (visible, hidden) =
            split_persisted_columns(Some("name:360:1,size:100:1,format:220:0,modified:160:1"));
        assert!(visible.iter().all(|c| c.key.as_ref() != "format"));
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].key.as_ref(), "format");
        let spec = columns_spec(&visible, &hidden, None);
        let (v2, h2) = split_persisted_columns(Some(&spec));
        assert!(v2.iter().all(|c| c.key.as_ref() != "format"));
        assert_eq!(h2.len(), 1);
        assert_eq!(h2[0].key.as_ref(), "format");
    }

    #[test]
    fn name_column_is_never_hidden() {
        // Even an explicit `name:...:0` keeps Name visible.
        let (visible, hidden) = split_persisted_columns(Some("name:360:0,size:100:1"));
        assert!(visible.iter().any(|c| c.key.as_ref() == "name"));
        assert!(hidden.iter().all(|c| c.key.as_ref() != "name"));
    }

    #[test]
    fn hostile_specs_cannot_wedge_the_table() {
        // Unknown keys, garbage widths, empty entries: defaults survive.
        let (cols, hidden) =
            split_persisted_columns(Some("bogus:10,,name:NaN,size:-50,modified:99999"));
        assert!(hidden.is_empty());
        assert_eq!(cols.len(), default_columns().len());
        // name kept its default width (unparseable), size clamped up,
        // modified clamped down.
        assert_eq!(cols[0].key.as_ref(), "name");
        assert_eq!(cols[0].width, px(360.0));
        assert_eq!(cols[1].width, px(super::COLUMN_WIDTH_MIN));
        assert_eq!(cols[2].width, px(super::COLUMN_WIDTH_MAX));
        // Live widths override construction widths in the spec, and each
        // visible token now carries the `:1` visibility flag.
        let spec = columns_spec(&cols, &hidden, Some(&[px(111.0), px(222.0)]));
        assert!(spec.starts_with("name:111:1,size:222:1,"));
    }
}

#[cfg(test)]
mod sort_tests {
    use super::{SortColumn, sort_in_place};
    use feraille_core::{EntryKind, FileEntry, NodeId};

    fn entry(id: u64, name: &str, kind: EntryKind, size: u64, mtime: i64) -> FileEntry {
        FileEntry {
            id: NodeId::from_raw(id).unwrap(),
            name: name.into(),
            display_name: name.into(),
            name_has_hazards: false,
            kind,
            size,
            mtime_unix: mtime,
            display_size: String::new(),
            display_kind: String::new(),
            display_magic: String::new(),
            display_description: String::new(),
            is_quarantined: false,
            quarantine: None,
            hidden: false,
        }
    }

    fn ids(entries: &[FileEntry]) -> Vec<u64> {
        entries.iter().map(|e| e.id.as_raw()).collect()
    }

    /// A re-enumeration arrives in raw readdir order, so the in-place
    /// reload path reapplies the tab's active sort. This guards the
    /// invariant that makes that correct: sorting is a pure function of
    /// the rows, so reapplying the same `(col, asc)` to the same set in
    /// any incoming order lands on one canonical order. If this ever
    /// stops holding, a watcher-driven reload (e.g. clearing the
    /// Mark-of-the-Web) would shuffle a sorted view and scramble a
    /// live Shift-range selection.
    #[test]
    fn reapplying_sort_is_order_independent() {
        let mk = || {
            vec![
                entry(1, "bravo.txt", EntryKind::File, 30, 100),
                entry(2, "alpha.txt", EntryKind::File, 10, 300),
                entry(3, "charlie.txt", EntryKind::File, 20, 200),
                entry(4, "sub", EntryKind::Directory, 0, 50),
            ]
        };
        for (col, asc) in [
            (SortColumn::Name, true),
            (SortColumn::Size, false),
            (SortColumn::Modified, false),
        ] {
            let mut from_load_order = mk();
            sort_in_place(&mut from_load_order, col, asc);

            // Same rows, different incoming (readdir) order.
            let mut reshuffled = mk();
            reshuffled.reverse();
            sort_in_place(&mut reshuffled, col, asc);

            assert_eq!(
                ids(&from_load_order),
                ids(&reshuffled),
                "sort {col:?} asc={asc} must be independent of arrival order"
            );
        }
    }

    /// Folders sort ahead of files regardless of direction; the sort
    /// key only orders within each group (Finder convention). The
    /// reload reapplies via this same helper, so the grouping survives.
    #[test]
    fn folders_lead_in_both_directions() {
        let mut rows = vec![
            entry(1, "zeta.txt", EntryKind::File, 0, 0),
            entry(2, "dir-b", EntryKind::Directory, 0, 0),
            entry(3, "alpha.txt", EntryKind::File, 0, 0),
            entry(4, "dir-a", EntryKind::Directory, 0, 0),
        ];
        sort_in_place(&mut rows, SortColumn::Name, false);
        assert!(
            matches!(rows[0].kind, EntryKind::Directory)
                && matches!(rows[1].kind, EntryKind::Directory),
            "directories must lead even on a descending sort"
        );
    }
}

#[cfg(test)]
mod menu_targets_tests {
    use super::{
        Availability, MenuTargets, TargetCap, avail_anchor_dir, avail_anchor_file,
        avail_any_quarantined,
    };
    use feraille_core::EntryKind;

    fn cap(is_quarantined: bool) -> TargetCap {
        TargetCap {
            kind: EntryKind::File,
            is_quarantined,
        }
    }

    fn dir_cap() -> TargetCap {
        TargetCap {
            kind: EntryKind::Directory,
            is_quarantined: false,
        }
    }

    fn targets(caps: Vec<TargetCap>) -> MenuTargets {
        MenuTargets { caps, anchor: None }
    }

    fn with_anchor(anchor: TargetCap) -> MenuTargets {
        MenuTargets {
            caps: vec![anchor],
            anchor: Some(anchor),
        }
    }

    /// The reported bug: a mixed selection (one quarantined, one clean)
    /// must offer Clear Quarantine no matter which row was right-clicked.
    /// `any` is the gate, so the order of the set is irrelevant — that is
    /// exactly what decouples the menu from "the file selected last".
    #[test]
    fn any_shows_on_mixed_selection_regardless_of_order() {
        let quarantined_first = targets(vec![cap(true), cap(false)]);
        let clean_first = targets(vec![cap(false), cap(true)]);
        assert!(quarantined_first.any(|c| c.is_quarantined));
        assert!(clean_first.any(|c| c.is_quarantined));
    }

    /// No quarantined target anywhere in the set → the bulk command is
    /// hidden. A single clean row behaves the same as a clean multi-set.
    #[test]
    fn any_hidden_when_no_target_qualifies() {
        assert!(!targets(vec![cap(false), cap(false)]).any(|c| c.is_quarantined));
        assert!(!targets(vec![]).any(|c| c.is_quarantined));
    }

    /// `all` is the conservative quantifier for commands that should only
    /// appear when the whole selection qualifies; the empty set is never
    /// "all" so an empty target list can't surface such a command.
    #[test]
    fn all_requires_every_target_and_rejects_empty() {
        assert!(targets(vec![cap(true), cap(true)]).all(|c| c.is_quarantined));
        assert!(!targets(vec![cap(true), cap(false)]).all(|c| c.is_quarantined));
        assert!(!targets(vec![]).all(|c| c.is_quarantined));
    }

    /// Count helpers back the SingleOnly archetype.
    #[test]
    fn count_helpers() {
        assert!(targets(vec![]).is_empty());
        assert!(targets(vec![cap(false)]).is_single());
        assert!(!targets(vec![cap(false)]).is_multi());
        assert!(targets(vec![cap(false), cap(false)]).is_multi());
        assert_eq!(targets(vec![cap(false), cap(false)]).len(), 2);
    }

    /// SingleOnly (Copy Path, Rename, Open With) shows for exactly one
    /// target and hides for zero or many — the Type-B rule.
    #[test]
    fn single_only_shows_for_exactly_one() {
        assert!(!Availability::SingleOnly.allows(&targets(vec![])));
        assert!(Availability::SingleOnly.allows(&targets(vec![cap(false)])));
        assert!(!Availability::SingleOnly.allows(&targets(vec![cap(false), cap(false)])));
    }

    /// The `When` callback drives capability and anchor rules. Clear
    /// Quarantine fires on any quarantined target regardless of order;
    /// the anchor predicates key off the clicked/lead row's kind.
    #[test]
    fn when_callbacks_for_capability_and_anchor() {
        let quar = Availability::When(avail_any_quarantined);
        assert!(quar.allows(&targets(vec![cap(false), cap(true)])));
        assert!(!quar.allows(&targets(vec![cap(false), cap(false)])));

        assert!(Availability::When(avail_anchor_dir).allows(&with_anchor(dir_cap())));
        assert!(!Availability::When(avail_anchor_dir).allows(&with_anchor(cap(false))));
        assert!(Availability::When(avail_anchor_file).allows(&with_anchor(cap(false))));
        assert!(!Availability::When(avail_anchor_file).allows(&with_anchor(dir_cap())));
        // No anchor → neither anchor rule fires.
        assert!(!Availability::When(avail_anchor_dir).allows(&targets(vec![])));
        assert!(!Availability::When(avail_anchor_file).allows(&targets(vec![])));
    }
}
