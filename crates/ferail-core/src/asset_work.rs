//! Bounded, platform-neutral scheduling state for visible asset work.
//!
//! Native icons, thumbnails and property providers can be slow or hostile.
//! This module owns no worker and performs no I/O; it only decides which
//! compact, path-free requests may start. The host keeps one process-level
//! coordinator and executes returned work on the appropriate bounded lane.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::NodeId;
use crate::platform_namespace::PlatformItemId;
use crate::revision_cache::FileRevision;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AssetIdentity {
    File(NodeId),
    /// A scan-local file identity (for example Flat View) which is meaningful
    /// only inside its owning surface. The compact numeric scope prevents two
    /// independent arenas that both minted NodeId(1) from sharing a cache job.
    SurfaceFile {
        surface: u64,
        node: NodeId,
    },
    Platform(PlatformItemId),
}

/// Revision is owned by the source surface. Provider tokens are opaque and
/// scoped to its tab/generation; they must not be persisted.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub enum AssetRevision {
    File(FileRevision),
    Provider(u64),
}

impl fmt::Debug for AssetRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(revision) => formatter.debug_tuple("File").field(revision).finish(),
            Self::Provider(_) => formatter.write_str("Provider(<opaque>)"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AssetKind {
    TypeIcon { size_px: u16 },
    ContentThumbnail { size_px: u16 },
    Preview,
    Properties,
    Shortcut,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AssetKey {
    pub identity: AssetIdentity,
    pub revision: AssetRevision,
    pub kind: AssetKind,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AssetPriority {
    Overscan,
    Visible,
    Selected,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AssetLane {
    Provider,
    Decode,
    Upload,
    Apply,
}

/// Process-local owner of an asset request. Generations are meaningful only
/// inside one surface, so every process-owned lane must carry this scope and
/// must never retire work by a bare generation number.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AssetWorkScope(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssetWorkRequest {
    pub key: AssetKey,
    pub scope: AssetWorkScope,
    /// UI/listing generation used to reject stale completion and purge work
    /// after navigation. It is not a filesystem/provider identity.
    pub generation: u64,
    pub priority: AssetPriority,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitOutcome {
    Queued,
    Reprioritized,
    AlreadyScheduled,
    ReplacedLowerPriority,
    QueueFull,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelOutcome {
    NotFound,
    RemovedPending,
    SignaledActive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubmitResult {
    pub outcome: SubmitOutcome,
    /// Present when a higher-priority request displaced pending work. The
    /// host must use this to release its path/pixel payload and retryable cache
    /// reservation; those deliberately do not live in the compact scheduler.
    pub evicted: Option<AssetWorkRequest>,
}

#[derive(Clone)]
pub struct StartedAssetWork {
    pub request: AssetWorkRequest,
    cancel: Arc<AtomicBool>,
}

impl StartedAssetWork {
    pub fn cancellation(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

impl fmt::Debug for StartedAssetWork {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StartedAssetWork")
            .field("request", &self.request)
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

struct ActiveWork {
    request: AssetWorkRequest,
    cancel: Arc<AtomicBool>,
}

/// One lane with fixed concurrency and a fixed pending-request budget.
/// Pending work is priority ordered at dispatch time; ties are newest-first so
/// scrolling naturally favors the latest viewport without an unbounded train.
pub struct BoundedAssetLane {
    concurrency: usize,
    pending_capacity: usize,
    pending: VecDeque<(u64, AssetWorkRequest)>,
    active: HashMap<(AssetWorkScope, AssetKey), ActiveWork>,
    sequence: u64,
}

impl BoundedAssetLane {
    pub fn new(concurrency: usize, pending_capacity: usize) -> Self {
        Self {
            concurrency,
            pending_capacity,
            pending: VecDeque::new(),
            active: HashMap::new(),
            sequence: 0,
        }
    }

    pub fn submit(&mut self, request: AssetWorkRequest) -> SubmitOutcome {
        self.submit_detailed(request).outcome
    }

    pub fn submit_detailed(&mut self, request: AssetWorkRequest) -> SubmitResult {
        let reservation = (request.scope, request.key);
        if let Some(active) = self.active.get(&reservation) {
            let outcome = if active.request.generation == request.generation {
                SubmitOutcome::AlreadyScheduled
            } else {
                // The old revision/key cannot equal the new request; only a UI
                // generation changed. Let purge_generation cancel it before a
                // retry instead of running two providers for one asset.
                SubmitOutcome::AlreadyScheduled
            };
            return SubmitResult {
                outcome,
                evicted: None,
            };
        }

        if let Some((sequence, queued)) = self
            .pending
            .iter_mut()
            .find(|(_, queued)| queued.scope == request.scope && queued.key == request.key)
        {
            if queued.generation == request.generation && queued.priority >= request.priority {
                return SubmitResult {
                    outcome: SubmitOutcome::AlreadyScheduled,
                    evicted: None,
                };
            }
            self.sequence = self.sequence.wrapping_add(1);
            *sequence = self.sequence;
            *queued = request;
            return SubmitResult {
                outcome: SubmitOutcome::Reprioritized,
                evicted: None,
            };
        }

        self.sequence = self.sequence.wrapping_add(1);
        if self.pending.len() < self.pending_capacity {
            self.pending.push_back((self.sequence, request));
            return SubmitResult {
                outcome: SubmitOutcome::Queued,
                evicted: None,
            };
        }

        let Some((worst_index, _)) = self
            .pending
            .iter()
            .enumerate()
            .min_by_key(|(_, (sequence, queued))| (queued.priority, *sequence))
        else {
            return SubmitResult {
                outcome: SubmitOutcome::QueueFull,
                evicted: None,
            };
        };
        if self.pending[worst_index].1.priority >= request.priority {
            return SubmitResult {
                outcome: SubmitOutcome::QueueFull,
                evicted: None,
            };
        }
        let evicted = self.pending[worst_index].1;
        self.pending[worst_index] = (self.sequence, request);
        SubmitResult {
            outcome: SubmitOutcome::ReplacedLowerPriority,
            evicted: Some(evicted),
        }
    }

    pub fn start_next(&mut self) -> Option<StartedAssetWork> {
        if self.active.len() >= self.concurrency {
            return None;
        }
        let next_index = self
            .pending
            .iter()
            .enumerate()
            .max_by_key(|(_, (sequence, request))| (request.priority, *sequence))?
            .0;
        let (_, request) = self.pending.remove(next_index)?;
        let cancel = Arc::new(AtomicBool::new(false));
        self.active.insert(
            (request.scope, request.key),
            ActiveWork {
                request,
                cancel: cancel.clone(),
            },
        );
        Some(StartedAssetWork { request, cancel })
    }

    /// A stale completion cannot release a newer generation's reservation.
    pub fn complete(&mut self, request: &AssetWorkRequest) -> bool {
        let reservation = (request.scope, request.key);
        if self
            .active
            .get(&reservation)
            .is_some_and(|active| active.request.generation == request.generation)
        {
            self.active.remove(&reservation);
            true
        } else {
            false
        }
    }

    /// Cancel one exact reservation. Pending work is removed immediately;
    /// active work retains its slot until `complete` acknowledges the worker.
    pub fn cancel(&mut self, request: &AssetWorkRequest) -> CancelOutcome {
        if let Some(index) = self.pending.iter().position(|(_, queued)| {
            queued.scope == request.scope
                && queued.key == request.key
                && queued.generation == request.generation
        }) {
            self.pending.remove(index);
            return CancelOutcome::RemovedPending;
        }
        let reservation = (request.scope, request.key);
        if let Some(active) = self.active.get(&reservation) {
            if active.request.generation == request.generation {
                active.cancel.store(true, Ordering::Relaxed);
                return CancelOutcome::SignaledActive;
            }
        }
        CancelOutcome::NotFound
    }

    /// Navigation/refresh boundary. Pending work is dropped; active workers
    /// receive cancellation and retain their slot until they acknowledge via
    /// `complete`, preventing concurrency from exceeding the configured cap.
    pub fn retain_scope_generation(&mut self, scope: AssetWorkScope, generation: u64) {
        let _ = self.retire_scope_generation(scope, generation);
    }

    /// Detailed retirement used by hosts that own payloads outside this lane.
    /// Active work is canceled but not returned because it still owns a slot;
    /// pending work is returned so its payload/reservation can be released.
    pub fn retire_scope_generation(
        &mut self,
        scope: AssetWorkScope,
        generation: u64,
    ) -> Vec<AssetWorkRequest> {
        let mut retired = Vec::new();
        self.pending.retain(|(_, request)| {
            let keep = request.scope != scope || request.generation == generation;
            if !keep {
                retired.push(*request);
            }
            keep
        });
        for active in self.active.values() {
            if active.request.scope == scope && active.request.generation != generation {
                active.cancel.store(true, Ordering::Relaxed);
            }
        }
        retired
    }

    pub fn counts(&self) -> (usize, usize) {
        (self.active.len(), self.pending.len())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssetLaneBudget {
    pub concurrency: usize,
    pub pending_capacity: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssetWorkBudgets {
    pub provider: AssetLaneBudget,
    pub decode: AssetLaneBudget,
    pub upload: AssetLaneBudget,
    pub apply: AssetLaneBudget,
}

/// Process-owned collection of independent resource lanes. This remains a
/// scheduling primitive: the host owns payloads and workers, and must call
/// `complete` only after a worker has acknowledged cancellation or delivered
/// its result.
pub struct AssetWorkCoordinator {
    provider: BoundedAssetLane,
    decode: BoundedAssetLane,
    upload: BoundedAssetLane,
    apply: BoundedAssetLane,
}

impl AssetWorkCoordinator {
    pub fn new(budgets: AssetWorkBudgets) -> Self {
        let lane = |budget: AssetLaneBudget| {
            BoundedAssetLane::new(budget.concurrency, budget.pending_capacity)
        };
        Self {
            provider: lane(budgets.provider),
            decode: lane(budgets.decode),
            upload: lane(budgets.upload),
            apply: lane(budgets.apply),
        }
    }

    pub fn submit(&mut self, lane: AssetLane, request: AssetWorkRequest) -> SubmitOutcome {
        self.lane_mut(lane).submit(request)
    }

    pub fn submit_detailed(&mut self, lane: AssetLane, request: AssetWorkRequest) -> SubmitResult {
        self.lane_mut(lane).submit_detailed(request)
    }

    pub fn start_next(&mut self, lane: AssetLane) -> Option<StartedAssetWork> {
        self.lane_mut(lane).start_next()
    }

    pub fn complete(&mut self, lane: AssetLane, request: &AssetWorkRequest) -> bool {
        self.lane_mut(lane).complete(request)
    }

    pub fn cancel(&mut self, lane: AssetLane, request: &AssetWorkRequest) -> CancelOutcome {
        self.lane_mut(lane).cancel(request)
    }

    pub fn retain_scope_generation(&mut self, scope: AssetWorkScope, generation: u64) {
        for lane in [
            AssetLane::Provider,
            AssetLane::Decode,
            AssetLane::Upload,
            AssetLane::Apply,
        ] {
            self.lane_mut(lane)
                .retain_scope_generation(scope, generation);
        }
    }

    pub fn retire_scope_generation(
        &mut self,
        scope: AssetWorkScope,
        generation: u64,
    ) -> Vec<(AssetLane, AssetWorkRequest)> {
        let mut retired = Vec::new();
        for lane in [
            AssetLane::Provider,
            AssetLane::Decode,
            AssetLane::Upload,
            AssetLane::Apply,
        ] {
            retired.extend(
                self.lane_mut(lane)
                    .retire_scope_generation(scope, generation)
                    .into_iter()
                    .map(|request| (lane, request)),
            );
        }
        retired
    }

    pub fn counts(&self, lane: AssetLane) -> (usize, usize) {
        self.lane(lane).counts()
    }

    fn lane(&self, lane: AssetLane) -> &BoundedAssetLane {
        match lane {
            AssetLane::Provider => &self.provider,
            AssetLane::Decode => &self.decode,
            AssetLane::Upload => &self.upload,
            AssetLane::Apply => &self.apply,
        }
    }

    fn lane_mut(&mut self, lane: AssetLane) -> &mut BoundedAssetLane {
        match lane {
            AssetLane::Provider => &mut self.provider,
            AssetLane::Decode => &mut self.decode,
            AssetLane::Upload => &mut self.upload,
            AssetLane::Apply => &mut self.apply,
        }
    }
}

impl fmt::Debug for AssetWorkCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AssetWorkCoordinator")
            .field("provider", &self.provider)
            .field("decode", &self.decode)
            .field("upload", &self.upload)
            .field("apply", &self.apply)
            .finish()
    }
}

impl fmt::Debug for BoundedAssetLane {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundedAssetLane")
            .field("concurrency", &self.concurrency)
            .field("pending_capacity", &self.pending_capacity)
            .field("active", &self.active.len())
            .field("pending", &self.pending.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: u64, generation: u64, priority: AssetPriority) -> AssetWorkRequest {
        AssetWorkRequest {
            key: AssetKey {
                identity: AssetIdentity::File(id.into()),
                revision: AssetRevision::File(FileRevision {
                    byte_len: id,
                    modified_ns: Some(1),
                }),
                kind: AssetKind::ContentThumbnail { size_px: 128 },
            },
            scope: AssetWorkScope(1),
            generation,
            priority,
        }
    }

    #[test]
    fn queue_and_active_work_never_exceed_their_budgets() {
        let mut lane = BoundedAssetLane::new(2, 8);
        for id in 1..=4_000_000 {
            let _ = lane.submit(request(id, 1, AssetPriority::Overscan));
        }
        assert_eq!(lane.counts(), (0, 8));
        assert!(lane.start_next().is_some());
        assert!(lane.start_next().is_some());
        assert!(lane.start_next().is_none());
        assert_eq!(lane.counts(), (2, 6));
    }

    #[test]
    fn selected_and_visible_work_displace_speculative_overscan() {
        let mut lane = BoundedAssetLane::new(1, 2);
        assert_eq!(
            lane.submit(request(1, 1, AssetPriority::Overscan)),
            SubmitOutcome::Queued
        );
        assert_eq!(
            lane.submit(request(2, 1, AssetPriority::Overscan)),
            SubmitOutcome::Queued
        );
        assert_eq!(
            lane.submit(request(3, 1, AssetPriority::Visible)),
            SubmitOutcome::ReplacedLowerPriority
        );
        assert_eq!(
            lane.submit(request(4, 1, AssetPriority::Selected)),
            SubmitOutcome::ReplacedLowerPriority
        );
        assert_eq!(
            lane.start_next()
                .expect("selected starts first")
                .request
                .key,
            request(4, 1, AssetPriority::Selected).key
        );
    }

    #[test]
    fn detailed_submit_returns_the_evicted_request_for_payload_cleanup() {
        let mut lane = BoundedAssetLane::new(1, 1);
        let overscan = request(1, 1, AssetPriority::Overscan);
        lane.submit(overscan);
        let visible = request(2, 1, AssetPriority::Visible);
        let result = lane.submit_detailed(visible);
        assert_eq!(result.outcome, SubmitOutcome::ReplacedLowerPriority);
        assert_eq!(result.evicted, Some(overscan));
    }

    #[test]
    fn navigation_cancels_active_and_drops_pending_without_freeing_early() {
        let mut lane = BoundedAssetLane::new(1, 4);
        let old = request(1, 1, AssetPriority::Selected);
        lane.submit(old);
        let started = lane.start_next().expect("old request starts");
        lane.submit(request(2, 1, AssetPriority::Visible));
        lane.submit(request(3, 2, AssetPriority::Visible));
        lane.retain_scope_generation(AssetWorkScope(1), 2);
        assert!(started.is_cancelled());
        assert_eq!(lane.counts(), (1, 1));
        assert!(lane.start_next().is_none());
        assert!(lane.complete(&old));
        assert_eq!(
            lane.start_next()
                .expect("new generation starts")
                .request
                .generation,
            2
        );
    }

    #[test]
    fn stale_completion_cannot_release_live_work() {
        let mut lane = BoundedAssetLane::new(1, 2);
        let live = request(1, 2, AssetPriority::Visible);
        lane.submit(live);
        lane.start_next().expect("starts");
        let stale = AssetWorkRequest {
            generation: 1,
            ..live
        };
        assert!(!lane.complete(&stale));
        assert_eq!(lane.counts().0, 1);
        assert!(lane.complete(&live));
    }

    #[test]
    fn retiring_one_surface_never_cancels_another_surface() {
        let mut lane = BoundedAssetLane::new(2, 4);
        let first = request(1, 1, AssetPriority::Visible);
        let second = AssetWorkRequest {
            scope: AssetWorkScope(2),
            ..request(1, 1, AssetPriority::Visible)
        };
        lane.submit(first);
        lane.submit(second);
        let started_first = lane.start_next().expect("first surface starts");
        let started_second = lane.start_next().expect("second surface starts");

        lane.retain_scope_generation(AssetWorkScope(1), 2);

        let (retired, live) = if started_first.request.scope == AssetWorkScope(1) {
            (started_first, started_second)
        } else {
            (started_second, started_first)
        };
        assert!(retired.is_cancelled());
        assert!(!live.is_cancelled());
    }

    #[test]
    fn detailed_retirement_returns_only_that_surfaces_pending_payloads() {
        let mut lane = BoundedAssetLane::new(1, 4);
        let old = request(1, 1, AssetPriority::Visible);
        let other = AssetWorkRequest {
            scope: AssetWorkScope(2),
            ..request(2, 1, AssetPriority::Visible)
        };
        lane.submit(old);
        lane.submit(other);

        assert_eq!(
            lane.retire_scope_generation(AssetWorkScope(1), 2),
            vec![old]
        );
        assert_eq!(lane.counts(), (0, 1));
        assert_eq!(
            lane.start_next().expect("other surface remains").request,
            other
        );
    }

    #[test]
    fn exact_cancel_removes_pending_but_only_signals_active_work() {
        let mut lane = BoundedAssetLane::new(1, 4);
        let active = request(1, 1, AssetPriority::Visible);
        let pending = request(2, 1, AssetPriority::Visible);
        lane.submit(active);
        lane.submit(pending);
        let started = lane.start_next().expect("newest request starts");
        let queued = if started.request == active {
            pending
        } else {
            active
        };
        assert_eq!(lane.cancel(&queued), CancelOutcome::RemovedPending);
        assert_eq!(lane.counts(), (1, 0));
        assert_eq!(lane.cancel(&started.request), CancelOutcome::SignaledActive);
        assert!(started.is_cancelled());
        assert_eq!(lane.counts(), (1, 0));
        assert!(lane.complete(&started.request));
    }

    #[test]
    fn process_coordinator_enforces_each_lane_budget_independently() {
        let one = AssetLaneBudget {
            concurrency: 1,
            pending_capacity: 2,
        };
        let mut coordinator = AssetWorkCoordinator::new(AssetWorkBudgets {
            provider: one,
            decode: one,
            upload: one,
            apply: one,
        });
        for id in 1..=10 {
            coordinator.submit(AssetLane::Provider, request(id, 1, AssetPriority::Visible));
            coordinator.submit(
                AssetLane::Apply,
                request(id + 100, 1, AssetPriority::Visible),
            );
        }
        assert_eq!(coordinator.counts(AssetLane::Provider), (0, 2));
        assert_eq!(coordinator.counts(AssetLane::Apply), (0, 2));
        assert!(coordinator.start_next(AssetLane::Provider).is_some());
        assert!(coordinator.start_next(AssetLane::Provider).is_none());
        assert_eq!(coordinator.counts(AssetLane::Apply), (0, 2));
    }
}
