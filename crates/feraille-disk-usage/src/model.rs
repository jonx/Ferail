//! Disk-usage data model. The tree is a DAG by contract: nodes carry no
//! embedded children, containment is expressed as `container -> members`
//! edges in [`DiskUsageTree::containers`]. A single node may belong to
//! more than one container, which is what makes future scopes (search
//! results, duplicate groups, ad-hoc filters) layer in without breaking
//! the model.

use std::collections::HashMap;
use std::time::SystemTime;

use feraille_core::NodeId;

use crate::file_category::FileCategory;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Container,
    File,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScanState {
    Unknown,
    Scanning,
    Complete,
}

/// A node in the graph. Carries intrinsic size only — the layout
/// step aggregates container totals separately.
///
/// `size_bytes` is the **apparent** size (what `ls -l` shows).
/// `allocated_bytes` is the on-disk block-aligned size, populated
/// only by the macOS scanner via `MetadataExt::blocks() * 512`. Use
/// [`SizeMode`] when aggregating to pick which one to roll up.
#[derive(Debug, Clone)]
pub struct DiskUsageNode {
    pub id: NodeId,
    pub size_bytes: u64,
    pub allocated_bytes: u64,
    pub scan_state: ScanState,
    pub kind: NodeKind,
    pub file_category: FileCategory,
    pub mtime: Option<SystemTime>,
    pub display_name: String,
    /// True when the path resolves under a known cloud-storage root
    /// (`~/Library/Mobile Documents/` on macOS). Coarse path-prefix
    /// detection — doesn't tell you whether a given file is
    /// downloaded vs a placeholder. Surfaced as a cloud-glyph overlay
    /// in the DU window.
    pub is_cloud: bool,
}

impl DiskUsageNode {
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            size_bytes: 0,
            allocated_bytes: 0,
            scan_state: ScanState::Unknown,
            kind: NodeKind::Container,
            file_category: FileCategory::Other,
            mtime: None,
            display_name: String::new(),
            is_cloud: false,
        }
    }
}

/// Which size to aggregate: apparent (file logical bytes) or
/// allocated (block-aligned on-disk bytes). Apparent matches Finder's
/// "Size" column; allocated reveals fragmentation and 4 KB block tax
/// on tiny files.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum SizeMode {
    #[default]
    Apparent,
    Allocated,
}

#[derive(Debug, Clone)]
pub struct DiskUsageTree {
    pub nodes: HashMap<NodeId, DiskUsageNode>,
    pub containers: HashMap<NodeId, Vec<NodeId>>,
    pub root: NodeId,
    pub complete: bool,
}

impl DiskUsageTree {
    pub fn new(root: NodeId) -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(root, DiskUsageNode::new(root));
        Self {
            nodes,
            containers: HashMap::new(),
            root,
            complete: false,
        }
    }

    pub fn ensure_node(&mut self, id: NodeId) -> &mut DiskUsageNode {
        self.nodes.entry(id).or_insert_with(|| DiskUsageNode::new(id))
    }

    pub fn ensure_node_with_meta(
        &mut self,
        id: NodeId,
        kind: NodeKind,
        file_category: FileCategory,
        mtime: Option<SystemTime>,
        name: &str,
        is_cloud: bool,
    ) {
        let entry = self.nodes.entry(id).or_insert_with(|| DiskUsageNode::new(id));
        if is_cloud {
            entry.is_cloud = true;
        }
        entry.kind = kind;
        entry.file_category = if kind == NodeKind::File {
            file_category
        } else {
            FileCategory::Other
        };
        if entry.mtime.is_none() {
            entry.mtime = mtime;
        }
        if entry.display_name.is_empty() && !name.is_empty() {
            entry.display_name = name.to_owned();
        }
    }

    pub fn add_link(&mut self, container: NodeId, node: NodeId) {
        let members = self.containers.entry(container).or_default();
        if !members.contains(&node) {
            members.push(node);
        }
    }

    /// Fast path for fact streams that already guarantee link
    /// uniqueness. Avoids the O(n) sibling scan in `add_link`.
    pub fn add_link_unchecked(&mut self, container: NodeId, node: NodeId) {
        self.containers.entry(container).or_default().push(node);
    }

    pub fn add_size(&mut self, id: NodeId, size_bytes: u64) {
        let entry = self.nodes.entry(id).or_insert_with(|| DiskUsageNode::new(id));
        entry.size_bytes = entry.size_bytes.saturating_add(size_bytes);
    }

    pub fn add_allocated(&mut self, id: NodeId, bytes: u64) {
        let entry = self.nodes.entry(id).or_insert_with(|| DiskUsageNode::new(id));
        entry.allocated_bytes = entry.allocated_bytes.saturating_add(bytes);
    }

    pub fn set_scan_state(&mut self, id: NodeId, state: ScanState) {
        let entry = self.nodes.entry(id).or_insert_with(|| DiskUsageNode::new(id));
        entry.scan_state = state;
    }

    /// Drop a node and every container/member edge that mentions it.
    /// Used by Move-to-Trash live-update so we don't have to re-scan.
    pub fn remove_subtree(&mut self, id: NodeId) {
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            self.nodes.remove(&n);
            if let Some(members) = self.containers.remove(&n) {
                stack.extend(members);
            }
        }
        for members in self.containers.values_mut() {
            members.retain(|m| *m != id);
        }
    }
}

/// Aggregated layout node. Built from [`DiskUsageTree`] for one focus
/// root by [`crate::aggregate::build_layout_node`].
#[derive(Debug, Clone)]
pub struct DiskUsageLayoutNode {
    pub node_id: NodeId,
    pub size_bytes: u64,
    pub scan_state: ScanState,
    pub kind: NodeKind,
    pub file_category: FileCategory,
    pub mtime: Option<SystemTime>,
    pub children: Vec<DiskUsageLayoutNode>,
}

impl DiskUsageLayoutNode {
    pub fn new(
        node_id: NodeId,
        size_bytes: u64,
        scan_state: ScanState,
        kind: NodeKind,
        file_category: FileCategory,
        children: Vec<DiskUsageLayoutNode>,
    ) -> Self {
        Self::with_mtime(node_id, size_bytes, scan_state, kind, file_category, None, children)
    }

    pub fn with_mtime(
        node_id: NodeId,
        size_bytes: u64,
        scan_state: ScanState,
        kind: NodeKind,
        file_category: FileCategory,
        mtime: Option<SystemTime>,
        children: Vec<DiskUsageLayoutNode>,
    ) -> Self {
        Self {
            node_id,
            size_bytes,
            scan_state,
            kind,
            file_category,
            mtime,
            children,
        }
    }
}

impl DiskUsageLayoutNode {
    pub fn sort_children_by_size(&mut self) {
        self.children
            .sort_by_key(|c| std::cmp::Reverse(c.size_bytes));
        for child in &mut self.children {
            child.sort_children_by_size();
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct DiskUsageStats {
    pub files_scanned: u64,
    pub dirs_scanned: u64,
    pub bytes_scanned: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(raw: u64) -> NodeId {
        NodeId::from_raw(raw).expect("nonzero")
    }

    #[test]
    fn tree_init_seeds_root() {
        let root = nid(42);
        let tree = DiskUsageTree::new(root);
        assert!(tree.nodes.contains_key(&root));
        assert_eq!(tree.root, root);
        assert!(!tree.complete);
    }

    #[test]
    fn add_link_dedupes() {
        let mut t = DiskUsageTree::new(nid(1));
        t.add_link(nid(1), nid(2));
        t.add_link(nid(1), nid(2));
        assert_eq!(t.containers.get(&nid(1)).unwrap().len(), 1);
    }

    #[test]
    fn add_size_saturates() {
        let mut t = DiskUsageTree::new(nid(1));
        t.add_size(nid(2), u64::MAX - 1);
        t.add_size(nid(2), 100);
        assert_eq!(t.nodes.get(&nid(2)).unwrap().size_bytes, u64::MAX);
    }

    #[test]
    fn remove_subtree_drops_descendants_and_back_edges() {
        let mut t = DiskUsageTree::new(nid(1));
        t.ensure_node(nid(2));
        t.ensure_node(nid(3));
        t.ensure_node(nid(4));
        t.add_link(nid(1), nid(2));
        t.add_link(nid(2), nid(3));
        t.add_link(nid(2), nid(4));
        t.remove_subtree(nid(2));
        assert!(!t.nodes.contains_key(&nid(2)));
        assert!(!t.nodes.contains_key(&nid(3)));
        assert!(!t.nodes.contains_key(&nid(4)));
        assert!(!t.containers.get(&nid(1)).unwrap().contains(&nid(2)));
    }

    #[test]
    fn ensure_node_with_meta_preserves_first_name_and_mtime() {
        let mut t = DiskUsageTree::new(nid(1));
        let when = SystemTime::UNIX_EPOCH;
        t.ensure_node_with_meta(
            nid(2),
            NodeKind::File,
            FileCategory::Image,
            Some(when),
            "a.png",
            false,
        );
        // Second call should not overwrite the cached display name.
        t.ensure_node_with_meta(
            nid(2),
            NodeKind::File,
            FileCategory::Image,
            None,
            "",
            false,
        );
        let n = t.nodes.get(&nid(2)).unwrap();
        assert_eq!(n.display_name, "a.png");
        assert_eq!(n.mtime, Some(when));
        assert_eq!(n.kind, NodeKind::File);
        assert_eq!(n.file_category, FileCategory::Image);
    }

    #[test]
    fn layout_sort_children_by_size_descending() {
        let mut node = DiskUsageLayoutNode::new(
            nid(1),
            0,
            ScanState::Complete,
            NodeKind::Container,
            FileCategory::Other,
            vec![
                DiskUsageLayoutNode::new(nid(2), 10, ScanState::Complete, NodeKind::File, FileCategory::Other, vec![]),
                DiskUsageLayoutNode::new(nid(3), 30, ScanState::Complete, NodeKind::File, FileCategory::Other, vec![]),
                DiskUsageLayoutNode::new(nid(4), 20, ScanState::Complete, NodeKind::File, FileCategory::Other, vec![]),
            ],
        );
        node.sort_children_by_size();
        assert_eq!(node.children[0].size_bytes, 30);
        assert_eq!(node.children[1].size_bytes, 20);
        assert_eq!(node.children[2].size_bytes, 10);
    }
}
