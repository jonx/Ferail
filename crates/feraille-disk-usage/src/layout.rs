//! Squarified treemap layout (Bruls/Huizing/van Wijk, 2000).
//!
//! Pure transform: input `(DiskUsageLayoutNode, bounds, max_depth)`,
//! output `Vec<TreemapRect>`. Recomputed only when bounds, zoom path, or
//! the underlying tree epoch change — never on hover or selection.

use feraille_core::NodeId;

use crate::file_category::FileCategory;
use crate::model::{DiskUsageLayoutNode, NodeKind, ScanState};

#[derive(Debug, Clone, Copy)]
pub struct TreemapRect {
    pub node_id: NodeId,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub depth: u32,
    pub size_bytes: u64,
    pub scan_state: ScanState,
    pub has_children: bool,
    pub kind: NodeKind,
    pub file_category: FileCategory,
}

impl TreemapRect {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }
    pub fn area(&self) -> f32 {
        self.width * self.height
    }
}

/// Compute the rect list for `root` inside `bounds` `(x, y, w, h)`.
/// `max_depth` controls recursion: 0 = root only, 1 = root + children, etc.
pub fn compute_treemap(
    root: &DiskUsageLayoutNode,
    bounds: (f32, f32, f32, f32),
    max_depth: u32,
) -> Vec<TreemapRect> {
    let (x, y, w, h) = bounds;
    if w <= 0.0 || h <= 0.0 || root.size_bytes == 0 {
        return Vec::new();
    }

    let mut out = Vec::new();
    out.push(TreemapRect {
        node_id: root.node_id,
        x,
        y,
        width: w,
        height: h,
        depth: 0,
        size_bytes: root.size_bytes,
        scan_state: root.scan_state,
        has_children: !root.children.is_empty(),
        kind: root.kind,
        file_category: root.file_category,
    });

    if !root.children.is_empty() && max_depth > 0 {
        layout_children(&root.children, root.size_bytes, (x, y, w, h), 1, max_depth, &mut out);
    }
    out
}

/// Hit-test: prefer the deepest rect that contains the point. Pinned by
/// unit test — clicking on a leaf inside a parent picks the leaf.
pub fn hit_test(rects: &[TreemapRect], px: f32, py: f32) -> Option<&TreemapRect> {
    rects
        .iter()
        .rev()
        .find(|r| r.contains(px, py))
}

fn layout_children(
    children: &[DiskUsageLayoutNode],
    parent_size: u64,
    bounds: (f32, f32, f32, f32),
    depth: u32,
    max_depth: u32,
    out: &mut Vec<TreemapRect>,
) {
    let (_x, _y, w, h) = bounds;
    if w < 1.0 || h < 1.0 || children.is_empty() {
        return;
    }
    let total_area = w * h;
    let scale = if parent_size > 0 {
        total_area / parent_size as f32
    } else {
        0.0
    };
    let items: Vec<(usize, f32)> = children
        .iter()
        .enumerate()
        .filter(|(_, c)| c.size_bytes > 0)
        .map(|(i, c)| (i, c.size_bytes as f32 * scale))
        .collect();
    if items.is_empty() {
        return;
    }
    squarify(&items, children, bounds, depth, max_depth, out);
}

fn squarify(
    items: &[(usize, f32)],
    children: &[DiskUsageLayoutNode],
    bounds: (f32, f32, f32, f32),
    depth: u32,
    max_depth: u32,
    out: &mut Vec<TreemapRect>,
) {
    let (x, y, w, h) = bounds;
    if items.is_empty() || w < 1.0 || h < 1.0 {
        return;
    }
    if items.len() == 1 {
        let (idx, _) = items[0];
        add_node_rect(&children[idx], bounds, depth, max_depth, out);
        return;
    }

    let vertical = w >= h;
    let edge_length = if vertical { h } else { w };

    let (row, remaining) = find_best_row(items, edge_length);

    let row_area: f32 = row.iter().map(|(_, a)| a).sum();
    let row_thickness = if edge_length > 0.0 {
        row_area / edge_length
    } else {
        0.0
    };

    let mut offset = 0.0;
    for &(idx, area) in &row {
        let item_length = if row_thickness > 0.0 {
            area / row_thickness
        } else {
            0.0
        };
        let item_bounds = if vertical {
            (x, y + offset, row_thickness, item_length)
        } else {
            (x + offset, y, item_length, row_thickness)
        };
        add_node_rect(&children[idx], item_bounds, depth, max_depth, out);
        offset += item_length;
    }

    if !remaining.is_empty() {
        let remaining_bounds = if vertical {
            (x + row_thickness, y, w - row_thickness, h)
        } else {
            (x, y + row_thickness, w, h - row_thickness)
        };
        squarify(&remaining, children, remaining_bounds, depth, max_depth, out);
    }
}

fn find_best_row(
    items: &[(usize, f32)],
    edge_length: f32,
) -> (Vec<(usize, f32)>, Vec<(usize, f32)>) {
    if items.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut row: Vec<(usize, f32)> = Vec::new();
    let mut best_ratio = f32::MAX;

    for (i, &item) in items.iter().enumerate() {
        row.push(item);
        let row_area: f32 = row.iter().map(|(_, a)| a).sum();
        let row_thickness = if edge_length > 0.0 {
            row_area / edge_length
        } else {
            0.0
        };
        let worst = row
            .iter()
            .map(|(_, a)| {
                let item_length = if row_thickness > 0.0 {
                    a / row_thickness
                } else {
                    1.0
                };
                aspect_ratio(row_thickness, item_length)
            })
            .fold(0.0f32, f32::max);

        if worst > best_ratio && i > 0 {
            row.pop();
            return (row, items[i..].to_vec());
        }
        best_ratio = worst;
    }
    (row, Vec::new())
}

fn aspect_ratio(a: f32, b: f32) -> f32 {
    if a <= 0.0 || b <= 0.0 {
        return f32::MAX;
    }
    let r = a / b;
    if r >= 1.0 {
        r
    } else {
        1.0 / r
    }
}

fn add_node_rect(
    node: &DiskUsageLayoutNode,
    bounds: (f32, f32, f32, f32),
    depth: u32,
    max_depth: u32,
    out: &mut Vec<TreemapRect>,
) {
    let (x, y, w, h) = bounds;
    if w < 1.0 || h < 1.0 {
        return;
    }
    out.push(TreemapRect {
        node_id: node.node_id,
        x,
        y,
        width: w,
        height: h,
        depth,
        size_bytes: node.size_bytes,
        scan_state: node.scan_state,
        has_children: !node.children.is_empty(),
        kind: node.kind,
        file_category: node.file_category,
    });
    if depth < max_depth && !node.children.is_empty() {
        let pad = 1.0;
        let inner_w = (w - 2.0 * pad).max(0.0);
        let inner_h = (h - 2.0 * pad).max(0.0);
        if inner_w > 2.0 && inner_h > 2.0 {
            layout_children(
                &node.children,
                node.size_bytes,
                (x + pad, y + pad, inner_w, inner_h),
                depth + 1,
                max_depth,
                out,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(raw: u64) -> NodeId {
        NodeId::from_raw(raw).expect("nonzero")
    }

    fn file(id: u64, size: u64) -> DiskUsageLayoutNode {
        DiskUsageLayoutNode::new(
            nid(id),
            size,
            ScanState::Complete,
            NodeKind::File,
            FileCategory::Other,
            vec![],
        )
    }

    fn dir(id: u64, children: Vec<DiskUsageLayoutNode>) -> DiskUsageLayoutNode {
        let size = children.iter().map(|c| c.size_bytes).sum();
        DiskUsageLayoutNode::new(
            nid(id),
            size,
            ScanState::Complete,
            NodeKind::Container,
            FileCategory::Other,
            children,
        )
    }

    #[test]
    fn single_file_fills_bounds() {
        let n = file(1, 1000);
        let r = compute_treemap(&n, (0.0, 0.0, 100.0, 100.0), 1);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].width, 100.0);
        assert_eq!(r[0].height, 100.0);
    }

    #[test]
    fn empty_root_returns_no_rects() {
        let n = dir(1, vec![]); // size = 0
        let r = compute_treemap(&n, (0.0, 0.0, 100.0, 100.0), 4);
        assert!(r.is_empty());
    }

    #[test]
    fn two_equal_children_split_evenly() {
        let n = dir(1, vec![file(2, 500), file(3, 500)]);
        let r = compute_treemap(&n, (0.0, 0.0, 100.0, 100.0), 2);
        let leaves: Vec<_> = r.iter().filter(|r| r.depth == 1).collect();
        assert_eq!(leaves.len(), 2);
        let area: f32 = leaves.iter().map(|r| r.area()).sum();
        // Combined leaf area should match parent (minus padding rounding).
        assert!((area - 100.0 * 100.0).abs() < 1.0);
    }

    #[test]
    fn aspect_ratios_stay_squarish_for_uniform_sizes() {
        let kids: Vec<_> = (2..14u64).map(|i| file(i, 100)).collect();
        let n = dir(1, kids);
        let r = compute_treemap(&n, (0.0, 0.0, 400.0, 400.0), 1);
        let leaves: Vec<_> = r.iter().filter(|r| r.depth == 1).collect();
        for leaf in &leaves {
            let ar = aspect_ratio(leaf.width, leaf.height);
            // For uniform sizes in a square frame, every rect should be
            // well below 4:1. Squarified guarantees roughly golden-ratio
            // worst-case for 12 uniform items in a square.
            assert!(ar < 4.0, "got aspect ratio {ar}");
        }
    }

    #[test]
    fn hit_test_picks_deepest_leaf_inside_parent() {
        // Parent rect covers (0,0,100,100); child leaf covers (10,10,20,20).
        let parent = TreemapRect {
            node_id: nid(1),
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            depth: 0,
            size_bytes: 100,
            scan_state: ScanState::Complete,
            has_children: true,
            kind: NodeKind::Container,
            file_category: FileCategory::Other,
        };
        let child = TreemapRect {
            node_id: nid(2),
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
            depth: 1,
            size_bytes: 50,
            scan_state: ScanState::Complete,
            has_children: false,
            kind: NodeKind::File,
            file_category: FileCategory::Other,
        };
        let rects = vec![parent, child];
        let hit = hit_test(&rects, 15.0, 15.0).unwrap();
        assert_eq!(hit.node_id, nid(2));
        // Outside the child but inside the parent — picks the parent.
        let hit = hit_test(&rects, 60.0, 60.0).unwrap();
        assert_eq!(hit.node_id, nid(1));
        // Fully outside.
        assert!(hit_test(&rects, 200.0, 200.0).is_none());
    }

    #[test]
    fn rect_contains_top_left_inclusive_bottom_right_exclusive() {
        let r = TreemapRect {
            node_id: nid(1),
            x: 10.0,
            y: 20.0,
            width: 50.0,
            height: 30.0,
            depth: 0,
            size_bytes: 1000,
            scan_state: ScanState::Complete,
            has_children: false,
            kind: NodeKind::File,
            file_category: FileCategory::Other,
        };
        assert!(r.contains(10.0, 20.0));
        assert!(r.contains(35.0, 35.0));
        assert!(!r.contains(60.0, 50.0));
        assert!(!r.contains(5.0, 25.0));
    }
}
