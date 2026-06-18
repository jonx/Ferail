//! Task registry — single source of truth for "what background work
//! is in flight right now."
//!
//! Pure logic — no GPUI dependency. Long-running jobs in the
//! shell (prefetch::start, future copy/move, disk-usage scans) call
//! `begin` / `end` on the Shell's `TaskRegistry`; the status bar
//! reads `iter()` to keep its text + progress strip in sync. Both
//! surfaces always agree because both read from the same store.
//!
//! Bridging to GPUI happens in the Shell (which owns the registry
//! as a `Rc<RefCell<TaskRegistry>>`) and the status-bar render.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TaskId(u64);

impl TaskId {
    /// Stable numeric form for element ids (task-panel cancel
    /// buttons need a per-task `ElementId`).
    pub fn raw(&self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskKind {
    Enumeration,
    IconPrefetch,
    MagicPrefetch,
    QuarantinePrefetch,
    /// Recursive folder sizes for the Size column.
    FolderSize,
    DiskUsage,
    /// Right-click file operations: Duplicate, Compress.
    FileOp,
    /// Recursive / global file search (docs/features/SEARCH.md).
    Search,
    /// Duplicate-finder funnel scan (docs/features/DUPLICATES.md).
    DuplicateScan,
}

impl TaskKind {
    /// Foreground tasks are user-initiated and actively waited on (file
    /// transfers, search, scans) — they win the status-bar's primary
    /// slot and carry rich progress. Ambient tasks (prefetch, folder
    /// sizes, enumeration) are passive housekeeping the user never
    /// explicitly asked for and never needs to cancel; they yield the
    /// spotlight. (docs/features/FILE_OPS.md)
    pub fn is_foreground(&self) -> bool {
        matches!(
            self,
            TaskKind::FileOp | TaskKind::Search | TaskKind::DiskUsage | TaskKind::DuplicateScan
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TaskProgress {
    Indeterminate,
    Determinate(f32),
}

/// Rich progress for a transfer (copy/move), sampled from the engine's
/// shared `TransferProgress` and decorated UI-side with a derived rate
/// and ETA. Plain numbers + one string so the registry stays
/// GPUI-free; rate/ETA are computed by the sampler, never the worker.
/// (docs/features/FILE_OPS.md)
#[derive(Clone, Debug, Default)]
pub struct TransferStats {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub items_done: u64,
    pub items_total: u64,
    /// Smoothed throughput (bytes/sec). 0 while still ramping.
    pub bytes_per_sec: f64,
    /// Seconds remaining, or `None` when not yet estimable (or when an
    /// instant clone jumped the bar, making a rate meaningless).
    pub eta_secs: Option<u64>,
    /// File currently in flight (throttled by the engine).
    pub current: String,
}

#[derive(Clone, Debug)]
pub struct ActiveTask {
    pub id: TaskId,
    pub kind: TaskKind,
    pub label: String,
    pub started_at: Instant,
    pub progress: TaskProgress,
    pub cancellable: bool,
    /// Cooperative cancel flag for tasks that support it. The task
    /// panel renders a ✕ that stores `true`; the worker polls it
    /// (docs/features/FILE_OPS.md). `None` for legacy `begin` callers
    /// whose cancellation runs through other plumbing (tab cancel
    /// flags etc.).
    pub cancel: Option<Arc<AtomicBool>>,
    /// Rich transfer progress, set by `update_transfer` for copy/move
    /// tasks. `None` for every other kind — the status bar / task panel
    /// fall back to the plain `progress` bar.
    pub transfer: Option<TransferStats>,
}

/// How long a task must live before any surface (status bar, task panel)
/// shows it. Instant clones and other sub-perceptual work begin and end
/// inside this window and never flicker into view; only the success
/// toast marks them. (docs/features/FILE_OPS.md)
pub const SURFACE_DELAY: std::time::Duration = std::time::Duration::from_millis(150);

impl ActiveTask {
    /// Whether this task has lived long enough to be worth drawing.
    pub fn is_surfaced(&self) -> bool {
        self.started_at.elapsed() >= SURFACE_DELAY
    }
}

pub struct TaskRegistry {
    tasks: Vec<ActiveTask>,
    next_id: u64,
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            next_id: 1,
        }
    }

    pub fn begin(&mut self, kind: TaskKind, label: impl Into<String>, cancellable: bool) -> TaskId {
        let id = TaskId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.tasks.push(ActiveTask {
            id,
            kind,
            label: label.into(),
            started_at: Instant::now(),
            progress: TaskProgress::Indeterminate,
            cancellable,
            cancel: None,
            transfer: None,
        });
        id
    }

    /// `begin` for tasks with a user-facing cancel button: the panel
    /// flips `cancel` to true, the worker honors it cooperatively.
    pub fn begin_with_cancel(
        &mut self,
        kind: TaskKind,
        label: impl Into<String>,
        cancel: Arc<AtomicBool>,
    ) -> TaskId {
        let id = self.begin(kind, label, true);
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            t.cancel = Some(cancel);
        }
        id
    }

    /// Update determinate progress for `id`. No-op for stale ids; flips
    /// the task to `Determinate` if it was `Indeterminate`.
    pub fn update(&mut self, id: TaskId, progress: f32) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            t.progress = TaskProgress::Determinate(progress.clamp(0.0, 1.0));
        }
    }

    /// Attach rich transfer stats to `id` (copy/move tasks). No-op for
    /// stale ids.
    pub fn update_transfer(&mut self, id: TaskId, stats: TransferStats) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            t.transfer = Some(stats);
        }
    }

    /// Relabel `id` — used to swap a transfer's label between its
    /// "Preparing…" planning phase and its running phase. No-op for
    /// stale ids.
    pub fn set_label(&mut self, id: TaskId, label: impl Into<String>) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            t.label = label.into();
        }
    }

    /// Remove `id`. Stale ids are silently ignored.
    pub fn end(&mut self, id: TaskId) {
        self.tasks.retain(|t| t.id != id);
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, ActiveTask> {
        self.tasks.iter()
    }

    /// The task that owns the spotlight: the most-recently-started
    /// *foreground* task (file ops, search, scans — user-initiated and
    /// actively awaited), falling back to the most recent task of any
    /// kind. So a copy in progress never gets buried under an ambient
    /// prefetch that happened to start later. (docs/features/FILE_OPS.md)
    pub fn primary(&self) -> Option<&ActiveTask> {
        self.tasks
            .iter()
            .rev()
            .find(|t| t.kind.is_foreground())
            .or_else(|| self.tasks.last())
    }

    pub fn find(&self, id: TaskId) -> Option<&ActiveTask> {
        self.tasks.iter().find(|t| t.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_then_end() {
        let mut r = TaskRegistry::new();
        let id = r.begin(TaskKind::Enumeration, "Reading folder\u{2026}", true);
        assert_eq!(r.len(), 1);
        assert!(!r.is_empty());
        r.end(id);
        assert!(r.is_empty());
    }

    #[test]
    fn stale_end_is_noop() {
        let mut r = TaskRegistry::new();
        let id = r.begin(TaskKind::IconPrefetch, "Loading icons\u{2026}", false);
        r.end(id);
        r.end(id);
        assert!(r.is_empty());
    }

    #[test]
    fn primary_is_most_recent() {
        let mut r = TaskRegistry::new();
        let _a = r.begin(TaskKind::Enumeration, "Reading folder\u{2026}", true);
        let b = r.begin(TaskKind::IconPrefetch, "Loading icons\u{2026}", false);
        assert_eq!(r.primary().map(|t| t.id), Some(b));
    }

    #[test]
    fn primary_falls_back_when_newer_ends() {
        let mut r = TaskRegistry::new();
        let a = r.begin(TaskKind::Enumeration, "Reading folder\u{2026}", true);
        let b = r.begin(TaskKind::IconPrefetch, "Loading icons\u{2026}", false);
        r.end(b);
        assert_eq!(r.primary().map(|t| t.id), Some(a));
    }

    #[test]
    fn determinate_clamps() {
        let mut r = TaskRegistry::new();
        let id = r.begin(TaskKind::Enumeration, "Reading folder\u{2026}", true);
        r.update(id, 2.5);
        match r.find(id).unwrap().progress {
            TaskProgress::Determinate(p) => {
                assert!((p - 1.0).abs() < f32::EPSILON)
            }
            _ => panic!("expected determinate"),
        }
    }

    #[test]
    fn update_for_stale_id_is_noop() {
        let mut r = TaskRegistry::new();
        let id = r.begin(TaskKind::Enumeration, "x", false);
        r.end(id);
        r.update(id, 0.5);
        assert!(r.is_empty());
    }

    #[test]
    fn ids_do_not_recycle() {
        let mut r = TaskRegistry::new();
        let id1 = r.begin(TaskKind::Enumeration, "x", false);
        r.end(id1);
        let id2 = r.begin(TaskKind::Enumeration, "x", false);
        assert_ne!(id1, id2);
    }

    #[test]
    fn primary_prefers_foreground_over_newer_ambient() {
        let mut r = TaskRegistry::new();
        // A foreground copy starts first, then an ambient prefetch fires
        // later (e.g. a folder load). The copy must keep the spotlight.
        let copy = r.begin(TaskKind::FileOp, "Copying…", true);
        let _prefetch = r.begin(TaskKind::IconPrefetch, "Loading icons…", false);
        assert_eq!(r.primary().map(|t| t.id), Some(copy));
    }

    #[test]
    fn update_transfer_attaches_stats() {
        let mut r = TaskRegistry::new();
        let id = r.begin(TaskKind::FileOp, "Copying…", true);
        r.update_transfer(
            id,
            TransferStats {
                bytes_done: 5,
                bytes_total: 10,
                ..Default::default()
            },
        );
        let t = r.find(id).unwrap();
        assert_eq!(t.transfer.as_ref().map(|s| s.bytes_done), Some(5));
    }

    #[test]
    fn foreground_classification() {
        assert!(TaskKind::FileOp.is_foreground());
        assert!(TaskKind::Search.is_foreground());
        assert!(!TaskKind::IconPrefetch.is_foreground());
        assert!(!TaskKind::Enumeration.is_foreground());
    }

    #[test]
    fn multiple_concurrent_tasks_iterate_in_start_order() {
        let mut r = TaskRegistry::new();
        let a = r.begin(TaskKind::Enumeration, "a", false);
        let b = r.begin(TaskKind::IconPrefetch, "b", false);
        let c = r.begin(TaskKind::MagicPrefetch, "c", false);
        let ids: Vec<_> = r.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![a, b, c]);
    }
}
