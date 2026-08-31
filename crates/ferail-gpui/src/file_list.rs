//! File-list table delegate: Phase 4.c.
//!
//! Wraps `ferail-fs-native` enumeration in a `TableDelegate` so
//! `gpui-component`'s virtualized `Table` renders the entries
//! efficiently even for directories with thousands of files. Columns
//! are Name / Size / Kind / Modified. Size/Kind are pre-formatted on the
//! domain side per the UI_NONBLOCKING contract; Modified is the exception,
//! rendered live from `mtime_unix` so its relative label keeps counting
//! (pure arithmetic, bounded to visible rows, still nonblocking).

use crate::text::{IconScale as _, TextScale as _, TruncateMiddle as _, elide_label};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ferail_core::{EntryKind, FileEntry, FormatFlag, NodeId};
use ferail_fs_native::NativeFs;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Div, Entity, ExternalPaths, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Pixels, Point, Render, RenderImage, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled, WeakEntity, Window, div, img, px, svg,
};
use gpui_component::{
    ActiveTheme,
    input::InputState,
    menu::{PopupMenu, PopupMenuItem},
    tooltip::Tooltip,
};
use smallvec::{SmallVec, smallvec};

use crate::icons::{IconCache, file_type_icon, tint_color};
use crate::multi_table::{Column, ColumnSort, TableDelegate, TableEvent, TableState};
use crate::tasks::{TaskKind, TaskRegistry};
use crate::thumbnails::{THUMB_PX, ThumbnailCache, is_thumbnailable, show_thumbnails};

/// The floating ghost shown under the cursor while dragging rows out of
/// the list (or to an in-app drop target): gpui's `on_drag` needs an
/// `Entity<impl Render>` for the drag image. A single-row drag shows the
/// item's icon/thumbnail + name as a labelled chip; a multi-row drag
/// renders the *actual* item images as a loose Finder-style stack
/// (capped at [`GHOST_STACK_CAP`]) with a red count badge, no "N items"
/// string. The images come straight from the already-warm thumbnail/icon
/// caches, so building the ghost never touches the filesystem.
///
/// gpui paints the drag view at `mouse − cursor_offset`, and
/// `cursor_offset` is the grab point within the *dragged element*, for
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
    /// Flips permanently for this gesture when GPUI promotes it to the native
    /// platform session. The typed payload may be restored on re-entry so
    /// Ferail drop targets still work, but OLE remains the sole visual owner.
    pub native_owned: Arc<AtomicBool>,
}

/// Max real images to render in a multi-item drag stack. Finder shows a
/// small fan regardless of selection size; more than this just adds
/// cache lookups and visual mush.
pub const GHOST_STACK_CAP: usize = 4;

/// Max file names listed beside a multi-item drag stack before the rest
/// collapse into a "+N more" line.
pub const GHOST_NAME_CAP: usize = 3;

/// A native drag pasteboard ultimately needs one URL per item. Keep row
/// painting bounded when a Flat-view selection contains millions of files.
pub const MAX_EAGER_DRAG_ITEMS: usize = 10_000;

impl Render for DragBadge {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.native_owned.load(Ordering::Acquire) {
            return gpui::Empty.into_any_element();
        }
        let theme = cx.theme();
        let private_names: SmallVec<[SharedString; GHOST_STACK_CAP]> = self
            .names
            .iter()
            .map(|name| crate::private_mode::present_leaf(name, false))
            .collect();
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
                        .child(private_names.first().cloned().unwrap_or_default()),
                );
            div().child(chip)
        } else {
            // Multiple items: a loose stack of the real item images (drawn
            // back-to-front so the lead lands on top) with a red count
            // badge, beside a short list of the names: the Finder stack,
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
                    .child(ferail_core::counts::format_count(self.count as u64)),
            );
            // Name list: the first GHOST_NAME_CAP, then a "+N more" line.
            let shown = private_names.len().min(GHOST_NAME_CAP);
            let mut names = div()
                .flex()
                .flex_col()
                .gap_0p5()
                .text_scale_sm()
                .text_color(theme.foreground);
            for name in private_names.iter().take(GHOST_NAME_CAP) {
                names = names.child(div().max_w(px(220.0)).truncate().child(name.clone()));
            }
            if self.count > shown {
                names = names.child(
                    div()
                        .text_scale_xs()
                        .text_color(theme.muted_foreground)
                        .child(trn!("+{n} more", "+{n} more", self.count - shown)),
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
            .into_any_element()
    }
}

/// One target row's capability snapshot for context-menu gating,
/// projected from the cached `FileEntry` at right-click time, no I/O,
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
                && ferail_archive::Format::is_archive_path(&e.name),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct MenuCapCounts {
    quarantined: usize,
    archives: usize,
}

impl MenuCapCounts {
    fn from_entries(entries: &[FileEntry]) -> Self {
        let mut counts = Self::default();
        counts.extend(entries);
        counts
    }

    fn extend(&mut self, entries: &[FileEntry]) {
        for entry in entries {
            let cap = TargetCap::from(entry);
            self.quarantined = self
                .quarantined
                .saturating_add(usize::from(cap.is_quarantined));
            self.archives = self.archives.saturating_add(usize::from(cap.is_archive));
        }
    }
}

/// Capabilities of the rows a context command will act on, resolved once
/// at menu-open time with the SAME logic as
/// `Shell::action_entries_visible_order` so the menu the user sees
/// matches the set the handler touches (see docs/features/CONTEXT_MENU.md).
///
/// This is deliberately a compact summary, not one `TargetCap` per row. A
/// symbolic Select All over four million rows therefore stays O(1) in both
/// time and memory when its menu opens. `anchor` drives commands that act on
/// the single clicked row.
#[derive(Clone, Default)]
pub struct MenuTargets {
    count: usize,
    any_quarantined: bool,
    any_archive: bool,
    pub anchor: Option<TargetCap>,
}

impl MenuTargets {
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Exactly one target: the gate for commands that only make sense
    /// per single file (Copy Path, Rename, Open With).
    pub fn is_single(&self) -> bool {
        self.count == 1
    }

    /// More than one target.
    pub fn is_multi(&self) -> bool {
        self.count > 1
    }

    pub fn any_quarantined(&self) -> bool {
        self.any_quarantined
    }

    pub fn any_archive(&self) -> bool {
        self.any_archive
    }
}

const MAX_MENU_CAP_SCAN_ROWS: usize = 65_536;

/// Resolve the rows a context command will target, from the row the user
/// right-clicked plus the selection as it stands *at that instant*.
///
/// This is the menu-side twin of `Shell::resolve_targets` and must agree
/// with it row for row (spec §2.4): a right-click **inside** the selection
/// targets the whole set; a right-click on an unselected row targets only
/// that row, because the click collapses the selection onto it before any
/// command dispatches.
///
/// Deliberately a pure function of `(entries, selected, row_ix)` rather
/// than a snapshot staged ahead of time: gpui-component builds the menu in
/// a `window.defer` callback that is queued *before* the table's
/// `RightClickedRow` event reaches the Shell, so anything the Shell stages
/// from that event arrives one right-click too late, which is what left
/// the first menu after a folder load missing every gated command
/// (Rename, Copy Path, Extract, Open With, …).
///
/// Cache-only (row caps project from the already-loaded `FileEntry`), so
/// it is safe on the menu-build path under the prime directive.
pub fn resolve_menu_targets(
    entries: &[FileEntry],
    selected: &HashSet<NodeId>,
    row_ix: usize,
) -> MenuTargets {
    resolve_menu_targets_with_mode(
        entries,
        selected,
        false,
        row_ix,
        MenuCapCounts::from_entries(entries),
    )
}

fn resolve_menu_targets_with_mode(
    entries: &[FileEntry],
    selected: &HashSet<NodeId>,
    selection_all: bool,
    row_ix: usize,
    all_caps: MenuCapCounts,
) -> MenuTargets {
    let Some(clicked) = entries.get(row_ix) else {
        return MenuTargets::default();
    };
    let anchor = TargetCap::from(clicked);
    let is_selected = |id: NodeId| {
        if selection_all {
            !selected.contains(&id)
        } else {
            selected.contains(&id)
        }
    };
    if !is_selected(clicked.id) {
        return MenuTargets {
            count: 1,
            any_quarantined: anchor.is_quarantined,
            any_archive: anchor.is_archive,
            anchor: Some(anchor),
        };
    }

    if selection_all {
        let count = entries.len().saturating_sub(selected.len());
        // Exceptions are intentionally not resolved back through a huge row
        // model. If every qualifying row happened to be excluded this may
        // leave a harmless subset command visible; its handler still filters
        // the true target set. It can never hide an operation that applies.
        return MenuTargets {
            count,
            any_quarantined: count != 0 && all_caps.quarantined != 0,
            any_archive: count != 0 && all_caps.archives != 0,
            anchor: Some(anchor),
        };
    }

    let count = selected.len();
    if entries.len() > MAX_MENU_CAP_SCAN_ROWS && count > 1 {
        // A large explicit range is opaque on the menu-open path. Subset
        // commands are conservatively offered; dispatch resolves/filter the
        // real targets off this rendering path.
        return MenuTargets {
            count,
            any_quarantined: all_caps.quarantined != 0,
            any_archive: all_caps.archives != 0,
            anchor: Some(anchor),
        };
    }

    let mut any_quarantined = false;
    let mut any_archive = false;
    for entry in entries.iter().filter(|entry| selected.contains(&entry.id)) {
        let cap = TargetCap::from(entry);
        any_quarantined |= cap.is_quarantined;
        any_archive |= cap.is_archive;
    }
    MenuTargets {
        count,
        any_quarantined,
        any_archive,
        anchor: Some(anchor),
    }
}

/// Availability rule for a context command whose visibility depends on
/// the resolved selection (docs/features/CONTEXT_MENU.md). Commands that
/// always apply to a group: whether as one batch op (Compress, Trash)
/// or fanned out per file (Open, Quick Look, Get Info): need no rule
/// and are added unconditionally; only the two cases below gate.
pub enum Availability {
    /// Meaningful for exactly one file; hidden once more than one row is
    /// targeted (Copy Path, Rename, Open With).
    SingleOnly,
    /// Per-command callback over the resolved targets: the escape hatch
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

/// Capability: a target carries the Mark-of-the-Web, or the anchor is a
/// directory whose descendants can carry it. Directory recursion is explicit
/// demand and remains discoverable even when the folder row itself is clean.
fn avail_any_quarantined(t: &MenuTargets) -> bool {
    t.any_quarantined() || matches!(t.anchor.map(|cap| cap.kind), Some(EntryKind::Directory))
}

/// Bulk rule: at least one target is an archive file: offer Extract, which
/// acts on the archive subset (mixed selections extract only their archives),
/// mirroring how Clear Quarantine acts on the quarantined subset.
fn avail_any_archive(t: &MenuTargets) -> bool {
    t.any_archive()
}

/// Anchor rule: the right-clicked (else lead) row is a folder, for
/// commands that act on one directory (Open Terminal Here, Favorites).
fn avail_anchor_dir(t: &MenuTargets) -> bool {
    matches!(t.anchor.map(|c| c.kind), Some(EntryKind::Directory))
}

/// Anchor rule: the right-clicked (else lead) row is a non-directory,
/// for file-anchored commands (Slideshow from Here).
fn avail_anchor_file(t: &MenuTargets) -> bool {
    t.anchor
        .map(|c| !matches!(c.kind, EntryKind::Directory))
        .unwrap_or(false)
}

/// Delegate that vends the current directory's entries to the
/// Table. Holds the live `Vec<FileEntry>`; the Shell rotates it on
/// every `navigate()`. The Vec is already filtered by both
/// `show_hidden` and `filter_text` at `load()` time: the Table
/// always sees the user-visible subset, no per-cell skipping.
pub struct FileListDelegate {
    pub entries: Vec<FileEntry>,
    /// Tree metadata for archive rows, parallel to `entries` (same shape as
    /// `tags` / `heats`). **Empty for ordinary directory listings**, which is
    /// what keeps this addition inert for normal browsing: the Name cell only
    /// draws indentation and a disclosure caret when a row has an entry here.
    pub archive_rows: Vec<ferail_archive::TreeRow>,
    /// The workbench that owns those rows, so a caret click can toggle the
    /// folder open. `None` whenever `archive_rows` is empty.
    pub archive_view: Option<WeakEntity<crate::archive::ArchiveView>>,
    pub columns: Vec<Column>,
    /// Columns the user has hidden (header right-click → uncheck). Kept
    /// out of `columns`: the table only ever sees the visible set, so
    /// its index-based reorder/sort/resize logic stays untouched, but
    /// retained here (with identity + width) so re-showing restores them
    /// and they persist across launches. See [`split_persisted_columns`].
    pub hidden_columns: Vec<Column>,
    pub fs: Arc<NativeFs>,
    /// Unique owner inside the process asset coordinator. Unlike TabId this
    /// also exists for archive/tool tables, and prevents one surface's local
    /// generation counter from retiring another surface's work.
    asset_scope: Option<ferail_core::asset_work::AssetWorkScope>,
    /// Snapshot of entry paths captured during enumeration/application.
    /// Rendering may read this cache, but must not call back into the
    /// filesystem resolver.
    pub paths: HashMap<NodeId, PathBuf>,
    /// Compact path ownership for a recursive Flat surface. Each distinct
    /// parent directory is stored once; rows keep only a u32 directory index,
    /// and their scan-local NodeId encodes the insertion index. Dropping the
    /// surface drops every path without touching process-global identity maps.
    flat_paths: Option<FlatPathStore>,
    /// Rows excluded by the current Flat filter. Visible and filtered rows
    /// partition the snapshot, so filtering never duplicates the million-row
    /// model and clearing the field never rescans the filesystem.
    flat_filtered_entries: Vec<FileEntry>,
    flat_filter_text: String,
    /// At most one viewport detail worker runs per surface. Rapid scrolling
    /// replaces this pending range instead of spawning an unbounded train of
    /// magic/xattr workers for viewports the user has already left.
    detail_in_flight: bool,
    detail_pending: Option<Range<usize>>,
    detail_cancel: Option<Arc<AtomicBool>>,
    /// Incremented whenever row positions can change. A bounded worker applies
    /// directly by captured row index, so this revision is what makes that O(1)
    /// apply safe across sorts, filters, and navigation.
    detail_revision: u64,
    /// Ordinary listings enable detail warming only after enumeration reaches
    /// Done; Flat enables it immediately because its scan streams indefinitely.
    detail_ready: bool,
    detail_force: bool,
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
    pub tags: Vec<Vec<ferail_core::commands::TagColor>>,
    /// `is_favorited[row]` is `true` when the entry's path is currently
    /// in the user-curated Favorites list: drives the §5 star indicator
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
    /// Compact complement representation used by Cmd/Ctrl+A on every list.
    /// When true, `selected_set` contains the deselected exceptions.
    pub selection_all: bool,
    /// Aggregate capabilities of the visible row model. Kept incrementally
    /// so a symbolic whole-list context menu never scans the model.
    all_menu_caps: MenuCapCounts,
    /// The keyboard-cursor / range-lead, mirrored from the active
    /// tab. At most one. Cosmetic only: the Table primitive's
    /// `selected_row` overlay is the visible focus ring.
    pub lead: Option<NodeId>,
    /// One persistent filename editor shared by the Shell's tabs.  The
    /// virtualized table mounts it only for the matching `(tab, NodeId)`;
    /// every other row stays allocation-free and renders its normal label.
    pub inline_name_edit: Option<InlineNameEditBinding>,
    /// Warm cache for the right-click "Open With" submenu: the most
    /// recently fetched `(path, LaunchServices candidates)` pair.
    /// Populated off the UI thread by [`spawn_open_with_warm`]:
    /// triggered on selection-lead changes and on a cache-miss menu
    /// build. The menu builder reads ONLY this cache (prime
    /// directive: no shell queries at menu-open time); a miss shows
    /// a disabled "loading" placeholder; when the fetch lands, the retained
    /// submenu entity rebuilds itself with the real apps. The cache is a
    /// latency optimisation, never a correctness requirement.
    ///
    /// Dispatch handlers (`Shell::open_with_slot`) resolve slot
    /// indices against this same cache so the app at slot N when
    /// the menu was BUILT is the app that opens: re-fetching at
    /// dispatch could reorder candidates and launch the wrong app.
    pub open_with_warm: Option<(PathBuf, Vec<crate::platform_shell::OpenWithCandidate>)>,
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
    /// previous-frame fallback, which left shortcuts blank for the
    /// first frame or two after the menu opened.
    pub shell_focus: gpui::FocusHandle,
    /// Whether this listing is a trash folder. Set by the Shell when it starts
    /// a load, from a lexical test on the path, so the context menu can branch
    /// on it without asking the filesystem anything at menu-open time.
    pub browsing_trash: bool,
    /// Lazily-built drag payload for the CURRENT selection, shared by
    /// every selected row's `on_drag`. Built once on the first
    /// selected-row render after a selection/model change; without it
    /// each selected visible row re-walked ALL entries per render
    /// (Select All in a 10k folder ≈ 400k HashSet probes + PathBuf
    /// clones per pass). Invalidated by every selection write and
    /// every structural entries change.
    drag_snapshot: RefCell<Option<DragSnapshot>>,
    /// Status-bar totals, computed lazily once per model/selection
    /// change instead of O(N) sums on every render pass (`Cell` so the
    /// read-only render path can fill them). Invalidated together with
    /// `drag_snapshot`, plus when folder sizes stream in.
    pub cached_total_size: std::cell::Cell<Option<u64>>,
    pub cached_selected_size: std::cell::Cell<Option<u64>>,
    /// `Some(folder name)` while a *slow* directory load is in flight:
    /// set by `Shell`'s slow-load timer when the first enumeration batch
    /// hasn't landed after `SLOW_LOAD_INDICATOR_DELAY` (a spun-down
    /// external drive, a cold network mount). Flips the table into its
    /// skeleton loading view with a "Reading '<name>'…" line, replacing
    /// the previous directory's stale (and still-clickable) rows.
    /// Cleared by `clear()` / `replace_entries()`, which every load
    /// exit path funnels through. Deliberately NOT set at load start:
    /// fast local navigations would flash a skeleton every click.
    pub slow_load: Option<SharedString>,
    /// How many entries the filter field excluded from the last
    /// completed load (0 when the field is empty). Only read by the
    /// empty state, which must say "filtered out" rather than "this
    /// folder is empty" when a needle hid every row. Written by
    /// `Shell::finish_directory_load_in_tab`; reset by `clear()` /
    /// `replace_entries()` like `slow_load`, so a new load can't paint
    /// the previous one's figure.
    pub filtered_out: usize,
}

/// Render-only bridge from a tab delegate to the Shell-owned generic inline
/// edit lifecycle.  It contains no paths and performs no filesystem work.
#[derive(Clone)]
pub struct InlineNameEditBinding {
    pub tab_id: u64,
    pub model: crate::inline_edit::InlineEditModel<crate::inline_edit::FileNameEditTarget>,
    pub input: Entity<InputState>,
}

/// Process-shared filename editor resources passed into a new tab. `build_tab`
/// binds them to the freshly minted tab id without growing its constructor by
/// one argument per reusable editor primitive.
#[derive(Clone)]
pub struct InlineNameEditResources {
    pub model: crate::inline_edit::InlineEditModel<crate::inline_edit::FileNameEditTarget>,
    pub input: Entity<InputState>,
}

/// Drag payload for entries inside an archive.
///
/// Archive entries have no path on disk, so in-app targets carry their archive
/// coordinates. On macOS the row also promotes to a native promised-file drag:
/// Finder chooses a destination first, then AppKit invokes extraction on a
/// background operation queue. Nothing is eagerly unpacked merely by starting
/// a drag.
#[derive(Clone, Debug)]
pub struct ArchiveEntryDrag {
    pub archive: PathBuf,
    /// Stored entry paths; a directory brings its whole subtree on extract.
    pub entries: Vec<String>,
    /// Parallel to `entries`, taken from the already-loaded TOC so starting a
    /// native drag never stats or opens anything on the GUI thread.
    pub directories: Vec<bool>,
    pub password: Option<String>,
}

// A promised-file drag is represented twice while AppKit owns the pointer:
// the native NSFilePromiseProvider payload consumed by Finder, and this
// in-process archive-coordinate payload consumed by Ferail windows. GPUI does
// not retain custom typed drags when AppKit crosses to a second window, so
// Ferail drop targets use this coordinator as a fallback for the synthetic
// MouseMove/MouseUp events emitted by gpui_macos. It is main-thread-only: all
// reads and writes happen from GPUI/AppKit callbacks, never from promise
// writers.
thread_local! {
    static NATIVE_ARCHIVE_DRAG: std::cell::RefCell<Option<ArchiveEntryDrag>> = const {
        std::cell::RefCell::new(None)
    };
}

pub(crate) fn native_archive_drag() -> Option<ArchiveEntryDrag> {
    NATIVE_ARCHIVE_DRAG.with(|drag| drag.borrow().clone())
}

/// Cheap presence check for per-row render paths: no payload clone.
pub(crate) fn native_archive_drag_active() -> bool {
    NATIVE_ARCHIVE_DRAG.with(|drag| drag.borrow().is_some())
}

/// How a file row should treat something dropped **onto** it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArchiveDropTarget {
    /// Not an archive by name: the row is not a drop target at all.
    No,
    /// An archive this build can add entries to in place (ZIP).
    Accepts,
    /// A recognized archive whose format cannot be edited in place (7z,
    /// tar family, single-member gz/bz2/xz, LHA). Still a target, so the
    /// drop is refused visibly instead of falling through to the folder
    /// underneath and quietly moving the files somewhere else.
    ReadOnly,
}

/// Classify a row as a drop target for adding files to an archive.
///
/// Name-based on purpose: this runs in render and per drag-move, where the
/// Prime Directive forbids touching the file. `Format::from_path` is pure
/// string matching, and the worker re-derives the real format before writing
/// anything, so a mislabelled file fails there rather than corrupting.
/// ZIP-based *packages* (`.docx`, `.jar`, `.apk`) are not recognized as
/// archives by suffix, so they are never offered as add targets.
pub(crate) fn archive_drop_target(name: &str, kind: EntryKind) -> ArchiveDropTarget {
    if matches!(kind, EntryKind::Directory) {
        return ArchiveDropTarget::No;
    }
    match ferail_archive::Format::from_path(name) {
        Some(format) if format.capabilities().can_edit_in_place => ArchiveDropTarget::Accepts,
        Some(_) => ArchiveDropTarget::ReadOnly,
        None => ArchiveDropTarget::No,
    }
}

pub(crate) fn take_native_archive_drag() -> Option<ArchiveEntryDrag> {
    NATIVE_ARCHIVE_DRAG.with(|drag| drag.borrow_mut().take())
}

/// Only the macOS promise-promotion path parks a payload here; on other
/// platforms nothing starts a native archive session, so the setter is unused
/// while its readers stay live.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn set_native_archive_drag(drag: Option<ArchiveEntryDrag>) {
    NATIVE_ARCHIVE_DRAG.with(|slot| *slot.borrow_mut() = drag);
}

/// Finder asks every promise for a leaf name. Preserve the archive's real leaf
/// (including unusual Unicode), but disambiguate a multi-selection containing
/// two different paths with the same leaf so one promised item cannot replace
/// another at the destination.
#[cfg(target_os = "macos")]
fn archive_promise_names(entries: &[String], directories: &[bool]) -> Vec<String> {
    let mut seen = std::collections::HashMap::<String, usize>::new();
    entries
        .iter()
        .zip(directories.iter().copied())
        .map(|(entry, is_dir)| {
            let leaf = entry
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|leaf| !leaf.is_empty())
                .unwrap_or("archive-item");
            let count = seen.entry(leaf.to_lowercase()).or_default();
            *count += 1;
            if *count == 1 {
                return leaf.to_string();
            }
            if !is_dir
                && let Some((stem, extension)) = leaf.rsplit_once('.')
                && !stem.is_empty()
                && !extension.is_empty()
            {
                return format!("{stem} {}.{extension}", *count);
            }
            format!("{leaf} {}", *count)
        })
        .collect()
}

#[cfg(all(test, target_os = "macos"))]
mod archive_promise_tests {
    use super::archive_promise_names;

    #[test]
    fn promise_names_preserve_real_leafs_and_disambiguate_collisions() {
        let names = archive_promise_names(
            &[
                "first/report.pdf".into(),
                "second/report.pdf".into(),
                "folder/".into(),
                "other/folder/".into(),
                "safe\u{202e}fdp.exe".into(),
            ],
            &[false, false, true, true, false],
        );
        assert_eq!(
            names,
            [
                "report.pdf",
                "report 2.pdf",
                "folder",
                "folder 2",
                "safe\u{202e}fdp.exe",
            ]
        );
    }
}

/// See [`FileListDelegate::drag_snapshot`].
#[derive(Clone, Default)]
pub(crate) struct DragSnapshot {
    /// Visible-order paths of the whole selection: the real OS drag
    /// payload. Rows still clone this into their `ExternalPaths`
    /// value (gpui's mac backend needs it by value), but the walk,
    /// membership probes, and ghost assembly happen once.
    pub(crate) paths: Rc<Vec<PathBuf>>,
    /// Parallel to `paths`: whether each entry is a directory, from the
    /// already-listed `EntryKind`: so promoting the drag to a native
    /// session (`external_drag_payload`) never stats anything.
    pub(crate) dirs: Rc<Vec<bool>>,
    pub(crate) names: SmallVec<[SharedString; GHOST_STACK_CAP]>,
    pub(crate) icons: SmallVec<[Arc<RenderImage>; GHOST_STACK_CAP]>,
    /// Shared visual handoff flag for the next selected-items gesture. The
    /// drag constructor resets it; the native payload resolver sets it.
    pub(crate) native_owned: Arc<AtomicBool>,
}

/// Surface-local path arena for Flat View. Keeping complete relative parent
/// paths per *directory* is deliberately simpler than a parent-chain arena and
/// has the same useful asymptotic shape: real trees contain far fewer
/// directories than entries, while action-time full-path reconstruction stays
/// O(1) and never touches the filesystem.
struct FlatPathStore {
    root: PathBuf,
    id_base: u64,
    /// One UTF-8 relative path per distinct directory. The same `Arc` backs
    /// both this stable arena and the append-time lookup table, so the scan
    /// does not transiently duplicate every directory string. Recursive
    /// enumeration already skips non-UTF-8 leaf names.
    directories: Vec<Arc<str>>,
    directory_index: Option<HashMap<Arc<str>, u32>>,
    /// Surface-local canonical display sizes/types. These values repeat
    /// heavily ("12.3 MB", "JPG", …); sharing them removes millions of tiny
    /// allocations. The cap prevents hostile unique extensions from turning
    /// the interner itself into an unbounded second row index.
    display_texts: Option<HashSet<Arc<str>>>,
    row_directories: Vec<u32>,
    /// Shares the exact allocation held by `FileEntry::name`; this index is
    /// needed after visible rows are sorted, but must not duplicate millions
    /// of leaf-name allocations merely to reconstruct action paths.
    row_names: Vec<Arc<str>>,
    /// 0 = unseen, 1 = viewport worker in flight, 2 = derived. One byte per
    /// row prevents repeated I/O without a path-keyed cache or HashSet.
    detail_states: Vec<u8>,
}

/// Empty a row buffer, optionally returning its backing allocation to the
/// allocator. Ordinary directory reloads benefit from reusing their capacity;
/// a Flat surface can be millions of rows, so retaining that capacity after
/// leaving the surface would keep hundreds of megabytes alive indefinitely.
fn clear_row_buffer<T>(buffer: &mut Vec<T>, release_capacity: bool) {
    if release_capacity {
        *buffer = Vec::new();
    } else {
        buffer.clear();
    }
}

impl FlatPathStore {
    fn new(root: PathBuf, id_base: u64) -> Self {
        Self {
            root,
            id_base,
            directories: Vec::new(),
            directory_index: Some(HashMap::new()),
            display_texts: Some(HashSet::new()),
            row_directories: Vec::new(),
            row_names: Vec::new(),
            detail_states: Vec::new(),
        }
    }

    fn append(&mut self, entries: &[FileEntry], paths: &HashMap<NodeId, PathBuf>) {
        let index = self
            .directory_index
            .as_mut()
            .expect("flat path arena is appendable until scan completion");
        self.row_directories.reserve(entries.len());
        for entry in entries {
            let relative = paths
                .get(&entry.id)
                .and_then(|path| path.parent())
                .and_then(|parent| parent.strip_prefix(&self.root).ok())
                .unwrap_or_else(|| Path::new(""));
            let relative_text = relative.to_string_lossy();
            let dir = if let Some(existing) = index.get(relative_text.as_ref()) {
                *existing
            } else {
                let next = u32::try_from(self.directories.len())
                    .expect("flat directory arena exceeds u32::MAX directories");
                let relative: Arc<str> = relative_text.into_owned().into();
                self.directories.push(relative.clone());
                index.insert(relative, next);
                next
            };
            self.row_directories.push(dir);
            self.row_names.push(entry.name.clone());
            self.detail_states.push(0);
        }
    }

    fn intern_display_texts(&mut self, entries: &mut [FileEntry]) {
        const MAX_INTERNED_TEXTS: usize = 65_536;
        let texts = self
            .display_texts
            .as_mut()
            .expect("flat text arena is appendable until scan completion");
        for entry in entries {
            for text in [&mut entry.display_size, &mut entry.display_kind] {
                if let Some(canonical) = texts.get(text.as_ref()) {
                    *text = canonical.clone();
                } else if texts.len() < MAX_INTERNED_TEXTS {
                    texts.insert(text.clone());
                }
            }
        }
    }

    fn row_index(&self, id: NodeId) -> Option<usize> {
        let offset = id.as_raw().checked_sub(self.id_base)?.checked_sub(1)?;
        let row = usize::try_from(offset).ok()?;
        (row < self.row_directories.len()).then_some(row)
    }

    fn path_for(&self, id: NodeId) -> Option<PathBuf> {
        let row = self.row_index(id)?;
        let dir = *self.row_directories.get(row)? as usize;
        Some(
            self.root
                .join(self.directories.get(dir)?.as_ref())
                .join(self.row_names.get(row)?.as_ref()),
        )
    }

    fn display_directory(&self, id: NodeId) -> Option<SharedString> {
        let row = self.row_index(id)?;
        let dir = *self.row_directories.get(row)? as usize;
        let relative = self.directories.get(dir)?;
        Some(if relative.is_empty() {
            "·".into()
        } else {
            SharedString::from(relative.clone())
        })
    }

    /// Lexical directory rank for every arena slot. Path sorting compares
    /// small integers per row instead of rebuilding or case-folding a full
    /// path inside the comparator.
    fn directory_ranks(&self) -> Vec<u32> {
        let mut order: Vec<usize> = (0..self.directories.len()).collect();
        order.sort_unstable_by(|&a, &b| {
            self.directories[a]
                .to_lowercase()
                .cmp(&self.directories[b].to_lowercase())
        });
        let mut ranks = vec![0; order.len()];
        for (rank, directory) in order.into_iter().enumerate() {
            ranks[directory] = u32::try_from(rank).unwrap_or(u32::MAX);
        }
        ranks
    }

    fn directory_rank(&self, id: NodeId, ranks: &[u32]) -> u32 {
        self.row_index(id)
            .and_then(|row| self.row_directories.get(row))
            .and_then(|directory| ranks.get(*directory as usize))
            .copied()
            .unwrap_or(u32::MAX)
    }

    fn claim_detail(&mut self, id: NodeId) -> bool {
        let Some(row) = self.row_index(id) else {
            return false;
        };
        let Some(state) = self.detail_states.get_mut(row) else {
            return false;
        };
        if *state != 0 {
            return false;
        }
        *state = 1;
        true
    }

    fn finish_detail(&mut self, id: NodeId) {
        if let Some(row) = self.row_index(id)
            && let Some(state) = self.detail_states.get_mut(row)
        {
            *state = 2;
        }
    }

    fn finish(&mut self) {
        self.directory_index = None;
        self.display_texts = None;
        self.directories.shrink_to_fit();
        self.row_directories.shrink_to_fit();
        self.row_names.shrink_to_fit();
        self.detail_states.shrink_to_fit();
    }
}

#[cfg(test)]
mod flat_path_store_tests {
    use super::*;

    fn row(id: u64, name: &str) -> FileEntry {
        FileEntry {
            id: NodeId::from(id),
            name: name.into(),
            display_name: name.into(),
            name_has_hazards: false,
            kind: EntryKind::File,
            size: 42,
            mtime_unix: 1,
            display_size: "42 B".into(),
            display_kind: "TXT".into(),
            display_magic: ferail_core::empty_entry_text(),
            display_description: ferail_core::empty_entry_text(),
            details_loaded: false,
            is_quarantined: false,
            quarantine: None,
            hidden: false,
            created_unix: None,
            locked: false,
        }
    }

    #[test]
    fn flat_arena_shares_names_and_reconstructs_paths_after_finish() {
        let root = PathBuf::from("/flat-root");
        let base = 1_u64 << 60;
        let mut entries = vec![row(base + 1, "one.txt"), row(base + 2, "two.txt")];
        let mut paths = HashMap::new();
        paths.insert(entries[0].id, root.join("nested/one.txt"));
        paths.insert(entries[1].id, root.join("nested/two.txt"));

        let mut store = FlatPathStore::new(root.clone(), base);
        store.intern_display_texts(&mut entries);
        assert!(Arc::ptr_eq(
            &entries[0].display_size,
            &entries[1].display_size
        ));
        assert!(Arc::ptr_eq(
            &entries[0].display_kind,
            &entries[1].display_kind
        ));
        store.append(&entries, &paths);
        assert!(Arc::ptr_eq(&store.row_names[0], &entries[0].name));
        assert_eq!(store.directories.len(), 1);
        let indexed = store
            .directory_index
            .as_ref()
            .unwrap()
            .keys()
            .next()
            .unwrap();
        assert!(Arc::ptr_eq(indexed, &store.directories[0]));
        assert!(store.claim_detail(entries[0].id));
        assert!(!store.claim_detail(entries[0].id));

        store.finish_detail(entries[0].id);
        store.finish();
        assert!(store.directory_index.is_none());
        assert!(store.display_texts.is_none());
        assert_eq!(
            store.path_for(entries[1].id),
            Some(root.join("nested/two.txt"))
        );
        assert_eq!(
            store.display_directory(entries[0].id).as_deref(),
            Some("nested")
        );
    }

    #[test]
    fn flat_per_row_indexes_stay_small() {
        let bytes = std::mem::size_of::<u32>()
            + std::mem::size_of::<Arc<str>>()
            + std::mem::size_of::<u8>();
        assert_eq!(bytes, 21);
    }

    #[test]
    fn flat_exit_releases_row_buffer_capacity() {
        let mut rows = Vec::with_capacity(4_096);
        rows.extend([1_u8, 2, 3]);

        clear_row_buffer(&mut rows, true);

        assert!(rows.is_empty());
        assert_eq!(rows.capacity(), 0);
    }

    #[test]
    fn ordinary_reload_keeps_row_buffer_capacity() {
        let mut rows = Vec::with_capacity(4_096);
        rows.extend([1_u8, 2, 3]);
        let capacity = rows.capacity();

        clear_row_buffer(&mut rows, false);

        assert!(rows.is_empty());
        assert_eq!(rows.capacity(), capacity);
    }
}

fn flat_filter_expr(text: &str) -> ferail_core::filter_expr::FilterExpr {
    ferail_core::filter_expr::FilterExpr::parse(
        text.trim(),
        ferail_core::filter_expr::DateCtx {
            now_unix: ferail_core::now_unix(),
            tz_offset_secs: ferail_fs_native::stat_info::local_tz_offset_secs(),
        },
    )
}

fn flat_entry_matches(
    paths: &FlatPathStore,
    entry: &FileEntry,
    expr: &ferail_core::filter_expr::FilterExpr,
) -> bool {
    let directory = paths.display_directory(entry.id).unwrap_or_default();
    let (format, _) = entry.format_label();
    let haystack = format!("{}/{} {}", directory, entry.display_name, format).to_lowercase();
    expr.text_matches(&haystack) && expr.metadata_matches(entry)
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
        // the in-memory cache, no I/O.
        let (columns, hidden_columns) =
            split_persisted_columns(crate::app_state::load().list_columns.as_deref());
        Self {
            entries: Vec::new(),
            archive_rows: Vec::new(),
            archive_view: None,
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
            asset_scope: None,
            paths: HashMap::new(),
            flat_paths: None,
            flat_filtered_entries: Vec::new(),
            flat_filter_text: String::new(),
            detail_in_flight: false,
            detail_pending: None,
            detail_cancel: None,
            detail_revision: 0,
            detail_ready: false,
            detail_force: false,
            icons,
            thumbnails,
            tasks,
            cut_marker,
            heats: Vec::new(),
            tags: Vec::new(),
            is_favorited: Vec::new(),
            selected_set: HashSet::new(),
            selection_all: false,
            all_menu_caps: MenuCapCounts::default(),
            lead: None,
            inline_name_edit: None,
            open_with_warm: None,
            current_sort,
            sort_state,
            shell_focus,
            browsing_trash: false,
            drag_snapshot: RefCell::new(None),
            cached_total_size: std::cell::Cell::new(None),
            cached_selected_size: std::cell::Cell::new(None),
            slow_load: None,
            filtered_out: 0,
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
    /// the set changed: the caller then `refresh`es and persists.
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
        if self.flat_paths.is_some() {
            self.columns.push(flat_path_column());
        }
    }

    /// Drop the cached selection drag payload. Call on every
    /// selection write and structural entries change.
    pub fn invalidate_drag_snapshot(&mut self) {
        *self.drag_snapshot.get_mut() = None;
        self.cached_total_size.set(None);
        self.cached_selected_size.set(None);
    }

    /// Selection-only invalidation. The visible model did not change, so its
    /// (potentially multi-million-row) total remains valid.
    pub fn invalidate_selection_snapshot(&mut self) {
        *self.drag_snapshot.get_mut() = None;
        self.cached_selected_size.set(None);
    }

    pub fn is_selected(&self, id: NodeId) -> bool {
        if self.selection_all {
            !self.selected_set.contains(&id)
        } else {
            self.selected_set.contains(&id)
        }
    }

    pub fn selected_count(&self) -> usize {
        if self.selection_all {
            self.entries.len().saturating_sub(self.selected_set.len())
        } else {
            self.selected_set.len()
        }
    }

    pub fn selection_is_empty(&self) -> bool {
        self.selected_count() == 0
    }

    pub(crate) fn note_quarantine_change(&mut self, was_quarantined: bool, is_quarantined: bool) {
        match (was_quarantined, is_quarantined) {
            (false, true) => {
                self.all_menu_caps.quarantined = self.all_menu_caps.quarantined.saturating_add(1);
            }
            (true, false) => {
                self.all_menu_caps.quarantined = self.all_menu_caps.quarantined.saturating_sub(1);
            }
            _ => {}
        }
    }

    pub(crate) fn note_quarantine_cleared(&mut self, count: usize) {
        self.all_menu_caps.quarantined = self.all_menu_caps.quarantined.saturating_sub(count);
    }

    /// Build the shared drag payload for the current selection:
    /// visible-order paths plus the capped ghost images/names. Ghost
    /// images come only from already-warm caches (thumbnail when
    /// cached, else the workspace type icon), per the UI_NONBLOCKING
    /// contract.
    fn build_drag_snapshot(&self, cx: &gpui::App) -> DragSnapshot {
        // Archive rows use their own coordinate payload and, on macOS, native
        // file promises. They therefore never enter the ordinary on-disk path
        // snapshot used by filesystem rows here.
        if self.is_archive_mode() {
            return DragSnapshot::default();
        }
        let selected_count = self.selected_count();
        if selected_count > MAX_EAGER_DRAG_ITEMS {
            return DragSnapshot::default();
        }
        let want_thumb = show_thumbnails(cx);
        let mut paths: Vec<PathBuf> = Vec::with_capacity(selected_count);
        let mut dirs: Vec<bool> = Vec::with_capacity(selected_count);
        let mut icons: SmallVec<[Arc<RenderImage>; GHOST_STACK_CAP]> = smallvec![];
        for entry in &self.entries {
            if !self.is_selected(entry.id) {
                continue;
            }
            let Some(path) = self.path_for_entry(entry.id) else {
                continue;
            };
            self.push_ghost_icon(entry, &path, want_thumb, &mut icons);
            paths.push(path);
            dirs.push(matches!(entry.kind, EntryKind::Directory));
        }
        // Names shown on the ghost, lead-first and capped: the
        // single chip uses the first; the multi list shows up to
        // GHOST_NAME_CAP with a "+N more" overflow.
        let names: SmallVec<[SharedString; GHOST_STACK_CAP]> = paths
            .iter()
            .take(GHOST_STACK_CAP)
            .map(|p| ghost_name(p))
            .collect();
        DragSnapshot {
            paths: Rc::new(paths),
            dirs: Rc::new(dirs),
            names,
            icons,
            native_owned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Shared drag snapshot for both list and grid views. `RefCell` is used
    /// only as a lazy render cache: model/selection writes still invalidate it
    /// through `&mut self`, and building it performs cache reads but no I/O.
    pub(crate) fn drag_snapshot(&self, cx: &gpui::App) -> Option<DragSnapshot> {
        if self.drag_snapshot.borrow().is_none() {
            let snapshot = self.build_drag_snapshot(cx);
            *self.drag_snapshot.borrow_mut() = Some(snapshot);
        }
        self.drag_snapshot
            .borrow()
            .as_ref()
            .filter(|snapshot| !snapshot.paths.is_empty())
            .cloned()
    }

    /// Push one warm-cache ghost image (thumbnail if cached, else the
    /// type icon), capped at [`GHOST_STACK_CAP`].
    fn push_ghost_icon(
        &self,
        entry: &FileEntry,
        path: &std::path::Path,
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
        self.detail_revision = self.detail_revision.wrapping_add(1);
        let (col, asc) = resolve_ant_sort(col, asc, !self.heats.is_empty());
        // Row order changes: the drag snapshot's visible-order paths
        // are stale (totals unchanged by a re-order, but cheap to
        // recompute and one invalidation path is simpler).
        self.invalidate_drag_snapshot();

        // Flat rows have no per-row tags, heat, or favorite state. Avoid the
        // ordinary HashMap shuffle (hundreds of MB at million-row scale), and
        // use the compact directory arena directly for the surface-only Path
        // column.
        // Ant Trail is deliberately not routed through the flat fast path:
        // reaching here at all means the rows carry heat (see the fallback
        // above), and ranking by it needs the row-parallel lookup below.
        if col != SortColumn::AntTrail
            && let Some(flat) = &self.flat_paths
        {
            if col == SortColumn::Path {
                use std::cmp::Reverse;
                let ranks = flat.directory_ranks();
                if asc {
                    self.entries.sort_by_cached_key(|entry| {
                        (
                            flat.directory_rank(entry.id, &ranks),
                            entry.display_name.to_lowercase(),
                            entry.id.as_raw(),
                        )
                    });
                } else {
                    self.entries.sort_by_cached_key(|entry| {
                        (
                            Reverse(flat.directory_rank(entry.id, &ranks)),
                            entry.display_name.to_lowercase(),
                            entry.id.as_raw(),
                        )
                    });
                }
            } else {
                sort_in_place(&mut self.entries, col, asc);
            }
            return;
        }

        let mut row_state: HashMap<NodeId, (f32, Vec<ferail_core::commands::TagColor>, bool)> =
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

        if col == SortColumn::AntTrail {
            let heats = &row_state;
            sort_by_heat(
                &mut self.entries,
                |id| heats.get(&id).map_or(0.0, |state| state.0),
                asc,
            );
        } else {
            sort_in_place(&mut self.entries, col, asc);
        }

        // Rebuild only the decoration vectors that were populated: an
        // empty one means the surface never ran that worker (Flat), and
        // filling it with defaults here would hand every row back the
        // inert per-row state that surface exists to avoid.
        let had_heats = !self.heats.is_empty();
        let had_tags = !self.tags.is_empty();
        let had_favorites = !self.is_favorited.is_empty();
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
            if had_heats {
                self.heats.push(heat);
            }
            if had_tags {
                self.tags.push(tags);
            }
            if had_favorites {
                self.is_favorited.push(favorited);
            }
        }
    }

    // (The old synchronous `load()`: enumerate + up-to-200 inline xattr
    // tag reads on the UI thread: was dead code; the streaming pipeline in
    // `Shell::load_path_for_tab` is the only listing path. Deleted so nobody
    // resurrects a Prime Directive violation.)

    pub fn clear(&mut self) {
        if let Some(cancel) = self.detail_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.detail_revision = self.detail_revision.wrapping_add(1);
        self.invalidate_drag_snapshot();
        self.slow_load = None;
        self.filtered_out = 0;
        // `Vec::clear` keeps the allocation. That is useful for an ordinary
        // directory reload, but after a multi-million-row Flat surface it can
        // leave ~600 MB of empty FileEntry capacity resident. The presence of
        // the surface-local path arena tells us these buffers must be dropped,
        // not merely emptied. This also covers filtered-out Flat rows.
        let leaving_flat = self.flat_paths.take().is_some();
        clear_row_buffer(&mut self.entries, leaving_flat);
        self.all_menu_caps = MenuCapCounts::default();
        self.paths.clear();
        clear_row_buffer(&mut self.flat_filtered_entries, leaving_flat);
        self.flat_filter_text.clear();
        self.detail_in_flight = false;
        self.detail_pending = None;
        self.detail_ready = false;
        self.detail_force = false;
        self.columns.retain(|column| column.key.as_ref() != "path");
        self.hidden_columns
            .retain(|column| column.key.as_ref() != "path");
        self.heats.clear();
        self.tags.clear();
        self.is_favorited.clear();
        // selected_set / lead are NodeId-keyed and reconciled by
        // Shell against the new model; not cleared here.
    }

    pub fn replace_entries(
        &mut self,
        entries: Vec<FileEntry>,
        paths: HashMap<NodeId, PathBuf>,
        heats: Vec<f32>,
    ) {
        if let Some(cancel) = self.detail_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.detail_revision = self.detail_revision.wrapping_add(1);
        self.invalidate_drag_snapshot();
        self.slow_load = None;
        self.filtered_out = 0;
        // A normal directory load leaves archive mode behind.
        self.archive_rows.clear();
        self.archive_view = None;
        // ...and Flat, whose Path column and path arena belong to that
        // surface alone. Without this, a prefetched load arriving after
        // Include Subfolders closed kept an empty Path column and a
        // delegate that still believed it was a flat surface, which
        // then took the flat sort path for ordinary directory rows.
        let leaving_flat = self.flat_paths.take().is_some();
        clear_row_buffer(&mut self.flat_filtered_entries, leaving_flat);
        self.flat_filter_text.clear();
        self.detail_in_flight = false;
        self.detail_pending = None;
        self.detail_ready = false;
        self.detail_force = false;
        self.columns.retain(|column| column.key.as_ref() != "path");
        self.hidden_columns
            .retain(|column| column.key.as_ref() != "path");
        self.all_menu_caps = MenuCapCounts::from_entries(&entries);
        self.entries = entries;
        self.paths = paths;
        self.heats = heats;
        self.tags = vec![Vec::new(); self.entries.len()];
        self.is_favorited = vec![false; self.entries.len()];
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
        let favorites = vec![false; entries.len()];
        self.append_entries_decorated(entries, paths, heats, favorites);
    }

    /// Append one streamed batch with every cheap row decoration already
    /// computed for that batch. This keeps streaming application O(batch):
    /// callers must not follow it with a whole-model favorites pass.
    pub fn append_entries_decorated(
        &mut self,
        entries: Vec<FileEntry>,
        paths: HashMap<NodeId, PathBuf>,
        heats: Vec<f32>,
        favorites: Vec<bool>,
    ) {
        debug_assert_eq!(entries.len(), heats.len());
        debug_assert_eq!(entries.len(), favorites.len());
        self.invalidate_drag_snapshot();
        self.paths.extend(paths);
        let n = entries.len();
        self.all_menu_caps.extend(&entries);
        self.entries.extend(entries);
        self.heats.extend(heats);
        self.tags.extend((0..n).map(|_| Vec::new()));
        self.is_favorited.extend(favorites);
        // selected_set / lead untouched: NodeId-keyed, not row-keyed.
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
        if let Some(flat) = &self.flat_paths {
            return flat.path_for(id);
        }
        self.paths.get(&id).cloned()
    }

    /// Start an empty Flat surface. The Path column is surface-specific: it is
    /// inserted while Flat is active and removed by [`Self::clear`], so normal
    /// directory layouts and their persisted column spec remain unchanged.
    pub fn begin_flat(&mut self, root: PathBuf, id_base: u64) {
        self.clear();
        self.flat_paths = Some(FlatPathStore::new(root, id_base));
        self.detail_ready = true;
        if !self.is_column_visible("path") {
            self.columns.push(flat_path_column());
        }
    }

    pub fn append_flat_entries(
        &mut self,
        mut entries: Vec<FileEntry>,
        paths: HashMap<NodeId, PathBuf>,
    ) {
        self.invalidate_drag_snapshot();
        let flat = self
            .flat_paths
            .as_mut()
            .expect("begin_flat must precede flat batches");
        flat.intern_display_texts(&mut entries);
        flat.append(&entries, &paths);
        if self.flat_filter_text.trim().is_empty() {
            self.all_menu_caps.extend(&entries);
            self.entries.extend(entries);
        } else {
            let expr = flat_filter_expr(&self.flat_filter_text);
            let flat = self.flat_paths.as_ref().expect("flat path arena exists");
            let (visible, filtered): (Vec<_>, Vec<_>) = entries
                .into_iter()
                .partition(|entry| flat_entry_matches(flat, entry, &expr));
            self.all_menu_caps.extend(&visible);
            self.entries.extend(visible);
            self.flat_filtered_entries.extend(filtered);
            self.filtered_out = self.flat_filtered_entries.len();
        }
        // Flat is files-only and intentionally does not run Ant Trail,
        // Finder-tag, or favorite-folder workers. Keep their optional
        // parallel vectors empty: every read already falls back to the same
        // zero/empty/false values, avoiding 29 bytes of inert state per row.
    }

    /// Replace the display root with the canonical root resolved by the
    /// worker. This lands before the first batch, preserving correct relative
    /// paths when Flat View was opened through a symlink.
    pub fn set_flat_root(&mut self, root: PathBuf) {
        if let Some(paths) = &mut self.flat_paths
            && paths.row_directories.is_empty()
        {
            paths.root = root;
        }
    }

    pub fn finish_flat(&mut self) {
        if let Some(paths) = &mut self.flat_paths {
            paths.finish();
        }
    }

    /// Enable viewport-owned detail enrichment after an ordinary listing has
    /// reached Done. A forced refresh re-sniffs each row once as it first
    /// enters a viewport instead of sweeping the entire directory upfront.
    pub fn enable_visible_details(&mut self, force: bool) {
        self.detail_ready = true;
        self.detail_force = force;
    }

    /// Stop the current viewport worker immediately. Used by the live
    /// performance setting; navigation/replace paths perform the same reset
    /// while rotating the row model.
    pub fn cancel_visible_details(&mut self) {
        if let Some(cancel) = self.detail_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.detail_revision = self.detail_revision.wrapping_add(1);
        self.detail_in_flight = false;
        self.detail_pending = None;
        self.detail_ready = false;
        self.detail_force = false;
    }

    /// Hydrate only the visible part of an ordinary or Flat surface. This
    /// keeps Format, Description, and quarantine badges fully functional while
    /// bounding file opens, memory, and UI apply work to the viewport.
    pub fn warm_visible_details(
        &mut self,
        visible_range: Range<usize>,
        cx: &mut Context<TableState<Self>>,
    ) {
        if !self.detail_ready || !crate::prefetch::file_detail_scan_enabled(cx) {
            return;
        }
        if self.detail_in_flight {
            self.detail_pending = Some(visible_range);
            return;
        }
        const OVERSCAN: usize = 16;
        let start = visible_range.start.saturating_sub(OVERSCAN);
        let end = visible_range
            .end
            .saturating_add(OVERSCAN)
            .min(self.entries.len());
        let is_flat = self.flat_paths.is_some();
        let mut seeds = Vec::with_capacity(end.saturating_sub(start));
        let mut attempted = Vec::with_capacity(end.saturating_sub(start));
        for row_ix in start..end {
            let entry = &self.entries[row_ix];
            if entry.details_loaded {
                continue;
            }
            if let Some(flat) = &mut self.flat_paths
                && !flat.claim_detail(entry.id)
            {
                continue;
            }
            let Some(path) = self.path_for_entry(entry.id) else {
                if let Some(flat) = &mut self.flat_paths {
                    flat.finish_detail(entry.id);
                }
                continue;
            };
            attempted.push(entry.id);
            seeds.push(crate::prefetch::PrefetchSeed {
                row_ix,
                node: entry.id,
                path,
                mtime_unix: entry.mtime_unix,
                size: entry.size,
                is_dir: matches!(entry.kind, EntryKind::Directory),
                details_loaded: entry.details_loaded,
            });
        }
        if seeds.is_empty() {
            return;
        }
        self.detail_in_flight = true;
        let revision = self.detail_revision;
        let force = self.detail_force;
        let db = if is_flat {
            None
        } else {
            crate::process_state::process_state(cx).db_snapshot()
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.detail_cancel = Some(cancel.clone());
        let tasks = self.tasks.clone();
        let task_id = tasks.borrow_mut().begin(
            TaskKind::MagicPrefetch,
            trn!("Indexing {n} entry…", "Indexing {n} entries…", seeds.len()),
            false,
        );
        cx.spawn(async move |table, cx| {
            let worker_cancel = cancel.clone();
            let batch = cx
                .background_executor()
                .spawn(async move {
                    if is_flat {
                        crate::prefetch::run_viewport(seeds)
                    } else {
                        crate::prefetch::run_cached_viewport(seeds, db, force, worker_cancel)
                    }
                })
                .await;
            let _ = table.update(cx, |state, cx| {
                let delegate = state.delegate_mut();
                let owns_worker = delegate
                    .detail_cancel
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &cancel));
                if !owns_worker {
                    return;
                }
                delegate.detail_cancel = None;
                delegate.detail_in_flight = false;
                if let Some(flat) = &mut delegate.flat_paths {
                    for node in attempted {
                        flat.finish_detail(node);
                    }
                }
                let revision_matches = delegate.detail_revision == revision;
                if revision_matches {
                    crate::prefetch::apply_viewport_batch(delegate, batch);
                }
                let pending = delegate.detail_pending.take();
                state.refresh(cx);
                if let Some(range) =
                    pending.or_else(|| (!revision_matches).then_some(visible_range))
                {
                    state.delegate_mut().warm_visible_details(range, cx);
                }
            });
            tasks.borrow_mut().end(task_id);
        })
        .detach();
    }

    /// Refilter the already-materialized Flat snapshot without filesystem I/O.
    /// Rows move between two vectors and therefore remain single-owned.
    pub fn apply_flat_filter(&mut self, text: &str) {
        if self.flat_paths.is_none() || self.flat_filter_text == text {
            return;
        }
        self.invalidate_drag_snapshot();
        self.detail_revision = self.detail_revision.wrapping_add(1);
        self.flat_filter_text.clear();
        self.flat_filter_text.push_str(text);
        let mut all = std::mem::take(&mut self.entries);
        all.append(&mut self.flat_filtered_entries);
        if text.trim().is_empty() {
            self.entries = all;
        } else {
            let expr = flat_filter_expr(text);
            let flat = self.flat_paths.as_ref().expect("flat path arena exists");
            (self.entries, self.flat_filtered_entries) = all
                .into_iter()
                .partition(|entry| flat_entry_matches(flat, entry, &expr));
        }
        self.filtered_out = self.flat_filtered_entries.len();
        self.all_menu_caps = MenuCapCounts::from_entries(&self.entries);
    }

    /// Show the contents of an archive instead of a directory listing.
    ///
    /// `entries` are synthesized rows (no on-disk path: the `paths` map is
    /// deliberately left empty, so `path_for_entry` returns `None` and every
    /// path-dependent affordance degrades to its "unknown" branch), and
    /// `rows` carries the tree metadata the Name cell draws.
    ///
    /// Sorting is *not* applied here: the tree's own folders-first order is
    /// the meaningful one until the user clicks a header, at which point
    /// `apply_effective_sort` takes over like any other listing.
    pub fn replace_archive_entries(
        &mut self,
        entries: Vec<FileEntry>,
        rows: Vec<ferail_archive::TreeRow>,
        view: WeakEntity<crate::archive::ArchiveView>,
    ) {
        if let Some(cancel) = self.detail_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.detail_revision = self.detail_revision.wrapping_add(1);
        self.detail_in_flight = false;
        self.detail_pending = None;
        self.detail_ready = false;
        self.detail_force = false;
        debug_assert_eq!(entries.len(), rows.len());
        self.invalidate_drag_snapshot();
        self.slow_load = None;
        self.filtered_out = 0;
        let n = entries.len();
        self.all_menu_caps = MenuCapCounts::from_entries(&entries);
        self.entries = entries;
        self.archive_rows = rows;
        self.archive_view = Some(view);
        self.paths = HashMap::new();
        self.heats = vec![0.0; n];
        self.tags = vec![Vec::new(); n];
        self.is_favorited = vec![false; n];
    }

    /// Whether the delegate is currently showing archive contents.
    pub fn is_archive_mode(&self) -> bool {
        !self.archive_rows.is_empty()
    }

    /// Build the drag payload for an archive row: the whole selection when the
    /// pressed row is part of it, otherwise just that row (mirroring the
    /// file-list rule).
    fn archive_drag_for_row(
        &self,
        row_ix: usize,
        row_is_selected: bool,
        cx: &gpui::App,
    ) -> Option<ArchiveEntryDrag> {
        let view = self.archive_view.as_ref()?;
        let (archive, password) = view.upgrade().map(|v| {
            let v = v.read(cx);
            (v.archive_path().to_path_buf(), v.password_for_drag())
        })?;
        let selected: Vec<(String, bool)> = if row_is_selected {
            self.archive_rows
                .iter()
                .enumerate()
                .filter(|(i, _)| self.entries.get(*i).is_some_and(|e| self.is_selected(e.id)))
                .map(|(_, r)| (r.path.clone(), r.is_dir))
                .collect()
        } else {
            let row = self.archive_rows.get(row_ix)?;
            vec![(row.path.clone(), row.is_dir)]
        };
        (!selected.is_empty()).then(|| ArchiveEntryDrag {
            archive,
            entries: selected.iter().map(|(path, _)| path.clone()).collect(),
            directories: selected.iter().map(|(_, is_dir)| *is_dir).collect(),
            password,
        })
    }

    /// Apply a plain / Cmd / Shift click to this delegate's own selection.
    ///
    /// The Shell drives selection for tab listings through its richer path
    /// (`apply_row_click_gesture`), which also moves the preview pane and warms
    /// the Open With cache. A **windowed** archive workbench has no Shell to
    /// route through, so it needs the core gesture on its own: this is that
    /// core, and nothing else calls it.
    pub fn apply_click_gesture(&mut self, row_ix: usize, modifiers: gpui::Modifiers) {
        let Some(id) = self.entries.get(row_ix).map(|e| e.id) else {
            return;
        };
        self.selection_all = false;
        let cmd = modifiers.secondary();
        if modifiers.shift {
            // Range from the current lead to the clicked row (inclusive).
            let lead_ix = self
                .lead
                .and_then(|lead| self.entries.iter().position(|e| e.id == lead));
            let (lo, hi) = match lead_ix {
                Some(l) if l <= row_ix => (l, row_ix),
                Some(l) => (row_ix, l),
                None => (row_ix, row_ix),
            };
            if !cmd {
                self.selected_set.clear();
            }
            for e in &self.entries[lo..=hi] {
                self.selected_set.insert(e.id);
            }
        } else if cmd {
            if !self.selected_set.remove(&id) {
                self.selected_set.insert(id);
            }
        } else {
            self.selected_set.clear();
            self.selected_set.insert(id);
        }
        self.lead = Some(id);
    }

    /// The full tree row behind a row index, when in archive mode.
    pub fn archive_row(&self, row_ix: usize) -> Option<&ferail_archive::TreeRow> {
        self.archive_rows.get(row_ix)
    }

    /// The archive path behind a row, when in archive mode.
    pub fn archive_path_for_row(&self, row_ix: usize) -> Option<&str> {
        self.archive_rows.get(row_ix).map(|r| r.path.as_str())
    }

    /// Indent + disclosure caret for an archive row. `None` for ordinary
    /// directory listings, which keeps the Name cell unchanged for them.
    ///
    /// The caret is a fixed 12-DIP box (an affordance pinned to a fixed-size
    /// slot, one of the documented `px` exceptions) so names stay aligned
    /// whether or not a row can expand.
    fn archive_tree_affordance(
        &self,
        row_ix: usize,
        cx: &mut Context<TableState<Self>>,
    ) -> Option<gpui::AnyElement> {
        let row = self.archive_rows.get(row_ix)?;
        const INDENT_PER_LEVEL: f32 = 14.0;
        const CARET_BOX: f32 = 12.0;
        let indent = row.depth as f32 * INDENT_PER_LEVEL;
        let muted = cx.theme().muted_foreground;

        let caret: gpui::AnyElement = if row.expandable {
            let path = row.path.clone();
            let view = self.archive_view.clone();
            let glyph = if row.expanded {
                "icons/chevron-down.svg"
            } else {
                "icons/chevron-right.svg"
            };
            div()
                .id(("archive-caret", row_ix))
                .w(px(CARET_BOX))
                .h(px(CARET_BOX))
                .flex_shrink_0()
                .cursor_pointer()
                .child(svg().path(glyph).icon_px(CARET_BOX).text_color(muted))
                .on_click(move |_, _window, app| {
                    if let Some(view) = view.as_ref() {
                        let path = path.clone();
                        let _ = view.update(app, |v, cx| v.toggle_expanded(&path, cx));
                    }
                })
                .into_any_element()
        } else {
            // Files and childless folders: keep the slot so names line up.
            div()
                .w(px(CARET_BOX))
                .h(px(CARET_BOX))
                .flex_shrink_0()
                .into_any_element()
        };

        Some(
            gpui_component::h_flex()
                .flex_shrink_0()
                .child(div().w(px(indent)).flex_shrink_0())
                .child(caret)
                .into_any_element(),
        )
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

    /// Warm native folder icons for the visible list rows. Folder artwork is
    /// path-specific (custom Finder icons and sync-provider badges), so unlike
    /// file-type icons it must never be swept across a million-row model.
    /// A small overscan keeps scrolling smooth while bounding both native
    /// lookups and path-keyed cache growth to folders the user actually sees.
    pub fn warm_folder_icons(
        &mut self,
        visible_range: Range<usize>,
        cx: &mut Context<TableState<Self>>,
    ) {
        if self.is_archive_mode() {
            return;
        }
        const OVERSCAN: usize = 8;
        let start = visible_range.start.saturating_sub(OVERSCAN);
        let end = visible_range
            .end
            .saturating_add(OVERSCAN)
            .min(self.entries.len());
        let mut candidates = Vec::with_capacity(end.saturating_sub(start));
        for row in start..end {
            let Some(entry) = self.entries.get(row) else {
                continue;
            };
            if !matches!(entry.kind, EntryKind::Directory) {
                continue;
            }
            if let Some(path) = self.path_for_entry(entry.id) {
                candidates.push(path);
            }
        }
        // Submit cached candidates too: the dispatcher drops resolved ones,
        // but if the sidebar/grid already started the same fetch it attaches
        // this table as an additional waiter so the list is repainted when
        // that shared result lands.
        let wanted: Vec<(PathBuf, Option<u32>)> =
            candidates.into_iter().map(|path| (path, None)).collect();
        if wanted.is_empty() {
            return;
        }
        let process = crate::process_state::process_state(cx);
        process.asset_dispatcher.borrow_mut().submit_path_icons(
            &mut process.asset_work.borrow_mut(),
            &mut process.thumbnails.borrow_mut(),
            &mut process.icons.borrow_mut(),
            crate::asset_dispatcher::IconTarget::Table(cx.entity().downgrade()),
            wanted,
        );
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
        self.warm_thumbnails_sized_for_target(
            visible_range,
            size_px,
            crate::asset_dispatcher::ThumbnailTarget::Table(cx.entity().downgrade()),
            cx,
        );
    }

    pub(crate) fn warm_thumbnails_sized_for_target(
        &mut self,
        visible_range: Range<usize>,
        size_px: u32,
        target: crate::asset_dispatcher::ThumbnailTarget,
        cx: &mut Context<TableState<Self>>,
    ) {
        if !show_thumbnails(cx) {
            return;
        }
        // Synchronize this surface's local generation before admitting any
        // viewport work. Today the legacy thumbnail loop still performs the
        // fetch; the process dispatcher will submit the per-row requests to
        // these same scoped lanes in the next slice.
        let process = crate::process_state::process_state(cx);
        let scope = *self
            .asset_scope
            .get_or_insert_with(|| process.mint_asset_scope());
        // A little overscan so a nudge of the wheel doesn't expose a
        // blank slot before its fetch is even scheduled.
        const OVERSCAN: usize = 8;
        let start = visible_range.start.saturating_sub(OVERSCAN);
        let end = (visible_range.end + OVERSCAN).min(self.entries.len());
        let surface_local_identity = self.flat_paths.is_some();
        let mut seeds = Vec::with_capacity(end.saturating_sub(start));
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
                if cache.is_resolved(&path, size_px) {
                    continue;
                }
                let priority = if self.selected_set.contains(&entry.id) {
                    ferail_core::asset_work::AssetPriority::Selected
                } else if visible_range.contains(&row) {
                    ferail_core::asset_work::AssetPriority::Visible
                } else {
                    ferail_core::asset_work::AssetPriority::Overscan
                };
                seeds.push(crate::asset_dispatcher::ThumbnailSeed {
                    row_ix: row,
                    node: entry.id,
                    path,
                    revision: ferail_core::revision_cache::FileRevision {
                        byte_len: entry.size,
                        modified_ns: Some(i128::from(entry.mtime_unix) * 1_000_000_000),
                    },
                    size_px,
                    priority,
                    surface_local_identity,
                });
            }
        }
        if seeds.is_empty() {
            return;
        }
        crate::obs::breadcrumb(format_args!(
            "thumbnail viewport submit size={size_px} requests={}",
            seeds.len()
        ));
        process.asset_dispatcher.borrow_mut().submit(
            &mut process.asset_work.borrow_mut(),
            &mut process.thumbnails.borrow_mut(),
            &mut process.icons.borrow_mut(),
            crate::asset_dispatcher::ThumbnailSubscription {
                table: cx.entity().downgrade(),
                target,
                scope,
                generation: self.detail_revision,
            },
            seeds,
        );
    }

    pub(crate) fn accepts_thumbnail_result(
        &self,
        scope: ferail_core::asset_work::AssetWorkScope,
        generation: u64,
        row_ix: usize,
        node: NodeId,
    ) -> bool {
        self.asset_scope == Some(scope)
            && self.detail_revision == generation
            && self
                .entries
                .get(row_ix)
                .is_some_and(|entry| entry.id == node)
    }
}

impl FileListDelegate {
    /// The row menu inside a trash folder.
    ///
    /// A deleted item answers to a different set of verbs. Most of the file
    /// menu is meaningless on it: renaming, duplicating, compressing, tagging
    /// or favouriting something the user threw away, and "Move to Trash" on an
    /// item already there. What is left is looking at it, finding out what it
    /// is, putting it back, and getting rid of it for good.
    ///
    /// Availability still applies on top: `Open in New Tab` only for a folder,
    /// and Put Back only where an original location is known.
    fn trash_context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        use crate::menu_plan::{MenuPlan, MenuSurface, ids};
        use crate::shell::{
            CopyPath, DeleteImmediately, EmptyTrash, GetInfo, OpenInNewTab, OpenSelected,
            QuickLook, RestoreFromTrash, RevealInFinder,
        };

        let targets = resolve_menu_targets_with_mode(
            &self.entries,
            &self.selected_set,
            self.selection_all,
            row_ix,
            self.all_menu_caps,
        );
        let anchor_is_dir = self
            .entries
            .get(row_ix)
            .is_some_and(|entry| matches!(entry.kind, EntryKind::Directory));
        let _ = (&targets, cx);

        let mut plan = MenuPlan::new(MenuSurface::TrashRow).action(
            ids::OPEN,
            tr!("Open"),
            Box::new(OpenSelected),
        );
        if anchor_is_dir {
            plan = plan.action(
                ids::OPEN_IN_NEW_TAB,
                tr!("Open in New Tab"),
                Box::new(OpenInNewTab),
            );
        }
        plan.separator()
            // Finder calls this "Put Back", and so does everyone who has ever
            // used it. Restore is what Windows calls the same thing.
            .action(
                ids::RESTORE_FROM_TRASH,
                tr!("Put Back"),
                Box::new(RestoreFromTrash),
            )
            .separator()
            .action(ids::GET_INFO, tr!("Get Info"), Box::new(GetInfo))
            .action(ids::QUICK_LOOK, tr!("Quick Look"), Box::new(QuickLook))
            .action(
                ids::REVEAL,
                crate::i18n::tr_static(ferail_core::commands::REVEAL_LABEL),
                Box::new(RevealInFinder),
            )
            .action(ids::COPY_PATH, tr!("Copy Path"), Box::new(CopyPath))
            .separator()
            .action(
                ids::DELETE_IMMEDIATELY,
                tr!("Delete Immediately\u{2026}"),
                Box::new(DeleteImmediately),
            )
            .action(
                ids::EMPTY_TRASH,
                tr!("Empty Trash\u{2026}"),
                Box::new(EmptyTrash),
            )
            .render(menu.action_context(self.shell_focus.clone()))
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

    /// Header text, translated at render time (see [`column_title`]).
    fn column_name(&self, col_ix: usize, _cx: &App) -> SharedString {
        crate::i18n::tr_static(column_title(&self.columns[col_ix].key))
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let _path_guard = ferail_core::path_guard::enter_render();
        // Ant Trail heat tint (Stage 9.b). Renders only on directory
        // rows: files aren't tracked in the trail. 0.0 → no tint;
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
        let in_set = entry_id.map(|id| self.is_selected(id)).unwrap_or(false);
        let is_lead = entry_id == self.lead && entry_id.is_some();
        let mut row = div().id(("file-row", row_ix));
        // Cut (Cmd+X) rows render dimmed until the move pastes (or the
        // mark is cleared by a fresh Copy/Cut), mirroring Explorer.
        let has_cut_marks = !self.cut_marker.borrow().is_empty();
        let is_cut = has_cut_marks
            && entry_id
                .and_then(|id| self.path_for_entry(id))
                .is_some_and(|path| self.cut_marker.borrow().iter().any(|cut| cut == &path));
        // Hidden entries (visible because show-hidden is on) dim more
        // gently, text and icon in one stroke, so they read as
        // distinct from normal files, Finder-style. `else if`: a cut
        // hidden row keeps the stronger cut treatment (opacity doesn't
        // compound; the last call would win).
        let is_hidden = self.entries.get(row_ix).map(|e| e.hidden).unwrap_or(false);
        if is_cut {
            row = row.opacity(0.45);
        } else if is_hidden {
            row = row.opacity(0.6);
        }
        // Folder rows are drop targets for OS file drags (dnd-spec
        // §3.5): accent ring on hover, drop surfaces as a TableEvent
        // for the shell to run the transfer into this folder. Stop
        // propagation so the pane-background target underneath
        // doesn't also fire.
        let archive_drop_allowed = if self.is_archive_mode() {
            self.archive_view
                .as_ref()
                .and_then(WeakEntity::upgrade)
                .is_some_and(|view| view.read(cx).can_stage_edits())
        } else {
            true
        };
        let archive_entry_drop_allowed = !self.is_archive_mode();
        // Archive **file** rows accept dropped files/folders: a ZIP adds them,
        // a format that can't be edited in place refuses visibly. Only in the
        // real filesystem list: inside an open archive, rows are members, and
        // nesting an add into a member is not a gesture we support.
        let archive_add_target = if self.is_archive_mode() {
            ArchiveDropTarget::No
        } else {
            self.entries
                .get(row_ix)
                .map(|entry| archive_drop_target(entry.name.as_ref(), entry.kind))
                .unwrap_or(ArchiveDropTarget::No)
        };
        if archive_add_target != ArchiveDropTarget::No {
            let accepts = archive_add_target == ArchiveDropTarget::Accepts;
            row = row
                .drag_over::<ExternalPaths>(move |style, _, _, cx| {
                    if accepts {
                        style
                            .cursor_copy()
                            .border_1()
                            .border_color(cx.theme().accent)
                            .bg(cx.theme().accent.opacity(0.10))
                    } else {
                        style
                            .cursor_not_allowed()
                            .border_1()
                            .border_color(cx.theme().danger)
                            .bg(cx.theme().danger.opacity(0.08))
                    }
                })
                .on_drop(
                    cx.listener(move |_state, paths: &ExternalPaths, _window, cx| {
                        // Consume either way: a refused archive must not fall
                        // through to the pane background and land the files in
                        // the current folder instead.
                        cx.stop_propagation();
                        if accepts {
                            cx.emit(TableEvent::ArchiveAddDrop {
                                row_ix,
                                paths: paths.paths().to_vec(),
                            });
                        }
                    }),
                )
                // Members dragged out of an archive workbench land here too:
                // an editable ZIP takes them, anything else refuses. Without
                // this the release would bubble to the pane target and
                // extract into the current folder instead.
                .drag_over::<ArchiveEntryDrag>(move |style, _, _, cx| {
                    if accepts {
                        style
                            .cursor_copy()
                            .border_1()
                            .border_color(cx.theme().accent)
                            .bg(cx.theme().accent.opacity(0.10))
                    } else {
                        style
                            .cursor_not_allowed()
                            .border_1()
                            .border_color(cx.theme().danger)
                            .bg(cx.theme().danger.opacity(0.08))
                    }
                })
                .on_drop(
                    cx.listener(move |_state, drag: &ArchiveEntryDrag, _window, cx| {
                        cx.stop_propagation();
                        if accepts {
                            cx.emit(TableEvent::ArchiveAddFromArchive {
                                row_ix,
                                archive: drag.archive.clone(),
                                entries: drag.entries.clone(),
                                password: drag.password.clone(),
                            });
                        }
                    }),
                )
                // Cross-window promise sessions carry no GPUI payload, so the
                // release arrives as a plain mouse-up (GPUI-UPSTREAM #11).
                .on_mouse_move(cx.listener(move |_state, _event, _window, cx| {
                    if native_archive_drag_active() {
                        cx.notify();
                    }
                }))
                .on_mouse_up(
                    gpui::MouseButton::Left,
                    cx.listener(move |_state, _event, _window, cx| {
                        if cx.has_active_drag() {
                            return;
                        }
                        let Some(drag) = take_native_archive_drag() else {
                            return;
                        };
                        cx.stop_propagation();
                        if accepts {
                            cx.emit(TableEvent::ArchiveAddFromArchive {
                                row_ix,
                                archive: drag.archive,
                                entries: drag.entries,
                                password: drag.password,
                            });
                        }
                    }),
                );
        }
        if kind_is_dir {
            row = row
                .drag_over::<ExternalPaths>(move |style, _, _, cx| {
                    if archive_drop_allowed {
                        style
                            .cursor_copy()
                            .border_1()
                            .border_color(cx.theme().accent)
                            .bg(cx.theme().accent.opacity(0.10))
                    } else {
                        style
                            .cursor_not_allowed()
                            .border_1()
                            .border_color(cx.theme().danger)
                            .bg(cx.theme().danger.opacity(0.08))
                    }
                })
                // Spring-load: while a drag hovers this folder row, tell
                // the shell (which times the dwell and drills in).
                .on_drag_move(cx.listener(
                    move |_state, e: &gpui::DragMoveEvent<ExternalPaths>, _window, cx| {
                        if e.bounds.contains(&e.event.position) {
                            let cursor = if archive_drop_allowed {
                                gpui::CursorStyle::DragCopy
                            } else {
                                gpui::CursorStyle::OperationNotAllowed
                            };
                            if cx.active_drag_cursor_style() != Some(cursor) {
                                cx.set_active_drag_cursor_style(cursor, _window);
                            }
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
                )
                // Twin target for entries dragged out of an archive window.
                .drag_over::<ArchiveEntryDrag>(move |style, _, _, cx| {
                    if archive_entry_drop_allowed {
                        style
                            .cursor_copy()
                            .border_1()
                            .border_color(cx.theme().accent)
                            .bg(cx.theme().accent.opacity(0.10))
                    } else {
                        style
                            .cursor_not_allowed()
                            .border_1()
                            .border_color(cx.theme().danger)
                            .bg(cx.theme().danger.opacity(0.08))
                    }
                })
                .on_drag_move(cx.listener(
                    move |_state, event: &gpui::DragMoveEvent<ArchiveEntryDrag>, window, cx| {
                        if event.bounds.contains(&event.event.position) {
                            let cursor = if archive_entry_drop_allowed {
                                gpui::CursorStyle::DragCopy
                            } else {
                                gpui::CursorStyle::OperationNotAllowed
                            };
                            if cx.active_drag_cursor_style() != Some(cursor) {
                                cx.set_active_drag_cursor_style(cursor, window);
                            }
                        }
                    },
                ))
                .on_drop(
                    cx.listener(move |_state, drag: &ArchiveEntryDrag, _window, cx| {
                        cx.stop_propagation();
                        if archive_entry_drop_allowed {
                            cx.emit(TableEvent::ArchiveDrop {
                                row_ix,
                                archive: drag.archive.clone(),
                                entries: drag.entries.clone(),
                                password: drag.password.clone(),
                            });
                        }
                    }),
                )
                // Once AppKit has moved a promised-file gesture into a
                // second Ferail window, GPUI may no longer own the custom
                // typed payload. Its platform layer still delivers ordinary
                // MouseMove/MouseUp events, so use the coordinator fallback
                // for hover and drop. If GPUI retained the typed drag, the
                // normal on_drop above wins and this path stays dormant.
                .on_mouse_move(cx.listener(move |_state, _event, _window, cx| {
                    if native_archive_drag_active() {
                        cx.notify();
                    }
                }))
                .on_mouse_up(
                    gpui::MouseButton::Left,
                    cx.listener(move |_state, _event, _window, cx| {
                        if cx.has_active_drag() {
                            return;
                        }
                        let Some(drag) = take_native_archive_drag() else {
                            return;
                        };
                        cx.stop_propagation();
                        if archive_entry_drop_allowed {
                            cx.emit(TableEvent::ArchiveDrop {
                                row_ix,
                                archive: drag.archive,
                                entries: drag.entries,
                                password: drag.password,
                            });
                        }
                    }),
                );
            // The matching hover ring for the native promise session lives in
            // `render_tr_hover` below: the table owns the row's single
            // `.hover()` slot, so it cannot be set here.
        }
        if kind_is_dir && heat > 0.0 && crate::ant_trail::enabled(cx) {
            // Customizable base tint, scaled by heat. Stable hue across
            // light/dark themes (it's a solid color, not a theme color
            // whose alpha would compound in dark mode). Suppressed when
            // the Ant Trail is disabled. See `crate::ant_trail`.
            row = row.bg(crate::ant_trail::tint(crate::ant_trail::base(cx), heat));
        }
        // Spec §2 multi-select fill. Painted for every set member
        // EXCEPT the lead: the Table primitive draws its own
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
        // OS drag-out: `on_drag` alone is a purely in-window gpui drag:
        // the `external_drag_payload` chained below is what promotes it
        // to a native platform drag (AppKit on macOS, Shell/OLE on Windows)
        // the moment the pointer leaves the viewport, so dragging rows
        // to Finder, Explorer, or other apps drops the actual files. The
        // resolver runs on the UI thread at promotion time: directory-ness comes
        // from the cached `EntryKind`, never from a stat. Spec §3.1:
        // pressing a selected row drags the full visible-order
        // selection; pressing an unselected row drags just that row.
        // Once a drag is active GPUI already owns the listener's value in an
        // Arc. Re-registering every visible row on every forced drag repaint
        // only rebuilt (and, for selections, deep-cloned) payloads that can no
        // longer start another gesture.
        if cx.has_active_drag() {
            return row;
        }
        if let Some(entry) = self.entries.get(row_ix) {
            let row_is_selected = self.is_selected(entry.id);
            // Archive rows carry archive coordinates rather than paths.
            if self.is_archive_mode() {
                if let Some(drag) = self.archive_drag_for_row(row_ix, row_is_selected, cx) {
                    let count = drag.entries.len();
                    let names: SmallVec<[SharedString; GHOST_STACK_CAP]> = drag
                        .entries
                        .iter()
                        .take(GHOST_STACK_CAP)
                        .map(|p| {
                            SharedString::from(
                                p.rsplit('/').next().unwrap_or(p.as_str()).to_string(),
                            )
                        })
                        .collect();
                    return row
                        .on_drag(drag, move |_d, offset, _window, cx| {
                            cx.new(|_| DragBadge {
                                names: names.clone(),
                                icons: smallvec![],
                                count,
                                offset,
                                native_owned: Arc::new(AtomicBool::new(false)),
                            })
                        })
                        .external_drag_payload::<ArchiveEntryDrag>(|drag, window, cx| {
                            // Promoting to a native promise session is macOS-only
                            // (docs/GPUI-UPSTREAM.md #11). Elsewhere the payload
                            // resolver has nothing to offer the platform, so the
                            // arguments go unread.
                            #[cfg(not(target_os = "macos"))]
                            let _ = (drag, window, cx);
                            #[cfg(target_os = "macos")]
                            {
                                use raw_window_handle::{HasWindowHandle, RawWindowHandle};

                                let source_window = gpui::Window::window_handle(window);
                                let Ok(handle) = HasWindowHandle::window_handle(window) else {
                                    return None;
                                };
                                let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                                    return None;
                                };
                                let archive = drag.archive.clone();
                                let password = drag.password.clone();
                                let promises =
                                    archive_promise_names(&drag.entries, &drag.directories)
                                        .into_iter()
                                        .zip(drag.entries.iter().cloned())
                                        .zip(drag.directories.iter().copied())
                                        .map(|((name, entry), is_dir)| {
                                            let archive = archive.clone();
                                            let password = password.clone();
                                            crate::platform_shell::FilePromise::new(
                                                name,
                                                is_dir,
                                                move |target| {
                                                    ferail_fs_native::materialize_archive_entry(
                                                        &archive,
                                                        &entry,
                                                        target,
                                                        password.as_deref(),
                                                    )
                                                    .map_err(|error| error.to_string())
                                                },
                                            )
                                        })
                                        .collect();
                                set_native_archive_drag(Some(drag.clone()));
                                crate::log_info!(
                                    100,
                                    "archive-drag: promoting {} entr{} to native promises",
                                    drag.entries.len(),
                                    if drag.entries.len() == 1 { "y" } else { "ies" }
                                );
                                if crate::platform_shell::start_file_promise_drag(
                                    handle.ns_view.as_ptr(),
                                    promises,
                                ) {
                                    crate::log_info!(100, "archive-drag: native session started");
                                    // From this point AppKit owns the physical
                                    // gesture. Deliberately retire GPUI's
                                    // typed drag instead of trying to smuggle
                                    // it across windows; Ferail destinations
                                    // use NATIVE_ARCHIVE_DRAG with the native
                                    // MouseMove/MouseUp callbacks below.
                                    cx.stop_active_drag(window);
                                    // Keep the coordinator payload alive while
                                    // AppKit owns the physical gesture. Other
                                    // Ferail windows consume its archive
                                    // coordinates; Finder consumes the native
                                    // promises. Retire it as soon as AppKit
                                    // reports that the session ended.
                                    cx.spawn(async move |cx| {
                                        // `willBeginAtPoint` is delivered
                                        // as the session starts. Yield once
                                        // before observing the shared flag
                                        // so a callback ordering change
                                        // cannot retire the drag early.
                                        cx.background_executor()
                                            .timer(std::time::Duration::from_millis(16))
                                            .await;
                                        while crate::platform_shell::native_drag_session_active() {
                                            cx.background_executor()
                                                .timer(std::time::Duration::from_millis(16))
                                                .await;
                                        }
                                        let _ = cx.update_window(source_window, |_, window, cx| {
                                            cx.stop_active_drag(window);
                                            set_native_archive_drag(None);
                                        });
                                        // Repaint every window at the terminal
                                        // edge to clear a hover border left in
                                        // a window the pointer departed.
                                        cx.refresh();
                                    })
                                    .detach();
                                } else {
                                    crate::log_warn!(
                                        100,
                                        "archive-drag: native session failed to start"
                                    );
                                    set_native_archive_drag(None);
                                }
                            }
                            None
                        });
                }
                return row;
            }
            if row_is_selected {
                // Shared snapshot for the whole selection: built once
                // per selection/model change, reused by every selected
                // row. The old per-row walk over ALL entries made a
                // big selection quadratic per render pass.
                if let Some(snapshot) = self.drag_snapshot(cx) {
                    let count = snapshot.paths.len();
                    let names = snapshot.names.clone();
                    let ghost_icons = snapshot.icons.clone();
                    let dirs = snapshot.dirs.clone();
                    let native_owned = snapshot.native_owned.clone();
                    let native_owned_for_badge = native_owned.clone();
                    let native_owned_for_payload = native_owned.clone();
                    return row
                        .on_drag(
                            ExternalPaths(snapshot.paths.as_ref().clone().into()),
                            move |_paths, offset, _window, cx| {
                                native_owned_for_badge.store(false, Ordering::Release);
                                cx.new(|_| DragBadge {
                                    names: names.clone(),
                                    icons: ghost_icons.clone(),
                                    count,
                                    offset,
                                    native_owned: native_owned_for_badge.clone(),
                                })
                            },
                        )
                        .external_drag_payload::<ExternalPaths>(move |paths, _window, _cx| {
                            native_owned_for_payload.store(true, Ordering::Release);
                            Some(gpui::ExternalDragPayload::Files(gpui::FileDragPaths::new(
                                paths.paths().iter().cloned().zip(dirs.iter().copied()),
                            )))
                        });
                }
            } else if let Some(path) = self.path_for_entry(entry.id) {
                // Unselected row: drags just itself: cheap, no snapshot.
                let mut ghost_icons: SmallVec<[Arc<RenderImage>; GHOST_STACK_CAP]> = smallvec![];
                self.push_ghost_icon(entry, &path, show_thumbnails(cx), &mut ghost_icons);
                let names: SmallVec<[SharedString; GHOST_STACK_CAP]> = smallvec![ghost_name(&path)];
                let is_dir = matches!(entry.kind, EntryKind::Directory);
                let native_owned = Arc::new(AtomicBool::new(false));
                let native_owned_for_badge = native_owned.clone();
                let native_owned_for_payload = native_owned.clone();
                return row
                    .on_drag(
                        ExternalPaths(vec![path].into()),
                        move |_paths, offset, _window, cx| {
                            native_owned_for_badge.store(false, Ordering::Release);
                            cx.new(|_| DragBadge {
                                names: names.clone(),
                                icons: ghost_icons.clone(),
                                count: 1,
                                offset,
                                native_owned: native_owned_for_badge.clone(),
                            })
                        },
                    )
                    .external_drag_payload::<ExternalPaths>(move |paths, _window, _cx| {
                        native_owned_for_payload.store(true, Ordering::Release);
                        Some(gpui::ExternalDragPayload::Files(gpui::FileDragPaths::new(
                            paths.paths().iter().cloned().map(|p| (p, is_dir)),
                        )))
                    });
            }
        }
        row
    }

    /// Accent/danger ring on folder rows while a native promise session is in
    /// flight: the twin of the `drag_over::<ArchiveEntryDrag>` style in
    /// `render_tr`. GPUI has no typed drag during that session, so
    /// `drag_over` never fires; the row's `on_mouse_move` repaints and the
    /// table merges this into its single hover slot so the ring follows the
    /// row AppKit is actually over.
    fn render_tr_hover(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Option<gpui::StyleRefinement> {
        if !native_archive_drag_active() {
            return None;
        }
        let is_dir = matches!(self.entries.get(row_ix)?.kind, EntryKind::Directory);
        if !is_dir {
            return None;
        }
        let style = gpui::StyleRefinement::default();
        Some(if self.is_archive_mode() {
            style
                .cursor_not_allowed()
                .border_1()
                .border_color(cx.theme().danger)
                .bg(cx.theme().danger.opacity(0.08))
        } else {
            style
                .cursor_copy()
                .border_1()
                .border_color(cx.theme().accent)
                .bg(cx.theme().accent.opacity(0.10))
        })
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let _path_guard = ferail_core::path_guard::enter_render();
        let Some(entry) = self.entries.get(row_ix) else {
            return div().into_any_element();
        };

        let col_key = self
            .columns
            .get(col_ix)
            .map(|col| col.key.as_ref())
            .unwrap_or("");

        match col_key {
            // Name: Lucide line-art icon tinted by category (files +
            // symlinks); macOS NSWorkspace bitmap for folders so
            // user-customised folder icons and cloud-sync overlays
            // still render. Optional quarantine badge in the top-right
            // corner. Tooltip carries the full filename so truncation
            // is recoverable. (Next-level Phase 1.)
            "name" => {
                use ferail_core::EntryKind;
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
                        // scaffold, AROS) yield the blank placeholder: show
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
                        // `get` is a non-mutating HashMap read: the
                        // fetch itself happens off the render path in
                        // `visible_rows_changed`.
                        let thumb = if thumbs_on && !crate::private_mode::enabled() {
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
                // the list, scanned first, never shows an invisible-char name
                // as innocuous. `name_has_hazards` is precomputed at enumerate
                // time, so the row paint just reads a bool.
                let display_name: SharedString = crate::private_mode::present_leaf_str(
                    &entry.display_name,
                    matches!(entry.kind, ferail_core::EntryKind::Directory),
                )
                .into();
                let inline_session = self.inline_name_edit.as_ref().and_then(|binding| {
                    let target = crate::inline_edit::FileNameEditTarget {
                        tab_id: binding.tab_id,
                        node_id: entry.id,
                    };
                    binding
                        .model
                        .snapshot()
                        .filter(|session| session.target == target)
                        .map(|session| (binding.input.clone(), session))
                });
                let is_editing = inline_session.is_some();
                let is_selected = if self.selection_all {
                    !self.selected_set.contains(&entry.id)
                } else {
                    self.selected_set.contains(&entry.id)
                };
                let tooltip_name = display_name.clone();
                let column_width = self
                    .columns
                    .get(col_ix)
                    .map(|column| f32::from(column.width))
                    .unwrap_or(240.0);
                // Hazard names are several independent text runs, so GPUI
                // cannot apply its ordinary pixel-exact ellipsis across the
                // whole label. Use a deliberately conservative advance here
                // (including badge padding) so the semantic elider always
                // wins before the cell's hard edge can clip a suffix.
                let name_budget = ((column_width - 52.0) / 9.0).floor().max(10.0) as usize;
                let elided_name = elide_label(display_name.as_ref(), name_budget);
                let name_child: gpui::AnyElement = if let Some((input, session)) = inline_session {
                    crate::inline_edit::InlineEditor::new(
                        ("inline-file-name", entry.id.as_raw()),
                        crate::inline_edit::InlineEditInput::Text(input),
                        crate::inline_edit::InlineEditLayout::Row,
                        &session,
                        tr!("File name"),
                    )
                    .into_any_element()
                } else if entry.name_has_hazards && !crate::private_mode::enabled() {
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(crate::entry_info::name_hazard_element_elided(
                            &display_name,
                            SharedString::from(format!("file-row-name-{row_ix}")),
                            name_budget,
                        ))
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .min_w_0()
                        // Finder-style middle ellipsis: keep the name's start
                        // AND its extension when the column is too narrow.
                        .truncate_middle()
                        .child(elided_name)
                        .into_any_element()
                };
                // Inline tag chips, 6-DIP coloured dots after the
                // filename, one per applied Finder tag (max 7). Read
                // synchronously at load() time and stored in the
                // delegate; render only consumes the cached Vec.
                let row_tags = self.tags.get(row_ix).cloned().unwrap_or_default();
                let mut chips = gpui_component::h_flex().gap_1().flex_shrink_0();
                if !is_editing && crate::platform_shell::SUPPORTS_TAGS {
                    for color in row_tags.iter().take(7) {
                        chips = chips.child(
                            div()
                                .w(px(6.0))
                                .h(px(6.0))
                                .rounded_full()
                                .bg(tag_color_rgba(*color)),
                        );
                    }
                }
                // §5 favorited indicator: small accent star trailing
                // the name. Only painted for folder rows (files can't
                // be favorited) where the row's path is in the favorites
                // index. The parallel vec is refreshed by Shell on every
                // load + every favorites mutation.
                let is_favorited = self.is_favorited.get(row_ix).copied().unwrap_or(false);
                let star_color = cx.theme().primary;
                let star =
                    if !is_editing && is_favorited && matches!(entry.kind, EntryKind::Directory) {
                        svg()
                            .path("icons/nav/star.svg")
                            .icon_px(12.0)
                            .text_color(star_color)
                            .into_any_element()
                    } else {
                        div().w(px(0.0)).h(px(12.0)).into_any_element()
                    };
                // Archive rows only: depth indent + a disclosure caret for
                // folders. Ordinary listings have no `archive_rows`, so this
                // is `None` and the cell is byte-for-byte what it was.
                let tree_affordance = self.archive_tree_affordance(row_ix, cx);
                div()
                    .id(("file-row-name", row_ix))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_scale_sm()
                    .text_color(cx.theme().foreground)
                    .children(tree_affordance)
                    .child(icon_wrapper)
                    .child(name_child)
                    .child(chips)
                    .child(star)
                    .on_click(
                        cx.listener(move |_table, event: &gpui::ClickEvent, _window, cx| {
                            let modifiers = event.modifiers();
                            let modified = modifiers.platform
                                || modifiers.control
                                || modifiers.alt
                                || modifiers.shift;
                            if crate::inline_edit::should_begin_click_rename(
                                is_selected,
                                is_editing,
                                event.click_count(),
                                modified,
                            ) {
                                cx.stop_propagation();
                                cx.emit(TableEvent::RenameRequested(row_ix));
                            }
                        }),
                    )
                    .when(!is_editing, |this| {
                        this.tooltip(move |window, cx| {
                            Tooltip::new(tooltip_name.clone()).build(window, cx)
                        })
                    })
                    .into_any_element()
            }
            "size" => div()
                .text_scale_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(
                    if crate::private_mode::enabled() && entry.size > 0 {
                        ferail_fs_native::humanize_bytes(crate::private_mode::present_bytes(
                            entry.id.as_raw(),
                            entry.size,
                        ))
                    } else {
                        entry.display_size.to_string()
                    },
                ))
                .into_any_element(),
            // Unified Format column: replaces the old Kind + Magic
            // duplication. The trailing indicator grades how the
            // extension and the content-detected type relate: a red
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
                            // `label` is the cached kind/magic word; translate
                            // placeholder kinds ("Folder", "File") at render.
                            .child(crate::i18n::tr_dyn(&label)),
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
                                Tooltip::new(tr!(
                                    "Extension says \u{201C}{kind}\u{201D} but the content is \u{201C}{magic}\u{201D}: possible disguised file.",
                                    kind = tip_kind,
                                    magic = tip_magic
                                ))
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
                                Tooltip::new(tr!(
                                    "Extension says \u{201C}{kind}\u{201D} but the content looks like \u{201C}{magic}\u{201D}.",
                                    kind = tip_kind,
                                    magic = tip_magic
                                ))
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
            // A non-positive stamp means "unknown", not 1970: some archive
            // formats simply don't record per-entry times. Render nothing
            // rather than a misleading epoch date.
            "modified" => div()
                .text_scale_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(if entry.mtime_unix > 0 {
                    let now = ferail_core::now_unix();
                    ferail_core::humanize_mtime(
                        crate::private_mode::present_timestamp(
                            entry.id.as_raw(),
                            entry.mtime_unix,
                            now,
                        ),
                        now,
                    )
                } else {
                    String::new()
                }))
                .into_any_element(),
            // Description: rich facts from the magic-byte parse,
            // populated lazily by the prefetch worker. Empty string
            // renders as an empty cell, no skeleton shimmer in v1.
            "description" => div()
                .text_scale_xs()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(if crate::private_mode::enabled() {
                    crate::i18n::tr_dyn(&entry.format_label().0).to_string()
                } else {
                    entry.display_description.to_string()
                }))
                .into_any_element(),
            "path" => {
                let raw_path = self
                    .flat_paths
                    .as_ref()
                    .and_then(|paths| paths.display_directory(entry.id))
                    .unwrap_or_default();
                let full_path: SharedString = if crate::private_mode::enabled() {
                    crate::private_mode::present_path(std::path::Path::new(raw_path.as_ref()))
                        .into()
                } else {
                    raw_path
                };
                let column_width = self
                    .columns
                    .get(col_ix)
                    .map(|column| f32::from(column.width))
                    .unwrap_or(280.0);
                let path_budget = ((column_width - 16.0) / 6.5).floor().max(12.0) as usize;
                let visible_path = elide_label(full_path.as_ref(), path_budget);
                div()
                    .id(("flat-path", row_ix))
                    .min_w_0()
                    .truncate_middle()
                    .text_scale_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(visible_path)
                    .tooltip(move |window, cx| Tooltip::new(full_path.clone()).build(window, cx))
                    .into_any_element()
            }
            _ => div().into_any_element(),
        }
    }

    /// Plain text of a cell: the same strings `render_td` paints,
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
            "name" => entry.display_name.to_string(),
            "size" => entry.display_size.to_string(),
            "format" => entry.format_label().0.to_string(),
            "modified" => ferail_core::humanize_mtime(entry.mtime_unix, ferail_core::now_unix()),
            "description" => entry.display_description.to_string(),
            "path" => self
                .flat_paths
                .as_ref()
                .and_then(|paths| paths.display_directory(entry.id))
                .map(|path| path.to_string())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// Viewport-driven thumbnail warming (prime directive: expensive
    /// work scheduled from a semantic event, off the UI thread, dropped
    /// if it lands late). Called by the table whenever the visible row
    /// range changes: first layout and every scroll. We fetch Quick
    /// Look thumbnails for the *visible* thumbnailable rows only, never
    /// the whole (possibly thousands-deep) folder.
    fn visible_rows_changed(
        &mut self,
        visible_range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.warm_visible_details(visible_range.clone(), cx);
        self.warm_folder_icons(visible_range.clone(), cx);
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
            let title = crate::i18n::tr_static(column_title(&key));
            let label = if visible {
                format!("\u{2713} {title}")
            } else {
                format!("\u{2007}\u{2007}{title}")
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
                PopupMenuItem::new(tr!("Reset Columns")).on_click(move |_ev, _w, cx| {
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
        // Archive rows are virtual, so ordinary filesystem verbs would target
        // paths that do not exist. Editable zip workbenches expose their own
        // staged Rename / Remove commands through the owning ArchiveView.
        if self.is_archive_mode() {
            let Some(view) = self.archive_view.clone() else {
                return menu;
            };
            let editable = view
                .upgrade()
                .is_some_and(|view| view.read(cx).can_stage_edits());
            if !editable {
                return menu;
            }
            let rename_view = view.clone();
            let remove_view = view;
            return menu
                .item(
                    PopupMenuItem::new(tr!("Rename…")).on_click(move |_event, window, cx| {
                        if let Some(view) = rename_view.upgrade() {
                            view.update(cx, |view, cx| view.rename_selected(window, cx));
                        }
                    }),
                )
                .item(PopupMenuItem::new(tr!("Remove from Archive")).on_click(
                    move |_event, _window, cx| {
                        if let Some(view) = remove_view.upgrade() {
                            view.update(cx, |view, cx| view.stage_remove_selected(cx));
                        }
                    },
                ));
        }
        if self.browsing_trash {
            return self.trash_context_menu(row_ix, menu, cx);
        }
        #[cfg(windows)]
        use crate::shell::ShowWindowsContextMenu;
        use crate::shell::{
            BulkRenameSelected, ClearQuarantine, Compress, CompressSevenZ, CompressTar,
            CompressTarBz2, CompressTarGz, CompressTarXz, ConvertArchive, CopyPath,
            CreateChecksumFile, DeleteImmediately, Duplicate, EditFile, EditImage, EditTextFile,
            Extract, ExtractTo, GenerateSha256, GetInfo, MakeAlias, MoveToTrash, NewArchive,
            OpenAsArchive, OpenInNewTab, OpenSelected, OpenTerminalHere, QuickLook, RenameSelected,
            RevealInFinder, ShowLockHolders, SlideshowFromHere, ToggleFavoriteForTarget,
            ToggleTagBlue, ToggleTagGray, ToggleTagGreen, ToggleTagOrange, ToggleTagPurple,
            ToggleTagRed, ToggleTagYellow, VerifyChecksums,
        };
        // Anchor keyboard-shortcut resolution to the shell's stable
        // dispatch path (carries SHELL_CONTEXT, always painted) so the
        // item hints render from the first frame instead of popping in
        // a frame or two later. See `shell_focus`'s doc comment.
        let menu = menu.action_context(self.shell_focus.clone());

        // Prime directive: menu building is read-only, no shell or
        // filesystem queries at menu-open time.
        //
        // Tags come from the per-row `self.tags` slots the bulk load
        // already populated; the checkmarks therefore always agree
        // with the row's visible tag dots (rows past the load cap
        // show no dots and no checkmarks: consistent).
        //
        // Open With candidates come from the `open_with_warm` cache,
        // populated off-thread on selection-lead changes (see
        // `Shell::warm_open_with_for_row`). On a cache miss, e.g.
        // a direct right-click on a row that was never selected: we
        // kick the warm fetch and show a disabled "loading" item inside a
        // retained submenu; when the fetch reports back only that submenu
        // rebuilds, preserving the root menu and its selection.
        let target_path = self
            .entries
            .get(row_ix)
            .and_then(|entry| self.path_for_entry(entry.id));
        let warmed_candidates: Option<Vec<crate::platform_shell::OpenWithCandidate>> =
            match (&target_path, &self.open_with_warm) {
                (Some(p), Some((warm_path, cands))) if warm_path == p => Some(cands.clone()),
                _ => None,
            };
        let applied_tags: Vec<ferail_core::commands::TagColor> =
            self.tags.get(row_ix).cloned().unwrap_or_default();

        // Tags submenu: built as a nested PopupMenu Entity via
        // PopupMenu::build. Each colour is a `menu_with_check` so
        // applied tags render with a leading checkmark. Click
        // toggles via the ToggleTagX action.
        let tag_red_on = applied_tags.contains(&ferail_core::commands::TagColor::Red);
        let tag_orange_on = applied_tags.contains(&ferail_core::commands::TagColor::Orange);
        let tag_yellow_on = applied_tags.contains(&ferail_core::commands::TagColor::Yellow);
        let tag_green_on = applied_tags.contains(&ferail_core::commands::TagColor::Green);
        let tag_blue_on = applied_tags.contains(&ferail_core::commands::TagColor::Blue);
        let tag_purple_on = applied_tags.contains(&ferail_core::commands::TagColor::Purple);
        let tag_gray_on = applied_tags.contains(&ferail_core::commands::TagColor::Gray);

        // Availability over the resolved target set (the same set the
        // handler will act on), routed through `Availability`. Anchor
        // rules use the clicked/lead row; `SingleOnly` and capability
        // rules use the whole set. See docs/features/CONTEXT_MENU.md.
        //
        // Resolved HERE, at build time, from the delegate's own mirrored
        // selection, not from a snapshot the Shell stages on right-click,
        // which lands a frame too late (see `resolve_menu_targets`).
        let targets = resolve_menu_targets_with_mode(
            &self.entries,
            &self.selected_set,
            self.selection_all,
            row_ix,
            self.all_menu_caps,
        );
        let t = &targets;
        let show_slideshow = Availability::When(avail_anchor_file).allows(t);
        let show_checksum =
            Availability::SingleOnly.allows(t) && Availability::When(avail_anchor_file).allows(t);
        let show_verify = show_checksum
            && self
                .entries
                .get(row_ix)
                .is_some_and(crate::shell::verify::entry_is_manifest);
        let show_terminal = Availability::When(avail_anchor_dir).allows(t);
        let show_favorites = Availability::When(avail_anchor_dir).allows(t);
        // A folder seeds a tab directly. One file seeds its parent tab and is
        // selected there; this is especially useful from recursive Search
        // results, where the containing folder is not already on screen.
        let show_new_tab = Availability::When(avail_anchor_dir).allows(t)
            || (Availability::SingleOnly.allows(t)
                && Availability::When(avail_anchor_file).allows(t));
        let show_clear_quarantine = Availability::When(avail_any_quarantined).allows(t);
        let show_extract = Availability::When(avail_any_archive).allows(t);
        let show_single_only = Availability::SingleOnly.allows(t);
        let show_single_file = show_single_only && Availability::When(avail_anchor_file).allows(t);
        // Bulk complement of the SingleOnly Rename: pattern rename over
        // the whole resolved set (docs/features/BULK_RENAME.md).
        let bulk_rename_count = t.len();

        let already_favorited = self.is_favorited.get(row_ix).copied().unwrap_or(false);
        let favorite_label = if already_favorited {
            tr!("Remove from Favorites")
        } else {
            tr!("Add to Favorites")
        };

        use crate::menu_plan::{MenuPlan, MenuSurface, ids};
        let mut plan = MenuPlan::new(MenuSurface::FileRow).action(
            ids::OPEN,
            tr!("Open"),
            Box::new(OpenSelected),
        );
        if show_new_tab {
            plan = plan.action(
                ids::OPEN_IN_NEW_TAB,
                tr!("Open in New Tab"),
                Box::new(OpenInNewTab),
            );
        }
        if show_single_file {
            // Built-in lightweight editor first (docs/features/TEXT_EDITOR.md),
            // then the explicit system-editor escape hatch. Image files the
            // bundled codecs can round-trip also get the redaction/annotation
            // editor (docs/features/IMAGE_EDITOR.md): extension check only,
            // over the already-cached row name.
            plan = plan.action(ids::EDIT, tr!("Edit"), Box::new(EditFile));
            let anchor_editable_image = self
                .entries
                .get(row_ix)
                .map(|e| crate::image_edit::editable_image_name(&e.name))
                .unwrap_or(false);
            if anchor_editable_image {
                plan = plan.action(ids::EDIT_IMAGE, tr!("Edit Image"), Box::new(EditImage));
            }
            let label = if cfg!(target_os = "macos") {
                tr!("Edit in TextEdit")
            } else if cfg!(windows) {
                tr!("Edit in Notepad")
            } else {
                tr!("Edit in Text Editor")
            };
            plan = plan.action(ids::EDIT_IN_SYSTEM_EDITOR, label, Box::new(EditTextFile));
        }
        plan = plan
            .separator()
            .action(ids::GET_INFO, tr!("Get Info"), Box::new(GetInfo))
            .action(ids::QUICK_LOOK, tr!("Quick Look"), Box::new(QuickLook));
        if show_slideshow {
            // Anchor command: start the viewer slideshow anchored to the
            // clicked file (docs/features/VIEWER.md). Folder anchors can't
            // start a slideshow, so the item is file-anchored.
            plan = plan.action(
                ids::SLIDESHOW_FROM_HERE,
                tr!("Slideshow from Here"),
                Box::new(SlideshowFromHere),
            );
        }
        plan = plan.separator().action(
            ids::REVEAL,
            crate::i18n::tr_static(ferail_core::commands::REVEAL_LABEL),
            Box::new(RevealInFinder),
        );
        if show_single_only {
            // SingleOnly: copying one path is the row action; copying many
            // joined paths is a deliberate, separate gesture, so this hides
            // past a single target rather than silently concatenating.
            plan = plan.action(ids::COPY_PATH, tr!("Copy Path"), Box::new(CopyPath));
        }
        if show_checksum {
            plan = plan.action(
                ids::GENERATE_SHA256,
                tr!("Generate SHA-256…"),
                Box::new(GenerateSha256),
            );
        }
        if show_verify {
            plan = plan.action(
                ids::VERIFY_CHECKSUMS,
                tr!("Verify Checksums…"),
                Box::new(VerifyChecksums),
            );
        }
        plan = plan.action(
            ids::CREATE_CHECKSUM_FILE,
            tr!("Create Checksum File…"),
            Box::new(CreateChecksumFile),
        );
        if crate::platform_shell::lock_diagnostics_available() {
            // Batch diagnostic over the whole resolved set: name the
            // processes holding these files open, with force-close
            // buttons. Hidden where the platform lookup is stubbed
            // (macOS/Linux) rather than showing an always-empty dialog.
            plan = plan.action(
                ids::SHOW_LOCK_HOLDERS,
                tr!("What’s Locking This?"),
                Box::new(ShowLockHolders),
            );
        }
        if show_terminal {
            // Anchor command: open a terminal at the clicked directory,
            // grouped with the path-oriented actions above.
            plan = plan.action(
                ids::OPEN_TERMINAL_HERE,
                tr!("Open Terminal Here"),
                Box::new(OpenTerminalHere),
            );
        }
        plan = plan.separator();
        if show_single_only {
            // SingleOnly: Rename targets one file (single-target, like
            // Finder's inline rename); hidden on a multi-selection.
            plan = plan.action(ids::RENAME, tr!("Rename\u{2026}"), Box::new(RenameSelected));
        }
        if bulk_rename_count >= 2 {
            // Multi-selection twin of Rename: the pattern-rule modal
            // over every resolved target (docs/features/BULK_RENAME.md).
            plan = plan.action(
                ids::BULK_RENAME,
                trn!(
                    "Rename {n} Item\u{2026}",
                    "Rename {n} Items\u{2026}",
                    bulk_rename_count
                ),
                Box::new(BulkRenameSelected),
            );
        }
        // Single "Compress" submenu grouping every creatable format. The tar
        // variants nest under a "TAR" group so they aren't each prefixed a
        // redundant "TAR.". Built here (deref cx → &mut App, like the
        // Tags/Open-With submenus below) so they attach in menu order.
        let tar_submenu = PopupMenu::build(window, cx, |m, _w, _c| {
            m.menu("Gzip", Box::new(CompressTarGz))
                .menu("Bzip2", Box::new(CompressTarBz2))
                .menu("XZ", Box::new(CompressTarXz))
                .separator()
                .menu(tr!("Uncompressed"), Box::new(CompressTar))
        });
        let compress_submenu = PopupMenu::build(window, cx, move |m, _w, _c| {
            m.menu("ZIP", Box::new(Compress))
                .menu("7-Zip", Box::new(CompressSevenZ))
                .item(PopupMenuItem::submenu("TAR", tar_submenu))
                // One-click entries above use sensible defaults; this opens the
                // dialog for format + compression level + password.
                .separator()
                .menu(tr!("New Archive\u{2026}"), Box::new(NewArchive))
        });
        plan = plan
            .action(ids::DUPLICATE, tr!("Duplicate"), Box::new(Duplicate))
            .action(ids::MAKE_ALIAS, tr!("Make Alias"), Box::new(MakeAlias))
            .submenu(ids::COMPRESS, tr!("Compress"), compress_submenu);
        if show_extract {
            // Capability command: shown when any target is an archive
            // (docs/features/CONTEXT_MENU.md). "Extract Here" unpacks into the
            // current folder; "Extract To…" opens a folder picker first. Both
            // choose a smart destination per archive.
            let extract_submenu = PopupMenu::build(window, cx, |m, _w, _c| {
                m.menu(tr!("Extract Here"), Box::new(Extract))
                    .menu(tr!("Extract To\u{2026}"), Box::new(ExtractTo))
            });
            plan = plan.submenu(ids::EXTRACT, tr!("Extract"), extract_submenu);
            if show_single_only {
                plan = plan.action(
                    ids::CONVERT_ARCHIVE,
                    tr!("Convert Archive…"),
                    Box::new(ConvertArchive),
                );
            }
        }
        if show_single_file {
            // This command is intentionally broader than the Extract menu.
            // The backend probes bytes off-thread, so OOXML/JAR/APK files and
            // extensionless or misnamed archives remain browsable without
            // adding synchronous sniffing to context-menu construction.
            plan = plan.action(
                ids::OPEN_AS_ARCHIVE,
                tr!("Open as Archive"),
                Box::new(OpenAsArchive),
            );
        }
        if show_clear_quarantine {
            // Capability command (docs/features/CONTEXT_MENU.md): show when
            // ANY row in the resolved target set carries the
            // Mark-of-the-Web, matching `Shell::on_clear_quarantine`, which
            // strips it from the quarantined subset. Reads the caps
            // projected from the loaded rows, no xattr query at
            // menu-open time. Right-clicking the clean file in a
            // mixed selection now offers the command too, instead of hiding
            // it based on the single clicked row.
            plan = plan.separator().action(
                ids::CLEAR_QUARANTINE,
                crate::i18n::tr_static(ferail_core::commands::CLEAR_QUARANTINE_LABEL),
                Box::new(ClearQuarantine),
            );
        }
        if show_favorites {
            // Anchor command: toggle the clicked folder's path against the
            // user's Favorites (docs/features/FAVORITES.md §2.1).
            // `resolve_favorite_target` reads the row from `context_row`.
            plan = plan.separator().action(
                ids::TOGGLE_FAVORITE,
                favorite_label,
                Box::new(ToggleFavoriteForTarget),
            );
        }

        // Build submenu Entities via `PopupMenu::build`, which only
        // needs `&mut App` (which we have via Context<TableState>'s
        // deref). The plan carries pre-built submenu entities.
        // SingleOnly: "Open With" resolves one warmed path; on a
        // multi-selection the slot indices wouldn't map to a single app,
        // so the submenu is hidden rather than acting on just the anchor.
        if show_single_only && target_path.is_some() {
            match &warmed_candidates {
                Some(candidates) => {
                    let candidates = candidates.clone();
                    let open_with_submenu = PopupMenu::build(window, cx, move |m, _w, _c| {
                        build_open_with_submenu(m, &candidates)
                    });
                    plan = plan.submenu(ids::OPEN_WITH, tr!("Open With"), open_with_submenu);
                }
                // Cache miss: retain a real submenu entity containing the
                // placeholder. The background completion rebuilds this exact
                // entity, so the root menu keeps its focus and highlighted row.
                None => {
                    let open_with_submenu = PopupMenu::build(window, cx, |m, _w, _c| {
                        m.item(PopupMenuItem::new(tr!("Loading\u{2026}")).disabled(true))
                    });
                    if let Some(path) = target_path.clone() {
                        spawn_open_with_submenu(
                            cx.entity().clone(),
                            path,
                            open_with_submenu.downgrade(),
                            window,
                            cx,
                        );
                    }
                    plan = plan.submenu(ids::OPEN_WITH, tr!("Open With"), open_with_submenu);
                }
            }
        }

        if crate::platform_shell::SUPPORTS_TAGS {
            // Names of the tags applied to the clicked row: offered for
            // pinning to the sidebar as Tag favorites (§9).
            let applied_tag_names: Vec<String> =
                applied_tags.iter().map(|c| c.name().to_string()).collect();
            let tags_submenu = PopupMenu::build(window, cx, move |m, _w, _c| {
                let mut m = m
                    .menu_with_check(tr!("Red"), tag_red_on, Box::new(ToggleTagRed))
                    .menu_with_check(tr!("Orange"), tag_orange_on, Box::new(ToggleTagOrange))
                    .menu_with_check(tr!("Yellow"), tag_yellow_on, Box::new(ToggleTagYellow))
                    .menu_with_check(tr!("Green"), tag_green_on, Box::new(ToggleTagGreen))
                    .menu_with_check(tr!("Blue"), tag_blue_on, Box::new(ToggleTagBlue))
                    .menu_with_check(tr!("Purple"), tag_purple_on, Box::new(ToggleTagPurple))
                    .menu_with_check(tr!("Gray"), tag_gray_on, Box::new(ToggleTagGray));
                // Pin each applied tag to the sidebar. Closure items add the
                // Tag favorite directly through the process-global entity,
                // no per-tag action needed (writes are off the paint path).
                if !applied_tag_names.is_empty() {
                    m = m.separator();
                    for name in &applied_tag_names {
                        let name = name.clone();
                        let label = tr!("Pin \u{201c}{name}\u{201d} to Sidebar", name = name);
                        m = m.item(PopupMenuItem::new(label).on_click(move |_ev, _w, cx| {
                            let favs = crate::process_state::process_state(cx).favorites().clone();
                            let name = name.clone();
                            favs.update(cx, |f, cx| {
                                f.add_tag(name, cx);
                            });
                        }));
                    }
                }
                m
            });
            plan = plan.submenu(ids::TAGS, tr!("Tags"), tags_submenu);
        }

        plan = plan
            .separator()
            .action(
                ids::MOVE_TO_TRASH,
                crate::i18n::tr_static(ferail_core::commands::TRASH_LABEL),
                Box::new(MoveToTrash),
            )
            .action(
                ids::DELETE_IMMEDIATELY,
                tr!("Delete Immediately\u{2026}"),
                Box::new(DeleteImmediately),
            );
        #[cfg(windows)]
        {
            plan = plan.separator().action(
                ids::WINDOWS_CONTEXT_MENU,
                tr!("More options from Windows\u{2026}"),
                Box::new(ShowWindowsContextMenu),
            );
        }
        plan.render(menu)
    }

    fn background_context_menu(
        &mut self,
        menu: PopupMenu,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        // Archive rows are virtual: the "current folder" lives inside an
        // archive, so the folder verbs below have no real path to act on.
        // Returning the menu unchanged keeps the empty space inert there.
        if self.is_archive_mode() {
            return menu;
        }
        use crate::shell::{
            AddCurrentFolderToFavorites, CopyContextPath, EmptyTrash, GetInfoAtContext, NewFolder,
            OpenTerminalAtContext, PasteFiles, Refresh, RevealContextPath, SelectAll,
        };
        if self.browsing_trash {
            // No New Folder and no Paste: the trash is not somewhere you put
            // things on purpose, and offering to would be offering to create
            // something already deleted.
            use crate::menu_plan::{MenuPlan, MenuSurface, ids};
            return MenuPlan::new(MenuSurface::TrashBackground)
                .action(ids::SELECT_ALL, tr!("Select All"), Box::new(SelectAll))
                .separator()
                .action(
                    ids::REVEAL,
                    crate::i18n::tr_static(ferail_core::commands::REVEAL_LABEL),
                    Box::new(RevealContextPath),
                )
                .action(ids::COPY_PATH, tr!("Copy Path"), Box::new(CopyContextPath))
                .separator()
                .action(
                    ids::EMPTY_TRASH,
                    tr!("Empty Trash\u{2026}"),
                    Box::new(EmptyTrash),
                )
                .action(ids::REFRESH, tr!("Refresh"), Box::new(Refresh))
                .render(menu.action_context(self.shell_focus.clone()));
        }

        // Empty-space right-click: the menu targets the folder being
        // browsed, never the selection. `NewFolder` / `PasteFiles` /
        // `SelectAll` / `Refresh` already act on the current directory;
        // the four context-path verbs act on `Shell::context_target`,
        // which the `RightClickedBackground` subscriber staged to the
        // current directory the instant this menu opened. Prime
        // directive: labels and actions only, no filesystem or shell
        // queries at menu-open time.
        use crate::menu_plan::{MenuPlan, MenuSurface, ids};
        MenuPlan::new(MenuSurface::FileBackground)
            .action(ids::NEW_FOLDER, tr!("New Folder"), Box::new(NewFolder))
            .separator()
            .action(ids::PASTE, tr!("Paste"), Box::new(PasteFiles))
            .action(ids::SELECT_ALL, tr!("Select All"), Box::new(SelectAll))
            .separator()
            .action(ids::GET_INFO, tr!("Get Info"), Box::new(GetInfoAtContext))
            .action(
                ids::REVEAL,
                crate::i18n::tr_static(ferail_core::commands::REVEAL_LABEL),
                Box::new(RevealContextPath),
            )
            .action(ids::COPY_PATH, tr!("Copy Path"), Box::new(CopyContextPath))
            .action(
                ids::OPEN_TERMINAL_HERE,
                tr!("Open Terminal Here"),
                Box::new(OpenTerminalAtContext),
            )
            .separator()
            .action(
                ids::PIN_TO_FAVORITES,
                tr!("Add Folder to Favorites"),
                Box::new(AddCurrentFolderToFavorites),
            )
            .separator()
            .action(ids::REFRESH, tr!("Refresh"), Box::new(Refresh))
            .render(menu.action_context(self.shell_focus.clone()))
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

    /// Click on a header runs this: we delegate to the existing
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
                // "Reset to natural order": sort by name ascending
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
        //
        // A folder whose rows were all excluded by the filter field is
        // not an empty folder, saying so sends the user looking for
        // missing files. Name the filter as the cause instead.
        // Same words as the status bar's chip: "hidden" is already
        // taken by the show-hidden toggle and would read as that.
        let message = match (self.flat_paths.is_some(), self.filtered_out) {
            (true, 0) => tr!("No files found in this location or its subfolders."),
            (_, 0) => tr!("This folder is empty."),
            (_, n) => trn!("{n} item filtered out.", "All {n} items filtered out.", n),
        };
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
                    .child(message),
            )
    }

    /// Swap the whole table body for the loading view while a slow
    /// device (spun-down external drive, cold network mount) is waking
    /// up. Also removes the previous directory's stale rows from the
    /// screen: they belong to the path the breadcrumb no longer shows
    /// and must not stay clickable.
    fn loading(&self, _cx: &App) -> bool {
        self.slow_load.is_some()
    }

    /// The built-in pulsing skeleton rows (the in-pane "still working"
    /// signal: the status bar runs its indeterminate stripe in
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
                    .child(tr!(
                        "Reading \u{201c}{label}\u{201d}\u{2026}",
                        label = label
                    )),
            )
    }
}

/// Fetch Open With candidates for `path` on the background executor
/// and store them in the delegate's [`FileListDelegate::open_with_warm`]
/// cache. The fetch (`open_with_candidates`, ~10–50 ms of
/// LaunchServices / IAssocHandler work) never runs on the UI thread.
/// Last-writer-wins by design: the cache holds one entry: the most
/// recently warmed path, which is always the row the user is about
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
            let delegate = state.delegate_mut();
            delegate.open_with_warm = Some((path, candidates));
            cx.notify();
        });
    })
    .detach();
}

/// Populate an Open-With submenu from the cache order used by the dispatch
/// handlers. Keeping the slot mapping in one function prevents a rebuilt
/// submenu from displaying an app under a different action index.
fn build_open_with_submenu(
    mut menu: PopupMenu,
    candidates: &[crate::platform_shell::OpenWithCandidate],
) -> PopupMenu {
    if candidates.is_empty() {
        return menu.item(PopupMenuItem::new(tr!("No applications found")).disabled(true));
    }

    for (slot, candidate) in candidates.iter().take(12).enumerate() {
        let label = if candidate.is_default {
            tr!("{name} (default)", name = candidate.name)
        } else {
            SharedString::from(candidate.name.clone())
        };
        menu = menu.menu(label, open_with_action(slot));
    }
    menu
}

fn open_with_action(slot: usize) -> Box<dyn gpui::Action> {
    use crate::shell::{
        OpenWithSlot0, OpenWithSlot1, OpenWithSlot2, OpenWithSlot3, OpenWithSlot4, OpenWithSlot5,
        OpenWithSlot6, OpenWithSlot7, OpenWithSlot8, OpenWithSlot9, OpenWithSlot10, OpenWithSlot11,
    };
    match slot {
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
    }
}

/// Cold-cache path for an already-open context menu. LaunchServices / Windows
/// association lookup remains off the UI thread; completion updates the
/// delegate cache first (so action slots resolve correctly), then rebuilds
/// only the retained submenu entity.
fn spawn_open_with_submenu(
    table: gpui::Entity<TableState<FileListDelegate>>,
    path: PathBuf,
    submenu: gpui::WeakEntity<PopupMenu>,
    window: &Window,
    cx: &mut Context<TableState<FileListDelegate>>,
) {
    let fetch_path = path.clone();
    cx.spawn_in(window, async move |_, cx| {
        let candidates = cx
            .background_executor()
            .spawn(async move { crate::platform_shell::open_with_candidates(&fetch_path) })
            .await;

        let candidates_for_menu = candidates.clone();
        let _ = table.update_in(cx, |state, _window, cx| {
            state.delegate_mut().open_with_warm = Some((path, candidates));
            cx.notify();
        });
        let _ = submenu.update_in(cx, |menu, window, cx| {
            menu.rebuild(window, cx, move |menu, _window, _cx| {
                build_open_with_submenu(menu, &candidates_for_menu)
            });
        });
    })
    .detach();
}

/// Lookup helper for double-click open / Enter key: turn a row
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
pub(crate) fn tag_color_rgba(c: ferail_core::commands::TagColor) -> gpui::Rgba {
    use ferail_core::commands::TagColor;
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

/// Mark-of-the-Web quarantine badge: small red dot in the icon's
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
    /// Unified Format column (next-level Phase 1): sorts by the
    /// magic-detected description, falling back to the extension-
    /// derived kind. Replaces the old `Kind` + `Magic` sort options.
    Format,
    Modified,
    /// Relative parent directory, available only on a Flat surface.
    Path,
    /// Ant Trail heat: how often the user has visited a folder
    /// (docs/features/ANT_TRAIL.md). Directory-only by nature: files
    /// have no heat, so they sort among themselves by name below the
    /// folders. The key is not on `FileEntry`; it lives in the
    /// delegate's row-parallel `heats`, so the ordering is applied by
    /// [`FileListDelegate::sort_model`] rather than [`sort_in_place`].
    AntTrail,
}

impl std::str::FromStr for SortColumn {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "name" => Ok(Self::Name),
            "size" => Ok(Self::Size),
            "format" | "kind" | "magic" => Ok(Self::Format),
            "modified" | "mtime" => Ok(Self::Modified),
            "path" => Ok(Self::Path),
            "ant" | "ant-trail" | "heat" => Ok(Self::AntTrail),
            _ => Err(()),
        }
    }
}

// In-place sort with folders-first grouping (Finder convention) lives in
// `sort_entries` below; pure logic, easy to extend.

/// The header label (English msgid) for a column key. The persisted key
/// (`"name"`, `"size"`, …) is the identity; the label is translated where
/// it is shown: `column_name` for the header / drag ghost / autofit, and
/// the header's show-hide menu, so a language switch repaints without
/// rebuilding the columns.
fn column_title(key: &str) -> &'static str {
    match key {
        "name" => ferail_core::msgid!("Name"),
        "size" => ferail_core::msgid!("Size"),
        "format" => ferail_core::msgid!("Format"),
        "modified" => ferail_core::msgid!("Modified"),
        "description" => ferail_core::msgid!("Description"),
        "path" => ferail_core::msgid!("Path"),
        _ => "",
    }
}

/// The built-in column set, in default order. `Column::name` holds the
/// English msgid; see [`column_title`].
fn default_columns() -> Vec<Column> {
    vec![
        Column::new("name", column_title("name"))
            .width(360.0)
            .sortable(),
        Column::new("size", column_title("size"))
            .width(100.0)
            .sortable(),
        Column::new("format", column_title("format"))
            .width(220.0)
            .sortable(),
        Column::new("modified", column_title("modified"))
            .width(160.0)
            .sortable(),
        // Description column: rich ` · `-joined facts derived
        // from the magic-byte parse (bitness/arch/subsystem
        // for binaries, w×h for images, channels/kHz/duration
        // for audio, etc.). Populated by the prefetch worker:
        // empty until the worker batch lands, then never
        // touched by paint. Not sortable in v1: lex sort of
        // description strings groups MP3s near MP4s but
        // separates 32-bit from 64-bit binaries, which is
        // confusing. Revisit if users ask.
        Column::new("description", column_title("description")).width(320.0),
    ]
}

fn flat_path_column() -> Column {
    Column::new("path", column_title("path"))
        .width(360.0)
        .sortable()
}

/// Column width clamp for persisted values: a corrupt entry can't
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
            continue; // unknown key (older/newer build): skip
        };
        let mut col = pool.remove(pos);
        // `"NaN".parse::<f32>()` succeeds: filter non-finite values
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
        .map(|n| ferail_fs_native::paths::display_leaf(n.to_string_lossy().as_ref()).into_owned())
        .unwrap_or_default()
        .into()
}

/// Sort with `sort_by_cached_key`: each element's key (its casefolded
/// name, plus the format label for the Format column) is built ONCE
/// per element, not per comparison. The previous comparator allocated
/// 2–4 lowercase Strings per COMPARISON, and the streaming pipeline
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
pub fn sort_in_place(entries: &mut [ferail_core::FileEntry], col: SortColumn, asc: bool) {
    use std::cmp::Reverse;
    fn non_dir(e: &ferail_core::FileEntry) -> bool {
        !matches!(e.kind, ferail_core::EntryKind::Directory)
    }
    fn name_key(e: &ferail_core::FileEntry) -> String {
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
                (
                    non_dir(e),
                    Reverse(e.mtime_unix),
                    name_key(e),
                    e.id.as_raw(),
                )
            });
        }
        // Path ordering needs the Flat surface's directory arena, and Ant
        // Trail ordering needs the delegate's row-parallel `heats`: both
        // are handled by `FileListDelegate::sort_model` above.
        (SortColumn::Path, _) | (SortColumn::AntTrail, _) => {}
    }
}

/// Resolve an Ant Trail pick against the rows actually in hand.
///
/// Ant Trail ranks by per-row heat, which a surface may simply not have:
/// Flat keeps `heats` empty on purpose. The decision is made from the
/// data, never from which surface we think we are: a surface flag that
/// disagrees with the rows (a stale `flat_paths` left behind when Include
/// Subfolders closed, say) would otherwise turn the pick into a silent
/// no-op, and the user would see a list that looks name-sorted with the
/// hot folder still sitting in the middle of it.
///
/// With no heat to rank by, Name ascending is the honest answer; every
/// other column passes through untouched.
fn resolve_ant_sort(col: SortColumn, asc: bool, has_heat: bool) -> (SortColumn, bool) {
    if col == SortColumn::AntTrail && !has_heat {
        (SortColumn::Name, true)
    } else {
        (col, asc)
    }
}

/// Order rows by Ant Trail heat (docs/features/ANT_TRAIL.md).
///
/// Heat is not a `FileEntry` field: it is looked up per row through
/// `heat_of`, which the delegate backs with its row-parallel `heats`
/// vector (an in-memory read, no I/O). Only directories ever carry
/// heat, so the folders-first grouping every other column uses doubles
/// as "the rows this ordering is about, first".
///
/// `asc == false` (the default first pick) puts the hottest folders on
/// top, which is the useful direction: "where do I actually go?".
/// Never-visited folders and all files fall to the bottom in name
/// order, so a cold directory still reads like a normal listing.
fn sort_by_heat(
    entries: &mut [ferail_core::FileEntry],
    heat_of: impl Fn(NodeId) -> f32,
    asc: bool,
) {
    use std::cmp::Reverse;
    fn non_dir(e: &ferail_core::FileEntry) -> bool {
        !matches!(e.kind, ferail_core::EntryKind::Directory)
    }
    // f32 is not `Ord`, and heat is a normalized 0.0..=1.0 ratio: quantize
    // to a u32 so the rows can ride the same `sort_by_cached_key` fast path
    // as every other column instead of a per-comparison float comparator.
    let key = |e: &ferail_core::FileEntry| (heat_of(e.id).clamp(0.0, 1.0) * 1_000_000.0) as u32;
    if asc {
        entries.sort_by_cached_key(|e| {
            (
                non_dir(e),
                key(e),
                e.display_name.to_lowercase(),
                e.id.as_raw(),
            )
        });
    } else {
        entries.sort_by_cached_key(|e| {
            (
                non_dir(e),
                Reverse(key(e)),
                e.display_name.to_lowercase(),
                e.id.as_raw(),
            )
        });
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

// Compile-time sanity check that FontWeight stays in scope: used
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
mod label_elision_tests {
    use crate::text::elide_label;

    #[test]
    fn moderately_long_labels_keep_their_ends() {
        assert_eq!(
            elide_label("abcdefghijklmnopqrstuvwxyz", 14).as_ref(),
            "abcdef…tuvwxyz"
        );
    }

    #[test]
    fn very_long_labels_keep_beginning_middle_and_end() {
        let label = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let elided = elide_label(label, 18);
        assert_eq!(elided.chars().filter(|ch| *ch == '…').count(), 2);
        assert!(elided.starts_with("abcde"));
        assert!(elided.ends_with("456789"));
        assert!(elided.contains("DEFGH"));
    }
}

#[cfg(test)]
mod sort_tests {
    use super::{SortColumn, resolve_ant_sort, sort_by_heat, sort_in_place};
    use ferail_core::{EntryKind, FileEntry, NodeId};

    fn entry(id: u64, name: &str, kind: EntryKind, size: u64, mtime: i64) -> FileEntry {
        FileEntry {
            id: NodeId::from_raw(id).unwrap(),
            name: name.into(),
            display_name: name.into(),
            name_has_hazards: false,
            kind,
            size,
            mtime_unix: mtime,
            display_size: ferail_core::empty_entry_text(),
            display_kind: ferail_core::empty_entry_text(),
            display_magic: ferail_core::empty_entry_text(),
            display_description: ferail_core::empty_entry_text(),
            details_loaded: false,
            is_quarantined: false,
            quarantine: None,
            hidden: false,
            created_unix: None,
            locked: false,
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

    /// Ant Trail sorting ranks folders by visit heat, hottest first on
    /// the default (descending) pick. Files carry no heat, so they stay
    /// below the folders in name order: a cold listing still reads
    /// like a normal one.
    #[test]
    fn ant_trail_sort_ranks_hot_folders_first() {
        let mut rows = vec![
            entry(1, "cold", EntryKind::Directory, 0, 0),
            entry(2, "zeta.txt", EntryKind::File, 0, 0),
            entry(3, "hot", EntryKind::Directory, 0, 0),
            entry(4, "alpha.txt", EntryKind::File, 0, 0),
            entry(5, "warm", EntryKind::Directory, 0, 0),
        ];
        let heat = |id: NodeId| match id.as_raw() {
            3 => 1.0,
            5 => 0.4,
            _ => 0.0,
        };
        sort_by_heat(&mut rows, heat, false);
        assert_eq!(ids(&rows), vec![3, 5, 1, 4, 2]);

        sort_by_heat(&mut rows, heat, true);
        assert_eq!(ids(&rows), vec![1, 5, 3, 4, 2]);
    }

    /// The bug this guards: picking Ant Trail did nothing: the list
    /// stayed in name order with the warm folder still in the middle,
    /// while the *same* pick worked after changing directory. The
    /// delegate was still carrying a Flat surface flag from a closed
    /// Include Subfolders view, and the flat sort path swallowed the
    /// pick. The rows had heat all along, so the rows are what decides.
    #[test]
    fn a_stale_surface_flag_cannot_swallow_an_ant_trail_pick() {
        // Rows with heat: the pick stands, direction included.
        assert_eq!(
            resolve_ant_sort(SortColumn::AntTrail, false, true),
            (SortColumn::AntTrail, false)
        );
        assert_eq!(
            resolve_ant_sort(SortColumn::AntTrail, true, true),
            (SortColumn::AntTrail, true)
        );
        // No heat anywhere (Flat): Name ascending, not the raw order.
        assert_eq!(
            resolve_ant_sort(SortColumn::AntTrail, false, false),
            (SortColumn::Name, true)
        );
        // Every other column is none of this function's business.
        for col in [
            SortColumn::Name,
            SortColumn::Size,
            SortColumn::Format,
            SortColumn::Modified,
            SortColumn::Path,
        ] {
            assert_eq!(resolve_ant_sort(col, false, false), (col, false));
            assert_eq!(resolve_ant_sort(col, true, true), (col, true));
        }
    }

    /// Every never-visited row has the same heat, so the name tiebreak
    /// is what the user actually sees: an Ant Trail sort of a folder
    /// nobody has browsed must not look shuffled.
    #[test]
    fn ant_trail_sort_falls_back_to_name_when_cold() {
        let mut rows = vec![
            entry(1, "delta", EntryKind::Directory, 0, 0),
            entry(2, "bravo", EntryKind::Directory, 0, 0),
            entry(3, "charlie", EntryKind::Directory, 0, 0),
        ];
        sort_by_heat(&mut rows, |_| 0.0, false);
        assert_eq!(ids(&rows), vec![2, 3, 1]);
    }
}

#[cfg(test)]
mod menu_targets_tests {
    use super::{
        Availability, MenuCapCounts, MenuTargets, TargetCap, avail_anchor_dir, avail_anchor_file,
        avail_any_quarantined, resolve_menu_targets, resolve_menu_targets_with_mode,
    };
    use ferail_core::{EntryKind, FileEntry, NodeId};
    use std::collections::HashSet;

    fn cap(is_quarantined: bool) -> TargetCap {
        TargetCap {
            kind: EntryKind::File,
            is_quarantined,
            is_archive: false,
        }
    }

    fn dir_cap() -> TargetCap {
        TargetCap {
            kind: EntryKind::Directory,
            is_quarantined: false,
            is_archive: false,
        }
    }

    fn entry(id: u64, name: &str, kind: EntryKind) -> FileEntry {
        FileEntry {
            id: NodeId::from_raw(id).unwrap(),
            name: name.into(),
            display_name: name.into(),
            name_has_hazards: false,
            kind,
            size: 0,
            mtime_unix: 0,
            display_size: ferail_core::empty_entry_text(),
            display_kind: ferail_core::empty_entry_text(),
            display_magic: ferail_core::empty_entry_text(),
            display_description: ferail_core::empty_entry_text(),
            details_loaded: false,
            is_quarantined: false,
            quarantine: None,
            hidden: false,
            created_unix: None,
            locked: false,
        }
    }

    fn rows() -> Vec<FileEntry> {
        vec![
            entry(1, "notes.txt", EntryKind::File),
            entry(2, "bundle.zip", EntryKind::File),
            entry(3, "sub", EntryKind::Directory),
        ]
    }

    fn selection(ids: &[u64]) -> HashSet<NodeId> {
        ids.iter()
            .map(|id| NodeId::from_raw(*id).unwrap())
            .collect()
    }

    fn targets(caps: Vec<TargetCap>) -> MenuTargets {
        MenuTargets {
            count: caps.len(),
            any_quarantined: caps.iter().any(|cap| cap.is_quarantined),
            any_archive: caps.iter().any(|cap| cap.is_archive),
            anchor: None,
        }
    }

    fn with_anchor(anchor: TargetCap) -> MenuTargets {
        MenuTargets {
            count: 1,
            any_quarantined: anchor.is_quarantined,
            any_archive: anchor.is_archive,
            anchor: Some(anchor),
        }
    }

    /// The regression this resolver exists for: right-clicking a row in a
    /// freshly loaded folder, nothing selected yet, nothing staged by any
    /// earlier click: must still resolve that row, so the gated commands
    /// (Rename, Copy Path, Extract, Open With) appear on the FIRST menu,
    /// not only on the second. Before, the caps arrived a right-click late
    /// and the first menu after a view switch was silently short.
    #[test]
    fn clicked_row_resolves_with_no_selection() {
        let t = resolve_menu_targets(&rows(), &selection(&[]), 1);
        assert!(t.is_single());
        assert!(t.any_archive());
        assert!(Availability::SingleOnly.allows(&t));
        assert!(Availability::When(avail_anchor_file).allows(&t));
    }

    /// Right-click on a row OUTSIDE the current selection targets just
    /// that row, matching the click that collapses the selection onto it
    /// (spec §2.4), so the menu and the later handler agree.
    #[test]
    fn click_outside_selection_targets_only_that_row() {
        let t = resolve_menu_targets(&rows(), &selection(&[1, 3]), 1);
        assert!(t.is_single());
        assert!(t.any_archive());
    }

    /// Right-click INSIDE the selection keeps the whole set, in visible
    /// order, with the clicked row as the anchor.
    #[test]
    fn click_inside_selection_targets_whole_set() {
        let t = resolve_menu_targets(&rows(), &selection(&[1, 3]), 2);
        assert_eq!(t.len(), 2);
        assert!(t.is_multi());
        assert!(!Availability::SingleOnly.allows(&t));
        // Anchor is the clicked folder, so folder-anchored commands show
        // even though the set also holds a file.
        assert!(Availability::When(avail_anchor_dir).allows(&t));
    }

    /// A row index past the end (a menu built against a list that shrank
    /// under it) resolves to nothing rather than the wrong row.
    #[test]
    fn out_of_range_row_resolves_empty() {
        let t = resolve_menu_targets(&rows(), &selection(&[1]), 9);
        assert!(t.is_empty());
        assert!(t.anchor.is_none());
    }

    /// The reported bug: a mixed selection (one quarantined, one clean)
    /// must offer Clear Quarantine no matter which row was right-clicked.
    /// `any` is the gate, so the order of the set is irrelevant: that is
    /// exactly what decouples the menu from "the file selected last".
    #[test]
    fn any_shows_on_mixed_selection_regardless_of_order() {
        let quarantined_first = targets(vec![cap(true), cap(false)]);
        let clean_first = targets(vec![cap(false), cap(true)]);
        assert!(quarantined_first.any_quarantined());
        assert!(clean_first.any_quarantined());
    }

    /// No quarantined target anywhere in the set → the bulk command is
    /// hidden. A single clean row behaves the same as a clean multi-set.
    #[test]
    fn any_hidden_when_no_target_qualifies() {
        assert!(!targets(vec![cap(false), cap(false)]).any_quarantined());
        assert!(!targets(vec![]).any_quarantined());
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

    #[test]
    fn symbolic_select_all_uses_constant_size_summary() {
        let rows = rows();
        let counts = MenuCapCounts::from_entries(&rows);
        let targets = resolve_menu_targets_with_mode(&rows, &selection(&[]), true, 0, counts);
        assert_eq!(targets.len(), rows.len());
        assert!(targets.any_archive());
        assert!(std::mem::size_of::<MenuTargets>() <= 64);
    }

    /// SingleOnly (Copy Path, Rename, Open With) shows for exactly one
    /// target and hides for zero or many: the Type-B rule.
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
        assert!(quar.allows(&with_anchor(dir_cap())));

        assert!(Availability::When(avail_anchor_dir).allows(&with_anchor(dir_cap())));
        assert!(!Availability::When(avail_anchor_dir).allows(&with_anchor(cap(false))));
        assert!(Availability::When(avail_anchor_file).allows(&with_anchor(cap(false))));
        assert!(!Availability::When(avail_anchor_file).allows(&with_anchor(dir_cap())));
        // No anchor → neither anchor rule fires.
        assert!(!Availability::When(avail_anchor_dir).allows(&targets(vec![])));
        assert!(!Availability::When(avail_anchor_file).allows(&targets(vec![])));
    }
}
