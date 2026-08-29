//! Search results in a tab (docs/features/SEARCH.md).
//!
//! A tab-local tool result surface keeps its `current_dir` (the search
//! root) for navigation, but its file list is fed by a search worker
//! instead of `enumerate`. Results stream into the same table via the
//! same [`LoadBatch`] shape the directory loader uses, so selection,
//! sort, preview, and context menus all work unchanged. The engine
//! (Spotlight-when-available, else the built-in recursive walker) is
//! chosen per the user's [`SearchConfig`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use ferail_core::filter_expr::FilterExpr;
use ferail_core::{EnumerationError, FileEntry};
use ferail_fs_native::{DEFAULT_SEARCH_BATCH, NativeFs};
use gpui::{AnyWindowHandle, Context};
use gpui_component::WindowExt;
use std::path::Path;

use super::Shell;
use super::loading::LoadBatch;
use super::tab::{TabId, ToolResultSurface};
use crate::feature_settings::{SearchConfig, SearchEnginePref};
use crate::tasks::TaskKind;

/// Streamed search result, mirroring the directory loader's `LoadMsg`.
pub(super) enum SearchMsg {
    /// The engine the worker actually resolved to, sent before the
    /// first batch. Engine resolution probes for `mdfind` (a blocking
    /// process spawn), so it happens on the worker — the UI shows an
    /// optimistic label until this corrects it (Prime Directive:
    /// launching a search must not block on the probe).
    Engine(&'static str),
    Batch(LoadBatch),
    Done(Option<EnumerationError>),
}

const SEARCH_CHANNEL_BATCHES: usize = 64;
const SEARCH_UI_ROWS_PER_TICK: usize = 16 * 1024;
const SEARCH_UI_TICK_MS: u64 = 100;

#[derive(Default)]
struct SearchUiUpdate {
    engine: Option<&'static str>,
    batch: Option<LoadBatch>,
    done: Option<Option<EnumerationError>>,
}

impl SearchUiUpdate {
    fn rows(&self) -> usize {
        self.batch.as_ref().map_or(0, |batch| batch.entries.len())
    }

    fn absorb(&mut self, msg: SearchMsg) {
        match msg {
            SearchMsg::Engine(engine) => self.engine = Some(engine),
            SearchMsg::Batch(next) => match &mut self.batch {
                Some(batch) => batch.append(next),
                None => self.batch = Some(next),
            },
            SearchMsg::Done(error) => self.done = Some(error),
        }
    }
}

pub(super) enum FlatMsg {
    Root(PathBuf),
    Batch(LoadBatch),
    Progress(ferail_fs_native::SearchStats),
    Done(Option<EnumerationError>),
}

/// Bound both memory and UI work when a warm SSD enumerates faster than the
/// window can paint. The worker may stay this many 256-row batches ahead; the
/// main thread coalesces up to `FLAT_UI_ROWS_PER_TICK` rows and then yields for
/// one UI tick. Ten updates/second is visually continuous for a progress scan
/// without asking GPUI to recompute scroll geometry at SSD speed. An unbounded
/// channel here previously reached ~1 GiB and let the receiver monopolize the
/// event loop for more than ten seconds.
const FLAT_CHANNEL_BATCHES: usize = 64;
const FLAT_UI_ROWS_PER_TICK: usize = 16 * 1024;
const FLAT_UI_TICK_MS: u64 = 100;

#[derive(Default)]
struct FlatUiUpdate {
    root: Option<PathBuf>,
    batch: Option<LoadBatch>,
    progress: Option<ferail_fs_native::SearchStats>,
    done: Option<Option<EnumerationError>>,
}

impl FlatUiUpdate {
    fn rows(&self) -> usize {
        self.batch.as_ref().map_or(0, |batch| batch.entries.len())
    }

    fn absorb(&mut self, msg: FlatMsg) {
        match msg {
            FlatMsg::Root(root) => self.root = Some(root),
            FlatMsg::Batch(next) => match &mut self.batch {
                Some(batch) => {
                    batch.entries.extend(next.entries);
                    batch.paths.extend(next.paths);
                }
                None => self.batch = Some(next),
            },
            // Only the newest progress sample matters within one UI tick.
            FlatMsg::Progress(progress) => self.progress = Some(progress),
            FlatMsg::Done(error) => self.done = Some(error),
        }
    }
}

// The high bit reserves a namespace that ordinary NativeFs identities will
// never realistically reach. Forty low bits leave room for over one trillion
// rows per scan; the remaining bits distinguish concurrent/repeated scans.
const FLAT_ID_MARKER: u64 = 1 << 63;
const FLAT_ROW_BITS: u32 = 40;
static NEXT_FLAT_SCAN: AtomicU64 = AtomicU64::new(1);

fn next_flat_id_base() -> u64 {
    let scan_mask = (1_u64 << (63 - FLAT_ROW_BITS)) - 1;
    let scan = NEXT_FLAT_SCAN.fetch_add(1, Ordering::Relaxed) & scan_mask;
    FLAT_ID_MARKER | (scan << FLAT_ROW_BITS)
}

#[allow(clippy::too_many_arguments)]
fn run_flat_load(
    fs: Arc<NativeFs>,
    root: PathBuf,
    include_hidden: bool,
    cancel: Arc<AtomicBool>,
    id_base: u64,
    tx: async_channel::Sender<FlatMsg>,
) {
    let scan_root = super::canonicalize_for_identity(root);
    if tx.send_blocking(FlatMsg::Root(scan_root.clone())).is_err() {
        return;
    }
    let error = fs.flat_subtree(
        &scan_root,
        include_hidden,
        false,
        DEFAULT_SEARCH_BATCH,
        &cancel,
        false,
        id_base,
        |hits| {
            let mut entries = Vec::with_capacity(hits.len());
            let mut paths = HashMap::with_capacity(hits.len());
            for hit in hits {
                paths.insert(hit.entry.id, hit.path);
                entries.push(hit.entry);
            }
            if tx
                .send_blocking(FlatMsg::Batch(LoadBatch { entries, paths }))
                .is_err()
            {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        |stats| {
            if tx.send_blocking(FlatMsg::Progress(stats)).is_err() {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    let _ = tx.send_blocking(FlatMsg::Done(error));
}

/// Stamp the result row's (otherwise-empty) Description column with the
/// hit's location relative to the search root, so identically-named hits
/// (`Cargo.toml` in every crate) are distinguishable in the list. Falls
/// back to the full parent path when the hit isn't under `root` (e.g. a
/// Spotlight path spelled differently than the canonical root).
fn with_location(mut entry: FileEntry, path: &Path, root: &Path) -> FileEntry {
    let location = path.parent().map(|parent| match parent.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => "·".to_string(),
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => parent.to_string_lossy().into_owned(),
    });
    if let Some(location) = location {
        entry.display_description = location.into();
    }
    entry
}

/// Decide whether to use Spotlight for this search, given the user's
/// engine preference. macOS only; everywhere else the walker is the only
/// engine.
#[cfg(target_os = "macos")]
fn resolve_spotlight(engine: SearchEnginePref) -> bool {
    match engine {
        SearchEnginePref::Walker => false,
        SearchEnginePref::Spotlight | SearchEnginePref::Auto => {
            ferail_shell_mac::spotlight_available()
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn resolve_spotlight(_engine: SearchEnginePref) -> bool {
    false
}

/// Worker body. Runs on the background executor; streams `SearchMsg`s
/// back over `tx`. Resolves Spotlight-vs-walker HERE, not at launch:
/// `resolve_spotlight` shells out to probe `mdfind` (a synchronous
/// process spawn+wait), which must never run on the UI thread.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_search_load(
    fs: Arc<NativeFs>,
    config: SearchConfig,
    root: PathBuf,
    needle: String,
    tag: Option<String>,
    cancel: Arc<AtomicBool>,
    tx: async_channel::Sender<SearchMsg>,
) {
    // Structured filter tokens (size:, mod:, locked:, …) work in
    // subtree search too: text terms drive the engine's name/content
    // match, metadata terms test each hit's built row.
    let expr = FilterExpr::parse(needle.trim(), super::loading::filter_date_ctx());
    let error = if let Some(tag) = tag {
        // Tag favorites (§9): a Spotlight `kMDItemUserTags` query. Finder
        // tags are a macOS concept, so there is no walker fallback — a
        // system without Spotlight simply returns no results.
        let _ = tx.send_blocking(SearchMsg::Engine(ferail_core::msgid!("Tag")));
        run_tag_search(&fs, &config, &root, &tag, &cancel, &tx)
    } else if resolve_spotlight(config.engine) && !expr.text_needle().is_empty() {
        // Spotlight needs a query string; a token-only filter
        // (`mod:week` alone) has none, so it walks instead.
        let _ = tx.send_blocking(SearchMsg::Engine(ferail_core::msgid!("Spotlight")));
        run_spotlight(&fs, &config, &root, &expr, &cancel, &tx)
    } else {
        let _ = tx.send_blocking(SearchMsg::Engine(ferail_core::msgid!("Subtree")));
        run_walker(&fs, &config, &root, expr.clone(), &cancel, &tx)
    };
    let _ = tx.send_blocking(SearchMsg::Done(error));
}

/// Stream files carrying the Finder tag `tag`, rooted at `root`, via a
/// Spotlight metadata query. Reuses the same batch/apply path as the
/// text search so the results render as an ordinary search surface.
#[cfg(target_os = "macos")]
fn run_tag_search(
    fs: &NativeFs,
    config: &SearchConfig,
    root: &Path,
    tag: &str,
    cancel: &AtomicBool,
    tx: &async_channel::Sender<SearchMsg>,
) -> Option<EnumerationError> {
    use ferail_shell_mac::{SpotlightScope, spotlight_search};
    // Raw `mdfind` predicate (passed as the natural-language query, i.e.
    // `name_only = false`). Escape embedded quotes so a crafted tag name
    // can't break out of the predicate string.
    let predicate = format!("kMDItemUserTags == \"{}\"cd", tag.replace('"', ""));
    let res = spotlight_search(
        SpotlightScope::Subtree(root.to_path_buf()),
        &predicate,
        false,
        DEFAULT_SEARCH_BATCH,
        cancel,
        |found| {
            let mut entries = Vec::with_capacity(found.len());
            let mut paths = HashMap::with_capacity(found.len());
            for path in found {
                let Some(entry) = fs.file_entry_for_path(&path) else {
                    continue;
                };
                if !config.include_hidden && entry.hidden {
                    continue;
                }
                let entry = with_location(entry, &path, root);
                paths.insert(entry.id, path);
                entries.push(entry);
            }
            if !entries.is_empty()
                && tx
                    .send_blocking(SearchMsg::Batch(LoadBatch { entries, paths }))
                    .is_err()
            {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    // Spotlight unavailable ⇒ no tag results (tags need the index).
    let _ = res;
    None
}

#[cfg(not(target_os = "macos"))]
fn run_tag_search(
    _fs: &NativeFs,
    _config: &SearchConfig,
    _root: &Path,
    _tag: &str,
    _cancel: &AtomicBool,
    _tx: &async_channel::Sender<SearchMsg>,
) -> Option<EnumerationError> {
    None
}

fn run_walker(
    fs: &NativeFs,
    config: &SearchConfig,
    root: &Path,
    expr: FilterExpr,
    cancel: &AtomicBool,
    tx: &async_channel::Sender<SearchMsg>,
) -> Option<EnumerationError> {
    let mut query = config.query(String::new());
    query.expr = Some(expr);
    fs.search_subtree(
        root,
        &query,
        DEFAULT_SEARCH_BATCH,
        cancel,
        false,
        |hits| {
            let mut entries = Vec::with_capacity(hits.len());
            let mut paths = HashMap::with_capacity(hits.len());
            for hit in hits {
                let entry = with_location(hit.entry, &hit.path, root);
                paths.insert(entry.id, hit.path);
                entries.push(entry);
            }
            if tx
                .send_blocking(SearchMsg::Batch(LoadBatch { entries, paths }))
                .is_err()
            {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        |_| {},
    )
}

#[cfg(target_os = "macos")]
fn run_spotlight(
    fs: &NativeFs,
    config: &SearchConfig,
    root: &Path,
    expr: &FilterExpr,
    cancel: &AtomicBool,
    tx: &async_channel::Sender<SearchMsg>,
) -> Option<EnumerationError> {
    use ferail_shell_mac::{SpotlightScope, spotlight_search};
    // Spotlight gets the free-text part of the query; structured
    // tokens (size:, mod:, locked:, …) are applied here to each hit's
    // built row, so both engines enforce identical value semantics.
    let needle = expr.text_needle();
    let res = spotlight_search(
        SpotlightScope::Subtree(root.to_path_buf()),
        &needle,
        // name-only when the user isn't matching paths; otherwise let
        // Spotlight's natural-language query reach content + metadata.
        !config.match_path,
        DEFAULT_SEARCH_BATCH,
        cancel,
        |found| {
            let mut entries = Vec::with_capacity(found.len());
            let mut paths = HashMap::with_capacity(found.len());
            for path in found {
                let Some(entry) = fs.file_entry_for_path(&path) else {
                    continue;
                };
                // Spotlight may surface hidden items; honor the toggle.
                if !config.include_hidden && entry.hidden {
                    continue;
                }
                if !expr.metadata_matches(&entry) {
                    continue;
                }
                let entry = with_location(entry, &path, root);
                paths.insert(entry.id, path);
                entries.push(entry);
            }
            if !entries.is_empty()
                && tx
                    .send_blocking(SearchMsg::Batch(LoadBatch { entries, paths }))
                    .is_err()
            {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    // A spawn failure (Spotlight disabled) falls back to the walker so the
    // user still gets results.
    if res.is_err() {
        return run_walker(fs, config, root, expr.clone(), cancel, tx);
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn run_spotlight(
    fs: &NativeFs,
    config: &SearchConfig,
    root: &Path,
    expr: &FilterExpr,
    cancel: &AtomicBool,
    tx: &async_channel::Sender<SearchMsg>,
) -> Option<EnumerationError> {
    run_walker(fs, config, root, expr.clone(), cancel, tx)
}

impl Shell {
    /// Toggle a tab-local recursive, files-only snapshot. There is no row
    /// ceiling: cancellation and progress stay live for as long as the walk
    /// takes, and every row/path allocation belongs to this surface.
    pub fn toggle_flat_view(
        &mut self,
        tab_id: TabId,
        notify_window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        if self.tabs[idx]
            .tool_result
            .as_ref()
            .is_some_and(|surface| surface.flat_mode().is_some())
        {
            let root = self.tabs[idx].current_dir.clone();
            self.tabs[idx].tool_result = None;
            self.load_path_for_tab(tab_id, root, cx);
        } else {
            let root = self.tabs[idx].current_dir.clone();
            self.start_flat_view(tab_id, root, notify_window, cx);
        }
    }

    pub fn restart_flat_view(
        &mut self,
        tab_id: TabId,
        notify_window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| tab.tool_result.as_ref())
            .and_then(|surface| surface.flat_mode())
            .map(|flat| flat.root.clone())
        else {
            return;
        };
        self.start_flat_view(tab_id, root, notify_window, cx);
    }

    fn start_flat_view(
        &mut self,
        tab_id: TabId,
        root: PathBuf,
        notify_window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        self.tabs[idx].load_generation = self.tabs[idx].load_generation.wrapping_add(1);
        let generation = self.tabs[idx].load_generation;
        for cancel in [
            self.tabs[idx].load_cancel.take(),
            self.tabs[idx].folder_size_cancel.take(),
            self.tabs[idx].prefetch_cancel.take(),
        ]
        .into_iter()
        .flatten()
        {
            cancel.store(true, Ordering::Relaxed);
        }
        self.tabs[idx].load_staging = None;
        self.tabs[idx].hidden_summary = Default::default();
        self.tabs[idx].filter_summary = Default::default();
        self.tabs[idx].clear_selection();
        self.tabs[idx].anchor = None;
        self.tabs[idx].lead = None;
        self.tabs[idx].range_live = false;
        self.tabs[idx].filtered_out.clear();
        self.tabs[idx].view_mode = crate::grid::ViewMode::List;
        self.tabs[idx].tool_result = Some(ToolResultSurface::flat(root.clone()));

        let id_base = next_flat_id_base();
        let table = self.tabs[idx].table.clone();
        table.update(cx, |state, cx| {
            state.delegate_mut().begin_flat(root.clone(), id_base);
            state.refresh(cx);
        });
        self.refresh_file_list_selection_in_tab(idx, cx);

        let cancel = Arc::new(AtomicBool::new(false));
        self.tabs[idx].load_cancel = Some(cancel.clone());
        let task = self.process.tasks.borrow_mut().begin_with_cancel(
            TaskKind::FlatScan,
            tr!("Include Subfolders").to_string(),
            cancel.clone(),
        );
        if let Some(previous) = self.tabs[idx].load_task.replace(task) {
            self.process.tasks.borrow_mut().end(previous);
        }

        let fs = self.process.fs.clone();
        let include_hidden = self.show_hidden;
        let (tx, rx) = async_channel::bounded(FLAT_CHANNEL_BATCHES);
        cx.background_executor()
            .spawn(async move {
                run_flat_load(fs, root, include_hidden, cancel, id_base, tx);
            })
            .detach();

        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                let mut update = FlatUiUpdate::default();
                update.absorb(msg);
                while update.done.is_none() && update.rows() < FLAT_UI_ROWS_PER_TICK {
                    match rx.try_recv() {
                        Ok(msg) => update.absorb(msg),
                        Err(async_channel::TryRecvError::Empty) => break,
                        Err(async_channel::TryRecvError::Closed) => {
                            update.done = Some(None);
                            break;
                        }
                    }
                }
                let done = update.done.is_some();
                let stale = this
                    .update(cx, |this, cx| {
                        let Some(idx) = this.tabs.iter().position(|tab| tab.id == tab_id) else {
                            return true;
                        };
                        if this.tabs[idx].load_generation != generation {
                            return true;
                        }
                        if let Some(root) = update.root {
                            this.apply_flat_msg_in_tab(idx, FlatMsg::Root(root), notify_window, cx);
                        }
                        if let Some(batch) = update.batch {
                            this.apply_flat_msg_in_tab(
                                idx,
                                FlatMsg::Batch(batch),
                                notify_window,
                                cx,
                            );
                        }
                        if let Some(progress) = update.progress {
                            this.apply_flat_msg_in_tab(
                                idx,
                                FlatMsg::Progress(progress),
                                notify_window,
                                cx,
                            );
                        }
                        if let Some(error) = update.done {
                            this.apply_flat_msg_in_tab(
                                idx,
                                FlatMsg::Done(error),
                                notify_window,
                                cx,
                            );
                        }
                        false
                    })
                    .unwrap_or(true);
                if stale || done {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(FLAT_UI_TICK_MS))
                    .await;
            }
        })
        .detach();
    }

    fn apply_flat_msg_in_tab(
        &mut self,
        idx: usize,
        msg: FlatMsg,
        notify_window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let visible = idx == self.active;
        match msg {
            FlatMsg::Root(root) => {
                let table = self.tabs[idx].table.clone();
                table.update(cx, |state, cx| {
                    state.delegate_mut().set_flat_root(root);
                    if visible {
                        state.refresh(cx);
                    }
                });
            }
            FlatMsg::Batch(batch) => {
                let table = self.tabs[idx].table.clone();
                table.update(cx, |state, _cx| {
                    state
                        .delegate_mut()
                        .append_flat_entries(batch.entries, batch.paths);
                    // Columns are unchanged. A full TableState refresh here
                    // needlessly rebuilt header state for every streamed
                    // batch; the outer shell notification is sufficient to
                    // update row count, scrollbar, and visible rows.
                });
                if visible {
                    cx.notify();
                }
            }
            FlatMsg::Progress(stats) => {
                if let Some(flat) = self.tabs[idx]
                    .tool_result
                    .as_mut()
                    .and_then(|surface| surface.flat_mode_mut())
                {
                    flat.progress = stats;
                }
                if let Some(task) = self.tabs[idx].load_task {
                    self.process.tasks.borrow_mut().set_label(
                        task,
                        trn!(
                            "Scanning: {n} file in {dirs} folders",
                            "Scanning: {n} files in {dirs} folders",
                            stats.matches as usize,
                            dirs = ferail_core::counts::format_count(stats.dirs_scanned)
                        )
                        .to_string(),
                    );
                }
                if visible {
                    cx.notify();
                }
            }
            FlatMsg::Done(error) => {
                let cancelled = self.tabs[idx]
                    .load_cancel
                    .as_ref()
                    .is_some_and(|cancel| cancel.load(Ordering::Relaxed));
                let table = self.tabs[idx].table.clone();
                table.update(cx, |state, cx| {
                    state.delegate_mut().finish_flat();
                    state.delegate_mut().enable_visible_details(false);
                    if visible {
                        state.refresh(cx);
                    }
                });
                if let Some(flat) = self.tabs[idx]
                    .tool_result
                    .as_mut()
                    .and_then(|surface| surface.flat_mode_mut())
                {
                    flat.complete = !cancelled && error.is_none();
                }
                if let Some(task) = self.tabs[idx].load_task.take() {
                    self.process.tasks.borrow_mut().end(task);
                }
                self.tabs[idx].load_cancel = None;
                self.refresh_file_list_selection_in_tab(idx, cx);
                self.warm_loaded_viewport_in_tab(idx, cx);
                if let (Some(window), Some(error)) = (notify_window, error) {
                    let message =
                        super::enumeration_error_message(&tr!("Include Subfolders"), &error);
                    let _ = window.update(cx, |_, window, cx| {
                        use gpui_component::notification::Notification;
                        window.push_notification(Notification::error(message), cx);
                    });
                }
                if visible {
                    cx.notify();
                }
            }
        }
    }

    /// Launch a recursive / global search rooted at the tab's current
    /// directory, replacing its listing with streamed results. Triggered
    /// by Enter in the filter box (and the `--search-subtree` CLI flag).
    pub fn start_subtree_search(
        &mut self,
        tab_id: TabId,
        needle: String,
        notify_window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let needle = needle.trim().to_string();
        if needle.is_empty() {
            return;
        }
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let root = self.tabs[idx].current_dir.clone();
        self.start_search_inner(tab_id, root, needle, None, notify_window, cx);
    }

    /// Launch a Finder-tag search (a Tag favorite was clicked, §9),
    /// rooted at Home so results span the user's files without dredging
    /// the whole system. Streams into the tab's listing like any search.
    pub fn start_tag_search(
        &mut self,
        tab_id: TabId,
        tag: String,
        notify_window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let tag = tag.trim().to_string();
        if tag.is_empty() {
            return;
        }
        let root = ferail_fs_native::home_dir();
        self.start_search_inner(tab_id, root, tag.clone(), Some(tag), notify_window, cx);
    }

    /// Shared launch path for text (`tag = None`) and tag
    /// (`tag = Some`) searches: bump the tab generation, cancel in-flight
    /// workers, clear the listing, and stream results into the tab.
    fn start_search_inner(
        &mut self,
        tab_id: TabId,
        root: PathBuf,
        needle: String,
        tag: Option<String>,
        notify_window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        // `load()` reads the memoized in-memory AppState — no disk. The
        // engine itself resolves on the worker (probing for `mdfind`
        // spawns a process); this label is the optimistic guess shown
        // until the worker's `SearchMsg::Engine` confirms/corrects it.
        let config = SearchConfig::load();
        let engine_label = if tag.is_some() {
            ferail_core::msgid!("Tag")
        } else if matches!(config.engine, SearchEnginePref::Walker) {
            ferail_core::msgid!("Subtree")
        } else {
            ferail_core::msgid!("Spotlight")
        };

        // Treat the search like a fresh load on this tab: bump the
        // generation, cancel any in-flight directory/search worker, and
        // clear the visible rows so results stream in fresh.
        self.tabs[idx].load_generation = self.tabs[idx].load_generation.wrapping_add(1);
        let generation = self.tabs[idx].load_generation;
        if let Some(cancel) = self.tabs[idx].load_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = self.tabs[idx].folder_size_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = self.tabs[idx].prefetch_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.tabs[idx].load_staging = None;
        // A results listing is not the directory the last load counted:
        // drop that load's skipped/filtered aggregates so their chips
        // don't describe a folder the user has navigated past.
        self.tabs[idx].hidden_summary = Default::default();
        self.tabs[idx].filter_summary = Default::default();
        self.tabs[idx].tool_result = Some(ToolResultSurface::search(
            needle.clone(),
            root.clone(),
            engine_label,
        ));
        let table = self.tabs[idx].table.clone();
        table.update(cx, |state, cx| {
            state.delegate_mut().clear();
            state.refresh(cx);
        });

        let cancel = Arc::new(AtomicBool::new(false));
        self.tabs[idx].load_cancel = Some(cancel.clone());
        let label = tr!("Searching \u{201c}{needle}\u{201d}", needle = needle).to_string();
        let task = self.process.tasks.borrow_mut().begin_with_cancel(
            TaskKind::Search,
            label,
            cancel.clone(),
        );
        if let Some(previous) = self.tabs[idx].load_task.replace(task) {
            self.process.tasks.borrow_mut().end(previous);
        }

        let fs = self.process.fs.clone();
        let (tx, rx) = async_channel::bounded(SEARCH_CHANNEL_BATCHES);
        cx.background_executor()
            .spawn(async move {
                run_search_load(fs, config, root, needle, tag, cancel, tx);
            })
            .detach();

        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                let mut update = SearchUiUpdate::default();
                update.absorb(msg);
                while update.done.is_none() && update.rows() < SEARCH_UI_ROWS_PER_TICK {
                    match rx.try_recv() {
                        Ok(msg) => update.absorb(msg),
                        Err(async_channel::TryRecvError::Empty) => break,
                        Err(async_channel::TryRecvError::Closed) => break,
                    }
                }
                let done = update.done.is_some();
                let stale = this
                    .update(cx, |this, cx| {
                        let Some(idx) = this.tabs.iter().position(|t| t.id == tab_id) else {
                            return true;
                        };
                        if this.tabs[idx].load_generation != generation {
                            return true;
                        }
                        if let Some(engine) = update.engine {
                            this.apply_search_msg_in_tab(
                                idx,
                                SearchMsg::Engine(engine),
                                notify_window,
                                cx,
                            );
                        }
                        if let Some(batch) = update.batch {
                            this.apply_search_msg_in_tab(
                                idx,
                                SearchMsg::Batch(batch),
                                notify_window,
                                cx,
                            );
                        }
                        if let Some(error) = update.done {
                            this.apply_search_msg_in_tab(
                                idx,
                                SearchMsg::Done(error),
                                notify_window,
                                cx,
                            );
                        }
                        false
                    })
                    .unwrap_or(true);
                if stale || done {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(SEARCH_UI_TICK_MS))
                    .await;
            }
        })
        .detach();
    }

    fn apply_search_msg_in_tab(
        &mut self,
        idx: usize,
        msg: SearchMsg,
        notify_window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let visible = idx == self.active;
        match msg {
            SearchMsg::Engine(label) => {
                // Worker-resolved engine (it owns the mdfind probe).
                // Corrects the optimistic label the launch path showed.
                if let Some(mode) = self
                    .tabs
                    .get_mut(idx)
                    .and_then(|tab| tab.tool_result.as_mut())
                    .and_then(|surface| surface.search_mode_mut())
                {
                    mode.engine_label = label;
                    if visible {
                        cx.notify();
                    }
                }
            }
            SearchMsg::Batch(batch) => self.apply_search_batch_in_tab(idx, batch, cx),
            SearchMsg::Done(error) => {
                let result_count = self.tabs[idx].table.read(cx).delegate().entries.len();
                let needle = self.tabs[idx]
                    .tool_result
                    .as_ref()
                    .and_then(|surface| surface.search_mode())
                    .map(|mode| mode.needle.clone())
                    .unwrap_or_default();
                let mut surfaced = false;
                if let Some(tab) = self.tabs.get_mut(idx) {
                    if let Some(id) = tab.load_task.take() {
                        surfaced = self.process.tasks.borrow_mut().end_and_was_surfaced(id);
                    }
                    tab.load_cancel = None;
                }
                if let Some(window) = notify_window {
                    if let Some(error) = error {
                        let message = super::enumeration_error_message(&tr!("Search"), &error);
                        let _ = window.update(cx, |_, window, cx| {
                            use gpui_component::notification::Notification;
                            window.push_notification(Notification::error(message), cx);
                        });
                    } else if surfaced {
                        let message = trn!(
                            "Search finished: {n} result for \u{201c}{needle}\u{201d}",
                            "Search finished: {n} results for \u{201c}{needle}\u{201d}",
                            result_count,
                            needle = needle
                        );
                        let _ = window.update(cx, |_, window, cx| {
                            use gpui_component::notification::Notification;
                            window.push_notification(Notification::success(message), cx);
                        });
                    }
                }
            }
        }
    }

    fn apply_search_batch_in_tab(&mut self, idx: usize, batch: LoadBatch, cx: &mut Context<Self>) {
        let visible = idx == self.active;
        for (id, path) in &batch.paths {
            self.process
                .node_store
                .borrow_mut()
                .get_or_create_path_with_id(path.clone(), *id);
        }
        let heats: Vec<f32> = batch.entries.iter().map(|e| self.ant_heat(e.id)).collect();
        let favorites: Vec<bool> = {
            let favs = self.process.favorites();
            let favs = favs.read(cx);
            batch
                .entries
                .iter()
                .map(|entry| {
                    batch
                        .paths
                        .get(&entry.id)
                        .is_some_and(|path| favs.contains_path(path))
                })
                .collect()
        };
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        let table = tab.table.clone();
        table.update(cx, |state, cx| {
            state.delegate_mut().append_entries_decorated(
                batch.entries,
                batch.paths,
                heats,
                favorites,
            );
            if visible {
                state.refresh(cx);
            }
        });
        self.refresh_file_list_selection_in_tab(idx, cx);
        // Land any deferred selection (keyboard / screenshot seed) once
        // its row has streamed in — same as the directory load path.
        self.apply_pending_select_row_in_tab(idx, cx);
        if visible {
            cx.notify();
        }
    }

    /// Leave search mode and reload the tab's directory. Called when the
    /// user clears the filter while results are showing.
    pub fn clear_search(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        if self.tabs[idx]
            .tool_result
            .as_ref()
            .and_then(|surface| surface.search_mode())
            .is_none()
        {
            return;
        }
        self.tabs[idx].tool_result = None;
        let path = self.tabs[idx].current_dir.clone();
        self.load_path_for_tab(tab_id, path, cx);
    }
}
