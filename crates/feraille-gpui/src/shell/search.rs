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
    atomic::{AtomicBool, Ordering},
};

use feraille_core::{EnumerationError, FileEntry};
use feraille_fs_native::{DEFAULT_SEARCH_BATCH, NativeFs};
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
    Batch(LoadBatch),
    Done(Option<EnumerationError>),
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
        entry.display_description = location;
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
            feraille_shell_mac::spotlight_available()
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn resolve_spotlight(_engine: SearchEnginePref) -> bool {
    false
}

/// Worker body. Runs on the background executor; streams `SearchMsg`s
/// back over `tx`. Picks Spotlight or the walker up front.
pub(super) fn run_search_load(
    fs: Arc<NativeFs>,
    use_spotlight: bool,
    config: SearchConfig,
    root: PathBuf,
    needle: String,
    cancel: Arc<AtomicBool>,
    tx: async_channel::Sender<SearchMsg>,
) {
    let error = if use_spotlight {
        run_spotlight(&fs, &config, &root, &needle, &cancel, &tx)
    } else {
        run_walker(&fs, &config, &root, &needle, &cancel, &tx)
    };
    let _ = tx.send_blocking(SearchMsg::Done(error));
}

fn run_walker(
    fs: &NativeFs,
    config: &SearchConfig,
    root: &PathBuf,
    needle: &str,
    cancel: &AtomicBool,
    tx: &async_channel::Sender<SearchMsg>,
) -> Option<EnumerationError> {
    let query = config.query(needle);
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
    root: &PathBuf,
    needle: &str,
    cancel: &AtomicBool,
    tx: &async_channel::Sender<SearchMsg>,
) -> Option<EnumerationError> {
    use feraille_shell_mac::{SpotlightScope, spotlight_search};
    let res = spotlight_search(
        SpotlightScope::Subtree(root.clone()),
        needle,
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
        return run_walker(fs, config, root, needle, cancel, tx);
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn run_spotlight(
    fs: &NativeFs,
    config: &SearchConfig,
    root: &PathBuf,
    needle: &str,
    cancel: &AtomicBool,
    tx: &async_channel::Sender<SearchMsg>,
) -> Option<EnumerationError> {
    run_walker(fs, config, root, needle, cancel, tx)
}

impl Shell {
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
        let config = SearchConfig::load();
        let use_spotlight = resolve_spotlight(config.engine);
        let engine_label = if use_spotlight {
            "Spotlight"
        } else {
            "Subtree"
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
        self.tabs[idx].load_staging = None;
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
        let label = format!("Searching \u{201c}{}\u{201d}", needle);
        let task = self.process.tasks.borrow_mut().begin_with_cancel(
            TaskKind::Search,
            label,
            cancel.clone(),
        );
        if let Some(previous) = self.tabs[idx].load_task.replace(task) {
            self.process.tasks.borrow_mut().end(previous);
        }

        let fs = self.process.fs.clone();
        let (tx, rx) = async_channel::unbounded();
        cx.background_executor()
            .spawn(async move {
                run_search_load(fs, use_spotlight, config, root, needle, cancel, tx);
            })
            .detach();

        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                let done = matches!(msg, SearchMsg::Done(_));
                let stale = this
                    .update(cx, |this, cx| {
                        let Some(idx) = this.tabs.iter().position(|t| t.id == tab_id) else {
                            return true;
                        };
                        if this.tabs[idx].load_generation != generation {
                            return true;
                        }
                        this.apply_search_msg_in_tab(idx, msg, notify_window.clone(), cx);
                        false
                    })
                    .unwrap_or(true);
                if stale || done {
                    break;
                }
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
        match msg {
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
                        let message = super::enumeration_error_message("Search", &error);
                        let _ = window.update(cx, |_, window, cx| {
                            use gpui_component::notification::Notification;
                            window.push_notification(Notification::error(message), cx);
                        });
                    } else if surfaced {
                        let message = if result_count == 1 {
                            format!("Search finished: 1 result for \u{201c}{needle}\u{201d}")
                        } else {
                            format!(
                                "Search finished: {result_count} results for \u{201c}{needle}\u{201d}"
                            )
                        };
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
        for (id, path) in &batch.paths {
            self.process
                .node_store
                .borrow_mut()
                .get_or_create_path_with_id(path.clone(), *id);
        }
        let heats: Vec<f32> = batch.entries.iter().map(|e| self.ant_heat(e.id)).collect();
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        let table = tab.table.clone();
        table.update(cx, |state, cx| {
            state
                .delegate_mut()
                .append_entries(batch.entries, batch.paths, heats);
            state.refresh(cx);
        });
        self.refresh_file_list_favorited_in_tab(idx, cx);
        self.refresh_file_list_selection_in_tab(idx, cx);
        // Land any deferred selection (keyboard / screenshot seed) once
        // its row has streamed in — same as the directory load path.
        self.apply_pending_select_row_in_tab(idx, cx);
        cx.notify();
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
