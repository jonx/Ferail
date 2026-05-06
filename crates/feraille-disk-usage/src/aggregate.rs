//! Build a [`DiskUsageLayoutNode`] tree from a [`DiskUsageTree`] DAG by
//! walking from a focus root, summing intrinsic sizes upward, and
//! sorting each level by descending size. Cycle-safe via a per-walk
//! visited set, so future scopes that graft additional containers don't
//! risk infinite recursion.

use std::collections::HashSet;

use feraille_core::NodeId;

use crate::model::{DiskUsageLayoutNode, DiskUsageTree, NodeKind};

/// Build the layout subtree rooted at `root`. `max_depth` limits
/// recursion; depth-0 returns just the root with no children.
pub fn build_layout_node(
    tree: &DiskUsageTree,
    root: NodeId,
    max_depth: u32,
) -> DiskUsageLayoutNode {
    let mut visited = HashSet::new();
    let mut node = build_inner(tree, root, max_depth, &mut visited);
    node.sort_children_by_size();
    node
}

fn build_inner(
    tree: &DiskUsageTree,
    id: NodeId,
    remaining_depth: u32,
    visited: &mut HashSet<NodeId>,
) -> DiskUsageLayoutNode {
    let (intrinsic, scan_state, kind, file_category) = match tree.nodes.get(&id) {
        Some(n) => (n.size_bytes, n.scan_state, n.kind, n.file_category),
        None => (
            0,
            crate::model::ScanState::Unknown,
            NodeKind::Container,
            crate::file_category::FileCategory::Other,
        ),
    };

    if !visited.insert(id) {
        // Already visited via another container — record as a leaf so the
        // user sees it but don't recurse again.
        return DiskUsageLayoutNode::new(id, intrinsic, scan_state, kind, file_category, vec![]);
    }

    let mut children = Vec::new();
    let mut children_sum: u64 = 0;
    if remaining_depth > 0 {
        if let Some(member_ids) = tree.containers.get(&id) {
            children.reserve(member_ids.len());
            for &child_id in member_ids {
                let child = build_inner(tree, child_id, remaining_depth - 1, visited);
                children_sum = children_sum.saturating_add(child.size_bytes);
                children.push(child);
            }
        }
    } else if let Some(member_ids) = tree.containers.get(&id) {
        // We're at the depth limit but still need an honest aggregate —
        // descend with depth=0 just to gather sums (no rects).
        for &child_id in member_ids {
            let child = build_inner(tree, child_id, 0, visited);
            children_sum = children_sum.saturating_add(child.size_bytes);
        }
    }

    let total = intrinsic.saturating_add(children_sum);
    DiskUsageLayoutNode::new(id, total, scan_state, kind, file_category, children)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::DiskUsageFact;
    use crate::file_category::FileCategory;

    fn nid(raw: u64) -> NodeId {
        NodeId::from_raw(raw).expect("nonzero")
    }

    fn seed(tree: &mut DiskUsageTree, id: NodeId, kind: NodeKind, size: u64) {
        tree.apply_fact(&DiskUsageFact::NodeDiscovered {
            node: id,
            kind,
            file_category: FileCategory::Other,
            mtime: None,
            name: format!("n{}", id.as_raw()),
        });
        if size > 0 {
            tree.apply_fact(&DiskUsageFact::NodeSizeAdded {
                node: id,
                size_bytes: size,
            });
        }
    }

    fn link(tree: &mut DiskUsageTree, c: NodeId, n: NodeId) {
        tree.apply_fact(&DiskUsageFact::NodeLinked {
            container: c,
            node: n,
        });
    }

    #[test]
    fn aggregates_two_levels_and_sorts_descending() {
        // root -> [a, b]; a -> [a1, a2]; sizes: a1=10, a2=30, b=50.
        let mut t = DiskUsageTree::new(nid(1));
        seed(&mut t, nid(2), NodeKind::Container, 0); // a
        seed(&mut t, nid(3), NodeKind::File, 50); // b
        seed(&mut t, nid(4), NodeKind::File, 10); // a1
        seed(&mut t, nid(5), NodeKind::File, 30); // a2
        link(&mut t, nid(1), nid(2));
        link(&mut t, nid(1), nid(3));
        link(&mut t, nid(2), nid(4));
        link(&mut t, nid(2), nid(5));

        let layout = build_layout_node(&t, nid(1), 4);
        // a aggregates to 40, b is 50, root is 90.
        assert_eq!(layout.size_bytes, 90);
        // Children sorted descending: b (50), a (40).
        assert_eq!(layout.children[0].node_id, nid(3));
        assert_eq!(layout.children[1].node_id, nid(2));
        // a's children sorted: a2 (30), a1 (10).
        assert_eq!(layout.children[1].children[0].node_id, nid(5));
        assert_eq!(layout.children[1].children[1].node_id, nid(4));
    }

    #[test]
    fn dag_with_cycle_terminates() {
        let mut t = DiskUsageTree::new(nid(1));
        seed(&mut t, nid(2), NodeKind::Container, 0);
        seed(&mut t, nid(3), NodeKind::File, 5);
        link(&mut t, nid(1), nid(2));
        link(&mut t, nid(2), nid(1)); // cycle: 1 -> 2 -> 1
        link(&mut t, nid(2), nid(3));
        let layout = build_layout_node(&t, nid(1), 8);
        // Just assert it returns; the visited set prevents infinite loop.
        assert!(!layout.children.is_empty());
    }

    #[test]
    fn depth_zero_returns_root_with_aggregated_total_no_children() {
        let mut t = DiskUsageTree::new(nid(1));
        seed(&mut t, nid(2), NodeKind::File, 7);
        seed(&mut t, nid(3), NodeKind::File, 11);
        link(&mut t, nid(1), nid(2));
        link(&mut t, nid(1), nid(3));
        let layout = build_layout_node(&t, nid(1), 0);
        assert_eq!(layout.size_bytes, 18);
        assert!(layout.children.is_empty());
    }
}
