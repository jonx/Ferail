//! Build a [`DiskUsageLayoutNode`] tree from a [`DiskUsageTree`] DAG by
//! walking from a focus root, summing intrinsic sizes upward, and
//! sorting each level by descending size. Cycle-safe via a per-walk
//! visited set, so future scopes that graft additional containers don't
//! risk infinite recursion.

use std::collections::HashSet;

use ferail_core::NodeId;

use crate::model::{DiskUsageLayoutNode, DiskUsageTree, NodeKind, SizeMode};

/// Build the layout subtree rooted at `root`. `max_depth` limits
/// recursion; depth-0 returns just the root with no children.
/// Defaults to apparent size — for the allocated mode use
/// [`build_layout_node_with_mode`].
pub fn build_layout_node(
    tree: &DiskUsageTree,
    root: NodeId,
    max_depth: u32,
) -> DiskUsageLayoutNode {
    build_layout_node_with_mode(tree, root, max_depth, SizeMode::Apparent)
}

pub fn build_layout_node_with_mode(
    tree: &DiskUsageTree,
    root: NodeId,
    max_depth: u32,
    mode: SizeMode,
) -> DiskUsageLayoutNode {
    let mut visited = HashSet::new();
    let mut node = build_inner(tree, root, max_depth, mode, &mut visited);
    node.sort_children_by_size();
    node
}

/// Build a treemap projection containing only nodes accepted by `matches` and
/// the container chain needed to reach them. A matching container is treated
/// as one complete result (its ordinary depth-limited subtree is retained),
/// while a non-matching container contributes only matching descendants.
///
/// Unlike the normal builder this may inspect the whole scanned tree even
/// though `max_depth` is small: a match below the visible depth still has to
/// contribute its bytes to the collapsed ancestor tile. Callers should run it
/// off the UI thread. `cancelled` is checked throughout the walk so an
/// interactive filter can abandon a superseded query promptly.
pub fn build_filtered_layout_node_with_mode<M, C>(
    tree: &DiskUsageTree,
    root: NodeId,
    max_depth: u32,
    mode: SizeMode,
    matches: M,
    cancelled: C,
) -> Option<DiskUsageLayoutNode>
where
    M: Fn(&crate::model::DiskUsageNode) -> bool,
    C: Fn() -> bool,
{
    let mut visited = HashSet::new();
    let mut node = build_filtered_inner(
        tree,
        root,
        max_depth,
        mode,
        &matches,
        &cancelled,
        &mut visited,
        true,
    )?;
    if cancelled() {
        return None;
    }
    node.sort_children_by_size();
    Some(node)
}

#[allow(clippy::too_many_arguments)]
fn build_filtered_inner<M, C>(
    tree: &DiskUsageTree,
    id: NodeId,
    remaining_depth: u32,
    mode: SizeMode,
    matches: &M,
    cancelled: &C,
    visited: &mut HashSet<NodeId>,
    structural_root: bool,
) -> Option<DiskUsageLayoutNode>
where
    M: Fn(&crate::model::DiskUsageNode) -> bool,
    C: Fn() -> bool,
{
    if cancelled() || !visited.insert(id) {
        return None;
    }
    let source = tree.nodes.get(&id)?;
    let mut scan_state = source.scan_state;
    if tree.complete && source.kind == NodeKind::Container {
        scan_state = crate::model::ScanState::Complete;
    }
    let intrinsic = node_size(source, mode);

    // The focus root is structural rather than a search result. Otherwise a
    // `kind:folder` query would match it and immediately restore the complete
    // unfiltered tree.
    let direct_match = !structural_root && matches(source);
    if direct_match && source.kind == NodeKind::Container {
        let mut subtree = build_inner(tree, id, remaining_depth, mode, &mut HashSet::new());
        subtree.sort_children_by_size();
        return Some(subtree);
    }
    if source.kind != NodeKind::Container {
        return direct_match.then(|| {
            DiskUsageLayoutNode::with_mtime(
                id,
                intrinsic,
                scan_state,
                source.kind,
                source.file_category,
                source.mtime,
                vec![],
            )
        });
    }

    let mut visible_children = Vec::new();
    let mut filtered_total = 0u64;
    if let Some(member_ids) = tree.containers.get(&id) {
        if remaining_depth > 0 {
            visible_children.reserve(member_ids.len().min(256));
        }
        for &child_id in member_ids {
            if cancelled() {
                return None;
            }
            let Some(child) = build_filtered_inner(
                tree,
                child_id,
                remaining_depth.saturating_sub(1),
                mode,
                matches,
                cancelled,
                visited,
                false,
            ) else {
                continue;
            };
            filtered_total = filtered_total.saturating_add(child.size_bytes);
            if remaining_depth > 0 {
                visible_children.push(child);
            }
        }
    }
    if filtered_total == 0 && visible_children.is_empty() {
        return None;
    }
    let mut node = DiskUsageLayoutNode::with_mtime(
        id,
        filtered_total,
        scan_state,
        source.kind,
        source.file_category,
        source.mtime,
        visible_children,
    );
    node.sort_children_by_size();
    Some(node)
}

fn node_size(node: &crate::model::DiskUsageNode, mode: SizeMode) -> u64 {
    match mode {
        SizeMode::Apparent => node.size_bytes,
        SizeMode::Allocated => {
            if node.allocated_bytes > 0 {
                node.allocated_bytes
            } else {
                node.size_bytes
            }
        }
    }
}

fn build_inner(
    tree: &DiskUsageTree,
    id: NodeId,
    remaining_depth: u32,
    mode: SizeMode,
    visited: &mut HashSet<NodeId>,
) -> DiskUsageLayoutNode {
    let (intrinsic, mut scan_state, kind, file_category, mtime) = match tree.nodes.get(&id) {
        Some(n) => {
            let size = match mode {
                SizeMode::Apparent => n.size_bytes,
                // Fall back to apparent when allocated wasn't reported
                // (non-macOS or pre-iter-7 fact streams).
                SizeMode::Allocated => {
                    if n.allocated_bytes > 0 {
                        n.allocated_bytes
                    } else {
                        n.size_bytes
                    }
                }
            };
            (size, n.scan_state, n.kind, n.file_category, n.mtime)
        }
        None => (
            0,
            crate::model::ScanState::Unknown,
            NodeKind::Container,
            crate::file_category::FileCategory::Other,
            None,
        ),
    };
    // Fast NTFS streams directory starts but deliberately avoids a completion
    // fact for every directory. Once the whole snapshot is complete, rendering
    // can derive that state here instead of mutating millions of nodes on the
    // UI thread in one terminal scan tick.
    if tree.complete && kind == NodeKind::Container {
        scan_state = crate::model::ScanState::Complete;
    }

    if !visited.insert(id) {
        // Already visited via another container — record as a leaf so the
        // user sees it but don't recurse again.
        return DiskUsageLayoutNode::with_mtime(
            id,
            intrinsic,
            scan_state,
            kind,
            file_category,
            mtime,
            vec![],
        );
    }

    if remaining_depth == 0 {
        let descendants = tree.nodes.get(&id).map_or(0, |node| match mode {
            SizeMode::Apparent => node.descendant_size_bytes,
            SizeMode::Allocated => node.descendant_effective_allocated_bytes,
        });
        return DiskUsageLayoutNode::with_mtime(
            id,
            intrinsic.saturating_add(descendants),
            scan_state,
            kind,
            file_category,
            mtime,
            vec![],
        );
    }

    let mut children = Vec::new();
    let mut children_sum: u64 = 0;
    if let Some(member_ids) = tree.containers.get(&id) {
        children.reserve(member_ids.len());
        for &child_id in member_ids {
            let child = build_inner(tree, child_id, remaining_depth - 1, mode, visited);
            children_sum = children_sum.saturating_add(child.size_bytes);
            children.push(child);
        }
    }

    let total = intrinsic.saturating_add(children_sum);
    DiskUsageLayoutNode::with_mtime(id, total, scan_state, kind, file_category, mtime, children)
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
            is_cloud: false,
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

    #[test]
    fn completed_tree_derives_container_state_without_rewriting_nodes() {
        let mut tree = DiskUsageTree::new(nid(1));
        seed(&mut tree, nid(2), NodeKind::Container, 0);
        link(&mut tree, nid(1), nid(2));
        tree.complete = true;

        let layout = build_layout_node(&tree, nid(1), 2);
        assert_eq!(layout.scan_state, crate::model::ScanState::Complete);
        assert_eq!(
            layout.children[0].scan_state,
            crate::model::ScanState::Complete
        );
        assert_eq!(
            tree.nodes[&nid(2)].scan_state,
            crate::model::ScanState::Unknown
        );
    }

    #[test]
    fn filtered_layout_keeps_ancestors_and_only_matching_file_bytes() {
        let mut tree = DiskUsageTree::new(nid(1));
        seed(&mut tree, nid(2), NodeKind::Container, 0);
        seed(&mut tree, nid(3), NodeKind::File, 10);
        seed(&mut tree, nid(4), NodeKind::File, 30);
        link(&mut tree, nid(1), nid(2));
        link(&mut tree, nid(2), nid(3));
        link(&mut tree, nid(2), nid(4));

        let layout = build_filtered_layout_node_with_mode(
            &tree,
            nid(1),
            4,
            SizeMode::Apparent,
            |node| node.id == nid(4),
            || false,
        )
        .unwrap();
        assert_eq!(layout.size_bytes, 30);
        assert_eq!(layout.children.len(), 1);
        assert_eq!(layout.children[0].node_id, nid(2));
        assert_eq!(layout.children[0].size_bytes, 30);
        assert_eq!(layout.children[0].children[0].node_id, nid(4));
    }

    #[test]
    fn matching_folder_keeps_its_complete_subtree() {
        let mut tree = DiskUsageTree::new(nid(1));
        seed(&mut tree, nid(2), NodeKind::Container, 0);
        seed(&mut tree, nid(3), NodeKind::File, 12);
        link(&mut tree, nid(1), nid(2));
        link(&mut tree, nid(2), nid(3));

        let layout = build_filtered_layout_node_with_mode(
            &tree,
            nid(1),
            4,
            SizeMode::Apparent,
            |node| node.id == nid(2),
            || false,
        )
        .unwrap();
        assert_eq!(layout.size_bytes, 12);
        assert_eq!(layout.children[0].children[0].node_id, nid(3));
    }

    #[test]
    fn cancelled_filtered_layout_stops_without_a_projection() {
        let mut tree = DiskUsageTree::new(nid(1));
        seed(&mut tree, nid(2), NodeKind::File, 10);
        link(&mut tree, nid(1), nid(2));
        assert!(
            build_filtered_layout_node_with_mode(
                &tree,
                nid(1),
                4,
                SizeMode::Apparent,
                |_| true,
                || true,
            )
            .is_none()
        );
    }
}
