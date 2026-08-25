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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssetWorkRequest {
    pub key: AssetKey,
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
    active: HashMap<AssetKey, ActiveWork>,
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
        if let Some(active) = self.active.get(&request.key) {
            return if active.request.generation == request.generation {
                SubmitOutcome::AlreadyScheduled
            } else {
                // The old revision/key cannot equal the new request; only a UI
                // generation changed. Let purge_generation cancel it before a
                // retry instead of running two providers for one asset.
                SubmitOutcome::AlreadyScheduled
            };
        }

        if let Some((sequence, queued)) = self
            .pending
            .iter_mut()
            .find(|(_, queued)| queued.key == request.key)
        {
            if queued.generation == request.generation && queued.priority >= request.priority {
                return SubmitOutcome::AlreadyScheduled;
            }
            self.sequence = self.sequence.wrapping_add(1);
            *sequence = self.sequence;
            *queued = request;
            return SubmitOutcome::Reprioritized;
        }

        self.sequence = self.sequence.wrapping_add(1);
        if self.pending.len() < self.pending_capacity {
            self.pending.push_back((self.sequence, request));
            return SubmitOutcome::Queued;
        }

        let Some((worst_index, _)) = self
            .pending
            .iter()
            .enumerate()
            .min_by_key(|(_, (sequence, queued))| (queued.priority, *sequence))
        else {
            return SubmitOutcome::QueueFull;
        };
        if self.pending[worst_index].1.priority >= request.priority {
            return SubmitOutcome::QueueFull;
        }
        self.pending[worst_index] = (self.sequence, request);
        SubmitOutcome::ReplacedLowerPriority
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
            request.key,
            ActiveWork {
                request,
                cancel: cancel.clone(),
            },
        );
        Some(StartedAssetWork { request, cancel })
    }

    /// A stale completion cannot release a newer generation's reservation.
    pub fn complete(&mut self, request: &AssetWorkRequest) -> bool {
        if self
            .active
            .get(&request.key)
            .is_some_and(|active| active.request.generation == request.generation)
        {
            self.active.remove(&request.key);
            true
        } else {
            false
        }
    }

    /// Navigation/refresh boundary. Pending work is dropped; active workers
    /// receive cancellation and retain their slot until they acknowledge via
    /// `complete`, preventing concurrency from exceeding the configured cap.
    pub fn retain_generation(&mut self, generation: u64) {
        self.pending
            .retain(|(_, request)| request.generation == generation);
        for active in self.active.values() {
            if active.request.generation != generation {
                active.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn counts(&self) -> (usize, usize) {
        (self.active.len(), self.pending.len())
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
    fn navigation_cancels_active_and_drops_pending_without_freeing_early() {
        let mut lane = BoundedAssetLane::new(1, 4);
        let old = request(1, 1, AssetPriority::Selected);
        lane.submit(old);
        let started = lane.start_next().expect("old request starts");
        lane.submit(request(2, 1, AssetPriority::Visible));
        lane.submit(request(3, 2, AssetPriority::Visible));
        lane.retain_generation(2);
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
}
