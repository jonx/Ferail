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

use ferail_core::{EnumerationError, FileEntry, NodeId};
use ferail_fs_native::{DEFAULT_DUPE_BATCH, DupeFact, DupeHashCache, DupeMode, DupeOpts, NativeFs};
use gpui::{AnyWindowHandle, Context, SharedString};
use gpui_component::WindowExt;

use super::Shell;
use super::tab::{DupeGroupMember, DupeGroupView, SimilarImageView, TabId, ToolResultSurface};
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
                    mode,
                    full_hash,
                    bytes_each,
                    members,
                    ..
                } = fact;
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
                        let Some((entry, path)) = member_row(
                            &fs,
                            &m.path,
                            &root,
                            group_no,
                            m.is_hardlink,
                            m.is_clone,
                        ) else {
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
    let location = location_with_note(&location, is_hardlink, is_clone);
    entry.display_description = tr!(
        "#{group} \u{00B7} {location}",
        group = group_no,
        location = location
    )
    .to_string();
    Some((entry, path.to_path_buf()))
}

/// The member's location with a trailing " · hard link" / " · clone — no
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
            "{location} \u{00B7} clone \u{2014} no extra space",
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
        // arrive pre-checked — one click on "Trash N marked" could
        // trash a file the user never marked in the panel.
        self.tabs[idx].selection.clear();
        self.tabs[idx].anchor = None;
        self.tabs[idx].lead = None;
        self.tabs[idx].range_live = false;
        self.tabs[idx].filtered_out.clear();
        self.tabs[idx].tool_result = Some(ToolResultSurface::duplicates(
            root.clone(),
            presentation,
            mode,
        ));
        self.tabs[idx].dupe_groups.clear();
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
                    cx.notify();
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

    /// Apply one worker-built batch. Pure data shuffling — the rows and
    /// the panel model were already built off the UI thread, so this only
    /// registers node ids, bumps the running totals, and appends.
    fn apply_dupe_batch_in_tab(
        &mut self,
        idx: usize,
        mut batch: DupeBatch,
        cx: &mut Context<Self>,
    ) {
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
        tab.selection.clear();
        tab.dupe_groups = vec![group];
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
