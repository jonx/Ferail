//! Constant-space, latest-wins scheduling for selection-driven previews.
//!
//! A user can move the lead row much faster than a native preview provider can
//! answer. Starting one provider per key repeat creates an unbounded train of
//! work (and, on Windows, one STA thread / in-process COM handler per request).
//! This queue admits one active request and remembers only the newest request
//! that arrived behind it. Provider execution remains in the owning preview
//! module; this type is pure scheduling logic and therefore unit-testable.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Enqueue {
    Start,
    Queued,
    AlreadyScheduled,
}

#[derive(Debug)]
pub(crate) struct LatestRequestQueue<T> {
    active: Option<T>,
    queued: Option<T>,
}

impl<T> Default for LatestRequestQueue<T> {
    fn default() -> Self {
        Self {
            active: None,
            queued: None,
        }
    }
}

impl<T: Eq + Clone> LatestRequestQueue<T> {
    pub(crate) fn is_active(&self, request: &T) -> bool {
        self.active.as_ref() == Some(request)
    }

    pub(crate) fn enqueue(&mut self, request: T) -> Enqueue {
        if self.active.as_ref() == Some(&request) || self.queued.as_ref() == Some(&request) {
            return Enqueue::AlreadyScheduled;
        }
        if self.active.is_none() {
            self.active = Some(request);
            Enqueue::Start
        } else {
            // Deliberately replace, rather than append: an off-screen preview
            // that has not started is no longer useful after another lead-row
            // change. This is the constant-space/latest-wins guarantee.
            self.queued = Some(request);
            Enqueue::Queued
        }
    }

    /// Retire `request` if it is the active one and return the newest queued
    /// request. A stale completion cannot disturb the live request.
    pub(crate) fn complete(&mut self, request: &T) -> Option<T> {
        if self.active.as_ref() != Some(request) {
            return None;
        }
        self.active = None;
        self.queued.take()
    }

    #[cfg(test)]
    fn counts(&self) -> (usize, usize) {
        (
            usize::from(self.active.is_some()),
            usize::from(self.queued.is_some()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapid_requests_keep_only_active_and_newest() {
        let mut q = LatestRequestQueue::default();
        assert_eq!(q.enqueue("a"), Enqueue::Start);
        assert_eq!(q.enqueue("b"), Enqueue::Queued);
        assert_eq!(q.enqueue("c"), Enqueue::Queued);
        assert_eq!(q.counts(), (1, 1));
        assert_eq!(q.complete(&"a"), Some("c"));
        assert_eq!(q.counts(), (0, 0));
    }

    #[test]
    fn duplicate_active_or_queued_request_is_not_started_twice() {
        let mut q = LatestRequestQueue::default();
        assert_eq!(q.enqueue("a"), Enqueue::Start);
        assert_eq!(q.enqueue("a"), Enqueue::AlreadyScheduled);
        assert_eq!(q.enqueue("b"), Enqueue::Queued);
        assert_eq!(q.enqueue("b"), Enqueue::AlreadyScheduled);
        assert_eq!(q.counts(), (1, 1));
    }

    #[test]
    fn stale_completion_cannot_release_live_request() {
        let mut q = LatestRequestQueue::default();
        assert_eq!(q.enqueue("live"), Enqueue::Start);
        assert_eq!(q.enqueue("next"), Enqueue::Queued);
        assert_eq!(q.complete(&"stale"), None);
        assert_eq!(q.counts(), (1, 1));
        assert_eq!(q.complete(&"live"), Some("next"));
    }
}
