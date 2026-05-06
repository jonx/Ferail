//! Disk-usage data model and squarified treemap layout. Pure-logic crate
//! (no I/O, no platform). The walker that produces facts lives in
//! `feraille-fs-native`; the visual control consumes the [`TreemapRect`]
//! sequence we hand back.
//!
//! Architecture lifted from the Ferail predecessor's spec at
//! `Ferail/docs/done/DISK_USAGE.md`. Container -> members is a DAG by
//! contract: a single node may belong to multiple containers (search
//! results, duplicate groups, ad-hoc filters can graft additional ones).

pub mod aggregate;
pub mod facts;
pub mod file_category;
pub mod layout;
pub mod model;

pub use aggregate::build_layout_node;
pub use facts::DiskUsageFact;
pub use file_category::{classify_extension, classify_path, FileCategory};
pub use layout::{compute_treemap, hit_test, TreemapRect};
pub use model::{
    DiskUsageLayoutNode, DiskUsageNode, DiskUsageStats, DiskUsageTree, NodeKind, ScanState,
};
