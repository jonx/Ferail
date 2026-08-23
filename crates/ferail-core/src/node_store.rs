//! Shared node identity and metadata store.
//!
//! This is intentionally platform-neutral so both the GPUI macOS shell and the
//! Windows shell can converge on the same model: UI state stores `NodeId`; path
//! resolution happens only at action/job boundaries.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::{path_guard, NodeId};

/// Normalize a path for use as an identity-map key. **Lexical only —
/// never touches the filesystem**, so it's safe at every call depth
/// including under the path guard.
///
/// What it folds together (mechanical aliases of the same spelling):
///   - trailing separators           `/Users/x/`  → `/Users/x`
///   - `.` segments                  `/Users/./x` → `/Users/x`
///   - redundant separators          `/Users//x`  → `/Users/x`
///
/// What it deliberately does NOT fold, and why:
///   - **case** — APFS and NTFS are *usually* case-insensitive but
///     it's a per-volume property; case-folding keys would wrongly
///     merge distinct files on case-sensitive volumes. Identity
///     stays case-preserving; case-insensitive *aliasing* is a
///     boundary concern (canonicalize at input edges).
///   - **`..` segments** — `a/../b` is not lexically `b` when `a`
///     is a symlink; collapsing requires filesystem knowledge this
///     function must not have. Typed input containing `..` is
///     canonicalized at the boundary (breadcrumb parse, CLI args).
///   - **symlinks** — same reason; boundary canonicalization owns it.
pub fn normalize_path_key(path: &Path) -> PathBuf {
    let mut out = PathBuf::with_capacity(path.as_os_str().len());
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum NodeKind {
    Path(PathBuf),
    Virtual(VirtualNode),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum VirtualNode {
    Root,
    Home,
    Applications,
    Desktop,
    Documents,
    Downloads,
    Movies,
    Music,
    Pictures,
    Volumes,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub kind: NodeKind,
    pub display_name: String,
    pub heat: f32,
}

#[derive(Debug)]
pub struct NodeStore {
    nodes: HashMap<NodeId, Node>,
    path_index: HashMap<PathBuf, NodeId>,
    next_dynamic: u64,
}

impl Default for NodeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeStore {
    pub fn new() -> Self {
        let mut store = Self {
            nodes: HashMap::new(),
            path_index: HashMap::new(),
            next_dynamic: 1_000_000,
        };
        store.register_virtual(
            NodeId::from_raw(1).unwrap(),
            None,
            VirtualNode::Root,
            "Computer",
        );
        store.register_virtual(
            NodeId::from_raw(2).unwrap(),
            Some(NodeId::from_raw(1).unwrap()),
            VirtualNode::Volumes,
            "Volumes",
        );
        store
    }

    pub fn get_or_create_path(&mut self, path: impl Into<PathBuf>) -> NodeId {
        // Identity contract: keys are lexically normalized so the
        // mechanical spellings of one path (trailing slash, ./, //)
        // can't mint two NodeIds. See `normalize_path_key`.
        let path = normalize_path_key(&path.into());
        if let Some(id) = self.path_index.get(&path) {
            return *id;
        }
        let id = self.allocate_id();
        self.insert_path(id, path);
        id
    }

    /// Register `path` with a platform/backend-provided id. This lets a
    /// platform filesystem adapter and the shared NodeStore agree on identity
    /// during an incremental migration.
    pub fn get_or_create_path_with_id(&mut self, path: impl Into<PathBuf>, id: NodeId) -> NodeId {
        let path = normalize_path_key(&path.into());
        if let Some(existing) = self.path_index.get(&path) {
            return *existing;
        }
        if let Some(existing) = self.nodes.get(&id) {
            if matches!(existing.kind, NodeKind::Path(_)) {
                return id;
            }
        }
        self.insert_path(id, path);
        self.next_dynamic = self.next_dynamic.max(id.as_raw().saturating_add(1));
        id
    }

    pub fn display_name(&self, id: NodeId) -> Option<&str> {
        self.nodes.get(&id).map(|node| node.display_name.as_str())
    }

    pub fn set_heat(&mut self, id: NodeId, heat: f32) {
        if let Some(node) = self.nodes.get_mut(&id) {
            node.heat = heat.clamp(0.0, 1.0);
        }
    }

    pub fn heat(&self, id: NodeId) -> f32 {
        self.nodes.get(&id).map(|node| node.heat).unwrap_or(0.0)
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(&id).and_then(|node| node.parent)
    }

    /// Controlled path resolution for filesystem jobs and command handlers.
    pub fn path_for_action(&self, id: NodeId, operation: &'static str) -> Option<&Path> {
        path_guard::assert_path_resolution_allowed(operation);
        match &self.nodes.get(&id)?.kind {
            NodeKind::Path(path) => Some(path.as_path()),
            NodeKind::Virtual(_) => None,
        }
    }

    /// Snapshot a path for handoff to a background job.
    pub fn path_snapshot_for_job(&self, id: NodeId, operation: &'static str) -> Option<PathBuf> {
        self.path_for_action(id, operation).map(Path::to_path_buf)
    }

    pub fn ancestors(&mut self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut cur = Some(id);
        while let Some(node_id) = cur {
            out.push(node_id);
            cur = self.parent(node_id);
        }
        out.reverse();
        out
    }

    fn register_virtual(
        &mut self,
        id: NodeId,
        parent: Option<NodeId>,
        kind: VirtualNode,
        display_name: &'static str,
    ) {
        self.nodes.insert(
            id,
            Node {
                id,
                parent,
                kind: NodeKind::Virtual(kind),
                display_name: display_name.to_string(),
                heat: 0.0,
            },
        );
    }

    fn insert_path(&mut self, id: NodeId, path: PathBuf) {
        let parent = path.parent().and_then(|p| self.path_index.get(p).copied());
        let display_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        self.path_index.insert(path.clone(), id);
        self.nodes.insert(
            id,
            Node {
                id,
                parent,
                kind: NodeKind::Path(path),
                display_name,
                heat: 0.0,
            },
        );
    }

    fn allocate_id(&mut self) -> NodeId {
        loop {
            let raw = self.next_dynamic.max(1);
            self.next_dynamic = raw.saturating_add(1);
            let id = NodeId::from_raw(raw).expect("dynamic node ids are nonzero");
            if !self.nodes.contains_key(&id) {
                return id;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_resolution_is_guarded() {
        let mut store = NodeStore::new();
        let id = store.get_or_create_path("/tmp");
        assert_eq!(
            store.path_snapshot_for_job(id, "test"),
            Some(PathBuf::from("/tmp"))
        );
    }

    // ---- identity contract (normalize_path_key) ----

    #[test]
    fn mechanical_spellings_share_one_id() {
        let mut store = NodeStore::new();
        let canonical = store.get_or_create_path("/Users/x/docs");
        assert_eq!(store.get_or_create_path("/Users/x/docs/"), canonical);
        assert_eq!(store.get_or_create_path("/Users/x/./docs"), canonical);
        assert_eq!(store.get_or_create_path("/Users/x//docs"), canonical);
    }

    #[test]
    fn case_variants_stay_distinct_by_design() {
        // Case-folding is a per-volume property; identity must not
        // merge spellings that are distinct files on case-sensitive
        // volumes. Case-insensitive aliasing is handled by boundary
        // canonicalization, not the key.
        let mut store = NodeStore::new();
        let lower = store.get_or_create_path("/users/x/docs");
        let upper = store.get_or_create_path("/Users/x/docs");
        assert_ne!(lower, upper);
    }

    #[test]
    fn parent_dir_segments_not_collapsed() {
        // `a/../b` ≠ lexical `b` when `a` is a symlink — collapsing
        // needs filesystem knowledge this layer must not have.
        let mut store = NodeStore::new();
        let direct = store.get_or_create_path("/Users/b");
        let dotted = store.get_or_create_path("/Users/a/../b");
        assert_ne!(direct, dotted);
    }

    #[test]
    fn normalize_is_prefix_stable() {
        // parent() of a normalized path is itself normalized — the
        // parent-index lookup in insert_path relies on this.
        let n = normalize_path_key(Path::new("/Users/x/./docs//"));
        assert_eq!(n, PathBuf::from("/Users/x/docs"));
        assert_eq!(n.parent(), Some(Path::new("/Users/x")));
        assert_eq!(
            normalize_path_key(n.parent().unwrap()),
            PathBuf::from("/Users/x")
        );
    }
}
