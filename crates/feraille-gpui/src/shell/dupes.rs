//! Duplicate-finder results in a tab (docs/features/DUPLICATES.md).
//!
//! Mirrors [`super::search`]: a tab-local tool result surface keeps
//! `current_dir` as the scan root while the file list holds duplicate
//! group members as adjacent rows. The funnel
//! ([`NativeFs::find_duplicates`]) runs off the UI thread, cache-backed
//! by [`crate::dupe_cache::DbHashCache`] so rescans skip full hashing,
//! and streams confirmed groups in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

use feraille_core::{EnumerationError, FileEntry};
use feraille_fs_native::{DEFAULT_DUPE_BATCH, DupeFact, DupeHashCache, DupeOpts, NativeFs};
use gpui::{AnyWindowHandle, Context};
use gpui_component::WindowExt;

use super::Shell;
use super::tab::{DupeGroupMember, DupeGroupView, TabId, ToolResultSurface};
use crate::dupe_cache::DbHashCache;
use crate::feature_settings::DupeConfig;
use crate::tasks::TaskKind;

/// A batch of confirmed groups, **fully built on the worker thread**:
/// table rows, their paths, and the retained panel model, so the UI
/// thread only appends — never stats a file (prime directive).
#[derive(Default)]
pub(super) struct DupeBatch {
    /// Ready-to-append table rows.
    pub entries: Vec<FileEntry>,
    /// `NodeId → path` for the rows in `entries`.
    pub paths: HashMap<feraille_core::NodeId, PathBuf>,
    /// Retained group views for the panel (members keyed by row NodeId).
    pub groups: Vec<DupeGroupView>,
    /// Reclaimable bytes contributed by `groups` (incremental; the UI
    /// just adds it to the running total, no O(n) re-sum per batch).
    pub reclaimable: u64,
}

impl DupeBatch {
    fn append(&mut self, other: DupeBatch) {
        self.entries.extend(other.entries);
        self.paths.extend(other.paths);
        self.groups.extend(other.groups);
        self.reclaimable = self.reclaimable.saturating_add(other.reclaimable);
    }
}

pub(super) enum DupeMsg {
    Batch(DupeBatch),
    Done(Option<EnumerationError>),
}

/// Worker body. Runs on the background executor; builds each confirmed
/// group's rows + panel model **here, off the UI thread**, and streams
/// them back over `tx`. `cache` (the DB-backed hash cache) is moved in so
/// the rescan fast path works. Group numbers are assigned by a running
/// counter so the UI never has to.
pub(super) fn run_dupe_load(
    fs: Arc<NativeFs>,
    opts: DupeOpts,
    cache: Option<DbHashCache>,
    root: PathBuf,
    cancel: Arc<AtomicBool>,
    tx: async_channel::Sender<DupeMsg>,
) {
    let cache_ref: Option<&dyn DupeHashCache> = cache.as_ref().map(|c| c as &dyn DupeHashCache);
    let mut group_no = 0usize;
    let error = fs.find_duplicates(
        &root,
        &opts,
        cache_ref,
        DEFAULT_DUPE_BATCH,
        &cancel,
        |facts| {
            let mut batch = DupeBatch::default();
            for fact in facts {
                let DupeFact::Group {
                    full_hash,
                    bytes_each,
                    members,
                    ..
                } = fact;
                group_no += 1;
                let mut gv_members = Vec::with_capacity(members.len());
                for m in members {
                    // file_entry_for_path is the only I/O here, and we're
                    // on the worker thread — exactly where it belongs.
                    let Some((entry, path)) =
                        member_row(&fs, &m.path, &root, group_no, m.is_hardlink, m.is_clone)
                    else {
                        continue;
                    };
                    gv_members.push(DupeGroupMember {
                        node: entry.id,
                        path: path.clone(),
                        mtime_unix: m.mtime_unix,
                        is_hardlink: m.is_hardlink,
                        is_clone: m.is_clone,
                    });
                    batch.paths.insert(entry.id, path);
                    batch.entries.push(entry);
                }
                if gv_members.len() < 2 {
                    continue; // a member vanished mid-scan; no longer a group
                }
                let view = DupeGroupView {
                    group_no,
                    full_hash,
                    bytes_each,
                    members: gv_members,
                    expanded: true,
                    keeper: None,
                };
                batch.reclaimable = batch.reclaimable.saturating_add(view.reclaimable_bytes());
                batch.groups.push(view);
            }
            if !batch.entries.is_empty() && tx.send_blocking(DupeMsg::Batch(batch)).is_err() {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        |_| {},
    );
    let _ = tx.send_blocking(DupeMsg::Done(error));
}

/// Build a row for one duplicate group member. The Description column
/// carries a group tag, the member's location relative to the scan root,
/// and a storage-sharing note (hard link / clone) so a name that
/// reclaims no space is obvious.
pub(super) fn member_row(
    fs: &NativeFs,
    path: &Path,
    root: &Path,
    group_no: usize,
    is_hardlink: bool,
    is_clone: bool,
) -> Option<(FileEntry, PathBuf)> {
    let mut entry = fs.file_entry_for_path(path)?;
    let location = path
        .parent()
        .map(|parent| match parent.strip_prefix(root) {
            Ok(rel) if rel.as_os_str().is_empty() => "\u{00B7}".to_string(),
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => parent.to_string_lossy().into_owned(),
        })
        .unwrap_or_default();
    let note = storage_note(is_hardlink, is_clone);
    entry.display_description = format!("#{group_no} \u{00B7} {location}{note}");
    Some((entry, path.to_path_buf()))
}

/// Trailing " · hard link" / " · clone — no extra space" note for a
/// member that reclaims nothing, or empty for a storage-owning copy.
/// Shared by the row description and the panel's member line.
pub(super) fn storage_note(is_hardlink: bool, is_clone: bool) -> &'static str {
    if is_hardlink {
        " \u{00B7} hard link"
    } else if is_clone {
        " \u{00B7} clone \u{2014} no extra space"
    } else {
        ""
    }
}

impl Shell {
    /// Launch a duplicate-finder scan rooted at the tab's current
    /// directory, streaming grouped results into its list.
    pub fn start_duplicate_scan(
        &mut self,
        tab_id: TabId,
        notify_window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let root = self.tabs[idx].current_dir.clone();
        let config = DupeConfig::load();
        let presentation = config.presentation;
        let opts = config.opts();

        self.tabs[idx].load_generation = self.tabs[idx].load_generation.wrapping_add(1);
        let generation = self.tabs[idx].load_generation;
        if let Some(cancel) = self.tabs[idx].load_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = self.tabs[idx].folder_size_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.tabs[idx].load_staging = None;
        self.tabs[idx].tool_result =
            Some(ToolResultSurface::duplicates(root.clone(), presentation));
        self.tabs[idx].dupe_groups.clear();
        self.tabs[idx].dupe_panel_scroll = crate::multi_table::VirtualListScrollHandle::new();
        let table = self.tabs[idx].table.clone();
        table.update(cx, |state, cx| {
            state.delegate_mut().clear();
            state.refresh(cx);
        });

        let cancel = Arc::new(AtomicBool::new(false));
        self.tabs[idx].load_cancel = Some(cancel.clone());
        let label = format!("Finding duplicates in {}", short_root(&root));
        let task = self.process.tasks.borrow_mut().begin_with_cancel(
            TaskKind::DuplicateScan,
            label,
            cancel.clone(),
        );
        if let Some(previous) = self.tabs[idx].load_task.replace(task) {
            self.process.tasks.borrow_mut().end(previous);
        }

        // DB-backed hash cache so a rescan skips full hashing.
        let cache = self.process.db_snapshot().map(|db| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            DbHashCache::new(db, now)
        });
        let fs = self.process.fs.clone();
        let (tx, rx) = async_channel::unbounded();
        cx.background_executor()
            .spawn(async move {
                run_dupe_load(fs, opts, cache, root, cancel, tx);
            })
            .detach();

        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                let mut batch: Option<DupeBatch> = None;
                let mut done_error: Option<Option<EnumerationError>> = None;

                absorb_dupe_msg(msg, &mut batch, &mut done_error);
                while done_error.is_none() {
                    match rx.try_recv() {
                        Ok(msg) => absorb_dupe_msg(msg, &mut batch, &mut done_error),
                        Err(async_channel::TryRecvError::Empty) => break,
                        Err(async_channel::TryRecvError::Closed) => {
                            done_error = Some(None);
                            break;
                        }
                    }
                }

                let done = done_error.is_some();
                let stale = this
                    .update(cx, |this, cx| {
                        let Some(idx) = this.tabs.iter().position(|t| t.id == tab_id) else {
                            return true;
                        };
                        if this.tabs[idx].load_generation != generation {
                            return true;
                        }
                        if let Some(batch) = batch {
                            this.apply_dupe_batch_in_tab(idx, batch, cx);
                        }
                        if let Some(error) = done_error {
                            this.apply_dupe_msg_in_tab(
                                idx,
                                DupeMsg::Done(error),
                                notify_window.clone(),
                                cx,
                            );
                        }
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

    fn apply_dupe_msg_in_tab(
        &mut self,
        idx: usize,
        msg: DupeMsg,
        notify_window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        match msg {
            DupeMsg::Batch(batch) => self.apply_dupe_batch_in_tab(idx, batch, cx),
            DupeMsg::Done(error) => {
                let groups = self.tabs[idx]
                    .tool_result
                    .as_ref()
                    .and_then(|surface| surface.dupe_mode())
                    .map(|mode| mode.groups)
                    .unwrap_or(0);
                let reclaimable = self.tabs[idx]
                    .tool_result
                    .as_ref()
                    .and_then(|surface| surface.dupe_mode())
                    .map(|mode| mode.wasted_bytes)
                    .unwrap_or(0);
                let mut surfaced = false;
                if let Some(tab) = self.tabs.get_mut(idx) {
                    if let Some(id) = tab.load_task.take() {
                        surfaced = self.process.tasks.borrow_mut().end_and_was_surfaced(id);
                    }
                    tab.load_cancel = None;
                    cx.notify();
                }
                if let Some(window) = notify_window {
                    if let Some(error) = error {
                        let message = super::enumeration_error_message("Duplicate scan", &error);
                        let _ = window.update(cx, |_, window, cx| {
                            use gpui_component::notification::Notification;
                            window.push_notification(Notification::error(message), cx);
                        });
                    } else if surfaced {
                        let message = if groups == 0 {
                            "Duplicate scan finished: no duplicates found".to_string()
                        } else {
                            format!(
                                "Duplicate scan finished: {groups} group{} \u{00B7} {} reclaimable",
                                if groups == 1 { "" } else { "s" },
                                feraille_fs_native::humanize_bytes(reclaimable)
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

    /// Apply one worker-built batch. Pure data shuffling — the rows and
    /// the panel model were already built off the UI thread, so this only
    /// registers node ids, bumps the running totals, and appends.
    fn apply_dupe_batch_in_tab(&mut self, idx: usize, batch: DupeBatch, cx: &mut Context<Self>) {
        if batch.entries.is_empty() {
            return;
        }
        let panel_mode = self
            .tabs
            .get(idx)
            .and_then(|t| t.tool_result.as_ref())
            .and_then(|surface| surface.dupe_mode())
            .is_some_and(|dm| dm.presentation == crate::feature_settings::DupePresentation::Panel);
        for (id, path) in &batch.paths {
            self.process
                .node_store
                .borrow_mut()
                .get_or_create_path_with_id(path.clone(), *id);
        }
        {
            let Some(dm) = self.tabs.get_mut(idx).and_then(|t| {
                t.tool_result
                    .as_mut()
                    .and_then(|surface| surface.dupe_mode_mut())
            }) else {
                return;
            };
            dm.groups += batch.groups.len();
            dm.wasted_bytes = dm.wasted_bytes.saturating_add(batch.reclaimable);
        }
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        tab.dupe_groups.extend(batch.groups);
        if panel_mode {
            cx.notify();
            return;
        }

        let heats: Vec<f32> = batch.entries.iter().map(|e| self.ant_heat(e.id)).collect();
        let Some(tab) = self.tabs.get(idx) else {
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
}

fn absorb_dupe_msg(
    msg: DupeMsg,
    batch: &mut Option<DupeBatch>,
    done_error: &mut Option<Option<EnumerationError>>,
) {
    match msg {
        DupeMsg::Batch(next) => match batch {
            Some(acc) => acc.append(next),
            None => *batch = Some(next),
        },
        DupeMsg::Done(error) => *done_error = Some(error),
    }
}

/// Last component of the scan root for the task label.
fn short_root(root: &Path) -> String {
    root.file_name()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}
