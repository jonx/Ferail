//! Duplicate-finder results in a tab (docs/features/DUPLICATES.md).
//!
//! Mirrors [`super::search`]: a tab in [`DupeViewMode`] keeps its
//! `current_dir` (the scan root) while the file list holds duplicate
//! group members as adjacent rows. The funnel
//! ([`NativeFs::find_duplicates`]) runs off the UI thread, cache-backed
//! by [`crate::dupe_cache::DbHashCache`] so rescans skip full hashing,
//! and streams confirmed groups in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{SystemTime, UNIX_EPOCH};

use feraille_core::{EnumerationError, FileEntry};
use feraille_fs_native::{DupeFact, DupeHashCache, DupeOpts, NativeFs, DEFAULT_DUPE_BATCH};
use gpui::Context;

use super::tab::{DupeViewMode, TabId};
use super::Shell;
use crate::dupe_cache::DbHashCache;
use crate::feature_settings::DupeConfig;
use crate::tasks::TaskKind;

pub(super) enum DupeMsg {
    Batch(Vec<DupeFact>),
    Done(Option<EnumerationError>),
}

/// Worker body. Runs on the background executor; streams confirmed groups
/// back over `tx`. `cache` (the DB-backed hash cache) is moved in so the
/// rescan fast path works.
pub(super) fn run_dupe_load(
    fs: Arc<NativeFs>,
    opts: DupeOpts,
    cache: Option<DbHashCache>,
    root: PathBuf,
    cancel: Arc<AtomicBool>,
    tx: async_channel::Sender<DupeMsg>,
) {
    let cache_ref: Option<&dyn DupeHashCache> =
        cache.as_ref().map(|c| c as &dyn DupeHashCache);
    let error = fs.find_duplicates(
        &root,
        &opts,
        cache_ref,
        DEFAULT_DUPE_BATCH,
        &cancel,
        |batch| {
            if tx.send_blocking(DupeMsg::Batch(batch)).is_err() {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        |_| {},
    );
    let _ = tx.send_blocking(DupeMsg::Done(error));
}

/// Build a row for one duplicate group member. The Description column
/// carries a group tag, the member's location relative to the scan root,
/// and a hard-link note so a name that reclaims no space is obvious.
fn member_row(
    fs: &NativeFs,
    path: &Path,
    root: &Path,
    group_no: usize,
    is_hardlink: bool,
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
    entry.display_description = if is_hardlink {
        format!("#{group_no} \u{00B7} {location} \u{00B7} hard link")
    } else {
        format!("#{group_no} \u{00B7} {location}")
    };
    Some((entry, path.to_path_buf()))
}

impl Shell {
    /// Launch a duplicate-finder scan rooted at the tab's current
    /// directory, streaming grouped results into its list.
    pub fn start_duplicate_scan(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let root = self.tabs[idx].current_dir.clone();
        let opts = DupeConfig::load().opts();

        self.tabs[idx].load_generation = self.tabs[idx].load_generation.wrapping_add(1);
        let generation = self.tabs[idx].load_generation;
        if let Some(cancel) = self.tabs[idx].load_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if let Some(cancel) = self.tabs[idx].folder_size_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.tabs[idx].load_staging = None;
        self.tabs[idx].search_mode = None;
        self.tabs[idx].dupe_mode = Some(DupeViewMode {
            root: root.clone(),
            groups: 0,
            wasted_bytes: 0,
        });
        let table = self.tabs[idx].table.clone();
        table.update(cx, |state, cx| {
            state.delegate_mut().clear();
            state.refresh(cx);
        });

        let cancel = Arc::new(AtomicBool::new(false));
        self.tabs[idx].load_cancel = Some(cancel.clone());
        let label = format!("Finding duplicates in {}", short_root(&root));
        let task = self
            .process
            .tasks
            .borrow_mut()
            .begin_with_cancel(TaskKind::DuplicateScan, label, cancel.clone());
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
                let done = matches!(msg, DupeMsg::Done(_));
                let stale = this
                    .update(cx, |this, cx| {
                        let Some(idx) = this.tabs.iter().position(|t| t.id == tab_id) else {
                            return true;
                        };
                        if this.tabs[idx].load_generation != generation {
                            return true;
                        }
                        this.apply_dupe_msg_in_tab(idx, msg, cx);
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

    fn apply_dupe_msg_in_tab(&mut self, idx: usize, msg: DupeMsg, cx: &mut Context<Self>) {
        match msg {
            DupeMsg::Batch(facts) => self.apply_dupe_batch_in_tab(idx, facts, cx),
            DupeMsg::Done(_error) => {
                if let Some(tab) = self.tabs.get_mut(idx) {
                    if let Some(id) = tab.load_task.take() {
                        self.process.tasks.borrow_mut().end(id);
                    }
                    tab.load_cancel = None;
                }
            }
        }
    }

    fn apply_dupe_batch_in_tab(
        &mut self,
        idx: usize,
        facts: Vec<DupeFact>,
        cx: &mut Context<Self>,
    ) {
        let root = match self.tabs.get(idx).and_then(|t| t.dupe_mode.as_ref()) {
            Some(dm) => dm.root.clone(),
            None => return,
        };
        let fs = self.process.fs.clone();
        let mut entries: Vec<FileEntry> = Vec::new();
        let mut paths: HashMap<feraille_core::NodeId, PathBuf> = HashMap::new();
        for fact in facts {
            let DupeFact::Group { bytes_each, members, distinct_occupants, .. } = fact;
            // Bump group counters first so the per-row tag uses the
            // group's 1-based number.
            let group_no = {
                let Some(dm) = self.tabs.get_mut(idx).and_then(|t| t.dupe_mode.as_mut()) else {
                    return;
                };
                dm.groups += 1;
                dm.wasted_bytes = dm
                    .wasted_bytes
                    .saturating_add(bytes_each.saturating_mul(distinct_occupants.saturating_sub(1) as u64));
                dm.groups
            };
            // Tag a member as a hard link when its (dev,inode) already
            // appeared in this group — those reclaim no space.
            let mut seen_ids: Vec<(u64, u64)> = Vec::new();
            for member in members {
                let is_hardlink = match member.file_id {
                    Some(id) => {
                        let dup = seen_ids.contains(&id);
                        if !dup {
                            seen_ids.push(id);
                        }
                        dup
                    }
                    None => false,
                };
                if let Some((entry, path)) = member_row(&fs, &member.path, &root, group_no, is_hardlink)
                {
                    paths.insert(entry.id, path);
                    entries.push(entry);
                }
            }
        }
        if entries.is_empty() {
            return;
        }
        for (id, path) in &paths {
            self.process
                .node_store
                .borrow_mut()
                .get_or_create_path_with_id(path.clone(), *id);
        }
        let heats: Vec<f32> = entries.iter().map(|e| self.ant_heat(e.id)).collect();
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        let table = tab.table.clone();
        table.update(cx, |state, cx| {
            state.delegate_mut().append_entries(entries, paths, heats);
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

/// Last component of the scan root for the task label.
fn short_root(root: &Path) -> String {
    root.file_name()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}
