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

#[cfg(windows)]
use gpui::AnyWindowHandle;

#[cfg(windows)]
use ferail_core::platform_shortcuts::{
    ShortcutFailureKind, ShortcutInfo, ShortcutResolveRequest, ShortcutResolver as _,
};

use crate::file_list::FileListDelegate;
use crate::icons::IconCache;
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

enum ProviderPayload {
    Thumbnail {
        path: PathBuf,
        size_px: u32,
    },
    PathIcon {
        path: PathBuf,
        size_px: Option<u32>,
    },
    TypeIcon {
        path: PathBuf,
        cache_key: String,
    },
    #[cfg(windows)]
    Shortcut {
        request: ShortcutResolveRequest,
        resolver: Arc<crate::platform_shell::WindowsShortcutResolver>,
    },
}

struct ProviderJob {
    request: AssetWorkRequest,
    payload: ProviderPayload,
}

struct ProviderCompletion {
    started: StartedAssetWork,
    payload: ProviderPayload,
    rgba: Option<(Vec<u8>, u32, u32)>,
    #[cfg(windows)]
    shortcut: Option<Result<ShortcutInfo, ShortcutFailureKind>>,
}

enum UploadedPayload {
    Thumbnail { path: PathBuf, size_px: u32 },
    PathIcon { path: PathBuf, size_px: Option<u32> },
    TypeIcon { cache_key: String },
}

struct UploadedCompletion {
    request: AssetWorkRequest,
    payload: UploadedPayload,
    image: Option<Arc<RenderImage>>,
}

enum AssetNotification {
    Row(ThumbnailWaiter),
    Shell(WeakEntity<Shell>),
    #[cfg(windows)]
    Shortcut {
        waiter: Box<ShortcutWaiter>,
        result: Result<ShortcutInfo, ShortcutFailureKind>,
    },
}

#[cfg(windows)]
#[derive(Clone)]
pub(crate) struct ShortcutWaiter {
    pub shell: WeakEntity<Shell>,
    pub window: Option<AnyWindowHandle>,
    pub tab_id: crate::shell::TabId,
    pub load_generation: u64,
    pub node: NodeId,
    pub source: PathBuf,
    pub revision: FileRevision,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(windows)]
struct ShortcutApplyCompletion {
    request: AssetWorkRequest,
    result: Result<ShortcutInfo, ShortcutFailureKind>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum IconKey {
    Path(PathBuf, u32),
    Type(String),
}

pub(crate) struct TypeIconSeed {
    pub entry: ferail_core::FileEntry,
    pub path: PathBuf,
}

pub(crate) struct ThumbnailDispatcher {
    next_generation: u64,
    pending_jobs: HashMap<WorkId, ProviderJob>,
    live_by_key: HashMap<AssetKey, WorkId>,
    waiters: HashMap<AssetKey, Vec<ThumbnailWaiter>>,
    live_icons: HashMap<IconKey, WorkId>,
    icon_waiters: HashMap<IconKey, Vec<WeakEntity<Shell>>>,
    upload_payloads: HashMap<WorkId, ProviderCompletion>,
    apply_payloads: HashMap<WorkId, UploadedCompletion>,
    #[cfg(windows)]
    live_shortcuts: HashMap<AssetKey, WorkId>,
    #[cfg(windows)]
    shortcut_waiters: HashMap<AssetKey, Vec<ShortcutWaiter>>,
    #[cfg(windows)]
    shortcut_apply_payloads: HashMap<WorkId, ShortcutApplyCompletion>,
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
            live_icons: HashMap::new(),
            icon_waiters: HashMap::new(),
            upload_payloads: HashMap::new(),
            apply_payloads: HashMap::new(),
            #[cfg(windows)]
            live_shortcuts: HashMap::new(),
            #[cfg(windows)]
            shortcut_waiters: HashMap::new(),
            #[cfg(windows)]
            shortcut_apply_payloads: HashMap::new(),
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
        icons: &mut IconCache,
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
                self.discard_pending(cache, icons, evicted);
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
                ProviderJob {
                    request,
                    payload: ProviderPayload::Thumbnail {
                        path: seed.path,
                        size_px: seed.size_px,
                    },
                },
            );
        }
        let _ = self.wake_tx.try_send(());
    }

    pub(crate) fn submit_path_icons(
        &mut self,
        work: &mut AssetWorkCoordinator,
        thumbnails: &mut ThumbnailCache,
        icons: &mut IconCache,
        target: WeakEntity<Shell>,
        items: Vec<(PathBuf, Option<u32>)>,
    ) {
        for (path, size_px) in items {
            let physical_size = IconCache::path_icon_px(size_px);
            let icon_key = IconKey::Path(path.clone(), physical_size);
            if self.live_icons.contains_key(&icon_key) {
                self.icon_waiters
                    .entry(icon_key)
                    .or_default()
                    .push(target.clone());
                continue;
            }
            if !icons.needs_path_icon(&path, size_px) {
                continue;
            }
            let request = self.new_icon_request(physical_size);
            let submit = work.submit_detailed(AssetLane::Provider, request);
            if let Some(evicted) = submit.evicted {
                self.discard_pending(thumbnails, icons, evicted);
            }
            if matches!(
                submit.outcome,
                SubmitOutcome::QueueFull | SubmitOutcome::AlreadyScheduled
            ) {
                continue;
            }
            let id = WorkId::from_request(request);
            icons.mark_path_icon_in_flight(&path, size_px);
            self.live_icons.insert(icon_key.clone(), id);
            self.icon_waiters.insert(icon_key, vec![target.clone()]);
            self.pending_jobs.insert(
                id,
                ProviderJob {
                    request,
                    payload: ProviderPayload::PathIcon { path, size_px },
                },
            );
        }
        let _ = self.wake_tx.try_send(());
    }

    pub(crate) fn submit_type_icons(
        &mut self,
        work: &mut AssetWorkCoordinator,
        thumbnails: &mut ThumbnailCache,
        icons: &mut IconCache,
        target: WeakEntity<Shell>,
        seeds: Vec<TypeIconSeed>,
    ) {
        for seed in seeds {
            let cache_key = IconCache::type_key(&seed.entry, &seed.path);
            let icon_key = IconKey::Type(cache_key.clone());
            if self.live_icons.contains_key(&icon_key) {
                self.icon_waiters
                    .entry(icon_key)
                    .or_default()
                    .push(target.clone());
                continue;
            }
            if !icons.needs_type_icon(&cache_key) {
                continue;
            }
            let request = self.new_icon_request(32);
            let submit = work.submit_detailed(AssetLane::Provider, request);
            if let Some(evicted) = submit.evicted {
                self.discard_pending(thumbnails, icons, evicted);
            }
            if matches!(
                submit.outcome,
                SubmitOutcome::QueueFull | SubmitOutcome::AlreadyScheduled
            ) {
                continue;
            }
            let id = WorkId::from_request(request);
            icons.mark_type_icon_in_flight(cache_key.clone());
            self.live_icons.insert(icon_key.clone(), id);
            self.icon_waiters.insert(icon_key, vec![target.clone()]);
            self.pending_jobs.insert(
                id,
                ProviderJob {
                    request,
                    payload: ProviderPayload::TypeIcon {
                        path: seed.path,
                        cache_key,
                    },
                },
            );
        }
        let _ = self.wake_tx.try_send(());
    }

    #[cfg(windows)]
    pub(crate) fn submit_shortcut(
        &mut self,
        work: &mut AssetWorkCoordinator,
        thumbnails: &mut ThumbnailCache,
        icons: &mut IconCache,
        resolver: Arc<crate::platform_shell::WindowsShortcutResolver>,
        waiter: ShortcutWaiter,
    ) {
        let key = AssetKey {
            identity: AssetIdentity::File(waiter.node),
            revision: AssetRevision::File(waiter.revision),
            kind: AssetKind::Shortcut,
        };
        if self.live_shortcuts.contains_key(&key) {
            self.shortcut_waiters.entry(key).or_default().push(waiter);
            return;
        }
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let request = AssetWorkRequest {
            key,
            scope: DISPATCH_SCOPE,
            generation: self.next_generation,
            priority: AssetPriority::Selected,
        };
        let submit = work.submit_detailed(AssetLane::Provider, request);
        if let Some(evicted) = submit.evicted {
            self.discard_pending(thumbnails, icons, evicted);
        }
        if matches!(
            submit.outcome,
            SubmitOutcome::QueueFull | SubmitOutcome::AlreadyScheduled
        ) {
            return;
        }
        let id = WorkId::from_request(request);
        self.live_shortcuts.insert(key, id);
        self.shortcut_waiters.insert(key, vec![waiter.clone()]);
        self.pending_jobs.insert(
            id,
            ProviderJob {
                request,
                payload: ProviderPayload::Shortcut {
                    request: ShortcutResolveRequest {
                        source: waiter.source,
                        revision: waiter.revision,
                    },
                    resolver,
                },
            },
        );
        let _ = self.wake_tx.try_send(());
    }

    fn new_icon_request(&mut self, size_px: u32) -> AssetWorkRequest {
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let generation = self.next_generation;
        AssetWorkRequest {
            key: AssetKey {
                identity: AssetIdentity::File(generation.into()),
                revision: AssetRevision::Provider(generation),
                kind: AssetKind::TypeIcon {
                    size_px: size_px.min(u16::MAX as u32) as u16,
                },
            },
            scope: DISPATCH_SCOPE,
            generation,
            priority: AssetPriority::Visible,
        }
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
                if let ProviderPayload::Thumbnail { path, size_px } = job.payload {
                    cache.cancel_in_flight(std::iter::once(&path), size_px);
                }
            }
        }
    }

    fn discard_pending(
        &mut self,
        cache: &mut ThumbnailCache,
        icons: &mut IconCache,
        request: AssetWorkRequest,
    ) {
        let id = WorkId::from_request(request);
        if let Some(job) = self.pending_jobs.remove(&id) {
            self.discard_provider_payload(cache, icons, request.key, job.payload);
        }
    }

    fn discard_provider_payload(
        &mut self,
        thumbnails: &mut ThumbnailCache,
        icons: &mut IconCache,
        asset_key: AssetKey,
        payload: ProviderPayload,
    ) {
        match payload {
            ProviderPayload::Thumbnail { path, size_px } => {
                self.live_by_key.remove(&asset_key);
                self.waiters.remove(&asset_key);
                thumbnails.cancel_in_flight(std::iter::once(&path), size_px);
            }
            ProviderPayload::PathIcon { path, size_px } => {
                let key = IconKey::Path(path.clone(), IconCache::path_icon_px(size_px));
                self.live_icons.remove(&key);
                self.icon_waiters.remove(&key);
                icons.cancel_path_icon_in_flight(&path, size_px);
            }
            ProviderPayload::TypeIcon { cache_key, .. } => {
                let key = IconKey::Type(cache_key.clone());
                self.live_icons.remove(&key);
                self.icon_waiters.remove(&key);
                icons.cancel_type_icon_in_flight(&cache_key);
            }
            #[cfg(windows)]
            ProviderPayload::Shortcut { .. } => {
                self.live_shortcuts.remove(&asset_key);
                if let Some(waiters) = self.shortcut_waiters.remove(&asset_key) {
                    for waiter in waiters {
                        waiter
                            .cancel
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
    }

    fn take_provider_starts(
        &mut self,
        work: &mut AssetWorkCoordinator,
    ) -> Vec<(StartedAssetWork, ProviderJob)> {
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
        thumbnails: &mut ThumbnailCache,
        icons: &mut IconCache,
    ) {
        while let Ok(completion) = self.completion_rx.try_recv() {
            let request = completion.started.request;
            let id = WorkId::from_request(request);
            let _ = work.complete(AssetLane::Provider, &request);
            if completion.started.is_cancelled() {
                self.discard_provider_payload(thumbnails, icons, request.key, completion.payload);
                continue;
            }
            #[cfg(windows)]
            if let Some(result) = completion.shortcut {
                let submit = work.submit_detailed(AssetLane::Apply, request);
                if let Some(evicted) = submit.evicted {
                    self.discard_apply(thumbnails, icons, evicted);
                }
                if matches!(
                    submit.outcome,
                    SubmitOutcome::QueueFull | SubmitOutcome::AlreadyScheduled
                ) {
                    self.discard_provider_payload(
                        thumbnails,
                        icons,
                        request.key,
                        completion.payload,
                    );
                    continue;
                }
                self.shortcut_apply_payloads
                    .insert(id, ShortcutApplyCompletion { request, result });
                continue;
            }
            let submit = work.submit_detailed(AssetLane::Upload, request);
            if let Some(evicted) = submit.evicted {
                self.discard_upload(thumbnails, icons, evicted);
            }
            if matches!(
                submit.outcome,
                SubmitOutcome::QueueFull | SubmitOutcome::AlreadyScheduled
            ) {
                self.discard_provider_payload(thumbnails, icons, request.key, completion.payload);
                continue;
            }
            self.upload_payloads.insert(id, completion);
        }
    }

    fn discard_upload(
        &mut self,
        thumbnails: &mut ThumbnailCache,
        icons: &mut IconCache,
        request: AssetWorkRequest,
    ) {
        let id = WorkId::from_request(request);
        if let Some(completion) = self.upload_payloads.remove(&id) {
            self.discard_provider_payload(thumbnails, icons, request.key, completion.payload);
        }
    }

    fn drain_ui_frame(
        &mut self,
        work: &mut AssetWorkCoordinator,
        thumbnails: &mut ThumbnailCache,
        icons: &mut IconCache,
    ) -> Vec<AssetNotification> {
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
            let payload = match completion.payload {
                ProviderPayload::Thumbnail { path, size_px } => {
                    UploadedPayload::Thumbnail { path, size_px }
                }
                ProviderPayload::PathIcon { path, size_px } => {
                    UploadedPayload::PathIcon { path, size_px }
                }
                ProviderPayload::TypeIcon { cache_key, .. } => {
                    UploadedPayload::TypeIcon { cache_key }
                }
                #[cfg(windows)]
                ProviderPayload::Shortcut { .. } => unreachable!("shortcuts bypass pixel upload"),
            };
            let _ = work.complete(AssetLane::Upload, &request);
            let submit = work.submit_detailed(AssetLane::Apply, request);
            if let Some(evicted) = submit.evicted {
                self.discard_apply(thumbnails, icons, evicted);
            }
            if matches!(
                submit.outcome,
                SubmitOutcome::QueueFull | SubmitOutcome::AlreadyScheduled
            ) {
                self.discard_uploaded_payload(thumbnails, icons, request.key, payload);
                continue;
            }
            self.apply_payloads.insert(
                id,
                UploadedCompletion {
                    request,
                    payload,
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
            #[cfg(windows)]
            if let Some(completion) = self.shortcut_apply_payloads.remove(&id) {
                self.live_shortcuts.remove(&completion.request.key);
                if let Some(waiters) = self.shortcut_waiters.remove(&completion.request.key) {
                    notifications.extend(waiters.into_iter().map(|waiter| {
                        AssetNotification::Shortcut {
                            waiter: Box::new(waiter),
                            result: completion.result.clone(),
                        }
                    }));
                }
                let _ = work.complete(AssetLane::Apply, &completion.request);
                continue;
            }
            let Some(completion) = self.apply_payloads.remove(&id) else {
                let _ = work.complete(AssetLane::Apply, &started.request);
                continue;
            };
            match completion.payload {
                UploadedPayload::Thumbnail { path, size_px } => {
                    thumbnails.insert_rendered(path, size_px, completion.image);
                    self.live_by_key.remove(&completion.request.key);
                    if let Some(waiters) = self.waiters.remove(&completion.request.key) {
                        notifications.extend(waiters.into_iter().map(AssetNotification::Row));
                    }
                }
                UploadedPayload::PathIcon { path, size_px } => {
                    icons.insert_path_icon_rendered(&path, size_px, completion.image);
                    let key = IconKey::Path(path, IconCache::path_icon_px(size_px));
                    self.live_icons.remove(&key);
                    if let Some(waiters) = self.icon_waiters.remove(&key) {
                        notifications.extend(waiters.into_iter().map(AssetNotification::Shell));
                    }
                }
                UploadedPayload::TypeIcon { cache_key } => {
                    icons.insert_type_icon_rendered(cache_key.clone(), completion.image);
                    let key = IconKey::Type(cache_key);
                    self.live_icons.remove(&key);
                    if let Some(waiters) = self.icon_waiters.remove(&key) {
                        notifications.extend(waiters.into_iter().map(AssetNotification::Shell));
                    }
                }
            }
            let _ = work.complete(AssetLane::Apply, &completion.request);
        }
        notifications
    }

    fn discard_apply(
        &mut self,
        thumbnails: &mut ThumbnailCache,
        icons: &mut IconCache,
        request: AssetWorkRequest,
    ) {
        let id = WorkId::from_request(request);
        if let Some(completion) = self.apply_payloads.remove(&id) {
            self.discard_uploaded_payload(thumbnails, icons, request.key, completion.payload);
        }
        #[cfg(windows)]
        if self.shortcut_apply_payloads.remove(&id).is_some() {
            self.live_shortcuts.remove(&request.key);
            if let Some(waiters) = self.shortcut_waiters.remove(&request.key) {
                for waiter in waiters {
                    waiter
                        .cancel
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    }

    fn discard_uploaded_payload(
        &mut self,
        thumbnails: &mut ThumbnailCache,
        icons: &mut IconCache,
        asset_key: AssetKey,
        payload: UploadedPayload,
    ) {
        match payload {
            UploadedPayload::Thumbnail { path, size_px } => {
                self.live_by_key.remove(&asset_key);
                self.waiters.remove(&asset_key);
                thumbnails.cancel_in_flight(std::iter::once(&path), size_px);
            }
            UploadedPayload::PathIcon { path, size_px } => {
                let key = IconKey::Path(path.clone(), IconCache::path_icon_px(size_px));
                self.live_icons.remove(&key);
                self.icon_waiters.remove(&key);
                icons.cancel_path_icon_in_flight(&path, size_px);
            }
            UploadedPayload::TypeIcon { cache_key } => {
                let key = IconKey::Type(cache_key.clone());
                self.live_icons.remove(&key);
                self.icon_waiters.remove(&key);
                icons.cancel_type_icon_in_flight(&cache_key);
            }
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
        dispatcher.accept_provider_completions(
            &mut work,
            &mut process.thumbnails.borrow_mut(),
            &mut process.icons.borrow_mut(),
        );
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
                #[cfg(windows)]
                let shortcut = if started.is_cancelled() {
                    None
                } else if let ProviderPayload::Shortcut { request, resolver } = &job.payload {
                    Some(resolver.resolve(request.clone(), &started.cancellation()))
                } else {
                    None
                };
                let rgba = if started.is_cancelled() {
                    None
                } else {
                    match &job.payload {
                        ProviderPayload::Thumbnail { path, size_px } => {
                            match crate::video_poster::fetch_content_thumbnail(path, *size_px) {
                                crate::video_poster::Fetched::Done(result) => result,
                                crate::video_poster::Fetched::NeedsPoster => {
                                    crate::video_poster::fetch_poster(path.clone(), *size_px).await
                                }
                            }
                        }
                        ProviderPayload::PathIcon { path, size_px } => {
                            let px = IconCache::path_icon_px(*size_px);
                            ferail_fs_native::fetch_icon_rgba(path, px)
                        }
                        ProviderPayload::TypeIcon { path, .. } => {
                            ferail_fs_native::fetch_icon_rgba(path, 32)
                        }
                        #[cfg(windows)]
                        ProviderPayload::Shortcut { .. } => None,
                    }
                };
                let _ = completion_tx
                    .send(ProviderCompletion {
                        started,
                        payload: job.payload,
                        rgba,
                        #[cfg(windows)]
                        shortcut,
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
                dispatcher.drain_ui_frame(
                    &mut work,
                    &mut process.thumbnails.borrow_mut(),
                    &mut process.icons.borrow_mut(),
                )
            };
            for notification in notifications {
                match notification {
                    AssetNotification::Row(waiter) => {
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
                    AssetNotification::Shell(shell) => {
                        let _ = shell.update(cx, |_shell, cx| cx.notify());
                    }
                    #[cfg(windows)]
                    AssetNotification::Shortcut { waiter, result } => {
                        let shell = waiter.shell.clone();
                        let _ = shell.update(cx, |shell, cx| {
                            shell.apply_windows_shortcut_result(*waiter, result, cx);
                        });
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
