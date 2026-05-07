//! Fact stream emitted by the scanner. Aggregation lives on the consumer
//! side ([`DiskUsageTree::apply_facts`]) so the worker stays free of
//! UI-thread concerns.

use std::time::SystemTime;

use feraille_core::NodeId;

use crate::file_category::FileCategory;
use crate::model::{DiskUsageTree, NodeKind, ScanState};

#[derive(Debug, Clone)]
pub enum DiskUsageFact {
    NodeDiscovered {
        node: NodeId,
        kind: NodeKind,
        file_category: FileCategory,
        mtime: Option<SystemTime>,
        name: String,
        is_cloud: bool,
    },
    NodeLinked {
        container: NodeId,
        node: NodeId,
    },
    NodeSizeAdded {
        node: NodeId,
        size_bytes: u64,
    },
    /// Allocated (on-disk, block-aligned) bytes for a node — emitted
    /// by the macOS scanner alongside `NodeSizeAdded`. Optional: not
    /// every platform supports it cheaply.
    NodeAllocatedAdded {
        node: NodeId,
        bytes: u64,
    },
    ContainerScanStarted {
        container: NodeId,
    },
    ContainerScanCompleted {
        container: NodeId,
    },
}

impl DiskUsageTree {
    pub fn apply_fact(&mut self, fact: &DiskUsageFact) {
        match fact {
            DiskUsageFact::NodeDiscovered {
                node,
                kind,
                file_category,
                mtime,
                name,
                is_cloud,
            } => {
                self.ensure_node_with_meta(
                    *node, *kind, *file_category, *mtime, name, *is_cloud,
                );
            }
            DiskUsageFact::NodeLinked { container, node } => {
                // Containers are always Container-kind even before they're
                // formally announced — the link itself implies as much.
                self.ensure_node(*container).kind = NodeKind::Container;
                self.add_link(*container, *node);
            }
            DiskUsageFact::NodeSizeAdded { node, size_bytes } => {
                self.add_size(*node, *size_bytes);
            }
            DiskUsageFact::NodeAllocatedAdded { node, bytes } => {
                self.add_allocated(*node, *bytes);
            }
            DiskUsageFact::ContainerScanStarted { container } => {
                self.set_scan_state(*container, ScanState::Scanning);
            }
            DiskUsageFact::ContainerScanCompleted { container } => {
                self.set_scan_state(*container, ScanState::Complete);
            }
        }
    }

    pub fn apply_facts(&mut self, facts: &[DiskUsageFact]) {
        for fact in facts {
            self.apply_fact(fact);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_category::FileCategory;
    use crate::model::DiskUsageNode;

    fn nid(raw: u64) -> NodeId {
        NodeId::from_raw(raw).expect("nonzero")
    }

    #[test]
    fn discover_link_size_complete_round_trip() {
        let mut t = DiskUsageTree::new(nid(1));
        t.apply_facts(&[
            DiskUsageFact::ContainerScanStarted { container: nid(1) },
            DiskUsageFact::NodeDiscovered {
                node: nid(2),
                kind: NodeKind::File,
                file_category: FileCategory::Image,
                mtime: None,
                name: "p.png".to_string(),
                is_cloud: false,
            },
            DiskUsageFact::NodeLinked {
                container: nid(1),
                node: nid(2),
            },
            DiskUsageFact::NodeSizeAdded {
                node: nid(2),
                size_bytes: 4096,
            },
            DiskUsageFact::ContainerScanCompleted { container: nid(1) },
        ]);

        let n: &DiskUsageNode = t.nodes.get(&nid(2)).unwrap();
        assert_eq!(n.size_bytes, 4096);
        assert_eq!(n.kind, NodeKind::File);
        assert_eq!(n.file_category, FileCategory::Image);
        assert_eq!(n.display_name, "p.png");

        assert_eq!(t.containers.get(&nid(1)).unwrap(), &vec![nid(2)]);
        assert_eq!(t.nodes.get(&nid(1)).unwrap().scan_state, ScanState::Complete);
    }

    #[test]
    fn link_implies_container_kind_even_without_discovery() {
        let mut t = DiskUsageTree::new(nid(1));
        t.apply_fact(&DiskUsageFact::NodeLinked {
            container: nid(7),
            node: nid(8),
        });
        assert_eq!(t.nodes.get(&nid(7)).unwrap().kind, NodeKind::Container);
    }
}
