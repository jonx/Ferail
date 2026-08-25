//! Per-tab ownership for pathless platform namespace surfaces.
//!
//! This layer contains no Windows mechanism. A provider is run on a worker and
//! sends bounded `ferail-core` batches back to this session. The session drops
//! with its tab, cancelling the worker and releasing every opaque identity.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ferail_core::platform_namespace::{
    PlatformBatchApply, PlatformItemId, PlatformListingBatch, PlatformListingRequest,
    PlatformLocation, PlatformLocationErrorKind, PlatformLocationHistory,
    PlatformNamespaceProvider, PlatformSurfacePhase, PlatformSurfaceStore,
};

/// A small number of pending batches provides backpressure without starving
/// a fast provider. At 512 rows per batch this caps the cross-thread queue at
/// 2,048 rows regardless of namespace size.
pub const PLATFORM_PENDING_BATCHES_MAX: usize = 4;

pub(crate) enum PlatformListingEvent {
    Batch(PlatformListingBatch),
    Finished {
        request: PlatformListingRequest,
        result: Result<(), PlatformLocationErrorKind>,
    },
}

/// Blocking provider adapter. Run only on a background executor: a full
/// bounded channel deliberately backpressures Shell enumeration instead of
/// allowing an invisible location to allocate an unbounded result queue.
pub(crate) fn run_provider_stream(
    provider: Arc<dyn PlatformNamespaceProvider>,
    request: PlatformListingRequest,
    cancel: Arc<AtomicBool>,
    sender: async_channel::Sender<PlatformListingEvent>,
) {
    let result = provider.enumerate(request.clone(), &cancel, &mut |batch| {
        !cancel.load(Ordering::Relaxed)
            && sender
                .send_blocking(PlatformListingEvent::Batch(batch))
                .is_ok()
    });
    let _ = sender.send_blocking(PlatformListingEvent::Finished { request, result });
}

/// Complement-based selection for a platform surface. Select All stays O(1)
/// even if a provider exposes millions of items; the set stores selected ids
/// in sparse mode and deselected exceptions in all-selected mode.
#[derive(Debug, Default)]
pub struct PlatformSelection {
    all: bool,
    ids: HashSet<PlatformItemId>,
    lead: Option<PlatformItemId>,
}

impl PlatformSelection {
    pub fn is_selected(&self, id: &PlatformItemId) -> bool {
        self.all ^ self.ids.contains(id)
    }

    pub fn toggle(&mut self, id: PlatformItemId) {
        if !self.ids.remove(&id) {
            self.ids.insert(id);
        }
        self.lead = Some(id);
    }

    pub fn select_only(&mut self, id: PlatformItemId) {
        self.all = false;
        self.ids.clear();
        self.ids.insert(id);
        self.lead = Some(id);
    }

    pub fn select_all(&mut self) {
        self.all = true;
        self.ids.clear();
    }

    pub fn clear(&mut self) {
        self.all = false;
        self.ids.clear();
        self.lead = None;
    }

    pub fn is_all(&self) -> bool {
        self.all
    }

    pub fn exception_or_selection_count(&self) -> usize {
        self.ids.len()
    }

    pub fn selected_count(&self, total: usize) -> usize {
        if self.all {
            total.saturating_sub(self.ids.len())
        } else {
            self.ids.len()
        }
    }

    pub fn descriptor(&self) -> ferail_core::platform_namespace::PlatformSelectionSpec {
        let ids: Vec<PlatformItemId> = self.ids.iter().copied().collect();
        if self.all {
            ferail_core::platform_namespace::PlatformSelectionSpec::all_except(ids)
        } else {
            ferail_core::platform_namespace::PlatformSelectionSpec::explicit(ids)
        }
    }

    pub fn lead(&self) -> Option<PlatformItemId> {
        self.lead
    }

    fn reconcile(&mut self, store: &PlatformSurfaceStore) {
        // `ids` is sparse in normal interaction and contains only explicit
        // exceptions after Select All. Scan the row slice for those few ids;
        // never construct a second all-row HashSet merely to reconcile.
        self.ids
            .retain(|id| store.items().iter().any(|item| &item.id == id));
        if self
            .lead
            .is_some_and(|lead| !store.items().iter().any(|item| item.id == lead))
        {
            self.lead = None;
        }
    }
}

/// State owned by exactly one pathless-location tab.
pub struct PlatformNamespaceSession {
    provider: Arc<dyn PlatformNamespaceProvider>,
    store: PlatformSurfaceStore,
    history: PlatformLocationHistory,
    selection: PlatformSelection,
    scroll: gpui::UniformListScrollHandle,
    cancel: Option<Arc<AtomicBool>>,
}

impl PlatformNamespaceSession {
    pub fn new(
        provider: Arc<dyn PlatformNamespaceProvider>,
        initial: PlatformLocation,
    ) -> Result<Self, PlatformLocationErrorKind> {
        if provider.id() != initial.provider {
            return Err(PlatformLocationErrorKind::Failed);
        }
        Ok(Self {
            provider,
            store: PlatformSurfaceStore::default(),
            history: PlatformLocationHistory::new(initial),
            selection: PlatformSelection::default(),
            scroll: gpui::UniformListScrollHandle::new(),
            cancel: None,
        })
    }

    pub fn provider(&self) -> Arc<dyn PlatformNamespaceProvider> {
        self.provider.clone()
    }

    pub fn store(&self) -> &PlatformSurfaceStore {
        &self.store
    }

    pub fn selection(&self) -> &PlatformSelection {
        &self.selection
    }

    pub fn selection_mut(&mut self) -> &mut PlatformSelection {
        &mut self.selection
    }

    pub fn move_selection(&mut self, delta: isize) -> Option<PlatformItemId> {
        let items = self.store.items();
        let current = self
            .selection
            .lead()
            .and_then(|lead| items.iter().position(|item| item.id == lead));
        let index = match current {
            Some(index) => index
                .saturating_add_signed(delta)
                .min(items.len().saturating_sub(1)),
            None if delta < 0 => items.len().checked_sub(1)?,
            None => 0,
        };
        let id = items.get(index)?.id;
        self.selection.select_only(id);
        self.scroll
            .scroll_to_item(index, gpui::ScrollStrategy::Center);
        Some(id)
    }

    pub fn select_edge(&mut self, last: bool) -> Option<PlatformItemId> {
        let id = if last {
            self.store.items().last()?.id
        } else {
            self.store.items().first()?.id
        };
        self.selection.select_only(id);
        let index = if last {
            self.store.items().len().saturating_sub(1)
        } else {
            0
        };
        self.scroll
            .scroll_to_item(index, gpui::ScrollStrategy::Center);
        Some(id)
    }

    pub fn scroll(&self) -> &gpui::UniformListScrollHandle {
        &self.scroll
    }

    pub fn current(&self) -> &PlatformLocation {
        self.history.current()
    }

    pub fn can_go_back(&self) -> bool {
        self.history.can_go_back()
    }

    pub fn can_go_forward(&self) -> bool {
        self.history.can_go_forward()
    }

    pub fn navigate_to(
        &mut self,
        target: PlatformLocation,
    ) -> Result<(PlatformListingRequest, Arc<AtomicBool>), PlatformLocationErrorKind> {
        if target.provider != self.provider.id() {
            return Err(PlatformLocationErrorKind::Failed);
        }
        self.history.navigate_to(target);
        self.selection.clear();
        Ok(self.begin_current_request())
    }

    pub fn refresh(&mut self) -> (PlatformListingRequest, Arc<AtomicBool>) {
        self.begin_current_request()
    }

    pub fn go_back(&mut self) -> Option<(PlatformListingRequest, Arc<AtomicBool>)> {
        self.history.go_back()?;
        self.selection.clear();
        Some(self.begin_current_request())
    }

    pub fn go_forward(&mut self) -> Option<(PlatformListingRequest, Arc<AtomicBool>)> {
        self.history.go_forward()?;
        self.selection.clear();
        Some(self.begin_current_request())
    }

    pub fn apply_batch(&mut self, batch: PlatformListingBatch) -> PlatformBatchApply {
        let is_last = batch.is_last;
        let result = self.store.apply_batch(batch);
        if result == PlatformBatchApply::Applied && is_last {
            self.selection.reconcile(&self.store);
            self.cancel = None;
        }
        result
    }

    pub fn finish_with_error(
        &mut self,
        request: &PlatformListingRequest,
        error: PlatformLocationErrorKind,
    ) -> bool {
        let applied = self.store.finish_with_error(&request.token, error);
        if applied {
            self.selection.clear();
            self.cancel = None;
        }
        applied
    }

    pub fn finish_provider(
        &mut self,
        request: &PlatformListingRequest,
        result: Result<(), PlatformLocationErrorKind>,
    ) -> bool {
        match result {
            // A conforming provider marks exactly one batch as final. If it
            // returns success without doing so, fail visibly instead of
            // leaving the tab in an endless loading state.
            Ok(()) if self.phase() == PlatformSurfacePhase::Loading => {
                self.finish_with_error(request, PlatformLocationErrorKind::Failed)
            }
            Ok(()) => false,
            // Navigation/tab closure owns cancellation and normally makes the
            // request stale. It is not a user-facing provider failure.
            Err(PlatformLocationErrorKind::Cancelled) => false,
            Err(error) => self.finish_with_error(request, error),
        }
    }

    pub fn phase(&self) -> PlatformSurfacePhase {
        self.store.phase()
    }

    pub fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn begin_current_request(&mut self) -> (PlatformListingRequest, Arc<AtomicBool>) {
        self.cancel();
        let request = self.store.begin(self.history.current().clone());
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        (request, cancel)
    }
}

impl std::fmt::Debug for PlatformNamespaceSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlatformNamespaceSession")
            .field("provider", &"<opaque>")
            .field("phase", &self.store.phase())
            .field("item_count", &self.store.items().len())
            .field("breadcrumb_count", &self.store.breadcrumbs().len())
            .field("history", &self.history)
            .field("selection", &self.selection)
            .field("has_active_request", &self.cancel.is_some())
            .finish()
    }
}

impl Drop for PlatformNamespaceSession {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferail_core::platform_namespace::{
        LocationTarget, PlatformBreadcrumb, PlatformCapabilities, PlatformItem, PlatformItemFlags,
        PlatformItemKind, PlatformNamespaceProvider, PlatformProviderId,
    };

    struct FakeProvider {
        id: PlatformProviderId,
    }

    impl FakeProvider {
        fn location(&self, item: &str) -> PlatformLocation {
            PlatformLocation::new(self.id.clone(), item_id(item))
        }

        fn item(&self, name: &str) -> PlatformItem {
            PlatformItem {
                id: item_id(name),
                label: Arc::from(name),
                kind: PlatformItemKind::Container,
                target: LocationTarget::Platform(self.location(name)),
                capabilities: PlatformCapabilities::OPEN.union(PlatformCapabilities::ENUMERATE),
                flags: PlatformItemFlags::default(),
                icon_key: Some(Arc::from("stock:folder")),
            }
        }
    }

    impl PlatformNamespaceProvider for FakeProvider {
        fn id(&self) -> PlatformProviderId {
            self.id.clone()
        }

        fn enumerate(
            &self,
            request: PlatformListingRequest,
            cancel: &AtomicBool,
            emit: &mut dyn FnMut(PlatformListingBatch) -> bool,
        ) -> Result<(), PlatformLocationErrorKind> {
            for (index, names) in [["A", "B"].as_slice(), ["C"].as_slice()]
                .into_iter()
                .enumerate()
            {
                if cancel.load(Ordering::Relaxed) {
                    return Err(PlatformLocationErrorKind::Cancelled);
                }
                if !emit(PlatformListingBatch {
                    token: request.token.clone(),
                    breadcrumbs: (index == 0).then(|| {
                        vec![PlatformBreadcrumb {
                            location: self.location("root"),
                            label: Arc::from("Fake root"),
                        }]
                    }),
                    items: names.iter().map(|name| self.item(name)).collect(),
                    is_last: index == 1,
                }) {
                    return Err(PlatformLocationErrorKind::Cancelled);
                }
            }
            Ok(())
        }
    }

    fn provider() -> FakeProvider {
        FakeProvider {
            id: PlatformProviderId::new("fake-provider"),
        }
    }

    fn item_id(name: &str) -> PlatformItemId {
        let raw = name.bytes().fold(1u64, |value, byte| {
            value.wrapping_mul(131) ^ u64::from(byte)
        });
        PlatformItemId::from_raw(raw.max(1)).unwrap()
    }

    #[test]
    fn fake_provider_streams_into_a_tab_owned_surface() {
        let provider = Arc::new(provider());
        let mut session =
            PlatformNamespaceSession::new(provider.clone(), provider.location("root")).unwrap();
        let (request, cancel) = session.refresh();
        let result = provider.enumerate(request, &cancel, &mut |batch| {
            session.apply_batch(batch) == PlatformBatchApply::Applied
        });
        assert_eq!(result, Ok(()));
        assert_eq!(session.phase(), PlatformSurfacePhase::Ready);
        let labels: Vec<&str> = session
            .store()
            .items()
            .iter()
            .map(|item| item.label.as_ref())
            .collect();
        assert_eq!(labels, ["A", "B", "C"]);
        assert_eq!(session.store().breadcrumbs().len(), 1);
    }

    #[test]
    fn navigation_cancels_the_previous_provider_request() {
        let provider = Arc::new(provider());
        let mut session =
            PlatformNamespaceSession::new(provider.clone(), provider.location("root")).unwrap();
        let (_, old_cancel) = session.refresh();
        session.navigate_to(provider.location("phone")).unwrap();
        assert!(old_cancel.load(Ordering::Relaxed));
        assert_eq!(session.current(), &provider.location("phone"));
    }

    #[test]
    fn dropping_a_tab_cancels_its_worker() {
        let provider = Arc::new(provider());
        let cancel = {
            let mut session =
                PlatformNamespaceSession::new(provider.clone(), provider.location("root")).unwrap();
            let (_, cancel) = session.refresh();
            cancel
        };
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn the_tab_session_owns_the_provider_arena_lifetime() {
        let provider = Arc::new(provider());
        let session =
            PlatformNamespaceSession::new(provider.clone(), provider.location("root")).unwrap();
        assert_eq!(Arc::strong_count(&provider), 2);
        drop(session);
        assert_eq!(Arc::strong_count(&provider), 1);
    }

    #[test]
    fn select_all_is_constant_state_and_survives_stream_completion() {
        let provider = Arc::new(provider());
        let mut session =
            PlatformNamespaceSession::new(provider.clone(), provider.location("root")).unwrap();
        let (request, _) = session.refresh();
        session.selection_mut().select_all();
        session.selection_mut().toggle(item_id("B"));
        assert!(session.selection().is_all());
        assert_eq!(session.selection().exception_or_selection_count(), 1);
        assert_eq!(
            session.apply_batch(PlatformListingBatch {
                token: request.token,
                breadcrumbs: None,
                items: ["A", "B", "C"]
                    .into_iter()
                    .map(|name| provider.item(name))
                    .collect(),
                is_last: true,
            }),
            PlatformBatchApply::Applied
        );
        assert!(session.selection().is_selected(&item_id("A")));
        assert!(!session.selection().is_selected(&item_id("B")));
        let descriptor = session.selection().descriptor();
        assert!(descriptor.all);
        assert_eq!(descriptor.ids, vec![item_id("B")]);
    }

    #[test]
    fn provider_stream_uses_bounded_batches_and_finishes() {
        let provider = Arc::new(provider());
        let initial = provider.location("root");
        let mut session = PlatformNamespaceSession::new(provider.clone(), initial.clone()).unwrap();
        let (request, cancel) = session.refresh();
        let (sender, receiver) = async_channel::bounded(PLATFORM_PENDING_BATCHES_MAX);
        let worker_request = request.clone();
        let worker = std::thread::spawn(move || {
            run_provider_stream(provider, worker_request, cancel, sender)
        });

        while let Ok(event) = receiver.recv_blocking() {
            assert!(receiver.len() <= PLATFORM_PENDING_BATCHES_MAX);
            match event {
                PlatformListingEvent::Batch(batch) => {
                    assert_eq!(session.apply_batch(batch), PlatformBatchApply::Applied);
                }
                PlatformListingEvent::Finished { request, result } => {
                    session.finish_provider(&request, result);
                    break;
                }
            }
        }
        worker.join().unwrap();
        assert_eq!(session.phase(), PlatformSurfacePhase::Ready);
        assert_eq!(session.store().items().len(), 3);
    }

    #[test]
    fn provider_success_without_final_batch_fails_visibly() {
        let provider = Arc::new(provider());
        let mut session =
            PlatformNamespaceSession::new(provider.clone(), provider.location("root")).unwrap();
        let (request, _) = session.refresh();
        assert_eq!(
            session.apply_batch(PlatformListingBatch {
                token: request.token.clone(),
                breadcrumbs: None,
                items: vec![provider.item("partial")],
                is_last: false,
            }),
            PlatformBatchApply::Applied
        );
        assert!(session.finish_provider(&request, Ok(())));
        assert_eq!(
            session.phase(),
            PlatformSurfacePhase::Unavailable(PlatformLocationErrorKind::Failed)
        );
        assert!(session.store().items().is_empty());
    }

    #[test]
    fn keyboard_selection_moves_by_identity_and_stays_sparse() {
        let provider = Arc::new(provider());
        let mut session =
            PlatformNamespaceSession::new(provider.clone(), provider.location("root")).unwrap();
        let (request, _) = session.refresh();
        session.apply_batch(PlatformListingBatch {
            token: request.token,
            breadcrumbs: None,
            items: ["A", "B", "C"]
                .into_iter()
                .map(|name| provider.item(name))
                .collect(),
            is_last: true,
        });
        assert_eq!(session.move_selection(1), Some(item_id("A")));
        assert_eq!(session.move_selection(1), Some(item_id("B")));
        assert_eq!(session.move_selection(-1), Some(item_id("A")));
        assert_eq!(session.selection().exception_or_selection_count(), 1);
    }

    #[test]
    fn a_tab_rejects_identity_from_another_provider_arena() {
        let provider = Arc::new(provider());
        let mut session =
            PlatformNamespaceSession::new(provider.clone(), provider.location("root")).unwrap();
        let foreign = PlatformLocation::new(
            PlatformProviderId::new("another-provider"),
            item_id("foreign"),
        );
        assert!(matches!(
            session.navigate_to(foreign),
            Err(PlatformLocationErrorKind::Failed)
        ));
        assert_eq!(session.current(), &provider.location("root"));
    }
}
