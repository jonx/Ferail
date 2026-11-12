//! Scan history: a bounded per-folder size summary of a completed scan, and
//! the comparison of a later scan of the same root against it ("what grew?").
//!
//! Pure logic. The GPUI layer persists [`DirRecord`]s through the metadata
//! store and runs both walks on the background executor.
//!
//! A summary is deliberately not a full copy of the scan. It keeps every
//! folder at or above a small fraction of the root's size, plus the direct
//! subfolders of those folders, so that a later scan can tell a folder that
//! did not exist ("new") from one that was merely too small to be recorded
//! ("unknown"). `children_complete` records whether that second set was kept
//! in full; the row cap can cut it short.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ferail_core::NodeId;

use crate::model::{DiskUsageNode, DiskUsageTree, NodeKind, SizeMode};

/// One recorded folder of a scan summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirRecord {
    /// Absolute path of the folder; the root itself is the first record.
    pub path: String,
    /// Recursive apparent bytes.
    pub apparent: u64,
    /// Recursive allocated bytes, with the per-file apparent fallback the
    /// view uses when a scanner reports no allocation.
    pub allocated: u64,
    /// Every direct subfolder of this folder is recorded too, so a subfolder
    /// missing from the summary did not exist when it was taken.
    pub children_complete: bool,
}

/// Bounds of one summary.
#[derive(Clone, Copy, Debug)]
pub struct SnapshotLimits {
    /// A folder at or above this fraction of the root (in either size mode)
    /// is recorded together with its direct subfolders.
    pub min_fraction: f64,
    /// Hard cap on recorded folders. The largest are kept.
    pub max_dirs: usize,
}

impl SnapshotLimits {
    pub const DEFAULT: Self = Self {
        min_fraction: 0.001,
        max_dirs: 10_000,
    };
}

fn apparent_total(node: &DiskUsageNode) -> u64 {
    node.size_bytes.saturating_add(node.descendant_size_bytes)
}

fn allocated_total(node: &DiskUsageNode) -> u64 {
    let own = if node.allocated_bytes > 0 {
        node.allocated_bytes
    } else {
        node.size_bytes
    };
    own.saturating_add(node.descendant_effective_allocated_bytes)
}

/// Recursive size of `node` in `mode`, the figure the treemap shows.
pub fn node_total(node: &DiskUsageNode, mode: SizeMode) -> u64 {
    match mode {
        SizeMode::Apparent => apparent_total(node),
        SizeMode::Allocated => allocated_total(node),
    }
}

fn child_containers<'a>(
    tree: &'a DiskUsageTree,
    id: NodeId,
) -> impl Iterator<Item = (NodeId, &'a DiskUsageNode)> + 'a {
    tree.containers
        .get(&id)
        .into_iter()
        .flatten()
        .filter_map(move |child| {
            let node = tree.nodes.get(child)?;
            (node.kind == NodeKind::Container).then_some((*child, node))
        })
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Build the summary of a completed scan rooted at `root_path`. `cancelled`
/// is polled per folder; a cancelled build returns `None`.
pub fn build_snapshot(
    tree: &DiskUsageTree,
    root_path: &Path,
    limits: SnapshotLimits,
    cancelled: impl Fn() -> bool,
) -> Option<Vec<DirRecord>> {
    struct Candidate {
        path: PathBuf,
        apparent: u64,
        allocated: u64,
        depth: u32,
        parent: Option<usize>,
        significant: bool,
        child_dirs: u32,
    }

    let root = tree.nodes.get(&tree.root)?;
    let root_apparent = apparent_total(root);
    let root_allocated = allocated_total(root);
    let min_apparent = (root_apparent as f64 * limits.min_fraction).ceil() as u64;
    let min_allocated = (root_allocated as f64 * limits.min_fraction).ceil() as u64;
    let significant =
        |apparent: u64, allocated: u64| apparent >= min_apparent.max(1) || allocated >= min_allocated.max(1);

    let mut candidates = vec![Candidate {
        path: root_path.to_path_buf(),
        apparent: root_apparent,
        allocated: root_allocated,
        depth: 0,
        parent: None,
        significant: significant(root_apparent, root_allocated),
        child_dirs: 0,
    }];
    // Only significant folders are descended; their direct subfolders are
    // recorded either way.
    let mut stack = vec![(tree.root, 0usize)];
    let mut visited = HashSet::new();
    while let Some((id, index)) = stack.pop() {
        if cancelled() {
            return None;
        }
        if !candidates[index].significant || !visited.insert(id) {
            continue;
        }
        for (child_id, child) in child_containers(tree, id) {
            let apparent = apparent_total(child);
            let allocated = allocated_total(child);
            let child_index = candidates.len();
            let mut path = candidates[index].path.clone();
            path.push(&child.display_name);
            candidates.push(Candidate {
                path,
                apparent,
                allocated,
                depth: candidates[index].depth + 1,
                parent: Some(index),
                significant: significant(apparent, allocated),
                child_dirs: 0,
            });
            candidates[index].child_dirs += 1;
            stack.push((child_id, child_index));
        }
    }

    // Keep the largest folders. A folder is never smaller than its subfolder,
    // and ties go to the shallower one, so the kept set stays closed under
    // "parent of": no orphan rows.
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    if order.len() > limits.max_dirs {
        order.sort_by(|&a, &b| {
            let ka = candidates[a].apparent.max(candidates[a].allocated);
            let kb = candidates[b].apparent.max(candidates[b].allocated);
            kb.cmp(&ka)
                .then(candidates[a].depth.cmp(&candidates[b].depth))
                .then(a.cmp(&b))
        });
        order.truncate(limits.max_dirs.max(1));
    }
    let mut kept = vec![false; candidates.len()];
    for &ix in &order {
        kept[ix] = true;
    }
    let mut kept_children = vec![0u32; candidates.len()];
    for (ix, candidate) in candidates.iter().enumerate() {
        if let (true, Some(parent)) = (kept[ix], candidate.parent) {
            kept_children[parent] += 1;
        }
    }
    let mut rows: Vec<DirRecord> = order
        .into_iter()
        .map(|ix| {
                let c = &candidates[ix];
                DirRecord {
                    path: path_string(&c.path),
                    apparent: c.apparent,
                    allocated: c.allocated,
                    children_complete: c.significant && kept_children[ix] == c.child_dirs,
                }
            })
        .collect();
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    Some(rows)
}

/// What the previous summary says about one current folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Baseline {
    /// Recorded then, with these recursive sizes.
    Known { apparent: u64, allocated: u64 },
    /// Did not exist then: its parent's subfolders were recorded in full, or
    /// its parent is new itself.
    New,
}

impl Baseline {
    fn old_bytes(self, mode: SizeMode) -> u64 {
        match self {
            Self::Known {
                apparent,
                allocated,
            } => match mode {
                SizeMode::Apparent => apparent,
                SizeMode::Allocated => allocated,
            },
            Self::New => 0,
        }
    }
}

/// How one folder changed, in one size mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Grew,
    Shrank,
    New,
    Unchanged,
    /// Too small to be recorded last time: no honest delta exists.
    Unknown,
}

/// Classify a folder of `current` bytes against its baseline. A change
/// smaller than `noise` bytes counts as unchanged. Returns the kind and the
/// signed delta in bytes (0 for `Unknown`).
pub fn classify(baseline: Option<Baseline>, current: u64, mode: SizeMode, noise: u64) -> (ChangeKind, i64) {
    match baseline {
        None => (ChangeKind::Unknown, 0),
        Some(Baseline::New) => (ChangeKind::New, clamp_delta(current as i128)),
        Some(known) => {
            let delta = clamp_delta(current as i128 - known.old_bytes(mode) as i128);
            let kind = if delta.unsigned_abs() < noise.max(1) {
                ChangeKind::Unchanged
            } else if delta > 0 {
                ChangeKind::Grew
            } else {
                ChangeKind::Shrank
            };
            (kind, delta)
        }
    }
}

fn clamp_delta(delta: i128) -> i64 {
    delta.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// The bytes below which a change is noise: 1/20 000 of the root, at least
/// 64 KiB. Keeps a log file's growth from tinting a 500 GB map.
pub fn noise_floor(root_total: u64) -> u64 {
    (root_total / 20_000).max(64 * 1024)
}

/// Result of comparing a scan with the previous summary of the same root.
#[derive(Clone, Debug, Default)]
pub struct ChangeSet {
    /// Baselines of recorded and top-most new folders. A folder that is not
    /// listed inherits `New` from its nearest listed ancestor, or is unknown.
    pub baselines: HashMap<NodeId, Baseline>,
    /// Folders of the previous summary that no longer exist (top-most only,
    /// so nested deletions are not counted twice).
    pub deleted_dirs: u64,
    pub deleted_apparent: u64,
    pub deleted_allocated: u64,
    /// Root totals of the previous summary.
    pub previous_apparent: u64,
    pub previous_allocated: u64,
    /// Where the change concentrates, per size mode: folders whose change is
    /// not mostly explained by one of their subfolders, largest first.
    pub hotspots_apparent: Vec<(NodeId, i64)>,
    pub hotspots_allocated: Vec<(NodeId, i64)>,
}

impl ChangeSet {
    /// Baseline of `id`, resolving inherited `New` through `parent_of`.
    pub fn baseline_for(
        &self,
        id: NodeId,
        mut parent_of: impl FnMut(NodeId) -> Option<NodeId>,
    ) -> Option<Baseline> {
        let mut current = id;
        for _ in 0..4096 {
            if let Some(baseline) = self.baselines.get(&current) {
                return match baseline {
                    Baseline::New => Some(Baseline::New),
                    // A recorded ancestor of an unlisted folder means the
                    // folder itself was not recorded: unknown, unless it is
                    // the folder asked about.
                    known if current == id => Some(*known),
                    _ => None,
                };
            }
            current = parent_of(current)?;
        }
        None
    }

    pub fn hotspots(&self, mode: SizeMode) -> &[(NodeId, i64)] {
        match mode {
            SizeMode::Apparent => &self.hotspots_apparent,
            SizeMode::Allocated => &self.hotspots_allocated,
        }
    }

    pub fn deleted_bytes(&self, mode: SizeMode) -> u64 {
        match mode {
            SizeMode::Apparent => self.deleted_apparent,
            SizeMode::Allocated => self.deleted_allocated,
        }
    }

    pub fn previous_total(&self, mode: SizeMode) -> u64 {
        match mode {
            SizeMode::Apparent => self.previous_apparent,
            SizeMode::Allocated => self.previous_allocated,
        }
    }
}

/// How many hotspots to keep per size mode.
pub const HOTSPOT_CAP: usize = 50;

/// Compare the completed scan `tree` (rooted at `root_path`) with the
/// previous summary of the same root. `None` when cancelled.
pub fn compare(
    tree: &DiskUsageTree,
    root_path: &Path,
    previous: &[DirRecord],
    cancelled: impl Fn() -> bool,
) -> Option<ChangeSet> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Context {
        /// The parent was recorded; `complete` says whether its subfolders
        /// were recorded in full.
        Recorded { complete: bool },
        New,
    }
    struct Visited {
        id: NodeId,
        parent: Option<usize>,
        delta: [i64; 2],
        child_max: [i64; 2],
    }

    let old: HashMap<&str, &DirRecord> = previous.iter().map(|r| (r.path.as_str(), r)).collect();
    let mut matched: HashSet<&str> = HashSet::new();
    let mut set = ChangeSet::default();
    let root_key = path_string(root_path);
    if let Some(root) = old.get(root_key.as_str()) {
        set.previous_apparent = root.apparent;
        set.previous_allocated = root.allocated;
    } else {
        // A summary of some other root: nothing is comparable.
        return Some(set);
    }

    let mut visited: Vec<Visited> = Vec::new();
    let mut seen = HashSet::new();
    // (node, path, context from its parent, parent index in `visited`)
    let mut stack: Vec<(NodeId, PathBuf, Option<Context>, Option<usize>)> =
        vec![(tree.root, root_path.to_path_buf(), None, None)];
    while let Some((id, path, context, parent)) = stack.pop() {
        if cancelled() {
            return None;
        }
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = tree.nodes.get(&id) else { continue };
        let key = path_string(&path);
        let (baseline, own_context) = match old.get(key.as_str()) {
            Some(record) => {
                matched.insert(record.path.as_str());
                (
                    Baseline::Known {
                        apparent: record.apparent,
                        allocated: record.allocated,
                    },
                    Context::Recorded {
                        complete: record.children_complete,
                    },
                )
            }
            None => match context {
                Some(Context::New) => {
                    // Inherited: not listed, resolved through the ancestor.
                    let index = visited.len();
                    visited.push(Visited {
                        id,
                        parent,
                        delta: [
                            clamp_delta(apparent_total(node) as i128),
                            clamp_delta(allocated_total(node) as i128),
                        ],
                        child_max: [0, 0],
                    });
                    for (child_id, child) in child_containers(tree, id) {
                        let mut child_path = path.clone();
                        child_path.push(&child.display_name);
                        stack.push((child_id, child_path, Some(Context::New), Some(index)));
                    }
                    continue;
                }
                Some(Context::Recorded { complete: true }) => (Baseline::New, Context::New),
                // Unknown: nothing below can be recorded either, because the
                // summary is closed under "parent of".
                _ => continue,
            },
        };
        set.baselines.insert(id, baseline);
        let index = visited.len();
        visited.push(Visited {
            id,
            parent,
            delta: [
                clamp_delta(apparent_total(node) as i128 - baseline.old_bytes(SizeMode::Apparent) as i128),
                clamp_delta(
                    allocated_total(node) as i128 - baseline.old_bytes(SizeMode::Allocated) as i128,
                ),
            ],
            child_max: [0, 0],
        });
        for (child_id, child) in child_containers(tree, id) {
            let mut child_path = path.clone();
            child_path.push(&child.display_name);
            stack.push((child_id, child_path, Some(own_context), Some(index)));
        }
    }

    // Top-most deletions: unmatched records whose parent still exists.
    for record in previous {
        if matched.contains(record.path.as_str()) {
            continue;
        }
        let parent_matched = Path::new(&record.path)
            .parent()
            .map(path_string)
            .is_some_and(|parent| matched.contains(parent.as_str()));
        if parent_matched {
            set.deleted_dirs += 1;
            set.deleted_apparent = set.deleted_apparent.saturating_add(record.apparent);
            set.deleted_allocated = set.deleted_allocated.saturating_add(record.allocated);
        }
    }

    // Hotspots: a folder whose change is not mostly (80 %) carried by one
    // subfolder changing the same way. Children are visited after parents,
    // so one reverse pass fills every parent's strongest same-sign child.
    for mode in 0..2 {
        for ix in (0..visited.len()).rev() {
            let delta = visited[ix].delta[mode];
            if let Some(parent) = visited[ix].parent {
                let parent_delta = visited[parent].delta[mode];
                if parent_delta.signum() == delta.signum() && delta != 0 {
                    let best = &mut visited[parent].child_max[mode];
                    if delta.unsigned_abs() > best.unsigned_abs() {
                        *best = delta;
                    }
                }
            }
        }
    }
    let root_now = tree.nodes.get(&tree.root);
    for (mode, size_mode) in [(0, SizeMode::Apparent), (1, SizeMode::Allocated)] {
        let noise = noise_floor(root_now.map_or(0, |n| node_total(n, size_mode)));
        let mut spots: Vec<(NodeId, i64)> = visited
            .iter()
            .filter(|v| v.id != tree.root)
            .filter(|v| {
                let delta = v.delta[mode].unsigned_abs();
                delta >= noise && (v.child_max[mode].unsigned_abs() as f64) < delta as f64 * 0.8
            })
            .map(|v| (v.id, v.delta[mode]))
            .collect();
        spots.sort_by(|a, b| b.1.unsigned_abs().cmp(&a.1.unsigned_abs()).then(a.0.cmp(&b.0)));
        spots.truncate(HOTSPOT_CAP);
        if mode == 0 {
            set.hotspots_apparent = spots;
        } else {
            set.hotspots_allocated = spots;
        }
    }
    Some(set)
}

/// Fill colour for a change, as straight RGBA. `intensity` (0..=1) scales
/// grew/shrank from the neutral grey toward the full warm/cool tone; new
/// folders always use their own distinct hue.
pub fn change_color_rgba(kind: ChangeKind, intensity: f32) -> (u8, u8, u8, u8) {
    const NEUTRAL: (f32, f32, f32) = (122.0, 128.0, 140.0);
    const WARM: (f32, f32, f32) = (230.0, 92.0, 30.0);
    const COOL: (f32, f32, f32) = (40.0, 120.0, 220.0);
    const NEW: (f32, f32, f32) = (176.0, 72.0, 200.0);
    let mix = |target: (f32, f32, f32), t: f32| {
        let t = t.clamp(0.0, 1.0);
        (
            (NEUTRAL.0 + (target.0 - NEUTRAL.0) * t).round() as u8,
            (NEUTRAL.1 + (target.1 - NEUTRAL.1) * t).round() as u8,
            (NEUTRAL.2 + (target.2 - NEUTRAL.2) * t).round() as u8,
        )
    };
    let ((r, g, b), a) = match kind {
        ChangeKind::Grew => (mix(WARM, intensity), 230),
        ChangeKind::Shrank => (mix(COOL, intensity), 230),
        ChangeKind::New => (mix(NEW, 0.55 + 0.45 * intensity.clamp(0.0, 1.0)), 230),
        ChangeKind::Unchanged => (mix(WARM, 0.0), 190),
        ChangeKind::Unknown => (mix(WARM, 0.0), 120),
    };
    (r, g, b, a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::DiskUsageFact;
    use crate::file_category::FileCategory;

    fn nid(raw: u64) -> NodeId {
        NodeId::from_raw(raw).expect("nonzero")
    }

    fn add(tree: &mut DiskUsageTree, parent: u64, id: u64, name: &str, kind: NodeKind, size: u64) {
        tree.apply_facts(&[
            DiskUsageFact::NodeDiscovered {
                node: nid(id),
                kind,
                file_category: FileCategory::Other,
                mtime: None,
                name: name.into(),
                is_cloud: false,
            },
            DiskUsageFact::NodeLinked {
                container: nid(parent),
                node: nid(id),
            },
        ]);
        if size > 0 {
            tree.apply_fact(&DiskUsageFact::NodeSizeAdded {
                node: nid(id),
                size_bytes: size,
            });
        }
    }

    /// root/{big/{inner/f 600, g 300}, small/h 1, k 99}
    fn sample() -> DiskUsageTree {
        let mut t = DiskUsageTree::new(nid(1));
        add(&mut t, 1, 2, "big", NodeKind::Container, 0);
        add(&mut t, 2, 3, "inner", NodeKind::Container, 0);
        add(&mut t, 3, 4, "f", NodeKind::File, 600);
        add(&mut t, 2, 5, "g", NodeKind::File, 300);
        add(&mut t, 1, 6, "small", NodeKind::Container, 0);
        add(&mut t, 6, 7, "h", NodeKind::File, 1);
        add(&mut t, 1, 8, "k", NodeKind::File, 99);
        t
    }

    fn never() -> bool {
        false
    }

    #[test]
    fn snapshot_records_significant_folders_and_their_direct_subfolders() {
        let limits = SnapshotLimits {
            min_fraction: 0.01, // 10 bytes of 1000
            max_dirs: 100,
        };
        let rows = build_snapshot(&sample(), Path::new("/r"), limits, never).unwrap();
        let paths: Vec<_> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["/r", "/r/big", "/r/big/inner", "/r/small"]);
        let small = rows.iter().find(|r| r.path == "/r/small").unwrap();
        // Recorded as a subfolder of a significant folder, but too small to
        // have its own subfolders listed.
        assert_eq!(small.apparent, 1);
        assert!(!small.children_complete);
        assert!(rows[0].children_complete);
        assert_eq!(rows[0].apparent, 1000);
    }

    #[test]
    fn snapshot_cap_keeps_the_largest_and_marks_parents_incomplete() {
        let limits = SnapshotLimits {
            min_fraction: 0.0,
            max_dirs: 2,
        };
        let rows = build_snapshot(&sample(), Path::new("/r"), limits, never).unwrap();
        let paths: Vec<_> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["/r", "/r/big"]);
        assert!(!rows[0].children_complete, "/r/small was cut");
        assert!(!rows[1].children_complete, "/r/big/inner was cut");
    }

    #[test]
    fn compare_classifies_grown_new_unknown_and_deleted_folders() {
        let previous = vec![
            DirRecord { path: "/r".into(), apparent: 800, allocated: 800, children_complete: true },
            DirRecord { path: "/r/big".into(), apparent: 500, allocated: 500, children_complete: false },
            DirRecord { path: "/r/gone".into(), apparent: 200, allocated: 200, children_complete: true },
            DirRecord { path: "/r/gone/sub".into(), apparent: 150, allocated: 150, children_complete: true },
        ];
        let tree = sample();
        let set = compare(&tree, Path::new("/r"), &previous, never).unwrap();
        assert_eq!(
            set.baselines.get(&nid(2)),
            Some(&Baseline::Known { apparent: 500, allocated: 500 })
        );
        // /r was complete, so /r/small did not exist.
        assert_eq!(set.baselines.get(&nid(6)), Some(&Baseline::New));
        // /r/big was incomplete: /r/big/inner may simply have been small.
        assert_eq!(set.baselines.get(&nid(3)), None);
        assert_eq!(set.deleted_dirs, 1);
        assert_eq!(set.deleted_apparent, 200);
        assert_eq!(set.previous_apparent, 800);

        let (kind, delta) = classify(set.baselines.get(&nid(2)).copied(), 900, SizeMode::Apparent, 10);
        assert_eq!((kind, delta), (ChangeKind::Grew, 400));
        assert_eq!(classify(None, 5, SizeMode::Apparent, 10).0, ChangeKind::Unknown);
        let known = Some(Baseline::Known { apparent: 100, allocated: 100 });
        assert_eq!(classify(known, 95, SizeMode::Apparent, 10).0, ChangeKind::Unchanged);
        assert_eq!(classify(known, 50, SizeMode::Apparent, 10), (ChangeKind::Shrank, -50));
    }

    #[test]
    fn new_folders_pass_new_to_unlisted_descendants() {
        let mut tree = sample();
        add(&mut tree, 6, 9, "deeper", NodeKind::Container, 0);
        let previous = vec![DirRecord {
            path: "/r".into(),
            apparent: 1,
            allocated: 1,
            children_complete: true,
        }];
        let set = compare(&tree, Path::new("/r"), &previous, never).unwrap();
        let parents: HashMap<NodeId, NodeId> = [(nid(9), nid(6)), (nid(6), nid(1))].into();
        assert_eq!(set.baseline_for(nid(9), |id| parents.get(&id).copied()), Some(Baseline::New));
        // The recorded root does not make an unlisted descendant known.
        let unlisted: HashMap<NodeId, NodeId> = [(nid(42), nid(1))].into();
        assert_eq!(set.baseline_for(nid(42), |id| unlisted.get(&id).copied()), None);
    }

    #[test]
    fn hotspots_point_at_the_folder_that_carries_the_change() {
        let mut tree = DiskUsageTree::new(nid(1));
        add(&mut tree, 1, 2, "a", NodeKind::Container, 0);
        add(&mut tree, 2, 3, "b", NodeKind::Container, 0);
        add(&mut tree, 3, 4, "blob", NodeKind::File, 10_000_000);
        let previous = vec![
            DirRecord { path: "/r".into(), apparent: 0, allocated: 0, children_complete: true },
            DirRecord { path: "/r/a".into(), apparent: 0, allocated: 0, children_complete: true },
        ];
        let set = compare(&tree, Path::new("/r"), &previous, never).unwrap();
        // /r/a grew by 10 MB, all of it in the new /r/a/b: b is the hotspot.
        assert_eq!(set.hotspots_apparent, vec![(nid(3), 10_000_000)]);
    }

    #[test]
    fn a_summary_of_another_root_compares_as_nothing() {
        let previous = vec![DirRecord {
            path: "/elsewhere".into(),
            apparent: 1,
            allocated: 1,
            children_complete: true,
        }];
        let set = compare(&sample(), Path::new("/r"), &previous, never).unwrap();
        assert!(set.baselines.is_empty());
    }

    #[test]
    fn change_colours_scale_with_intensity_and_keep_new_distinct() {
        let faint = change_color_rgba(ChangeKind::Grew, 0.0);
        let strong = change_color_rgba(ChangeKind::Grew, 1.0);
        assert_eq!(faint.0, change_color_rgba(ChangeKind::Unchanged, 1.0).0);
        assert!(strong.0 > faint.0 && strong.2 < faint.2, "warm");
        let cool = change_color_rgba(ChangeKind::Shrank, 1.0);
        assert!(cool.2 > cool.0, "cool");
        let new = change_color_rgba(ChangeKind::New, 0.0);
        assert_ne!((new.0, new.1, new.2), (faint.0, faint.1, faint.2));
    }

    #[test]
    fn noise_floor_has_a_minimum() {
        assert_eq!(noise_floor(0), 64 * 1024);
        assert_eq!(noise_floor(20_000 * 1_000_000), 1_000_000);
    }
}
