//! Domain types shared between FS, controls, and app layers.
//! This crate has zero platform deps and zero UI deps. That is enforced by
//! convention, not the compiler — if you find yourself reaching for `windows`
//! or `winit` here, stop.

pub mod commands;
pub mod navigation;
pub mod node_store;
pub mod path_guard;

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
    /// Friendly type label — "Folder", "Symlink", uppercased extension
    /// (e.g. "RS", "MD"), or "File" when there's no extension. macOS shell
    /// crate (iter-4) replaces this with `NSWorkspace.localizedDescription`.
    pub display_kind: String,
    /// Magic-byte detected type, e.g. "PNG image", "Mach-O 64-bit", "Plain text".
    /// Empty string when not yet detected or no match. Populated lazily by
    /// the host (App) — `feraille-core` never blocks on file I/O.
    pub display_magic: String,
    /// Hot-path flag for the icon-overlay dot. True when the file carries
    /// `com.apple.quarantine` (macOS Mark-of-the-Web equivalent). Populated
    /// lazily by the host alongside `quarantine`; defaults to false.
    pub is_quarantined: bool,
    /// Detail-panel rows for downloaded files. `None` until the prefetch
    /// worker reports back; `Some` with empty fields means "we looked,
    /// nothing to show beyond the flag."
    pub quarantine: Option<QuarantineDetails>,
}

/// Display-ready provenance fields for a quarantined file. Strings are
/// pre-formatted in the worker so paint never allocates or parses.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QuarantineDetails {
    /// Quarantining agent name from the `com.apple.quarantine` string —
    /// e.g. "Safari", "com.google.Chrome". `None` when the field was empty.
    pub agent: Option<String>,
    /// ISO-8601 download timestamp from the quarantine record. `None` when
    /// missing or unparseable.
    pub downloaded_iso: Option<String>,
    /// Source URLs from `kMDItemWhereFroms`. May be empty.
    pub where_from: Vec<String>,
}

/// Filesystem trait — implemented by `feraille-fs-native` (cross-platform std::fs)
/// and `feraille-shell-win32` (Windows shell namespace, PIDLs, virtual roots).
/// The UI talks to *this*, never to platform APIs directly.
pub trait FsBackend: Send + Sync {
    /// Begin an enumeration of `node`. The returned handle can be polled for
    /// streamed batches; the UI never blocks.
    fn enumerate(&self, node: NodeId) -> EnumerationHandle;
}

/// Why an enumeration failed to produce a complete listing. UI surfaces
/// this as an empty-state when `initial` is empty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnumerationError {
    /// macOS TCC / Unix EACCES — the user can grant access via System
    /// Settings → Privacy & Security → Files and Folders (macOS) or by
    /// running with appropriate permissions (Linux).
    PermissionDenied,
    /// Path doesn't exist or has been moved/deleted.
    NotFound,
    /// Other I/O error. The string is a human-readable hint, not a
    /// programmable code.
    Other(String),
}

/// Opaque handle to a streamed enumeration. Real impl pushes batches over a
/// channel; the slice's stub returns one synchronous batch. `error` is
/// `Some` only on hard failure — partial listings are not currently
/// represented (would land alongside async enumeration).
pub struct EnumerationHandle {
    pub initial: Vec<FileEntry>,
    pub error: Option<EnumerationError>,
}

/// Folder usage heat — how many times the user has navigated into a node.
/// Iter-3 keeps this in-memory; iter-6 persists it to SQLite (matching
/// Ferail's predecessor implementation). Heat is reported normalized to
/// the most-visited node so a single very-busy folder doesn't washed out
/// the rest.
#[derive(Default, Clone, Debug)]
pub struct AntTrail {
    visits: std::collections::HashMap<NodeId, u32>,
    max: u32,
}

impl AntTrail {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, id: NodeId) {
        let v = self.visits.entry(id).or_insert(0);
        *v += 1;
        if *v > self.max {
            self.max = *v;
        }
    }

    /// 0.0..=1.0 normalized heat. Log-scaled so a 10-visit folder isn't
    /// 10× brighter than a 5-visit one. Returns 0.0 for never-visited.
    pub fn heat(&self, id: NodeId) -> f32 {
        let Some(&v) = self.visits.get(&id) else { return 0.0 };
        if self.max <= 1 {
            return 1.0;
        }
        ((v as f32 + 1.0).log2() / (self.max as f32 + 1.0).log2()).clamp(0.0, 1.0)
    }

    /// Up to `n` most-visited NodeIds, descending by visit count.
    /// Ties broken by NodeId order. Used by the tree to populate the
    /// "Recents" section.
    pub fn most_visited(&self, n: usize) -> Vec<NodeId> {
        let mut v: Vec<(NodeId, u32)> = self.visits.iter().map(|(k, c)| (*k, *c)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v.into_iter().take(n).map(|(id, _)| id).collect()
    }
}
