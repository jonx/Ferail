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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ferail_core::{EnumerationError, FileEntry, NodeId};
use ferail_fs_native::perceptual::SimilarityCluster;
use ferail_fs_native::{
    DEFAULT_DUPE_BATCH, DupeFact, DupeHashCache, DupeMode, DupeOpts, DupeStats, NativeFs,
    SimilarImageIndexEntry,
};
use gpui::{AnyWindowHandle, Context, Pixels, SharedString};
use gpui_component::WindowExt;

use super::Shell;
use super::tab::{
    DupeGroupMember, DupeGroupView, SimilarImageIndexView, SimilarImageView, TabId,
    ToolResultSurface,
};
use crate::dupe_cache::DbHashCache;
use crate::feature_settings::DupeConfig;
use crate::tasks::TaskKind;

/// A batch of confirmed groups, **fully built on the worker thread**:
/// table rows, their paths, and the retained panel model, so the UI
/// thread only appends, never stats a file (prime directive).
#[derive(Default)]
pub(super) struct DupeBatch {
    /// Ready-to-append table rows.
    pub entries: Vec<FileEntry>,
    /// `NodeId → path` for the rows in `entries`.
    pub paths: HashMap<ferail_core::NodeId, PathBuf>,
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
    SimilarIndex {
        images: Vec<SimilarImageIndexEntry>,
        clusters: Vec<SimilarityCluster>,
    },
    Progress(DupeStats),
    Done(Option<EnumerationError>),
}

/// Hashing can outrun GPUI when most files form tiny groups. Keep only a
/// bounded train of already-built result batches; `send_blocking` then slows
/// the scanner instead of allowing scan-local paths and thumbnails to pile up.
const DUPE_CHANNEL_MESSAGES: usize = 16;
/// A busy scanner can refill the bounded channel while the foreground drains
/// it. Stop after one channel's worth even when it never becomes momentarily
/// empty, then yield a frame so duplicate-heavy scans cannot monopolise GPUI.
const DUPE_UI_MESSAGES_PER_TICK: usize = DUPE_CHANNEL_MESSAGES;
const DUPE_UI_TICK: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SimilarCriterion {
    Structure,
    Detail,
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
            let mut similar_index = None;
            for fact in facts {
                let (mode, full_hash, bytes_each, members) = match fact {
                    DupeFact::Group {
                        mode,
                        full_hash,
                        bytes_each,
                        members,
                        ..
                    } => (mode, full_hash, bytes_each, members),
                    DupeFact::SimilarIndex { images, clusters } => {
                        similar_index = Some((images, clusters));
                        continue;
                    }
                };
                group_no += 1;
                let mut gv_members = Vec::with_capacity(members.len());
                for m in members {
                    // Similar mode deliberately does not call
                    // file_entry_for_path: that helper registers paths in
                    // NativeFs's process-wide node map. Its cards need only
                    // this scan-local member model.
                    let (node, path, entry) = if mode == DupeMode::Similar {
                        (m.node, m.path.clone(), None)
                    } else {
                        let Some((entry, path)) =
                            member_row(&fs, &m.path, &root, group_no, m.is_hardlink, m.is_clone)
                        else {
                            continue;
                        };
                        (entry.id, path, Some(entry))
                    };
                    gv_members.push(DupeGroupMember {
                        node,
                        path: path.clone(),
                        mtime_unix: m.mtime_unix,
                        bytes: m.bytes,
                        is_hardlink: m.is_hardlink,
                        is_clone: m.is_clone,
                        image: m.image.map(|image| SimilarImageView {
                            width: image.width,
                            height: image.height,
                            dhash_distance: image.dhash_distance,
                            phash_distance: image.phash_distance,
                            is_best: image.is_best,
                            raw_thumbnail: image.thumbnail,
                            thumbnail: None,
                        }),
                    });
                    if let Some(entry) = entry {
                        batch.paths.insert(node, path);
                        batch.entries.push(entry);
                    }
                }
                if gv_members.len() < 2 {
                    continue; // a member vanished mid-scan; no longer a group
                }
                let keeper = if mode == DupeMode::Similar {
                    gv_members
                        .iter()
                        .find(|member| member.image.as_ref().is_some_and(|image| image.is_best))
                        .map(|member| member.node)
                } else {
                    None
                };
                let view = DupeGroupView {
                    group_no,
                    full_hash,
                    mode,
                    bytes_each,
                    members: gv_members,
                    expanded: true,
                    keeper,
                };
                batch.reclaimable = batch.reclaimable.saturating_add(view.reclaimable_bytes());
                batch.groups.push(view);
            }
            if !batch.groups.is_empty() && tx.send_blocking(DupeMsg::Batch(batch)).is_err() {
                cancel.store(true, Ordering::Relaxed);
            }
            if let Some((images, clusters)) = similar_index {
                if tx
                    .send_blocking(DupeMsg::SimilarIndex { images, clusters })
                    .is_err()
                {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        },
        |stats| {
            if tx.send_blocking(DupeMsg::Progress(stats)).is_err() {
                cancel.store(true, Ordering::Relaxed);
            }
        },
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
    let location = location_with_note(&location, is_hardlink, is_clone);
    entry.display_description = tr!(
        "#{group} \u{00B7} {location}",
        group = group_no,
        location = location
    )
    .to_string()
    .into();
    Some((entry, path.to_path_buf()))
}

/// The member's location with a trailing " · hard link" / " · clone, no
/// extra space" note when it reclaims nothing, or the bare location for a
/// storage-owning copy. Shared by the row description and the panel's
/// member line.
pub(super) fn location_with_note(
    location: &str,
    is_hardlink: bool,
    is_clone: bool,
) -> SharedString {
    if is_hardlink {
        tr!("{location} \u{00B7} hard link", location = location)
    } else if is_clone {
        tr!(
            "{location} \u{00B7} clone, no extra space",
            location = location
        )
    } else {
        SharedString::from(location.to_owned())
    }
}

impl Shell {
    /// Launch a duplicate-finder scan rooted at the tab's current
    /// directory, streaming grouped results into its list.
    pub fn start_duplicate_scan(
        &mut self,
        tab_id: TabId,
        mode: DupeMode,
        notify_window: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let root = self.tabs[idx].current_dir.clone();
        let config = DupeConfig::load();
        let presentation = if mode == DupeMode::Similar {
            crate::feature_settings::DupePresentation::Panel
        } else {
            config.presentation
        };
        let mut opts = config.opts();
        opts.mode = mode;

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
        // The directory listing's selection must not survive into the
        // result surface: the dupe panel reuses `tab.selection` as its
        // marked-for-trash set, so rows selected in the folder would
        // arrive pre-checked, one click on "Trash N marked" could
        // trash a file the user never marked in the panel.
        self.tabs[idx].clear_selection();
        self.tabs[idx].anchor = None;
        self.tabs[idx].lead = None;
        self.tabs[idx].range_live = false;
        self.tabs[idx].filtered_out.clear();
        self.tabs[idx].tool_result = Some(ToolResultSurface::duplicates(
            root.clone(),
            presentation,
            mode,
        ));
        self.similar_criteria_dragging = None;
        self.tabs[idx].dupe_groups.clear();
        self.tabs[idx].similar_image_index = Arc::new(Vec::new());
        self.tabs[idx].similar_regroup_generation =
            self.tabs[idx].similar_regroup_generation.wrapping_add(1);
        self.tabs[idx].dupe_panel_focus = None;
        self.tabs[idx].dupe_panel_scroll = crate::multi_table::VirtualListScrollHandle::new();
        let table = self.tabs[idx].table.clone();
        table.update(cx, |state, cx| {
            state.delegate_mut().clear();
            state.refresh(cx);
        });

        let cancel = Arc::new(AtomicBool::new(false));
        self.tabs[idx].load_cancel = Some(cancel.clone());
        let label = if mode == DupeMode::Similar {
            tr!("Finding similar images in {root}", root = short_root(&root))
        } else {
            tr!("Finding duplicates in {root}", root = short_root(&root))
        };
        let task = self.process.tasks.borrow_mut().begin_with_cancel(
            TaskKind::DuplicateScan,
            label,
            cancel.clone(),
        );
        if let Some(previous) = self.tabs[idx].load_task.replace(task) {
            self.process.tasks.borrow_mut().end(previous);
        }

        // Exact hashes may use the metadata DB. Similar-image signatures and
        // previews are deliberately scan-local and never persisted.
        let cache = (mode == DupeMode::Exact)
            .then(|| self.process.db_snapshot())
            .flatten()
            .map(|db| {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                DbHashCache::new(db, now)
            });
        let fs = self.process.fs.clone();
        let (tx, rx) = async_channel::bounded(DUPE_CHANNEL_MESSAGES);
        cx.background_executor()
            .spawn(async move {
                run_dupe_load(fs, opts, cache, root, cancel, tx);
            })
            .detach();

        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                let mut batch: Option<DupeBatch> = None;
                let mut similar_index = None;
                let mut progress = None;
                let mut done_error: Option<Option<EnumerationError>> = None;
                let mut messages = 1usize;

                absorb_dupe_msg(
                    msg,
                    &mut batch,
                    &mut similar_index,
                    &mut progress,
                    &mut done_error,
                );
                while done_error.is_none() && messages < DUPE_UI_MESSAGES_PER_TICK {
                    match rx.try_recv() {
                        Ok(msg) => {
                            messages += 1;
                            absorb_dupe_msg(
                                msg,
                                &mut batch,
                                &mut similar_index,
                                &mut progress,
                                &mut done_error,
                            );
                        }
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
                        if let Some((images, clusters)) = similar_index {
                            this.apply_similar_index_in_tab(idx, images, clusters, cx);
                        }
                        if let Some(progress) = progress {
                            this.apply_dupe_progress_in_tab(idx, progress, cx);
                        }
                        if let Some(error) = done_error {
                            this.apply_dupe_msg_in_tab(
                                idx,
                                DupeMsg::Done(error),
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
                cx.background_executor().timer(DUPE_UI_TICK).await;
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
        let visible = idx == self.active;
        match msg {
            DupeMsg::Batch(batch) => self.apply_dupe_batch_in_tab(idx, batch, cx),
            DupeMsg::SimilarIndex { images, clusters } => {
                self.apply_similar_index_in_tab(idx, images, clusters, cx)
            }
            DupeMsg::Progress(progress) => self.apply_dupe_progress_in_tab(idx, progress, cx),
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
                let mode = self.tabs[idx]
                    .tool_result
                    .as_ref()
                    .and_then(|surface| surface.dupe_mode())
                    .map(|mode| mode.mode)
                    .unwrap_or(DupeMode::Exact);
                let mut surfaced = false;
                if let Some(tab) = self.tabs.get_mut(idx) {
                    if let Some(id) = tab.load_task.take() {
                        surfaced = self.process.tasks.borrow_mut().end_and_was_surfaced(id);
                    }
                    tab.load_cancel = None;
                    if visible {
                        cx.notify();
                    }
                }
                if let Some(window) = notify_window {
                    if let Some(error) = error {
                        let title = if mode == DupeMode::Similar {
                            tr!("Similar image scan")
                        } else {
                            tr!("Duplicate scan")
                        };
                        let message = super::enumeration_error_message(&title, &error);
                        let _ = window.update(cx, |_, window, cx| {
                            use gpui_component::notification::Notification;
                            window.push_notification(Notification::error(message), cx);
                        });
                    } else if surfaced {
                        let message = if mode == DupeMode::Similar && groups == 0 {
                            tr!("Similar image scan finished: no similar images found")
                        } else if groups == 0 {
                            tr!("Duplicate scan finished: no duplicates found")
                        } else if mode == DupeMode::Similar {
                            trn!(
                                "Similar image scan finished: {n} group · {reclaimable} reclaimable",
                                "Similar image scan finished: {n} groups · {reclaimable} reclaimable",
                                groups,
                                reclaimable = ferail_fs_native::humanize_bytes(reclaimable)
                            )
                        } else {
                            trn!(
                                "Duplicate scan finished: {n} group \u{00B7} {reclaimable} reclaimable",
                                "Duplicate scan finished: {n} groups \u{00B7} {reclaimable} reclaimable",
                                groups,
                                reclaimable = ferail_fs_native::humanize_bytes(reclaimable)
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

    fn apply_dupe_progress_in_tab(
        &mut self,
        idx: usize,
        progress: DupeStats,
        cx: &mut Context<Self>,
    ) {
        let visible = idx == self.active;
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        if let Some(mode) = tab
            .tool_result
            .as_mut()
            .and_then(|surface| surface.dupe_mode_mut())
        {
            mode.progress = progress;
        }
        if let Some(task) = tab.load_task {
            match progress.phase {
                ferail_fs_native::DupePhase::Enumerating => {}
                ferail_fs_native::DupePhase::Analyzing if progress.images_total > 0 => {
                    self.process.tasks.borrow_mut().update(
                        task,
                        0.9 * progress.images_analyzed as f32 / progress.images_total as f32,
                    );
                }
                ferail_fs_native::DupePhase::Analyzing => {}
                ferail_fs_native::DupePhase::Grouping => {
                    // Pairing and result-thumbnail preparation have no cheap
                    // exact denominator. Reserve the tail instead of showing
                    // a misleading 100% before the result index is ready.
                    self.process.tasks.borrow_mut().update(task, 0.95);
                }
            }
        }
        if visible {
            cx.notify();
        }
    }

    /// Land the one scan-local perceptual index. Raw thumbnails become GPUI
    /// images exactly once; paths and signatures stay on this tab only.
    fn apply_similar_index_in_tab(
        &mut self,
        idx: usize,
        images: Vec<SimilarImageIndexEntry>,
        clusters: Vec<SimilarityCluster>,
        cx: &mut Context<Self>,
    ) {
        let index = images
            .into_iter()
            .map(|image| {
                let thumbnail = image.thumbnail.as_ref().map(|raw| {
                    Arc::new(crate::icons::build_render_image(
                        raw.rgba.clone(),
                        raw.width,
                        raw.height,
                    ))
                });
                SimilarImageIndexView {
                    node: image.node,
                    path: image.path,
                    mtime_unix: image.mtime_unix,
                    bytes: image.bytes,
                    file_id: image.file_id,
                    clone_key: image.clone_key,
                    signature: image.signature,
                    thumbnail,
                }
            })
            .collect::<Vec<_>>();
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        tab.similar_image_index = Arc::new(index);
        tab.similar_regroup_generation = tab.similar_regroup_generation.wrapping_add(1);
        self.apply_similar_clusters_in_tab(idx, clusters, cx);
    }

    fn apply_similar_clusters_in_tab(
        &mut self,
        idx: usize,
        clusters: Vec<SimilarityCluster>,
        cx: &mut Context<Self>,
    ) {
        let visible = idx == self.active;
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        let groups = similar_group_views(&tab.similar_image_index, &clusters);
        let old_focus_node = tab.dupe_panel_focus.map(|(_, node)| node);
        tab.clear_selection();
        tab.dupe_groups = groups;
        tab.dupe_panel_focus = old_focus_node.and_then(|node| {
            tab.dupe_groups.iter().find_map(|group| {
                group
                    .members
                    .iter()
                    .any(|member| member.node == node)
                    .then_some((group.group_no, node))
            })
        });
        if let Some(mode) = tab
            .tool_result
            .as_mut()
            .and_then(|surface| surface.dupe_mode_mut())
        {
            mode.groups = tab.dupe_groups.len();
            mode.wasted_bytes = tab
                .dupe_groups
                .iter()
                .map(DupeGroupView::reclaimable_bytes)
                .sum();
        }
        if visible {
            cx.notify();
        }
    }

    /// Update one or both live criteria and debounce a pure, off-thread
    /// regroup. The retained signatures are `Copy`; no path is opened and no
    /// thumbnail is decoded on this path.
    pub(super) fn update_similar_criteria(
        &mut self,
        tab_id: TabId,
        structure: Option<u32>,
        detail: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        let Some(mode) = self.tabs[idx]
            .tool_result
            .as_mut()
            .and_then(|surface| surface.dupe_mode_mut())
        else {
            return;
        };
        if mode.mode != DupeMode::Similar {
            return;
        }
        let previous = mode.similarity_criteria;
        if let Some(value) = structure {
            mode.similarity_criteria.structure =
                value.min(ferail_fs_native::perceptual::DHASH_MAX_DISTANCE);
        }
        if let Some(value) = detail {
            mode.similarity_criteria.detail =
                value.min(ferail_fs_native::perceptual::PHASH_MAX_DISTANCE);
        }
        let criteria = mode.similarity_criteria;
        if criteria == previous {
            return;
        }
        self.tabs[idx].similar_regroup_generation =
            self.tabs[idx].similar_regroup_generation.wrapping_add(1);
        let generation = self.tabs[idx].similar_regroup_generation;
        cx.notify();

        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(75))
                .await;
            let snapshot = this
                .update(cx, |this, _cx| {
                    let idx = this.tabs.iter().position(|tab| tab.id == tab_id)?;
                    (this.tabs[idx].similar_regroup_generation == generation).then(|| {
                        this.tabs[idx]
                            .similar_image_index
                            .iter()
                            .map(|image| image.signature)
                            .collect::<Vec<_>>()
                    })
                })
                .ok()
                .flatten();
            let Some(signatures) = snapshot else {
                return;
            };
            let clusters = cx
                .background_executor()
                .spawn(async move {
                    ferail_fs_native::perceptual::similarity_clusters_with(&signatures, criteria)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(idx) = this.tabs.iter().position(|tab| {
                    tab.id == tab_id && tab.similar_regroup_generation == generation
                }) {
                    this.apply_similar_clusters_in_tab(idx, clusters, cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn begin_similar_criteria_drag(
        &mut self,
        criterion: SimilarCriterion,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
        let tab_id = self.active_tab().id;
        self.similar_criteria_dragging = Some((tab_id, criterion));
        self.set_similar_criterion_from_x(tab_id, criterion, x, cx);
    }

    fn set_similar_criterion_from_x(
        &mut self,
        tab_id: TabId,
        criterion: SimilarCriterion,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
        let bounds = match criterion {
            SimilarCriterion::Structure => self.similar_structure_track,
            SimilarCriterion::Detail => self.similar_detail_track,
        };
        let Some(value) = similar_criterion_range(criterion).value_at(bounds, x) else {
            return;
        };
        let value = value as u32;
        match criterion {
            SimilarCriterion::Structure => {
                self.update_similar_criteria(tab_id, Some(value), None, cx)
            }
            SimilarCriterion::Detail => self.update_similar_criteria(tab_id, None, Some(value), cx),
        }
    }

    pub(super) fn on_similar_criteria_drag(
        &mut self,
        event: &gpui::MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        let Some((tab_id, criterion)) = self.similar_criteria_dragging else {
            return;
        };
        if event.pressed_button != Some(gpui::MouseButton::Left) {
            self.end_similar_criteria_drag();
            return;
        }
        self.set_similar_criterion_from_x(tab_id, criterion, event.position.x, cx);
    }

    pub(super) fn end_similar_criteria_drag(&mut self) {
        self.similar_criteria_dragging = None;
    }

    pub(super) fn reset_similar_criteria(&mut self, cx: &mut Context<Self>) {
        let tab_id = self.active_tab().id;
        self.similar_criteria_dragging = None;
        self.update_similar_criteria(
            tab_id,
            Some(ferail_fs_native::perceptual::DHASH_MAX_DISTANCE),
            Some(ferail_fs_native::perceptual::PHASH_MAX_DISTANCE),
            cx,
        );
    }

    /// Apply one worker-built batch. Pure data shuffling: the rows and
    /// the panel model were already built off the UI thread, so this only
    /// registers node ids, bumps the running totals, and appends.
    fn apply_dupe_batch_in_tab(
        &mut self,
        idx: usize,
        mut batch: DupeBatch,
        cx: &mut Context<Self>,
    ) {
        let visible = idx == self.active;
        if batch.entries.is_empty() && batch.groups.is_empty() {
            return;
        }
        let dupe_display = self
            .tabs
            .get(idx)
            .and_then(|t| t.tool_result.as_ref())
            .and_then(|surface| surface.dupe_mode())
            .map(|dm| (dm.presentation, dm.mode));
        let panel_mode = dupe_display.is_some_and(|(presentation, _)| {
            presentation == crate::feature_settings::DupePresentation::Panel
        });
        // Similar-image paths belong only to the active panel. Do not copy
        // them into the process-wide node store, where they could outlive it.
        if !dupe_display.is_some_and(|(_, mode)| mode == DupeMode::Similar) {
            for (id, path) in &batch.paths {
                self.process
                    .node_store
                    .borrow_mut()
                    .get_or_create_path_with_id(path.clone(), *id);
            }
        }
        for group in &mut batch.groups {
            for member in &mut group.members {
                let Some(image) = member.image.as_mut() else {
                    continue;
                };
                let Some(raw) = image.raw_thumbnail.take() else {
                    continue;
                };
                image.thumbnail = Some(Arc::new(crate::icons::build_render_image(
                    raw.rgba, raw.width, raw.height,
                )));
            }
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
            if visible {
                cx.notify();
            }
            return;
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
        let Some(tab) = self.tabs.get(idx) else {
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
        // its row has streamed in, same as the directory load path.
        self.apply_pending_select_row_in_tab(idx, cx);
        if visible {
            cx.notify();
        }
    }

    /// Deterministic Similar Images state for the headless screenshot harness.
    /// The paths and pixels are synthetic; no filesystem or metadata DB is read.
    pub(crate) fn seed_similar_images_for_screenshot(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.active_tab_mut().load_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let root = PathBuf::from("/Sample Library");
        let specs = [
            (
                9_100_001,
                "coast-original.jpg",
                4032,
                3024,
                7_800_000,
                0,
                0,
                true,
                0x3e87a8,
            ),
            (
                9_100_002,
                "coast-edited.jpg",
                3024,
                2268,
                4_200_000,
                3,
                5,
                false,
                0x4a91ad,
            ),
            (
                9_100_003,
                "coast-thumbnail.jpg",
                800,
                600,
                310_000,
                4,
                7,
                false,
                0x5799b2,
            ),
        ];
        let members = specs
            .into_iter()
            .map(
                |(id, name, width, height, bytes, dhash, phash, is_best, color)| DupeGroupMember {
                    node: NodeId::from(id),
                    path: root.join(name),
                    mtime_unix: 1_700_000_000 + id as i64,
                    bytes,
                    is_hardlink: false,
                    is_clone: false,
                    image: Some(SimilarImageView {
                        width,
                        height,
                        dhash_distance: dhash,
                        phash_distance: phash,
                        is_best,
                        raw_thumbnail: None,
                        thumbnail: Some(similar_fixture_preview(color)),
                    }),
                },
            )
            .collect::<Vec<_>>();
        let group = DupeGroupView {
            group_no: 1,
            full_hash: String::new(),
            mode: DupeMode::Similar,
            bytes_each: 0,
            keeper: Some(NodeId::from(9_100_001)),
            members,
            expanded: true,
        };
        let reclaimable = group.reclaimable_bytes();
        let tab = self.active_tab_mut();
        tab.current_dir = root.clone();
        tab.clear_selection();
        tab.dupe_groups = vec![group];
        tab.similar_image_index = Arc::new(
            tab.dupe_groups[0]
                .members
                .iter()
                .map(|member| {
                    let image = member.image.as_ref().expect("similar screenshot image");
                    SimilarImageIndexView {
                        node: member.node,
                        path: member.path.clone(),
                        mtime_unix: member.mtime_unix,
                        bytes: member.bytes,
                        file_id: None,
                        clone_key: None,
                        signature: ferail_fs_native::perceptual::PerceptualSignature {
                            dhash: distance_mask(image.dhash_distance),
                            phash: distance_mask(image.phash_distance),
                            mean_rgb: [112, 138, 154],
                            luma_variance: 96,
                            width: image.width,
                            height: image.height,
                        },
                        thumbnail: image.thumbnail.clone(),
                    }
                })
                .collect(),
        );
        tab.dupe_panel_focus = Some((1, NodeId::from(9_100_001)));
        tab.tool_result = Some(ToolResultSurface::duplicates(
            root,
            crate::feature_settings::DupePresentation::Panel,
            DupeMode::Similar,
        ));
        if let Some(dm) = tab
            .tool_result
            .as_mut()
            .and_then(|surface| surface.dupe_mode_mut())
        {
            dm.groups = 1;
            dm.wasted_bytes = reclaimable;
        }
        cx.notify();
    }
}

fn distance_mask(distance: u32) -> u64 {
    if distance == 0 {
        0
    } else {
        (1u64 << distance.min(63)) - 1
    }
}

pub(super) fn similar_criterion_range(
    criterion: SimilarCriterion,
) -> crate::scrub_slider::ScrubRange {
    let max = match criterion {
        SimilarCriterion::Structure => ferail_fs_native::perceptual::DHASH_MAX_DISTANCE,
        SimilarCriterion::Detail => ferail_fs_native::perceptual::PHASH_MAX_DISTANCE,
    };
    crate::scrub_slider::ScrubRange::new(0.0, max as f32, 1.0)
}

fn similar_fixture_preview(color: u32) -> Arc<gpui::RenderImage> {
    const WIDTH: u32 = 96;
    const HEIGHT: u32 = 64;
    let base = [
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        (color & 0xff) as u8,
    ];
    let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let light = ((x + y) % 29) as u8;
            rgba.extend_from_slice(&[
                base[0].saturating_add(light / 3),
                base[1].saturating_add(light / 2),
                base[2].saturating_add(light),
                255,
            ]);
        }
    }
    Arc::new(crate::icons::build_render_image(rgba, WIDTH, HEIGHT))
}

fn absorb_dupe_msg(
    msg: DupeMsg,
    batch: &mut Option<DupeBatch>,
    similar_index: &mut Option<(Vec<SimilarImageIndexEntry>, Vec<SimilarityCluster>)>,
    progress: &mut Option<DupeStats>,
    done_error: &mut Option<Option<EnumerationError>>,
) {
    match msg {
        DupeMsg::Batch(next) => match batch {
            Some(acc) => acc.append(next),
            None => *batch = Some(next),
        },
        DupeMsg::SimilarIndex { images, clusters } => {
            *similar_index = Some((images, clusters));
        }
        DupeMsg::Progress(next) => *progress = Some(next),
        DupeMsg::Done(error) => *done_error = Some(error),
    }
}

/// Pure projection from compact index + clustering into the existing card
/// model. Storage-sharing flags are recomputed from scan-captured identities,
/// so changing criteria never stats or opens a path on the UI thread.
fn similar_group_views(
    index: &[SimilarImageIndexView],
    clusters: &[SimilarityCluster],
) -> Vec<DupeGroupView> {
    clusters
        .iter()
        .filter_map(|cluster| {
            let reference = index.get(cluster.medoid)?.signature;
            let mut member_indices = cluster
                .members
                .iter()
                .copied()
                .filter(|member| *member < index.len())
                .collect::<Vec<_>>();
            if member_indices.len() < 2 {
                return None;
            }
            member_indices.sort_by(|a, b| {
                let a = &index[*a];
                let b = &index[*b];
                b.signature
                    .pixel_area()
                    .cmp(&a.signature.pixel_area())
                    .then_with(|| b.bytes.cmp(&a.bytes))
                    .then_with(|| {
                        let a_time = if a.mtime_unix > 0 {
                            a.mtime_unix
                        } else {
                            i64::MAX
                        };
                        let b_time = if b.mtime_unix > 0 {
                            b.mtime_unix
                        } else {
                            i64::MAX
                        };
                        a_time.cmp(&b_time)
                    })
                    .then_with(|| a.path.cmp(&b.path))
            });

            let mut seen_files = std::collections::HashSet::new();
            let mut seen_clones = std::collections::HashSet::new();
            let mut members = Vec::with_capacity(member_indices.len());
            for (position, member_index) in member_indices.into_iter().enumerate() {
                let source = &index[member_index];
                let is_hardlink = source.file_id.is_some_and(|id| !seen_files.insert(id));
                let is_clone =
                    !is_hardlink && source.clone_key.is_some_and(|key| !seen_clones.insert(key));
                let (dhash_distance, phash_distance) =
                    ferail_fs_native::perceptual::hash_distances(reference, source.signature);
                members.push(DupeGroupMember {
                    node: source.node,
                    path: source.path.clone(),
                    mtime_unix: source.mtime_unix,
                    bytes: source.bytes,
                    is_hardlink,
                    is_clone,
                    image: Some(SimilarImageView {
                        width: source.signature.width,
                        height: source.signature.height,
                        dhash_distance,
                        phash_distance,
                        is_best: position == 0,
                        raw_thumbnail: None,
                        thumbnail: source.thumbnail.clone(),
                    }),
                });
            }
            let keeper = members.first().map(|member| member.node);
            Some(DupeGroupView {
                group_no: 0,
                full_hash: String::new(),
                mode: DupeMode::Similar,
                bytes_each: 0,
                members,
                expanded: true,
                keeper,
            })
        })
        .enumerate()
        .map(|(index, mut group)| {
            group.group_no = index + 1;
            group
        })
        .collect()
}

/// Last component of the scan root for the task label.
fn short_root(root: &Path) -> String {
    root.file_name()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferail_fs_native::perceptual::PerceptualSignature;

    fn indexed(id: u64, width: u32, height: u32, bytes: u64) -> SimilarImageIndexView {
        SimilarImageIndexView {
            node: NodeId::from(id),
            path: PathBuf::from(format!("/private/photo-{id}.jpg")),
            mtime_unix: id as i64,
            bytes,
            file_id: None,
            clone_key: None,
            signature: PerceptualSignature {
                dhash: id - 1,
                phash: id - 1,
                mean_rgb: [100; 3],
                luma_variance: 100,
                width,
                height,
            },
            thumbnail: None,
        }
    }

    #[test]
    fn live_regroup_projection_keeps_the_highest_quality_image() {
        let index = vec![
            indexed(1, 800, 600, 300),
            indexed(2, 4_000, 3_000, 8_000),
            indexed(3, 2_000, 1_500, 3_000),
        ];
        let groups = similar_group_views(
            &index,
            &[SimilarityCluster {
                medoid: 0,
                members: vec![0, 1, 2],
            }],
        );

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].keeper, Some(NodeId::from(2)));
        assert!(
            groups[0].members[0]
                .image
                .as_ref()
                .is_some_and(|image| image.is_best)
        );
        assert_eq!(groups[0].reclaimable_bytes(), 3_300);
    }
}
