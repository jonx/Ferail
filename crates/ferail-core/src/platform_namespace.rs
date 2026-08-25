//! Platform-owned locations which do not necessarily have filesystem paths.
//!
//! Windows Shell namespace roots such as This PC, Recycle Bin and MTP are the
//! first consumer. The contract deliberately carries only owned opaque keys
//! and display-ready data: no PIDL pointer, COM object, HWND or platform error
//! crosses into `ferail-core`, and ordinary [`crate::FileEntry`] rows remain
//! unchanged.

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Maximum number of rows accepted in one provider emission. Providers may
/// emit any number of batches, but each UI apply remains predictably bounded.
pub const PLATFORM_LISTING_BATCH_MAX: usize = 512;

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Arc<str>);

        impl $name {
            pub fn new(value: impl Into<Arc<str>>) -> Self {
                Self(value.into())
            }

            /// Provider boundary only. Opaque keys may still encode personal
            /// or machine-specific information and must not enter diagnostics.
            pub fn as_provider_key(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), "(<opaque>)"))
            }
        }
    };
}

opaque_id!(PlatformProviderId);
opaque_id!(PlatformItemId);

/// Stable process-owned identity for a platform item.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct PlatformLocation {
    pub provider: PlatformProviderId,
    pub item: PlatformItemId,
}

impl PlatformLocation {
    pub fn new(provider: PlatformProviderId, item: PlatformItemId) -> Self {
        Self { provider, item }
    }
}

impl fmt::Debug for PlatformLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformLocation")
            .field("provider", &self.provider)
            .field("item", &self.item)
            .finish()
    }
}

/// A navigation target is either the existing fast filesystem path or an
/// opaque item owned by one platform provider. A provider should hand a real
/// path back as soon as one exists so normal browsing stays in `NativeFs`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocationTarget {
    FileSystem(PathBuf),
    Platform(PlatformLocation),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformItemKind {
    Container,
    File,
    Link,
}

/// Small capability mask for menu/action gating. Absence means unsupported;
/// callers never infer destructive or recoverable behavior from item kind.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlatformCapabilities(u16);

impl PlatformCapabilities {
    pub const OPEN: Self = Self(1 << 0);
    pub const ENUMERATE: Self = Self(1 << 1);
    pub const PROPERTIES: Self = Self(1 << 2);
    pub const REVEAL: Self = Self(1 << 3);
    pub const NATIVE_MENU: Self = Self(1 << 4);
    pub const TRANSFER: Self = Self(1 << 5);
    pub const TRASH_RECOVERABLE: Self = Self(1 << 6);
    pub const DELETE_PERMANENT: Self = Self(1 << 7);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, capability: Self) -> bool {
        self.0 & capability.0 == capability.0
    }
}

/// One row returned by a platform namespace provider. It belongs only to the
/// specialized platform surface and must never be converted into a fake path
/// or inserted into the global filesystem `NodeStore`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformItem {
    pub id: PlatformItemId,
    pub label: Arc<str>,
    pub kind: PlatformItemKind,
    pub target: LocationTarget,
    pub capabilities: PlatformCapabilities,
    /// Opaque, non-personal cache identity such as a stock icon kind. Raw
    /// PIDLs, paths and provider object addresses are forbidden here.
    pub icon_key: Option<Arc<str>>,
}

/// Display-only breadcrumb identity supplied by the same provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformBreadcrumb {
    pub location: PlatformLocation,
    pub label: Arc<str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformLocationErrorKind {
    Unavailable,
    NotFound,
    PermissionDenied,
    TimedOut,
    Cancelled,
    Failed,
}

/// Monotonic request identity. A token includes the location so a result for
/// one virtual folder cannot be applied to another even if a caller mixes up
/// worker channels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformListingToken {
    generation: u64,
    location: PlatformLocation,
}

impl PlatformListingToken {
    pub fn location(&self) -> &PlatformLocation {
        &self.location
    }
}

/// Request passed to a provider on a background worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformListingRequest {
    pub token: PlatformListingToken,
    pub suggested_batch_size: usize,
}

/// One bounded emission from a provider. Breadcrumbs are normally sent only
/// with the first batch but may be corrected by a later one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformListingBatch {
    pub token: PlatformListingToken,
    pub breadcrumbs: Option<Vec<PlatformBreadcrumb>>,
    pub items: Vec<PlatformItem>,
    pub is_last: bool,
}

/// Synchronous provider boundary intended to be called from a background
/// executor. Returning `false` from `emit` asks the provider to stop promptly;
/// `cancel` handles navigation, tab closure and application shutdown.
pub trait PlatformNamespaceProvider: Send + Sync {
    fn id(&self) -> PlatformProviderId;

    fn enumerate(
        &self,
        request: PlatformListingRequest,
        cancel: &AtomicBool,
        emit: &mut dyn FnMut(PlatformListingBatch) -> bool,
    ) -> Result<(), PlatformLocationErrorKind>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PlatformSurfacePhase {
    #[default]
    Idle,
    Loading,
    Ready,
    Unavailable(PlatformLocationErrorKind),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformBatchApply {
    Applied,
    Stale,
    RejectedOversized,
    RejectedAfterCompletion,
}

/// Tab/surface-owned streamed row store. Dropping it releases every provider
/// identity at once; none enter the process-global filesystem stores.
#[derive(Debug, Default)]
pub struct PlatformSurfaceStore {
    generation: u64,
    location: Option<PlatformLocation>,
    phase: PlatformSurfacePhase,
    breadcrumbs: Vec<PlatformBreadcrumb>,
    items: Vec<PlatformItem>,
}

impl PlatformSurfaceStore {
    pub fn phase(&self) -> PlatformSurfacePhase {
        self.phase
    }

    pub fn location(&self) -> Option<&PlatformLocation> {
        self.location.as_ref()
    }

    pub fn breadcrumbs(&self) -> &[PlatformBreadcrumb] {
        &self.breadcrumbs
    }

    pub fn items(&self) -> &[PlatformItem] {
        &self.items
    }

    /// Starts navigation or refresh and releases the previous surface rows
    /// immediately. The UI may stage them separately if it wants an atomic
    /// visual swap, but stale batches can no longer mutate this store.
    pub fn begin(&mut self, location: PlatformLocation) -> PlatformListingRequest {
        self.generation = self.generation.wrapping_add(1);
        self.location = Some(location.clone());
        self.phase = PlatformSurfacePhase::Loading;
        self.breadcrumbs.clear();
        self.items.clear();
        PlatformListingRequest {
            token: PlatformListingToken {
                generation: self.generation,
                location,
            },
            suggested_batch_size: PLATFORM_LISTING_BATCH_MAX,
        }
    }

    pub fn apply_batch(&mut self, batch: PlatformListingBatch) -> PlatformBatchApply {
        if !self.token_is_current(&batch.token) {
            return PlatformBatchApply::Stale;
        }
        if self.phase != PlatformSurfacePhase::Loading {
            return PlatformBatchApply::RejectedAfterCompletion;
        }
        if batch.items.len() > PLATFORM_LISTING_BATCH_MAX {
            return PlatformBatchApply::RejectedOversized;
        }
        if let Some(breadcrumbs) = batch.breadcrumbs {
            self.breadcrumbs = breadcrumbs;
        }
        self.items.extend(batch.items);
        if batch.is_last {
            self.phase = PlatformSurfacePhase::Ready;
        }
        PlatformBatchApply::Applied
    }

    pub fn finish_with_error(
        &mut self,
        token: &PlatformListingToken,
        error: PlatformLocationErrorKind,
    ) -> bool {
        if !self.token_is_current(token) || self.phase != PlatformSurfacePhase::Loading {
            return false;
        }
        self.phase = PlatformSurfacePhase::Unavailable(error);
        self.items.clear();
        self.breadcrumbs.clear();
        true
    }

    fn token_is_current(&self, token: &PlatformListingToken) -> bool {
        token.generation == self.generation && self.location.as_ref() == Some(&token.location)
    }
}

/// History for pathless locations. Filesystem tabs retain their existing
/// compact `NodeId` navigation state and pay no enum/payload cost per row.
#[derive(Clone, Debug)]
pub struct PlatformLocationHistory {
    current: PlatformLocation,
    back: Vec<PlatformLocation>,
    forward: Vec<PlatformLocation>,
    max_len: usize,
}

impl PlatformLocationHistory {
    pub fn new(initial: PlatformLocation) -> Self {
        Self {
            current: initial,
            back: Vec::new(),
            forward: Vec::new(),
            max_len: 100,
        }
    }

    pub fn current(&self) -> &PlatformLocation {
        &self.current
    }

    pub fn navigate_to(&mut self, target: PlatformLocation) {
        if self.current == target {
            return;
        }
        self.back.push(self.current.clone());
        if self.back.len() > self.max_len {
            self.back.remove(0);
        }
        self.current = target;
        self.forward.clear();
    }

    pub fn go_back(&mut self) -> Option<&PlatformLocation> {
        let previous = self.back.pop()?;
        self.forward
            .push(std::mem::replace(&mut self.current, previous));
        Some(&self.current)
    }

    pub fn go_forward(&mut self) -> Option<&PlatformLocation> {
        let next = self.forward.pop()?;
        self.back.push(std::mem::replace(&mut self.current, next));
        Some(&self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> PlatformProviderId {
        PlatformProviderId::new("provider-secret")
    }

    fn location(name: &str) -> PlatformLocation {
        PlatformLocation::new(provider(), PlatformItemId::new(name))
    }

    fn item(name: &str, target: LocationTarget) -> PlatformItem {
        PlatformItem {
            id: PlatformItemId::new(name),
            label: Arc::from(name),
            kind: PlatformItemKind::Container,
            target,
            capabilities: PlatformCapabilities::OPEN.union(PlatformCapabilities::ENUMERATE),
            icon_key: Some(Arc::from("stock:folder")),
        }
    }

    fn batch(request: &PlatformListingRequest, names: &[&str], last: bool) -> PlatformListingBatch {
        PlatformListingBatch {
            token: request.token.clone(),
            breadcrumbs: None,
            items: names
                .iter()
                .map(|name| item(name, LocationTarget::Platform(location(name))))
                .collect(),
            is_last: last,
        }
    }

    #[test]
    fn opaque_ids_do_not_leak_through_debug() {
        assert_eq!(format!("{:?}", provider()), "PlatformProviderId(<opaque>)");
        assert!(!format!("{:?}", location("personal-device-name")).contains("personal"));
    }

    #[test]
    fn stale_batches_cannot_cross_navigation_generations() {
        let mut store = PlatformSurfaceStore::default();
        let old = store.begin(location("old"));
        let current = store.begin(location("current"));
        assert_eq!(
            store.apply_batch(batch(&old, &["stale"], true)),
            PlatformBatchApply::Stale
        );
        assert_eq!(
            store.apply_batch(batch(&current, &["kept"], true)),
            PlatformBatchApply::Applied
        );
        assert_eq!(store.items()[0].label.as_ref(), "kept");
    }

    #[test]
    fn streaming_batches_append_and_complete_once() {
        let mut store = PlatformSurfaceStore::default();
        let request = store.begin(location("root"));
        assert_eq!(
            store.apply_batch(batch(&request, &["a", "b"], false)),
            PlatformBatchApply::Applied
        );
        assert_eq!(store.phase(), PlatformSurfacePhase::Loading);
        assert_eq!(
            store.apply_batch(batch(&request, &["c"], true)),
            PlatformBatchApply::Applied
        );
        assert_eq!(store.phase(), PlatformSurfacePhase::Ready);
        assert_eq!(store.items().len(), 3);
        assert_eq!(
            store.apply_batch(batch(&request, &["late"], true)),
            PlatformBatchApply::RejectedAfterCompletion
        );
    }

    #[test]
    fn oversized_batches_are_rejected_without_partial_apply() {
        let mut store = PlatformSurfaceStore::default();
        let request = store.begin(location("root"));
        let names = vec!["row"; PLATFORM_LISTING_BATCH_MAX + 1];
        assert_eq!(
            store.apply_batch(batch(&request, &names, false)),
            PlatformBatchApply::RejectedOversized
        );
        assert!(store.items().is_empty());
        assert_eq!(store.phase(), PlatformSurfacePhase::Loading);
    }

    #[test]
    fn disappearance_is_an_explicit_recoverable_state() {
        let mut store = PlatformSurfaceStore::default();
        let request = store.begin(location("phone"));
        assert!(store.finish_with_error(&request.token, PlatformLocationErrorKind::NotFound));
        assert_eq!(
            store.phase(),
            PlatformSurfacePhase::Unavailable(PlatformLocationErrorKind::NotFound)
        );
        assert!(store.items().is_empty());
    }

    #[test]
    fn platform_history_needs_no_filesystem_path() {
        let mut history = PlatformLocationHistory::new(location("this-pc"));
        history.navigate_to(location("phone"));
        history.navigate_to(location("photos"));
        assert_eq!(history.go_back(), Some(&location("phone")));
        assert_eq!(history.go_back(), Some(&location("this-pc")));
        assert_eq!(history.go_forward(), Some(&location("phone")));
    }

    #[test]
    fn capabilities_never_imply_recoverable_trash() {
        let read_only = PlatformCapabilities::OPEN.union(PlatformCapabilities::PROPERTIES);
        assert!(read_only.contains(PlatformCapabilities::OPEN));
        assert!(!read_only.contains(PlatformCapabilities::TRASH_RECOVERABLE));
        assert!(!read_only.contains(PlatformCapabilities::DELETE_PERMANENT));
    }

    #[test]
    fn filesystem_handoff_is_explicit() {
        let path = PathBuf::from(r"C:\Users\Test\OneDrive");
        let row = item("OneDrive", LocationTarget::FileSystem(path.clone()));
        assert_eq!(row.target, LocationTarget::FileSystem(path));
    }
}
