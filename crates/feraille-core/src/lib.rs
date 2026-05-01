//! Domain types shared between FS, controls, and app layers.
//! This crate has zero platform deps and zero UI deps. That is enforced by
//! convention, not the compiler — if you find yourself reaching for `windows`
//! or `winit` here, stop.

use std::num::NonZeroU64;

/// Stable identifier for a tree/list node. Opaque to the UI; the FS layer
/// owns the mapping `NodeId <-> path/PIDL`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NodeId(NonZeroU64);

impl NodeId {
    pub fn from_raw(raw: u64) -> Option<Self> {
        NonZeroU64::new(raw).map(Self)
    }
    pub fn as_raw(self) -> u64 {
        self.0.get()
    }
}

impl From<u64> for NodeId {
    fn from(v: u64) -> Self {
        Self(NonZeroU64::new(v.max(1)).expect("post-max nonzero"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
}

/// One row in the file pane. Display strings are pre-formatted; paint never
/// formats numbers or dates.
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub id: NodeId,
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_unix: i64,
    pub display_size: String,
    pub display_mtime: String,
}

/// Filesystem trait — implemented by `feraille-fs-native` (cross-platform std::fs)
/// and `feraille-shell-win32` (Windows shell namespace, PIDLs, virtual roots).
/// The UI talks to *this*, never to platform APIs directly.
pub trait FsBackend: Send + Sync {
    /// Begin an enumeration of `node`. The returned handle can be polled for
    /// streamed batches; the UI never blocks.
    fn enumerate(&self, node: NodeId) -> EnumerationHandle;
}

/// Opaque handle to a streamed enumeration. Real impl pushes batches over a
/// channel; the slice's stub returns one synchronous batch.
pub struct EnumerationHandle {
    pub initial: Vec<FileEntry>,
}
