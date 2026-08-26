//! Process-owned thumbnail dispatch.
//!
//! File-list surfaces submit only viewport seeds. Paths and GPUI handles stay
//! in this host-side payload store; `ferail_core::asset_work` sees compact
//! identities, revisions, priorities and cancellation state only.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ferail_core::NodeId;
use ferail_core::asset_work::{
    AssetIdentity, AssetKey, AssetKind, AssetLane, AssetPriority, AssetRevision,
    AssetWorkCoordinator, AssetWorkRequest, AssetWorkScope, StartedAssetWork, SubmitOutcome,
};
use ferail_core::revision_cache::FileRevision;
use gpui::{App, AsyncApp, RenderImage, WeakEntity};

use crate::file_list::FileListDelegate;
use crate::multi_table::TableState;
use crate::shell::Shell;
use crate::thumbnails::ThumbnailCache;

const DISPATCH_SCOPE: AssetWorkScope = AssetWorkScope(u64::MAX);
const UPLOADS_PER_FRAME: usize = 2;
const APPLIES_PER_FRAME: usize = 8;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Clone)]
pub(crate) struct ThumbnailSeed {
    pub row_ix: usize,
    pub node: NodeId,
    pub path: PathBuf,
    pub revision: FileRevision,
    pub size_px: u32,
    pub priority: AssetPriority,
    pub surface_local_identity: bool,
}

#[derive(Clone)]
struct ThumbnailWaiter {
    table: WeakEntity<TableState<FileListDelegate>>,
    target: ThumbnailTarget,
    scope: AssetWorkScope,
    generation: u64,
    row_ix: usize,
    node: NodeId,
}

#[derive(Clone)]
pub(crate) enum ThumbnailTarget {
    Table(WeakEntity<TableState<FileListDelegate>>),
    Shell(WeakEntity<Shell>),
}

pub(crate) struct ThumbnailSubscription {
    pub table: WeakEntity<TableState<FileListDelegate>>,
    pub target: ThumbnailTarget,
    pub scope: AssetWorkScope,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct WorkId {
    key: AssetKey,
    generation: u64,
}

impl WorkId {
    fn from_request(request: AssetWorkRequest) -> Self {
        Self {
            key: request.key,
            generation: request.generation,
        }
    }
}

struct ThumbnailJob {
    request: AssetWorkRequest,
    path: PathBuf,
    size_px: u32,
}

struct ProviderCompletion {
    started: StartedAssetWork,
    path: PathBuf,
    size_px: u32,
    rgba: Option<(Vec<u8>, u32, u32)>,
}

struct UploadedCompletion {
    request: AssetWorkRequest,
    path: PathBuf,
    size_px: u32,
    image: Option<Arc<RenderImage>>,
}

struct RowNotification {
    waiter: ThumbnailWaiter,
}

pub(crate) struct ThumbnailDispatcher {
    next_generation: u64,
    pending_jobs: HashMap<WorkId, ThumbnailJob>,
    live_by_key: HashMap<AssetKey, WorkId>,
    waiters: HashMap<AssetKey, Vec<ThumbnailWaiter>>,
    upload_payloads: HashMap<WorkId, ProviderCompletion>,
    apply_payloads: HashMap<WorkId, UploadedCompletion>,
    wake_tx: async_channel::Sender<()>,
    wake_rx: async_channel::Receiver<()>,
    completion_tx: async_channel::Sender<ProviderCompletion>,
    completion_rx: async_channel::Receiver<ProviderCompletion>,
}

impl ThumbnailDispatcher {
    pub(crate) fn new() -> Self {
        let (wake_tx, wake_rx) = async_channel::bounded(1);
        let (completion_tx, completion_rx) = async_channel::unbounded();
        Self {
            next_generation: 0,
            pending_jobs: HashMap::new(),
            live_by_key: HashMap::new(),
            waiters: HashMap::new(),
            upload_payloads: HashMap::new(),
            apply_payloads: HashMap::new(),
            wake_tx,
            wake_rx,
            completion_tx,
            completion_rx,
        }
    }

    pub(crate) fn wake_receiver(&self) -> async_channel::Receiver<()> {
        self.wake_rx.clone()
    }

    pub(crate) fn submit(
        &mut self,
        work: &mut AssetWorkCoordinator,
        cache: &mut ThumbnailCache,
        subscription: ThumbnailSubscription,
        seeds: Vec<ThumbnailSeed>,
    ) {
        self.retain_surface_generation(work, cache, subscription.scope, subscription.generation);
        for seed in seeds {
            if cache.is_resolved(&seed.path, seed.size_px) {
                continue;
            }
            let identity = if seed.surface_local_identity {
                AssetIdentity::SurfaceFile {
                    surface: subscription.scope.0,
                    node: seed.node,
                }
            } else {
                AssetIdentity::File(seed.node)
            };
            let key = AssetKey {
                identity,
                revision: AssetRevision::File(seed.revision),
                kind: AssetKind::ContentThumbnail {
                    size_px: seed.size_px.min(u16::MAX as u32) as u16,
                },
            };
            let waiter = ThumbnailWaiter {
                table: subscription.table.clone(),
                target: subscription.target.clone(),
                scope: subscription.scope,
                generation: subscription.generation,
                row_ix: seed.row_ix,
                node: seed.node,
            };
            if let Some(id) = self.live_by_key.get(&key).copied() {
                self.waiters.entry(key).or_default().push(waiter);
                if let Some(job) = self.pending_jobs.get_mut(&id)
                    && seed.priority > job.request.priority
                {
                    let reprioritized = AssetWorkRequest {
                        priority: seed.priority,
                        ..job.request
                    };
                    let _ = work.submit(AssetLane::Provider, reprioritized);
                    job.request = reprioritized;
                }
                continue;
            }

            self.next_generation = self.next_generation.wrapping_add(1).max(1);
            let request = AssetWorkRequest {
                key,
                scope: DISPATCH_SCOPE,
                generation: self.next_generation,
                priority: seed.priority,
            };
            let submit = work.submit_detailed(AssetLane::Provider, request);
            if let Some(evicted) = submit.evicted {
                self.discard_pending(work, cache, evicted);
            }
            if matches!(
                submit.outcome,
                SubmitOutcome::QueueFull | SubmitOutcome::AlreadyScheduled
            ) {
                continue;
            }
            let id = WorkId::from_request(request);
            cache.mark_in_flight(seed.path.clone(), seed.size_px);
            self.live_by_key.insert(key, id);
            self.waiters.insert(key, vec![waiter]);
            self.pending_jobs.insert(
                id,
                ThumbnailJob {
                    request,
                    path: seed.path,
                    size_px: seed.size_px,
                },
            );
        }
        let _ = self.wake_tx.try_send(());
    }

    fn retain_surface_generation(
        &mut self,
        work: &mut AssetWorkCoordinator,
        cache: &mut ThumbnailCache,
        scope: AssetWorkScope,
        generation: u64,
    ) {
        let keys: Vec<AssetKey> = self.waiters.keys().copied().collect();
        for key in keys {
            let Some(waiters) = self.waiters.get_mut(&key) else {
                continue;
            };
            waiters.retain(|waiter| waiter.scope != scope || waiter.generation == generation);
            if !waiters.is_empty() {
                continue;
            }
            self.waiters.remove(&key);
            let Some(id) = self.live_by_key.get(&key).copied() else {
                continue;
            };
            // A payload still in `pending_jobs` has not started and can be
            // removed without consuming a provider call. Active work is left
            // alone: it is bounded, may already be inside COM, and its cache
            // result can be reused if another surface asks before completion.
            if self.pending_jobs.contains_key(&id)
                && let Some(job) = self.pending_jobs.remove(&id)
            {
                let _ = work.cancel(AssetLane::Provider, &job.request);
                self.live_by_key.remove(&key);
                cache.cancel_in_flight(std::iter::once(&job.path), job.size_px);
            }
        }
    }

    fn discard_pending(
        &mut self,
        _work: &mut AssetWorkCoordinator,
        cache: &mut ThumbnailCache,
        request: AssetWorkRequest,
    ) {
        let id = WorkId::from_request(request);
        if let Some(job) = self.pending_jobs.remove(&id) {
            self.live_by_key.remove(&request.key);
            self.waiters.remove(&request.key);
            cache.cancel_in_flight(std::iter::once(&job.path), job.size_px);
        }
    }

    fn take_provider_starts(
        &mut self,
        work: &mut AssetWorkCoordinator,
    ) -> Vec<(StartedAssetWork, ThumbnailJob)> {
        let mut starts = Vec::new();
        while let Some(started) = work.start_next(AssetLane::Provider) {
            let id = WorkId::from_request(started.request);
            if let Some(job) = self.pending_jobs.remove(&id) {
                starts.push((started, job));
            } else {
                let _ = work.complete(AssetLane::Provider, &started.request);
            }
        }
        starts
    }

    fn accept_provider_completions(
        &mut self,
        work: &mut AssetWorkCoordinator,
        cache: &mut ThumbnailCache,
    ) {
        while let Ok(completion) = self.completion_rx.try_recv() {
            let request = completion.started.request;
            let id = WorkId::from_request(request);
            let _ = work.complete(AssetLane::Provider, &request);
            if completion.started.is_cancelled() {
                self.finish_without_result(
                    cache,
                    request.key,
                    &completion.path,
                    completion.size_px,
                );
                continue;
            }
            let submit = work.submit_detailed(AssetLane::Upload, request);
            if let Some(evicted) = submit.evicted {
                self.discard_upload(cache, evicted);
            }
            if matches!(
                submit.outcome,
                SubmitOutcome::QueueFull | SubmitOutcome::AlreadyScheduled
            ) {
                self.finish_without_result(
                    cache,
                    request.key,
                    &completion.path,
                    completion.size_px,
                );
                continue;
            }
            self.upload_payloads.insert(id, completion);
        }
    }

    fn discard_upload(&mut self, cache: &mut ThumbnailCache, request: AssetWorkRequest) {
        let id = WorkId::from_request(request);
        if let Some(completion) = self.upload_payloads.remove(&id) {
            self.finish_without_result(cache, request.key, &completion.path, completion.size_px);
        }
    }

    fn finish_without_result(
        &mut self,
        cache: &mut ThumbnailCache,
        key: AssetKey,
        path: &PathBuf,
        size_px: u32,
    ) {
        self.live_by_key.remove(&key);
        self.waiters.remove(&key);
        cache.cancel_in_flight(std::iter::once(path), size_px);
    }

    fn drain_ui_frame(
        &mut self,
        work: &mut AssetWorkCoordinator,
        cache: &mut ThumbnailCache,
    ) -> Vec<RowNotification> {
        for _ in 0..UPLOADS_PER_FRAME {
            let Some(started) = work.start_next(AssetLane::Upload) else {
                break;
            };
            let request = started.request;
            let id = WorkId::from_request(request);
            let Some(completion) = self.upload_payloads.remove(&id) else {
                let _ = work.complete(AssetLane::Upload, &request);
                continue;
            };
            let image = completion.rgba.map(|(bytes, width, height)| {
                Arc::new(crate::icons::build_render_image(bytes, width, height))
            });
            let _ = work.complete(AssetLane::Upload, &request);
            let submit = work.submit_detailed(AssetLane::Apply, request);
            if let Some(evicted) = submit.evicted {
                self.discard_apply(cache, evicted);
            }
            if matches!(
                submit.outcome,
                SubmitOutcome::QueueFull | SubmitOutcome::AlreadyScheduled
            ) {
                self.finish_without_result(
                    cache,
                    request.key,
                    &completion.path,
                    completion.size_px,
                );
                continue;
            }
            self.apply_payloads.insert(
                id,
                UploadedCompletion {
                    request,
                    path: completion.path,
                    size_px: completion.size_px,
                    image,
                },
            );
        }

        let mut notifications = Vec::new();
        for _ in 0..APPLIES_PER_FRAME {
            let Some(started) = work.start_next(AssetLane::Apply) else {
                break;
            };
            let id = WorkId::from_request(started.request);
            let Some(completion) = self.apply_payloads.remove(&id) else {
                let _ = work.complete(AssetLane::Apply, &started.request);
                continue;
            };
            cache.insert_rendered(completion.path, completion.size_px, completion.image);
            self.live_by_key.remove(&completion.request.key);
            if let Some(waiters) = self.waiters.remove(&completion.request.key) {
                notifications.extend(waiters.into_iter().map(|waiter| RowNotification { waiter }));
            }
            let _ = work.complete(AssetLane::Apply, &completion.request);
        }
        notifications
    }

    fn discard_apply(&mut self, cache: &mut ThumbnailCache, request: AssetWorkRequest) {
        let id = WorkId::from_request(request);
        if let Some(completion) = self.apply_payloads.remove(&id) {
            self.finish_without_result(cache, request.key, &completion.path, completion.size_px);
        }
    }

    fn has_ui_work(&self, work: &AssetWorkCoordinator) -> bool {
        work.counts(AssetLane::Upload).1 > 0 || work.counts(AssetLane::Apply).1 > 0
    }
}

pub(crate) fn start(cx: &mut App) {
    let process = crate::process_state::process_state(cx);
    let wake_rx = process.asset_dispatcher.borrow().wake_receiver();
    cx.spawn(async move |cx: &mut AsyncApp| {
        while wake_rx.recv().await.is_ok() {
            dispatch_ready_work(cx).await;
        }
    })
    .detach();
}

async fn dispatch_ready_work(cx: &mut AsyncApp) {
    let (starts, completion_tx, wake_tx, has_ui_work) = cx.update(|cx| {
        let process = crate::process_state::process_state(cx);
        let mut work = process.asset_work.borrow_mut();
        let mut dispatcher = process.asset_dispatcher.borrow_mut();
        dispatcher.accept_provider_completions(&mut work, &mut process.thumbnails.borrow_mut());
        let starts = dispatcher.take_provider_starts(&mut work);
        (
            starts,
            dispatcher.completion_tx.clone(),
            dispatcher.wake_tx.clone(),
            dispatcher.has_ui_work(&work),
        )
    });

    for (started, job) in starts {
        let completion_tx = completion_tx.clone();
        let wake_tx = wake_tx.clone();
        cx.background_executor()
            .spawn(async move {
                let rgba = if started.is_cancelled() {
                    None
                } else {
                    match crate::video_poster::fetch_content_thumbnail(&job.path, job.size_px) {
                        crate::video_poster::Fetched::Done(result) => result,
                        crate::video_poster::Fetched::NeedsPoster => {
                            crate::video_poster::fetch_poster(job.path.clone(), job.size_px).await
                        }
                    }
                };
                let _ = completion_tx
                    .send(ProviderCompletion {
                        started,
                        path: job.path,
                        size_px: job.size_px,
                        rgba,
                    })
                    .await;
                let _ = wake_tx.try_send(());
            })
            .detach();
    }

    if has_ui_work {
        cx.background_executor().timer(FRAME_INTERVAL).await;
        let more = cx.update(|cx| {
            let process = crate::process_state::process_state(cx);
            let notifications = {
                let mut work = process.asset_work.borrow_mut();
                let mut dispatcher = process.asset_dispatcher.borrow_mut();
                dispatcher.drain_ui_frame(&mut work, &mut process.thumbnails.borrow_mut())
            };
            for notification in notifications {
                let waiter = notification.waiter;
                let accepted = waiter
                    .table
                    .update(cx, |state, _cx| {
                        state.delegate().accepts_thumbnail_result(
                            waiter.scope,
                            waiter.generation,
                            waiter.row_ix,
                            waiter.node,
                        )
                    })
                    .unwrap_or(false);
                if !accepted {
                    continue;
                }
                match waiter.target {
                    ThumbnailTarget::Table(table) => {
                        let _ = table.update(cx, |_state, cx| cx.notify());
                    }
                    ThumbnailTarget::Shell(shell) => {
                        let _ = shell.update(cx, |_shell, cx| cx.notify());
                    }
                }
            }
            let work = process.asset_work.borrow();
            process.asset_dispatcher.borrow().has_ui_work(&work)
        });
        if more {
            let _ = wake_tx.try_send(());
        }
    }
}
