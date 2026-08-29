//! Platform-neutral contract for operating-system shortcut files.
//!
//! Windows `.lnk` files are the first consumer. Resolution is explicitly a
//! background operation and is kept separate from mutation: rename, copy,
//! move and trash always target the shortcut file itself. Only Open may use
//! the resolved target, and even then Ferail navigates directly only to a real
//! filesystem directory; every other shortcut is invoked by the platform so
//! arguments, working directory and provider semantics are preserved.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::platform_namespace::PlatformLocation;
pub use crate::revision_cache::FileRevision;
use crate::revision_cache::RevisionCache;
use crate::NodeId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutTargetKind {
    File,
    Directory,
    Application,
    Other,
}

/// Resolved target. Personal paths, URLs and provider identities are usable by
/// the host but deliberately redacted from `Debug` output.
#[derive(Clone, Eq, PartialEq)]
pub enum ShortcutTarget {
    FileSystem {
        path: PathBuf,
        kind: ShortcutTargetKind,
    },
    Platform(PlatformLocation),
    Url(Arc<str>),
}

impl fmt::Debug for ShortcutTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileSystem { kind, .. } => formatter
                .debug_struct("FileSystem")
                .field("path", &"<redacted>")
                .field("kind", kind)
                .finish(),
            Self::Platform(location) => formatter.debug_tuple("Platform").field(location).finish(),
            Self::Url(_) => formatter.write_str("Url(<redacted>)"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutFailureKind {
    Broken,
    TargetMissing,
    PermissionDenied,
    Unsupported,
    Cancelled,
    Failed,
}

/// Cached resolution result. It is process-memory-only: arguments, working
/// directory and icon location may contain personal information and must not
/// be persisted or emitted in default diagnostics.
#[derive(Clone, Eq, PartialEq)]
pub struct ShortcutInfo {
    pub target: Result<ShortcutTarget, ShortcutFailureKind>,
    pub arguments: Vec<OsString>,
    pub working_directory: Option<PathBuf>,
    pub icon_location: Option<(PathBuf, i32)>,
}

impl fmt::Debug for ShortcutInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShortcutInfo")
            .field("target", &self.target)
            .field("argument_count", &self.arguments.len())
            .field(
                "working_directory",
                &self.working_directory.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "icon_location",
                &self
                    .icon_location
                    .as_ref()
                    .map(|(_, index)| ("<redacted>", index)),
            )
            .finish()
    }
}

/// Privacy-safe resolver request. The source path is never printed by Debug.
#[derive(Clone, Eq, PartialEq)]
pub struct ShortcutResolveRequest {
    pub source: PathBuf,
    pub revision: FileRevision,
}

impl fmt::Debug for ShortcutResolveRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShortcutResolveRequest")
            .field("source", &"<redacted>")
            .field("revision", &self.revision)
            .finish()
    }
}

/// Implemented by the platform layer and called only from a worker.
pub trait ShortcutResolver: Send + Sync {
    fn resolve(
        &self,
        request: ShortcutResolveRequest,
        cancel: &AtomicBool,
    ) -> Result<ShortcutInfo, ShortcutFailureKind>;
}

#[derive(Clone, Eq, PartialEq)]
pub enum ShortcutOpenDisposition {
    /// A resolved real directory stays inside Ferail's NativeFs fast path.
    Navigate(PathBuf),
    /// Invoke the `.lnk` itself through the platform Shell. This preserves
    /// command-line arguments, working directory and non-filesystem targets.
    InvokeShortcut,
    Unavailable(ShortcutFailureKind),
}

impl fmt::Debug for ShortcutOpenDisposition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Navigate(_) => formatter.write_str("Navigate(<redacted>)"),
            Self::InvokeShortcut => formatter.write_str("InvokeShortcut"),
            Self::Unavailable(error) => formatter.debug_tuple("Unavailable").field(error).finish(),
        }
    }
}

impl ShortcutInfo {
    pub fn open_disposition(&self) -> ShortcutOpenDisposition {
        match &self.target {
            Ok(ShortcutTarget::FileSystem {
                path,
                kind: ShortcutTargetKind::Directory,
            }) => ShortcutOpenDisposition::Navigate(path.clone()),
            Ok(_) => ShortcutOpenDisposition::InvokeShortcut,
            Err(error) => ShortcutOpenDisposition::Unavailable(*error),
        }
    }
}

/// Bounded process-memory shortcut cache with compact, path-free keys.
pub type ShortcutCache = RevisionCache<NodeId, FileRevision, ShortcutInfo>;

#[cfg(test)]
mod tests {
    use super::*;

    fn revision(n: i128) -> FileRevision {
        FileRevision {
            byte_len: 128,
            modified_ns: Some(n),
        }
    }

    fn resolved(kind: ShortcutTargetKind) -> ShortcutInfo {
        ShortcutInfo {
            target: Ok(ShortcutTarget::FileSystem {
                path: PathBuf::from(r"C:\Users\Private\target"),
                kind,
            }),
            arguments: vec![OsString::from("--personal-value")],
            working_directory: Some(PathBuf::from(r"C:\Users\Private")),
            icon_location: Some((PathBuf::from(r"C:\Private\icon.dll"), 2)),
        }
    }

    #[test]
    fn only_real_directories_navigate_inside_ferail() {
        assert!(matches!(
            resolved(ShortcutTargetKind::Directory).open_disposition(),
            ShortcutOpenDisposition::Navigate(_)
        ));
        for kind in [
            ShortcutTargetKind::File,
            ShortcutTargetKind::Application,
            ShortcutTargetKind::Other,
        ] {
            assert_eq!(
                resolved(kind).open_disposition(),
                ShortcutOpenDisposition::InvokeShortcut
            );
        }
    }

    #[test]
    fn broken_shortcuts_never_fabricate_a_target() {
        let info = ShortcutInfo {
            target: Err(ShortcutFailureKind::TargetMissing),
            arguments: Vec::new(),
            working_directory: None,
            icon_location: None,
        };
        assert_eq!(
            info.open_disposition(),
            ShortcutOpenDisposition::Unavailable(ShortcutFailureKind::TargetMissing)
        );
    }

    #[test]
    fn debug_redacts_every_personal_shortcut_field() {
        let request = ShortcutResolveRequest {
            source: PathBuf::from(r"C:\Users\Private\family.lnk"),
            revision: revision(1),
        };
        let debug = format!(
            "{request:?} {:?}",
            resolved(ShortcutTargetKind::Application)
        );
        for private in ["Users", "Private", "family", "personal-value", "icon.dll"] {
            assert!(!debug.contains(private));
        }
    }

    #[test]
    fn cache_is_bounded_and_revision_aware_without_path_keys() {
        let mut cache = ShortcutCache::new(2);
        cache.insert(1.into(), revision(1), resolved(ShortcutTargetKind::File));
        cache.insert(2.into(), revision(1), resolved(ShortcutTargetKind::File));
        cache.insert(3.into(), revision(1), resolved(ShortcutTargetKind::File));
        assert_eq!(cache.len(), 2);
        assert!(cache.get(1.into(), revision(1)).is_none());
        assert!(cache.get(2.into(), revision(2)).is_none());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn zero_capacity_never_retains_private_resolution_data() {
        let mut cache = ShortcutCache::new(0);
        cache.insert(1.into(), revision(1), resolved(ShortcutTargetKind::File));
        assert!(cache.is_empty());
    }
}
