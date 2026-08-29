//! Platform-neutral, privacy-safe property contract.
//!
//! Windows `IPropertyStore` is the first platform provider. The native worker
//! converts selected properties into these owned values; no PROPVARIANT, COM
//! interface, PIDL or borrowed string crosses into shared/UI state. Property
//! values may be personal and are therefore redacted from every `Debug` path
//! and retained only in a bounded process-memory cache.

use std::fmt;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::platform_namespace::LocationTarget;
use crate::revision_cache::RevisionCache;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformPropertyValueKind {
    Text,
    TextList,
    Boolean,
    Signed,
    Unsigned,
    Timestamp,
}

/// Useful scalar/list values only. Native blobs and arbitrary serialized
/// PROPVARIANT data are intentionally not accepted into the shared model.
#[derive(Clone, PartialEq)]
pub enum PlatformPropertyValue {
    Text(Arc<str>),
    TextList(Vec<Arc<str>>),
    Boolean(bool),
    Signed(i64),
    Unsigned(u64),
    TimestampUnixMillis(i64),
}

impl PlatformPropertyValue {
    pub const fn kind(&self) -> PlatformPropertyValueKind {
        match self {
            Self::Text(_) => PlatformPropertyValueKind::Text,
            Self::TextList(_) => PlatformPropertyValueKind::TextList,
            Self::Boolean(_) => PlatformPropertyValueKind::Boolean,
            Self::Signed(_) => PlatformPropertyValueKind::Signed,
            Self::Unsigned(_) => PlatformPropertyValueKind::Unsigned,
            Self::TimestampUnixMillis(_) => PlatformPropertyValueKind::Timestamp,
        }
    }
}

impl fmt::Debug for PlatformPropertyValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PlatformPropertyValue")
            .field(&self.kind())
            .finish()
    }
}

/// Canonical keys are selected by Ferail's provider implementation rather
/// than accepted from file content. Display names and values are untrusted,
/// potentially personal provider output and remain redacted from diagnostics.
#[derive(Clone, PartialEq)]
pub struct PlatformProperty {
    pub canonical_key: Arc<str>,
    pub display_name: Arc<str>,
    pub value: PlatformPropertyValue,
}

impl fmt::Debug for PlatformProperty {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformProperty")
            .field("canonical_key", &self.canonical_key)
            .field("display_name", &"<redacted>")
            .field("value_kind", &self.value.kind())
            .finish()
    }
}

#[derive(Clone, PartialEq)]
pub struct PlatformPropertySection {
    pub title: Arc<str>,
    pub properties: Vec<PlatformProperty>,
}

impl fmt::Debug for PlatformPropertySection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformPropertySection")
            .field("title", &"<redacted>")
            .field("property_count", &self.properties.len())
            .finish()
    }
}

#[derive(Clone, Default, PartialEq)]
pub struct PlatformProperties {
    pub sections: Vec<PlatformPropertySection>,
}

impl fmt::Debug for PlatformProperties {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let property_count: usize = self
            .sections
            .iter()
            .map(|section| section.properties.len())
            .sum();
        formatter
            .debug_struct("PlatformProperties")
            .field("section_count", &self.sections.len())
            .field("property_count", &property_count)
            .finish()
    }
}

/// The target is privacy-redacted through `LocationTarget::Debug`. The host
/// attaches its compact identity/revision separately when caching the result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformPropertiesRequest {
    pub target: LocationTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformPropertiesErrorKind {
    Unavailable,
    Unsupported,
    NotFound,
    PermissionDenied,
    Cancelled,
    Failed,
}

/// Implemented by the native layer and called only from a bounded worker.
pub trait PlatformPropertiesProvider: Send + Sync {
    fn read_properties(
        &self,
        request: PlatformPropertiesRequest,
        cancel: &AtomicBool,
    ) -> Result<PlatformProperties, PlatformPropertiesErrorKind>;
}

/// The key and revision types are chosen by the owning surface: filesystem
/// rows normally use `NodeId` + `FileRevision`; provider rows use their compact
/// tab-local id + listing-generation/revision token. Neither form needs a path.
pub type PlatformPropertiesCache<K, R> = RevisionCache<K, R, PlatformProperties>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::revision_cache::FileRevision;
    use crate::NodeId;
    use std::path::PathBuf;

    fn private_properties() -> PlatformProperties {
        PlatformProperties {
            sections: vec![PlatformPropertySection {
                title: Arc::from("Family details"),
                properties: vec![PlatformProperty {
                    canonical_key: Arc::from("System.Photo.CameraModel"),
                    display_name: Arc::from("Private camera"),
                    value: PlatformPropertyValue::Text(Arc::from("Alice's camera")),
                }],
            }],
        }
    }

    #[test]
    fn property_and_request_debug_redact_personal_values_and_paths() {
        let request = PlatformPropertiesRequest {
            target: LocationTarget::FileSystem(PathBuf::from(
                r"C:\Users\Alice\Family Photos\private.jpg",
            )),
        };
        let debug = format!("{request:?} {:?}", private_properties());
        for private in ["Users", "Alice", "Family", "private.jpg", "camera"] {
            assert!(!debug.contains(private));
        }
        assert!(debug.contains("property_count: 1"));
    }

    #[test]
    fn property_cache_is_memory_only_bounded_and_revision_aware() {
        let mut cache: PlatformPropertiesCache<NodeId, FileRevision> =
            PlatformPropertiesCache::new(1);
        let revision = FileRevision {
            byte_len: 10,
            modified_ns: Some(1),
        };
        cache.insert(1.into(), revision, private_properties());
        assert!(cache.get(1.into(), revision).is_some());
        assert!(cache
            .get(
                1.into(),
                FileRevision {
                    byte_len: 11,
                    modified_ns: Some(2),
                },
            )
            .is_none());
        assert!(cache.is_empty());
    }
}
