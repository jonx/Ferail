//! Task registry — single source of truth for "what background work is
//! in flight right now."
//!
//! Long-running jobs (streaming enumeration, icon prefetch, magic
//! prefetch, future copy/move) call `App::begin_task` / `App::end_task`,
//! which forward to this registry and bridge to the existing
//! `ProgressStrip` so the 2-DIP comet keeps painting. The registry feeds
//! the status-bar text and the popover task list; both surfaces always
//! agree because both read `tasks.iter()` from the same store.
//!
//! The registry is platform- and renderer-agnostic. Bridging to the
//! strip and to AppKit lives in `main.rs`.

use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TaskId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskKind {
    Enumeration,
    IconPrefetch,
    MagicPrefetch,
    QuarantinePrefetch,
    DiskUsage,
}

#[derive(Clone, Copy, Debug)]
pub enum TaskProgress {
    Indeterminate,
    Determinate(f32),
}

#[derive(Clone, Debug)]
pub struct ActiveTask {
    pub id: TaskId,
    pub kind: TaskKind,
    pub label: String,
    pub started_at: Instant,
    pub progress: TaskProgress,
    pub cancellable: bool,
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
        });
        id
    }

    /// Update determinate progress for `id`. No-op for stale ids; flips
    /// the task to `Determinate` if it was `Indeterminate`.
    pub fn update(&mut self, id: TaskId, progress: f32) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            t.progress = TaskProgress::Determinate(progress.clamp(0.0, 1.0));
        }
    }

    /// Remove `id`. Stale ids are silently ignored — same shape as
    /// `ProgressStrip::complete`, so call sites can safely call this
    /// from the late event handler even when a newer task has already
    /// taken over.
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

    /// Most-recently-started task, if any. The status-bar text uses
    /// this when exactly one task is in flight; the panel uses it as
    /// the implicit "primary" row order anchor.
    pub fn primary(&self) -> Option<&ActiveTask> {
        self.tasks.last()
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
        let id = r.begin(TaskKind::Enumeration, "Reading folder…", true);
        assert_eq!(r.len(), 1);
        assert!(!r.is_empty());
        r.end(id);
        assert!(r.is_empty());
    }

    #[test]
    fn stale_end_is_noop() {
        let mut r = TaskRegistry::new();
        let id = r.begin(TaskKind::IconPrefetch, "Loading icons…", false);
        r.end(id);
        r.end(id);
        assert!(r.is_empty());
    }

    #[test]
    fn primary_is_most_recent() {
        let mut r = TaskRegistry::new();
        let _a = r.begin(TaskKind::Enumeration, "Reading folder…", true);
        let b = r.begin(TaskKind::IconPrefetch, "Loading icons…", false);
        assert_eq!(r.primary().map(|t| t.id), Some(b));
    }

    #[test]
    fn primary_falls_back_when_newer_ends() {
        let mut r = TaskRegistry::new();
        let a = r.begin(TaskKind::Enumeration, "Reading folder…", true);
        let b = r.begin(TaskKind::IconPrefetch, "Loading icons…", false);
        r.end(b);
        assert_eq!(r.primary().map(|t| t.id), Some(a));
    }

    #[test]
    fn determinate_clamps() {
        let mut r = TaskRegistry::new();
        let id = r.begin(TaskKind::Enumeration, "Reading folder…", true);
        r.update(id, 2.5);
        match r.find(id).unwrap().progress {
            TaskProgress::Determinate(p) => assert!((p - 1.0).abs() < f32::EPSILON),
            _ => panic!("expected determinate"),
        }
    }

    #[test]
    fn update_for_stale_id_is_noop() {
        let mut r = TaskRegistry::new();
        let id = r.begin(TaskKind::Enumeration, "x", false);
        r.end(id);
        r.update(id, 0.5); // stale; should not panic or insert
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
    fn multiple_concurrent_tasks_iterate_in_start_order() {
        let mut r = TaskRegistry::new();
        let a = r.begin(TaskKind::Enumeration, "a", false);
        let b = r.begin(TaskKind::IconPrefetch, "b", false);
        let c = r.begin(TaskKind::MagicPrefetch, "c", false);
        let ids: Vec<_> = r.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![a, b, c]);
    }
}
