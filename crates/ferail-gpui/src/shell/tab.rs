use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicBool};

use ferail_core::{EnumerationError, FileEntry, NodeId, navigation::NavigationState};
use ferail_fs_native::perceptual::{PerceptualSignature, SimilarityCriteria};
use ferail_fs_native::{DupeMode, PerceptualThumbnail};
use gpui::{
    AppContext, Entity, FocusHandle, Pixels, RenderImage, SharedString, Subscription,
    UniformListScrollHandle, px,
};
use gpui_component::input::InputState;

use crate::file_list::FileListDelegate;
use crate::grid::ViewMode;
use crate::multi_table::{TableState, VirtualListScrollHandle};
use crate::tasks::TaskId;

/// One entry in a tab's back/forward history. Spec §2.6 history
/// exception: a back-navigation should restore the selection state
/// that tab had for the destination directory, reconciled against
/// the freshly streamed model on `Done`. Each push records the
/// selection that was live at the moment of leaving.
#[derive(Clone)]
pub struct HistoryEntry {
    pub path: PathBuf,
    pub selection: HashSet<NodeId>,
    pub anchor: Option<NodeId>,
    pub lead: Option<NodeId>,
}

impl HistoryEntry {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            selection: HashSet::new(),
            anchor: None,
            lead: None,
        }
    }
}

/// State for a tab-local tool result surface. A tool result replaces the
/// normal directory listing inside the active tab, but keeps `current_dir`
/// as the root so navigation, Back, preview, and context-menu plumbing can
/// stay shared with ordinary file-list rows.
#[derive(Clone)]
pub struct ToolResultSurface {
    pub mode: ToolResultMode,
}

impl ToolResultSurface {
    pub fn search(needle: String, root: PathBuf, engine_label: &'static str) -> Self {
        Self {
            mode: ToolResultMode::Search(SearchMode {
                needle,
                root,
                engine_label,
            }),
        }
    }

    pub fn flat(root: PathBuf) -> Self {
        Self {
            mode: ToolResultMode::Flat(FlatMode {
                root,
                progress: ferail_fs_native::SearchStats::default(),
                complete: false,
            }),
        }
    }

    pub fn duplicates(
        root: PathBuf,
        presentation: crate::feature_settings::DupePresentation,
        mode: DupeMode,
    ) -> Self {
        Self {
            mode: ToolResultMode::Duplicates(DupeViewMode {
                root,
                groups: 0,
                wasted_bytes: 0,
                presentation,
                mode,
                similarity_criteria: SimilarityCriteria::RECOMMENDED,
                progress: ferail_fs_native::DupeStats::default(),
            }),
        }
    }

    pub fn disk_usage(root: PathBuf, view: Entity<crate::disk_usage::DiskUsageView>) -> Self {
        Self {
            mode: ToolResultMode::DiskUsage(DiskUsageMode { root, view }),
        }
    }

    pub fn archive(archive: PathBuf, view: Entity<crate::archive::ArchiveView>) -> Self {
        Self {
            mode: ToolResultMode::Archive(ArchiveMode { archive, view }),
        }
    }

    pub fn archive_mode(&self) -> Option<&ArchiveMode> {
        match &self.mode {
            ToolResultMode::Archive(a) => Some(a),
            _ => None,
        }
    }

    pub fn handle_host_event<C: AppContext>(
        &self,
        event: crate::tool_results::ToolHostEvent,
        cx: &mut C,
    ) {
        match &self.mode {
            ToolResultMode::DiskUsage(du) => {
                du.view
                    .update(cx, |view, cx| view.handle_host_event(event, cx));
            }
            ToolResultMode::Archive(a) => {
                a.view
                    .update(cx, |view, cx| view.handle_host_event(event, cx));
            }
            // Search and duplicates render through the tab and have no
            // windowed form, so host changes mean nothing to them.
            ToolResultMode::Search(_) | ToolResultMode::Flat(_) | ToolResultMode::Duplicates(_) => {
            }
        }
    }

    pub fn search_mode(&self) -> Option<&SearchMode> {
        match &self.mode {
            ToolResultMode::Search(search) => Some(search),
            _ => None,
        }
    }

    pub fn search_mode_mut(&mut self) -> Option<&mut SearchMode> {
        match &mut self.mode {
            ToolResultMode::Search(search) => Some(search),
            _ => None,
        }
    }

    pub fn flat_mode(&self) -> Option<&FlatMode> {
        match &self.mode {
            ToolResultMode::Flat(flat) => Some(flat),
            _ => None,
        }
    }

    pub fn flat_mode_mut(&mut self) -> Option<&mut FlatMode> {
        match &mut self.mode {
            ToolResultMode::Flat(flat) => Some(flat),
            _ => None,
        }
    }

    pub fn dupe_mode(&self) -> Option<&DupeViewMode> {
        match &self.mode {
            ToolResultMode::Duplicates(dupes) => Some(dupes),
            _ => None,
        }
    }

    pub fn dupe_mode_mut(&mut self) -> Option<&mut DupeViewMode> {
        match &mut self.mode {
            ToolResultMode::Duplicates(dupes) => Some(dupes),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub enum ToolResultMode {
    Search(SearchMode),
    Flat(FlatMode),
    Duplicates(DupeViewMode),
    DiskUsage(DiskUsageMode),
    Archive(ArchiveMode),
}

/// A recursive, files-only snapshot of a directory tree. The rows and their
/// path arena are owned by the tab surface and are discarded together.
#[derive(Clone, Debug)]
pub struct FlatMode {
    pub root: PathBuf,
    pub progress: ferail_fs_native::SearchStats,
    pub complete: bool,
}

#[derive(Clone)]
pub struct DiskUsageMode {
    /// Root the disk-usage scan was launched from.
    pub root: PathBuf,
    /// Stateful treemap/results view hosted inside the tab. The same view
    /// type is still usable in a standalone window.
    pub view: Entity<crate::disk_usage::DiskUsageView>,
}

/// State for the archive workbench surface. The tab keeps its `current_dir`
/// (the folder the archive lives in) for navigation and Back; the pane body
/// is the archive contents view.
#[derive(Clone)]
pub struct ArchiveMode {
    /// The archive file being browsed.
    pub archive: PathBuf,
    /// The embedded contents/extraction view.
    pub view: Entity<crate::archive::ArchiveView>,
}

/// State for a search result surface. The tab still has a
/// `current_dir` (the search root, used for navigation and Back), but the
/// file list is the result stream for `needle`.
#[derive(Clone, Debug)]
pub struct SearchMode {
    /// The query the user entered.
    pub needle: String,
    /// Root the search was launched from (the tab's directory at launch).
    pub root: PathBuf,
    /// Which engine produced these results, for the breadcrumb label
    /// ("Spotlight" / "Subtree").
    pub engine_label: &'static str,
}

/// State for a duplicate-finder result surface. Like [`SearchMode`]
/// the tab keeps its `current_dir` (the scan root); the file list holds
/// duplicate group members as adjacent rows.
#[derive(Clone, Debug)]
pub struct DupeViewMode {
    /// Root the scan was launched from.
    pub root: PathBuf,
    /// Confirmed duplicate groups seen so far.
    pub groups: usize,
    /// Reclaimable bytes. Exact groups use `bytes_each * (distinct occupants
    /// - 1)`; Similar groups sum every non-keeper member's own byte size.
    pub wasted_bytes: u64,
    /// Presentation resolved once at scan launch (from `DupeConfig`) and
    /// cached here so the per-frame render never reads settings off disk.
    /// Grouped → adjacent table rows; Panel → the dedicated card view.
    pub presentation: crate::feature_settings::DupePresentation,
    /// Exact byte duplicates or perceptually similar images.
    pub mode: DupeMode,
    /// Live, session-only thresholds for Similar Images regrouping.
    pub similarity_criteria: SimilarityCriteria,
    /// Latest phase/count snapshot from the worker for the panel's progress
    /// explanation. No paths or filenames are included.
    pub progress: ferail_fs_native::DupeStats,
}

/// Compact scan-local source used to rebuild Similar Images groups when the
/// criteria sliders move. Paths, hashes, and previews are dropped with the tab
/// and are never persisted or registered process-wide.
#[derive(Clone)]
pub struct SimilarImageIndexView {
    pub node: NodeId,
    pub path: PathBuf,
    pub mtime_unix: i64,
    pub bytes: u64,
    pub file_id: Option<(u64, u64)>,
    pub clone_key: Option<(u64, i64)>,
    pub signature: PerceptualSignature,
    pub thumbnail: Option<Arc<RenderImage>>,
}

/// Similar-image-only display data. The decoded preview begins as raw RGBA
/// from the worker and is converted once on the UI thread. Both forms live
/// only on this result surface and are dropped when it closes.
#[derive(Clone)]
pub struct SimilarImageView {
    pub width: u32,
    pub height: u32,
    pub dhash_distance: u32,
    pub phash_distance: u32,
    pub is_best: bool,
    pub raw_thumbnail: Option<PerceptualThumbnail>,
    pub thumbnail: Option<Arc<RenderImage>>,
}

/// One member of a retained duplicate group (the backing model the
/// dedicated panel renders and the group actions operate on). Mirrors
/// the worker's `DupeMember`, minus the bits only the funnel needs.
#[derive(Clone)]
pub struct DupeGroupMember {
    pub node: NodeId,
    pub path: PathBuf,
    /// Last-modified time (Unix seconds) — drives "keep newest".
    pub mtime_unix: i64,
    /// Logical bytes of this member. Similar-image members can differ.
    pub bytes: u64,
    /// Shares an inode with an earlier member (hard link): reclaims
    /// nothing.
    pub is_hardlink: bool,
    /// Shares physical storage with an earlier member via an APFS clone:
    /// reclaims nothing.
    pub is_clone: bool,
    pub image: Option<SimilarImageView>,
}

impl DupeGroupMember {
    /// True when removing this member frees no space (hard link or clone
    /// of an earlier member).
    pub fn shares_storage(&self) -> bool {
        self.is_hardlink || self.is_clone
    }
}

/// A retained duplicate group: the panel's backing model. The grouped-
/// rows presentation never builds these (it streams straight into the
/// table); only [`DupePresentation::Panel`] populates and renders them.
/// Kept on [`Tab`] (not in `DupeViewMode`, which is `.clone()`d every
/// frame for the breadcrumb) so the per-frame breadcrumb path never
/// copies the whole group list.
#[derive(Clone)]
pub struct DupeGroupView {
    /// 1-based group number, matching the row tags.
    pub group_no: usize,
    /// BLAKE3 hex of the content (empty in paranoid mode).
    pub full_hash: String,
    pub mode: DupeMode,
    /// Logical bytes of each copy.
    pub bytes_each: u64,
    pub members: Vec<DupeGroupMember>,
    /// Card expand/collapse state in the panel.
    pub expanded: bool,
    /// User-picked keeper (per-row "keep this" radio). Similar groups begin
    /// with their ranked best copy; Exact actions can fall back to newest or
    /// first as appropriate.
    pub keeper: Option<NodeId>,
}

impl DupeGroupView {
    /// Distinct on-disk occupants: members that own their storage.
    pub fn distinct_occupants(&self) -> usize {
        self.members.iter().filter(|m| !m.shares_storage()).count()
    }

    /// Reclaimable bytes if the group is reduced to its keeper. Similar image
    /// members have distinct sizes, so the value changes with that choice.
    pub fn reclaimable_bytes(&self) -> u64 {
        if self.mode == DupeMode::Similar {
            let keeper = self
                .keeper
                .or_else(|| self.best())
                .or_else(|| self.members.first().map(|member| member.node));
            return self
                .members
                .iter()
                .filter(|member| Some(member.node) != keeper)
                .map(|member| member.bytes)
                .sum();
        }
        let distinct = self.distinct_occupants() as u64;
        self.bytes_each.saturating_mul(distinct.saturating_sub(1))
    }

    /// The worker ranks the best similar-image keeper first, and marks it
    /// explicitly so seeded/test state remains order-independent.
    pub fn best(&self) -> Option<NodeId> {
        self.members
            .iter()
            .find(|member| member.image.as_ref().is_some_and(|image| image.is_best))
            .map(|member| member.node)
    }

    /// The newest member by mtime (ties broken by original order). The
    /// keeper "keep newest" selects.
    pub fn newest(&self) -> Option<NodeId> {
        self.members
            .iter()
            .enumerate()
            .max_by_key(|(i, m)| (m.mtime_unix, std::cmp::Reverse(*i)))
            .map(|(_, m)| m.node)
    }

    /// Nodes to trash if `keeper` is the survivor: every other member.
    pub fn victims_for_keeper(&self, keeper: NodeId) -> Vec<NodeId> {
        self.members
            .iter()
            .filter(|m| m.node != keeper)
            .map(|m| m.node)
            .collect()
    }

    /// Victims for "keep newest": everything but the newest member.
    pub fn victims_keep_newest(&self) -> Vec<NodeId> {
        match self.newest() {
            Some(keeper) => self.victims_for_keeper(keeper),
            None => Vec::new(),
        }
    }

    /// Victims for "select all but one": everything but the user-picked
    /// keeper, or the first member when none is picked.
    pub fn victims_all_but_one(&self) -> Vec<NodeId> {
        let keeper = self
            .keeper
            .or_else(|| self.best())
            .or_else(|| self.members.first().map(|m| m.node));
        match keeper {
            Some(keeper) => self.victims_for_keeper(keeper),
            None => Vec::new(),
        }
    }
}

/// Process-local stable identifier for a tab. Minted from
/// `ProcessState::mint_tab_id`. Survives reorder; survives tear-off
/// to a different window (Phase F). Cheaper than path equality for
/// "is this still the same tab" checks across the streaming pipeline.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TabId(pub u64);

/// Live rubber-band (marquee) drag state for the icon grid. A press on
/// empty grid background starts one; dragging sweeps a selection
/// rectangle over the cells. Coordinates are window-space (raw mouse
/// event positions); the render maps them into the grid's content space
/// via the cached `grid_pane_origin` + scroll offset. Only present while
/// a marquee is in flight.
pub struct Marquee {
    /// Window-space position where the press began.
    pub start: gpui::Point<Pixels>,
    /// Window-space position of the pointer now.
    pub current: gpui::Point<Pixels>,
    /// Shift / Cmd held at press — union the swept set into `base`
    /// instead of replacing the selection.
    pub additive: bool,
    /// Selection snapshot captured at press, unioned with the swept
    /// hits when `additive`. Empty for a plain (replacing) marquee.
    pub base: HashSet<NodeId>,
    /// Cells intersected by the previous rectangle. Updating this delta is
    /// proportional to the swept area instead of cloning the entire base
    /// selection on every mouse packet.
    pub hits: HashSet<NodeId>,
    /// Whether the first selection update has replaced the pre-gesture state
    /// with `base`. A plain background click must not clear until it actually
    /// crosses the movement threshold.
    pub applied: bool,
    /// True once the pointer has moved past the start threshold — until
    /// then the gesture is still a candidate plain background click
    /// (which clears the selection on release).
    pub moved: bool,
}

pub struct Tab {
    /// Process-local stable identity. Stays the same through
    /// reorder, history navigation, and (later) tear-off.
    pub id: TabId,
    /// Authoritative location identity. `current_dir` remains as a
    /// display/job snapshot during the migration, but navigation logic
    /// moves through this NodeId state first.
    pub nav: NavigationState,
    pub current_dir: PathBuf,
    /// Present only for pathless provider-owned locations (This PC, Recycle
    /// Bin, MTP, etc.). Ordinary filesystem and Flat View tabs keep this
    /// `None`; opaque provider identities therefore never enter their rows.
    pub platform_namespace: Option<crate::platform_namespace::PlatformNamespaceSession>,
    pub history: Vec<HistoryEntry>,
    pub history_index: usize,
    /// Multi-selection set keyed by `NodeId`. Per spec §2.2 this is
    /// in-memory interaction state, not persisted truth; reconciled
    /// against the model on streaming `Done` and on every model
    /// change. Empty is a valid common state.
    pub selection: HashSet<NodeId>,
    /// Flat listings can contain millions of rows. In that surface a
    /// Cmd+A selection is represented as "all rows except these ids" so
    /// selecting everything stays O(1) memory instead of building a
    /// multi-million-entry `HashSet`. Ordinary listings keep the sparse
    /// representation above.
    pub selection_all: bool,
    /// Anchor — fixed end of a Shift-range; set on plain click or
    /// the first Cmd-click into an empty selection. (Spec §2.3.)
    pub anchor: Option<NodeId>,
    /// Lead / cursor — moving end of a range, target of keyboard
    /// navigation, focus-ring row. Mirrored to the underlying
    /// `TableState::selected_row` so the primitive's native focus
    /// overlay marks it.
    pub lead: Option<NodeId>,
    /// Spec §2.6 filter holding set: NodeIds that were in
    /// `selection` but got filtered out of the visible model. When
    /// the filter loosens or clears, members whose rows reappear
    /// move back to `selection`. Dropped entirely on navigation.
    pub filtered_out: HashSet<NodeId>,
    /// Spec §2.6 live Shift-range marker: when true the current
    /// selection is the anchor→lead inclusive span and should be
    /// recomputed against the model on every batch arrival, so
    /// rows streaming in between the endpoints join the selection
    /// automatically. Set by Shift-click / Cmd+Shift-click / any
    /// Shift-extend keyboard nav; cleared on any non-range gesture
    /// (plain click, Cmd-click, plain kbd nav, Cmd+A, Esc, sort).
    pub range_live: bool,
    /// gpui-component's virtualized Table state for *this* tab.
    /// Per-tab so tab-switching doesn't re-enumerate — inactive
    /// tabs' enumerations keep streaming into their own table and
    /// the result is ready when the user switches back.
    pub table: Entity<TableState<FileListDelegate>>,
    /// File-pane view mode for this tab (list table vs icon grid).
    /// Per-tab interaction state (Finder-style per-folder), seeded from
    /// the persisted default on creation.
    pub view_mode: ViewMode,
    /// Scroll handle for this tab's icon-grid `uniform_list`. Per-tab so
    /// switching tabs preserves the grid scroll position.
    pub grid_scroll: UniformListScrollHandle,
    /// Last grid viewport submitted to the thumbnail/icon warmers:
    /// `(load generation, entry start, entry end, thumbnail px, icon px)`.
    /// The uniform-list closure runs on every repaint, including every drag
    /// frame; this key keeps an unchanged viewport from allocating and
    /// hashing the same paths again.
    pub grid_warm_key: Option<(u64, usize, usize, u32, u32)>,
    /// Last measured file-pane content width (logical px), cached for
    /// the grid's columns-per-row math — `uniform_list` needs the row
    /// count before layout, so we derive columns from the previous
    /// frame's width. One-frame stale on resize is invisible.
    pub grid_pane_width: Pixels,
    /// Focus handle backing the grid's `"FerailGrid"` key context, so
    /// the grid receives arrow-key navigation independently of the
    /// table's own key context.
    pub grid_focus: FocusHandle,
    /// Window-space origin of the grid pane's content box, captured by
    /// the same measuring `canvas` that tracks `grid_pane_width`. Used to
    /// map raw mouse-event positions into grid content space for marquee
    /// hit-testing and rubber-band painting.
    pub grid_pane_origin: gpui::Point<Pixels>,
    /// Live rubber-band drag over the icon grid, if any. `Some` only
    /// between a background press and its release.
    pub marquee: Option<Marquee>,
    /// Monotonic generation for *this* tab's directory loads.
    /// Background enumeration results apply only if their generation
    /// still matches the tab they were spawned for.
    pub load_generation: u64,
    /// Cooperative cancel flag for the active directory enumeration
    /// on this tab. Replaced on every navigation/filter/show-hidden
    /// reload that targets this tab.
    pub load_cancel: Option<Arc<AtomicBool>>,
    /// Bounded WSL `readlink -f` request for explicit link activation or a
    /// failed UNC directory load. Kept separately from enumeration so a new
    /// navigation cancels both.
    pub wsl_resolve_cancel: Option<Arc<AtomicBool>>,
    /// Explicit Windows `.lnk` activation. Separate from WSL link resolution
    /// so either provider can be canceled independently on navigation.
    pub shortcut_resolve_cancel: Option<Arc<AtomicBool>>,
    /// Cooperative cancel flag for this tab's in-flight folder-size
    /// pass (`folder_sizes::start`). Flipped alongside `load_cancel`
    /// on every navigation/reload so a deep `recursive_size` walk
    /// stops at its next dirent instead of finishing for a listing
    /// the user already left.
    pub folder_size_cancel: Option<Arc<AtomicBool>>,
    /// Cooperative cancel flag for this tab's in-flight magic /
    /// quarantine prefetch (`prefetch::start`). Same lifecycle as
    /// `folder_size_cancel` — flipped on navigation/reload so a
    /// superseded pass stops sniffing instead of finishing a listing
    /// the user already left.
    pub prefetch_cancel: Option<Arc<AtomicBool>>,
    /// Task-registry row for this tab's in-flight enumeration.
    pub load_task: Option<TaskId>,
    /// True after a navigation starts and before the first batch
    /// lands for this tab. Keeps the old rows visible during the
    /// gap instead of flashing empty.
    pub load_pending_first_batch: bool,
    /// When set, the magic/description prefetch that follows this
    /// load ignores the metadata-DB cache and re-sniffs every row
    /// from disk. Flipped on by the Refresh command so a user can
    /// pick up content whose *derived* data went stale without the
    /// file's mtime changing (e.g. after the sniffer's logic itself
    /// changed). Reset to `false` at the start of every load and
    /// consumed by `finish_directory_load_in_tab`.
    pub force_resniff: bool,
    /// When set, the folder-size pass that follows this load ignores the
    /// `folder_sizes` cache and re-walks every directory row. Armed only
    /// by the Refresh command — it is the user's explicit "measure this
    /// again", and the one gesture that should pay for a full re-walk.
    /// Everything else (navigation, returning to a folder, the window
    /// coming forward) rides the cache; deep external changes are caught
    /// by the TTL and by watcher-driven ancestor invalidation instead.
    /// Reset to `false` at the start of every load and consumed by
    /// `finish_directory_load_in_tab`, so a superseded Refresh doesn't
    /// leak a forced walk into the load that replaced it.
    pub force_folder_sizes: bool,
    /// Off-screen accumulator for an *in-place reload* (Refresh, Esc
    /// clear-filter, show-hidden toggle, watcher reload — any load
    /// that re-reads the directory already on screen). `Some` for the
    /// duration of such a load: batches accumulate here instead of
    /// touching the live table, and the complete listing is swapped in
    /// atomically on `Done`. The old rows stay put until then, so a
    /// refresh never collapses the list to the first batch and streams
    /// back — i.e. no flicker. `None` for fresh navigation, which keeps
    /// the progressive streaming reveal.
    pub(crate) load_staging: Option<super::loading::LoadBatch>,
    /// `Some` while this tab is showing a tool result surface rather than
    /// a directory listing (docs/features/TOOL_RESULTS.md). The tab stays
    /// rooted at `current_dir` for navigation, but the file-list body is
    /// fed by the tool (search, duplicates, or Disk Usage).
    /// Watcher reloads are suppressed and the shared result header explains
    /// the tool/root. Cleared by `navigate` and by closing the result.
    pub tool_result: Option<ToolResultSurface>,
    /// Retained duplicate groups backing the dedicated panel
    /// ([`DupePresentation::Panel`]). Empty in grouped-rows mode and whenever
    /// the active tool result is not Duplicates. Populated alongside the table
    /// rows in `apply_dupe_batch_in_tab`; the panel render borrows it without
    /// copying.
    pub dupe_groups: Vec<DupeGroupView>,
    /// Widest-match Similar Images candidates retained for instant, no-I/O
    /// regrouping as the two criteria sliders move.
    pub similar_image_index: Arc<Vec<SimilarImageIndexView>>,
    /// Invalidates background regroup results superseded by a newer slider
    /// value or scan.
    pub similar_regroup_generation: u64,
    /// Member last focused in the duplicate-card panel. Similar Images uses
    /// this as the starting item when Space opens the current group in the
    /// full-size viewer. It is presentation state only and never persisted.
    pub dupe_panel_focus: Option<(usize, NodeId)>,
    /// Scroll handle for the dedicated duplicate-card panel. Per-tab so
    /// switching tabs preserves the result-list position, and so the
    /// visible scrollbar can drive the virtual list.
    pub dupe_panel_scroll: VirtualListScrollHandle,
    /// `Some(err)` when this tab's last enumerate returned an OS
    /// error (most commonly macOS TCC denial). Drives an empty-
    /// state in the file pane when the tab is active.
    pub last_error: Option<EnumerationError>,
    /// Cached free-space for this tab's volume, refreshed off-thread
    /// on load completion and on volume-watch events. Render reads
    /// this cache only — the underlying NSURL/statfs query can do a
    /// remote round-trip on network mounts (Prime Directive).
    pub volume_free_bytes: Option<u64>,
    /// Cached display name of this tab's volume; same lifecycle as
    /// `volume_free_bytes`.
    pub volume_name: Option<SharedString>,
    /// Cached read-only mount state of this tab's volume; same
    /// lifecycle as `volume_free_bytes`. The status bar swaps its
    /// free-space label for "is read-only" when set — "0 B free" on
    /// a CD is true but the wrong message.
    pub volume_read_only: bool,
    /// Count + bytes of the hidden entries the last completed load
    /// skipped (show-hidden off). Written by
    /// `finish_directory_load_in_tab`, zeroed when a load starts;
    /// render reads this cache only. Drives the status bar's passive
    /// "N hidden · X" chip.
    pub hidden_summary: super::loading::HiddenSummary,
    /// Count + bytes of the entries the *filter field* excluded from
    /// the last completed load. Same lifecycle as `hidden_summary`, and
    /// zero whenever the field is empty. Drives the status bar's
    /// "N filtered out · X" chip, so the item count and size stay
    /// honest about the whole folder while a filter narrows the view.
    pub filter_summary: super::loading::FilterSummary,
    /// Screenshot-driver row index queued for selection once a
    /// streaming batch lands for this tab. Cleared on apply or on
    /// navigation. Internal/CLI use only.
    pub pending_select_row: Option<usize>,
    /// Same as `pending_select_row` but for the multi-row
    /// screenshot seed (`--select-rows`).
    pub pending_select_rows: Vec<usize>,
    /// Leaf names queued for selection once this tab's rows land. Names
    /// rather than row indices because the queuing happens *before* the
    /// directory is enumerated — the extract confirmation navigates to the
    /// destination and then wants to select what it just wrote there, and
    /// those rows do not exist at click time. Cleared on apply.
    pub pending_select_names: Vec<String>,
    /// Live filter text for *this* tab. Spec §3.1: each tab owns its
    /// own filter. Switching tabs swaps the visible filter input
    /// to this tab's `filter_input`; typing only re-enumerates
    /// this tab.
    pub filter_text: String,
    /// gpui-component `Input` state for the filter field, scoped to
    /// this tab. The title-bar render mounts the active tab's input
    /// directly so cursor / focus / value are naturally per-tab.
    pub filter_input: Entity<InputState>,
    /// Subscription handle for this tab's table-event bridge into
    /// `Shell`. Owned by the tab so dropping the tab drops the
    /// subscription — important for Phase D's tab-close path.
    #[allow(dead_code)]
    pub(crate) _table_subscription: Subscription,
    /// Subscription for this tab's filter `Input`. Same lifecycle as
    /// `_table_subscription`.
    #[allow(dead_code)]
    pub(crate) _filter_subscription: Subscription,
}

impl Tab {
    /// Whether `id` belongs to the logical selection.  When
    /// `selection_all` is set, `selection` stores the exceptions.
    pub fn is_selected(&self, id: NodeId) -> bool {
        if self.selection_all {
            !self.selection.contains(&id)
        } else {
            self.selection.contains(&id)
        }
    }

    pub fn selection_is_empty(&self, visible_count: usize) -> bool {
        if self.selection_all {
            visible_count <= self.selection.len()
        } else {
            self.selection.is_empty()
        }
    }

    pub fn selection_count(&self, visible_count: usize) -> usize {
        if self.selection_all {
            visible_count.saturating_sub(self.selection.len())
        } else {
            self.selection.len()
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection_all = false;
        self.selection.clear();
    }

    /// Internal constructor. Use `Shell::make_tab` from view code so
    /// the new tab is correctly wired to a per-tab `TableState`
    /// subscription and filter `Input`.
    #[allow(clippy::too_many_arguments)]
    pub fn new_internal(
        id: TabId,
        at: PathBuf,
        node_id: NodeId,
        table: Entity<TableState<FileListDelegate>>,
        table_subscription: Subscription,
        filter_input: Entity<InputState>,
        filter_subscription: Subscription,
        view_mode: ViewMode,
        grid_focus: FocusHandle,
    ) -> Self {
        Self {
            id,
            nav: NavigationState::new(node_id),
            current_dir: at.clone(),
            platform_namespace: None,
            history: vec![HistoryEntry::new(at)],
            history_index: 0,
            selection: HashSet::new(),
            selection_all: false,
            anchor: None,
            lead: None,
            filtered_out: HashSet::new(),
            range_live: false,
            table,
            view_mode,
            grid_scroll: UniformListScrollHandle::new(),
            grid_warm_key: None,
            grid_pane_width: px(0.0),
            grid_focus,
            grid_pane_origin: gpui::Point::default(),
            marquee: None,
            load_generation: 0,
            load_cancel: None,
            wsl_resolve_cancel: None,
            shortcut_resolve_cancel: None,
            folder_size_cancel: None,
            prefetch_cancel: None,
            load_task: None,
            load_pending_first_batch: false,
            force_resniff: false,
            force_folder_sizes: false,
            load_staging: None,
            tool_result: None,
            dupe_groups: Vec::new(),
            similar_image_index: Arc::new(Vec::new()),
            similar_regroup_generation: 0,
            dupe_panel_focus: None,
            dupe_panel_scroll: VirtualListScrollHandle::new(),
            last_error: None,
            volume_free_bytes: None,
            volume_name: None,
            volume_read_only: false,
            hidden_summary: Default::default(),
            filter_summary: Default::default(),
            pending_select_row: None,
            pending_select_rows: Vec::new(),
            pending_select_names: Vec::new(),
            filter_text: String::new(),
            filter_input,
            _table_subscription: table_subscription,
            _filter_subscription: filter_subscription,
        }
    }

    /// Row index of the lead within `entries`, or `None` if no lead
    /// is set or the lead is not currently in the view's model.
    /// Used by every site that previously read `Tab::selected` as a
    /// row index — preview pane, status bar, screenshot driver,
    /// activate_row's keyboard fallback.
    pub fn lead_row(&self, entries: &[FileEntry]) -> Option<usize> {
        let lead = self.lead?;
        entries.iter().position(|e| e.id == lead)
    }

    /// Short label for the tabstrip. Last path component, or "/" for
    /// the filesystem root.
    pub fn label(&self) -> String {
        self.current_dir
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| self.current_dir.to_string_lossy().into_owned())
    }

    /// Snapshot the parts of this tab the closed-tab stack needs to
    /// reopen it later (Phase D, spec §3.3 "Reopen closed tab").
    ///
    /// Captures directory, history, filter, and selection. Drops
    /// load-in-flight bookkeeping, the gpui-component `TableState`,
    /// and the filter `Input` entity — those are remade fresh on
    /// reopen, and the reopen path re-issues a streaming enumeration
    /// like any new tab. Sort restore is deferred (TableState's
    /// current sort isn't exposed on the public surface today); spec
    /// acceptance lists sort under "restore on reopen" but it's a
    /// follow-on polish item.
    pub fn snapshot_for_close(&self) -> ClosedTab {
        ClosedTab {
            current_dir: self.current_dir.clone(),
            history: self.history.clone(),
            history_index: self.history_index,
            filter_text: self.filter_text.clone(),
            selection: if self.selection_all {
                HashSet::new()
            } else {
                self.selection.clone()
            },
            anchor: self.anchor,
            lead: self.lead,
        }
    }
}

/// Pure index math for within-strip tab drag-reorder (spec §3.3).
/// Gap positions number `0..=len`: gap 0 before the first tab, gap
/// `len` after the last. Returns the index to `insert` at AFTER
/// `remove(from_idx)`, or `None` when the drop is invalid or a no-op
/// (dropping into the gap on either side of the dragged tab itself).
///
/// Extracted from `Shell::reorder_tab` so the arithmetic is testable
/// without a gpui harness — the active-index bookkeeping stays in
/// the Shell method.
pub fn reorder_insert_index(from_idx: usize, to_pos: usize, len: usize) -> Option<usize> {
    if from_idx >= len || to_pos > len {
        return None;
    }
    if to_pos == from_idx || to_pos == from_idx + 1 {
        return None;
    }
    // After removal, indices > from_idx shift down by one: a gap to
    // the RIGHT of the dragged tab maps to `to_pos - 1` in the
    // post-remove list; a gap to the left is unchanged.
    Some(if from_idx < to_pos {
        to_pos - 1
    } else {
        to_pos
    })
}

/// Map a drop ON a tab chip (rather than into a between-chip gap) to
/// the gap position that puts the dragged tab at the target chip's
/// current slot: dragging rightward inserts after the target, leftward
/// inserts before it — in both cases the dragged tab ends up exactly
/// where the target chip was. Dropping a chip on itself maps to its
/// own gap, which `reorder_insert_index` rejects as a no-op.
pub fn chip_drop_gap_index(from_idx: usize, chip_idx: usize) -> usize {
    if from_idx < chip_idx {
        chip_idx + 1
    } else {
        chip_idx
    }
}

#[cfg(test)]
mod dupe_group_tests {
    use super::{DupeGroupMember, DupeGroupView, SimilarImageView};
    use ferail_core::NodeId;
    use ferail_fs_native::DupeMode;
    use std::path::PathBuf;

    fn member(id: u64, mtime: i64) -> DupeGroupMember {
        DupeGroupMember {
            node: NodeId::from(id),
            path: PathBuf::from(format!("/f/{id}")),
            mtime_unix: mtime,
            bytes: 1000,
            is_hardlink: false,
            is_clone: false,
            image: None,
        }
    }

    fn group(members: Vec<DupeGroupMember>) -> DupeGroupView {
        DupeGroupView {
            group_no: 1,
            full_hash: "deadbeef".into(),
            mode: DupeMode::Exact,
            bytes_each: 1000,
            members,
            expanded: true,
            keeper: None,
        }
    }

    #[test]
    fn similar_reclaim_changes_with_the_keeper_size() {
        let mut members = vec![member(1, 100), member(2, 200), member(3, 300)];
        members[0].bytes = 8_000;
        members[1].bytes = 3_000;
        members[2].bytes = 1_000;
        members[0].image = Some(SimilarImageView {
            width: 4000,
            height: 3000,
            dhash_distance: 0,
            phash_distance: 0,
            is_best: true,
            raw_thumbnail: None,
            thumbnail: None,
        });
        let mut group = group(members);
        group.mode = DupeMode::Similar;
        group.keeper = Some(NodeId::from(1));
        assert_eq!(group.reclaimable_bytes(), 4_000);
        group.keeper = Some(NodeId::from(3));
        assert_eq!(group.reclaimable_bytes(), 11_000);
    }

    #[test]
    fn keep_newest_keeps_max_mtime_and_trashes_the_rest() {
        let g = group(vec![member(1, 100), member(2, 300), member(3, 200)]);
        assert_eq!(g.newest(), Some(NodeId::from(2)));
        let victims = g.victims_keep_newest();
        assert_eq!(victims, vec![NodeId::from(1), NodeId::from(3)]);
    }

    #[test]
    fn keep_newest_breaks_mtime_ties_by_first_seen() {
        // Two members share the newest mtime — keep the earlier-listed
        // one so the choice is stable across runs.
        let g = group(vec![member(1, 300), member(2, 300), member(3, 100)]);
        assert_eq!(g.newest(), Some(NodeId::from(1)));
    }

    #[test]
    fn all_but_one_defaults_to_first_then_honours_picked_keeper() {
        let mut g = group(vec![member(1, 100), member(2, 300), member(3, 200)]);
        // No keeper picked → first member survives.
        assert_eq!(
            g.victims_all_but_one(),
            vec![NodeId::from(2), NodeId::from(3)]
        );
        // Pick member 3 as keeper → 1 and 2 are victims.
        g.keeper = Some(NodeId::from(3));
        assert_eq!(
            g.victims_all_but_one(),
            vec![NodeId::from(1), NodeId::from(2)]
        );
    }

    #[test]
    fn reclaimable_excludes_hard_links_and_clones() {
        // 4 names, but #2 is a hard link and #4 is a clone of an
        // earlier member → 2 distinct occupants → one copy's worth
        // reclaimable.
        let mut members = vec![
            member(1, 100),
            member(2, 100),
            member(3, 100),
            member(4, 100),
        ];
        members[1].is_hardlink = true;
        members[3].is_clone = true;
        let g = group(members);
        assert_eq!(g.distinct_occupants(), 2);
        assert_eq!(g.reclaimable_bytes(), 1000);
    }

    #[test]
    fn single_occupant_group_reclaims_nothing() {
        // Two names, both the same inode (hard link) → nothing to free.
        let mut members = vec![member(1, 100), member(2, 100)];
        members[1].is_hardlink = true;
        let g = group(members);
        assert_eq!(g.distinct_occupants(), 1);
        assert_eq!(g.reclaimable_bytes(), 0);
    }
}

#[cfg(test)]
mod reorder_tests {
    use super::reorder_insert_index;

    #[test]
    fn forward_move_adjusts_for_removal_shift() {
        // [A B C D], drag A (0) to gap 2 (between B and C) → remove A,
        // insert at 1 → [B A C D].
        assert_eq!(reorder_insert_index(0, 2, 4), Some(1));
        // Drag A to the far end (gap 4) → insert at 3 → [B C D A].
        assert_eq!(reorder_insert_index(0, 4, 4), Some(3));
    }

    #[test]
    fn backward_move_keeps_position() {
        // [A B C D], drag D (3) to gap 0 → insert at 0 → [D A B C].
        assert_eq!(reorder_insert_index(3, 0, 4), Some(0));
        // Drag C (2) to gap 1 → insert at 1 → [A C B D].
        assert_eq!(reorder_insert_index(2, 1, 4), Some(1));
    }

    #[test]
    fn adjacent_gaps_are_noops() {
        // Gap to the immediate left and right of the dragged tab.
        assert_eq!(reorder_insert_index(1, 1, 4), None);
        assert_eq!(reorder_insert_index(1, 2, 4), None);
        // First and last tabs' own gaps.
        assert_eq!(reorder_insert_index(0, 0, 4), None);
        assert_eq!(reorder_insert_index(0, 1, 4), None);
        assert_eq!(reorder_insert_index(3, 3, 4), None);
        assert_eq!(reorder_insert_index(3, 4, 4), None);
    }

    #[test]
    fn out_of_range_is_rejected() {
        assert_eq!(reorder_insert_index(4, 0, 4), None); // from beyond end
        assert_eq!(reorder_insert_index(0, 5, 4), None); // gap beyond end
        assert_eq!(reorder_insert_index(0, 0, 0), None); // empty strip
    }

    #[test]
    fn single_tab_strip_has_no_valid_moves() {
        for gap in 0..=1 {
            assert_eq!(reorder_insert_index(0, gap, 1), None);
        }
    }

    #[test]
    fn chip_drop_lands_on_target_slot() {
        use super::{chip_drop_gap_index, reorder_insert_index};
        // [A B C D]: drag A (0) onto C (2) → gap 3 → insert at 2 →
        // [B C A D]; A sits where C was.
        assert_eq!(chip_drop_gap_index(0, 2), 3);
        assert_eq!(reorder_insert_index(0, 3, 4), Some(2));
        // Drag D (3) onto B (1) → gap 1 → insert at 1 → [A D B C].
        assert_eq!(chip_drop_gap_index(3, 1), 1);
        assert_eq!(reorder_insert_index(3, 1, 4), Some(1));
        // Adjacent neighbors still reorder (unlike gap drops, a drop
        // ON the neighbor chip is an unambiguous swap intent):
        // drag A (0) onto B (1) → gap 2 → insert at 1 → [B A C D].
        assert_eq!(chip_drop_gap_index(0, 1), 2);
        assert_eq!(reorder_insert_index(0, 2, 4), Some(1));
    }

    #[test]
    fn chip_drop_on_self_is_noop() {
        use super::{chip_drop_gap_index, reorder_insert_index};
        for idx in 0..4 {
            let gap = chip_drop_gap_index(idx, idx);
            assert_eq!(reorder_insert_index(idx, gap, 4), None);
        }
    }
}

/// Per-tab state captured at close time for `Cmd+Shift+T`. Lives on
/// `ProcessState::closed_tabs` so it survives the tab's owning window
/// closing — a closed window's tabs can still be reopened from any
/// other window. Spec §3.3 + §3.4.
///
/// `ClosedTab` is intentionally plain data (no gpui entities, no
/// `Subscription`s) so it can sit in a `VecDeque` indefinitely without
/// pinning view-tree resources. The reopen path in `Shell` rebuilds
/// the live `Tab` via `Shell::make_tab` and then applies these fields
/// onto the fresh tab before scheduling its initial load.
#[derive(Clone)]
pub struct ClosedTab {
    pub current_dir: PathBuf,
    pub history: Vec<HistoryEntry>,
    pub history_index: usize,
    pub filter_text: String,
    pub selection: HashSet<NodeId>,
    pub anchor: Option<NodeId>,
    pub lead: Option<NodeId>,
}
