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

/// Complement-based selection for a platform surface. Select All stays O(1)
/// even if a provider exposes millions of items; the set stores selected ids
/// in sparse mode and deselected exceptions in all-selected mode.
#[derive(Debug, Default)]
pub struct PlatformSelection {
    all: bool,
    ids: HashSet<PlatformItemId>,
}

impl PlatformSelection {
    pub fn is_selected(&self, id: &PlatformItemId) -> bool {
        self.all ^ self.ids.contains(id)
    }

    pub fn toggle(&mut self, id: PlatformItemId) {
        if !self.ids.remove(&id) {
            self.ids.insert(id);
        }
    }

    pub fn select_all(&mut self) {
        self.all = true;
        self.ids.clear();
    }

    pub fn clear(&mut self) {
        self.all = false;
        self.ids.clear();
    }

    pub fn is_all(&self) -> bool {
        self.all
    }

    pub fn exception_or_selection_count(&self) -> usize {
        self.ids.len()
    }

    fn reconcile(&mut self, store: &PlatformSurfaceStore) {
        // `ids` is sparse in normal interaction and contains only explicit
        // exceptions after Select All. Scan the row slice for those few ids;
        // never construct a second all-row HashSet merely to reconcile.
        self.ids
            .retain(|id| store.items().iter().any(|item| &item.id == id));
    }
}

/// State owned by exactly one pathless-location tab.
pub struct PlatformNamespaceSession {
    provider: Arc<dyn PlatformNamespaceProvider>,
    store: PlatformSurfaceStore,
    history: PlatformLocationHistory,
    selection: PlatformSelection,
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

    pub fn current(&self) -> &PlatformLocation {
        self.history.current()
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
        LocationTarget, PlatformCapabilities, PlatformItem, PlatformItemFlags, PlatformItemKind,
        PlatformNamespaceProvider, PlatformProviderId,
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
                    breadcrumbs: None,
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
