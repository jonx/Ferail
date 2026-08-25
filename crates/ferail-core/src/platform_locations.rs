//! Platform-provided roots which become ordinary filesystem paths once ready.
//!
//! WSL distributions are the first consumer: Windows discovers a small list
//! of Linux roots, but browsing a ready distribution still belongs to the
//! normal filesystem backend.  The types here deliberately contain no COM,
//! registry or process state and are allocated once per root, never per file.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Opaque provider-owned identity for one dynamic root.
///
/// The UI may compare and clone this value, but must not display or log its
/// contents. On Windows the WSL provider uses a distribution registry id, not
/// its VHD `BasePath` or the user's Linux path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PlatformRootId(Arc<str>);

impl PlatformRootId {
    pub fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }

    /// Provider boundary only. Callers must not include this value in normal
    /// diagnostics: even an opaque platform key can be user-specific.
    pub fn as_provider_key(&self) -> &str {
        &self.0
    }
}

/// Privacy-safe failure categories shared by every platform implementation.
/// Raw command output, registry values and paths never cross this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformRootErrorKind {
    Unavailable,
    NotFound,
    TimedOut,
    Cancelled,
    Failed,
}

/// Live state of one path-backed platform root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathBackedRootState {
    /// Installed/discovered, but its backing service is not ready.
    Stopped,
    /// Explicit activation is in flight.
    Starting,
    /// Ready to hand to the normal filesystem backend.
    Ready(PathBuf),
    /// Discovery or activation failed without exposing provider details.
    Unavailable(PlatformRootErrorKind),
}

/// One cached dynamic root shown by the shared UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathBackedPlatformRoot {
    pub id: PlatformRootId,
    pub label: Arc<str>,
    pub state: PathBackedRootState,
    /// Provider version when useful (WSL 1 or 2); not rendered per file.
    pub version: Option<u32>,
    pub is_default: bool,
}

impl PathBackedPlatformRoot {
    pub fn stopped(id: PlatformRootId, label: impl Into<Arc<str>>) -> Self {
        Self {
            id,
            label: label.into(),
            state: PathBackedRootState::Stopped,
            version: None,
            is_default: false,
        }
    }
}

/// Generation token for a discovery pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlatformDiscovery(u64);

/// Generation token for one explicit root activation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformActivation {
    root: PlatformRootId,
    discovery: u64,
    serial: u64,
}

impl PlatformActivation {
    pub fn root(&self) -> &PlatformRootId {
        &self.root
    }
}

/// Small process-level state machine for dynamic roots.
///
/// It is intentionally independent from GPUI so stale-result and transition
/// behavior can be proven on every development host. A new discovery
/// generation invalidates every older activation completion.
#[derive(Debug, Default)]
pub struct PlatformRootsStore {
    discovery: u64,
    activation_serial: u64,
    activations: HashMap<PlatformRootId, u64>,
    roots: Vec<PathBackedPlatformRoot>,
}

impl PlatformRootsStore {
    pub fn roots(&self) -> &[PathBackedPlatformRoot] {
        &self.roots
    }

    pub fn begin_discovery(&mut self) -> PlatformDiscovery {
        self.discovery = self.discovery.wrapping_add(1);
        PlatformDiscovery(self.discovery)
    }

    /// Replace the cached snapshot only when it still belongs to the newest
    /// request. Provider order is normalized for deterministic UI/tests.
    pub fn apply_discovery(
        &mut self,
        token: PlatformDiscovery,
        mut roots: Vec<PathBackedPlatformRoot>,
    ) -> bool {
        if token.0 != self.discovery {
            return false;
        }
        roots.sort_by(|a, b| {
            b.is_default
                .cmp(&a.is_default)
                .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
                .then_with(|| a.id.cmp(&b.id))
        });
        self.activations.clear();
        self.roots = roots;
        true
    }

    /// Mark one root as starting. Repeated clicks while the same activation
    /// is in flight coalesce into a no-op.
    pub fn begin_activation(&mut self, id: &PlatformRootId) -> Option<PlatformActivation> {
        let root = self.roots.iter_mut().find(|root| &root.id == id)?;
        match root.state {
            PathBackedRootState::Starting | PathBackedRootState::Ready(_) => return None,
            PathBackedRootState::Stopped | PathBackedRootState::Unavailable(_) => {}
        }
        root.state = PathBackedRootState::Starting;
        self.activation_serial = self.activation_serial.wrapping_add(1);
        self.activations.insert(id.clone(), self.activation_serial);
        Some(PlatformActivation {
            root: id.clone(),
            discovery: self.discovery,
            serial: self.activation_serial,
        })
    }

    /// Apply a provider result if discovery and activation identity are still
    /// current. Returns the ready path only for a successfully applied result.
    pub fn finish_activation(
        &mut self,
        token: &PlatformActivation,
        result: Result<PathBuf, PlatformRootErrorKind>,
    ) -> Option<PathBuf> {
        if token.discovery != self.discovery
            || self.activations.get(&token.root) != Some(&token.serial)
        {
            return None;
        }
        let root = self.roots.iter_mut().find(|root| root.id == token.root)?;
        if !matches!(root.state, PathBackedRootState::Starting) {
            return None;
        }
        let ready = match result {
            Ok(path) => {
                root.state = PathBackedRootState::Ready(path.clone());
                Some(path)
            }
            Err(kind) => {
                root.state = if kind == PlatformRootErrorKind::Cancelled {
                    PathBackedRootState::Stopped
                } else {
                    PathBackedRootState::Unavailable(kind)
                };
                None
            }
        };
        self.activations.remove(&token.root);
        ready
    }

    pub fn ready_path(&self, id: &PlatformRootId) -> Option<&PathBuf> {
        let root = self.roots.iter().find(|root| &root.id == id)?;
        match &root.state {
            PathBackedRootState::Ready(path) => Some(path),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(id: &str, label: &str, default: bool) -> PathBackedPlatformRoot {
        PathBackedPlatformRoot {
            id: PlatformRootId::new(id),
            label: Arc::from(label),
            state: PathBackedRootState::Stopped,
            version: Some(2),
            is_default: default,
        }
    }

    #[test]
    fn discovery_is_sorted_default_then_label() {
        let mut store = PlatformRootsStore::default();
        let token = store.begin_discovery();
        assert!(store.apply_discovery(
            token,
            vec![
                root("b", "Zulu", false),
                root("a", "ubuntu", true),
                root("c", "Alpine", false),
            ],
        ));
        let labels: Vec<&str> = store.roots().iter().map(|r| r.label.as_ref()).collect();
        assert_eq!(labels, ["ubuntu", "Alpine", "Zulu"]);
    }

    #[test]
    fn stale_discovery_cannot_replace_new_snapshot() {
        let mut store = PlatformRootsStore::default();
        let old = store.begin_discovery();
        let current = store.begin_discovery();
        assert!(store.apply_discovery(current, vec![root("new", "New", false)]));
        assert!(!store.apply_discovery(old, vec![root("old", "Old", false)]));
        assert_eq!(store.roots()[0].label.as_ref(), "New");
    }

    #[test]
    fn activation_coalesces_and_yields_ready_path() {
        let mut store = PlatformRootsStore::default();
        let discovery = store.begin_discovery();
        let id = PlatformRootId::new("id");
        store.apply_discovery(discovery, vec![root("id", "Ubuntu", false)]);

        let activation = store.begin_activation(&id).expect("first activation");
        assert_eq!(store.begin_activation(&id), None);
        assert!(matches!(
            store.roots()[0].state,
            PathBackedRootState::Starting
        ));

        let path = PathBuf::from(r"\\wsl.localhost\Ubuntu");
        assert_eq!(
            store.finish_activation(&activation, Ok(path.clone())),
            Some(path.clone())
        );
        assert_eq!(store.ready_path(&id), Some(&path));
    }

    #[test]
    fn refresh_invalidates_late_activation() {
        let mut store = PlatformRootsStore::default();
        let discovery = store.begin_discovery();
        let id = PlatformRootId::new("id");
        store.apply_discovery(discovery, vec![root("id", "Ubuntu", false)]);
        let activation = store.begin_activation(&id).unwrap();

        let refresh = store.begin_discovery();
        store.apply_discovery(refresh, vec![root("id", "Ubuntu", false)]);
        assert_eq!(
            store.finish_activation(&activation, Ok(PathBuf::from("late"))),
            None
        );
        assert_eq!(store.ready_path(&id), None);
    }

    #[test]
    fn cancellation_returns_root_to_stopped() {
        let mut store = PlatformRootsStore::default();
        let discovery = store.begin_discovery();
        let id = PlatformRootId::new("id");
        store.apply_discovery(discovery, vec![root("id", "Ubuntu", false)]);
        let activation = store.begin_activation(&id).unwrap();
        assert_eq!(
            store.finish_activation(&activation, Err(PlatformRootErrorKind::Cancelled)),
            None
        );
        assert!(matches!(
            store.roots()[0].state,
            PathBackedRootState::Stopped
        ));
    }

    #[test]
    fn different_roots_can_activate_concurrently() {
        let mut store = PlatformRootsStore::default();
        let discovery = store.begin_discovery();
        let first = PlatformRootId::new("first");
        let second = PlatformRootId::new("second");
        store.apply_discovery(
            discovery,
            vec![
                root("first", "Ubuntu", false),
                root("second", "Debian", false),
            ],
        );
        let first_token = store.begin_activation(&first).unwrap();
        let second_token = store.begin_activation(&second).unwrap();
        assert_eq!(
            store.finish_activation(&first_token, Ok(PathBuf::from("first-ready"))),
            Some(PathBuf::from("first-ready"))
        );
        assert_eq!(
            store.finish_activation(&second_token, Ok(PathBuf::from("second-ready"))),
            Some(PathBuf::from("second-ready"))
        );
    }

    #[test]
    fn empty_refresh_removes_disappeared_roots() {
        let mut store = PlatformRootsStore::default();
        let first = store.begin_discovery();
        store.apply_discovery(first, vec![root("old", "Ubuntu", false)]);
        let refresh = store.begin_discovery();
        assert!(store.apply_discovery(refresh, Vec::new()));
        assert!(store.roots().is_empty());
    }

    #[test]
    fn failed_activation_is_retryable_without_raw_error_text() {
        let mut store = PlatformRootsStore::default();
        let discovery = store.begin_discovery();
        let id = PlatformRootId::new("id");
        store.apply_discovery(discovery, vec![root("id", "Ubuntu", false)]);
        let activation = store.begin_activation(&id).unwrap();
        assert_eq!(
            store.finish_activation(&activation, Err(PlatformRootErrorKind::TimedOut)),
            None
        );
        assert!(matches!(
            store.roots()[0].state,
            PathBackedRootState::Unavailable(PlatformRootErrorKind::TimedOut)
        ));
        assert!(store.begin_activation(&id).is_some());
    }
}
