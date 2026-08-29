//! Disk-usage data model and squarified treemap layout. Pure-logic crate
//! (no I/O, no platform). The walker that produces facts lives in
//! `ferail-fs-native`; the visual control consumes the [`TreemapRect`]
//! sequence we hand back.
//!
//! Architecture lifted from the Ferail predecessor's spec at
//! `Ferail/docs/done/DISK_USAGE.md`. Container -> members is a DAG by
//! contract: a single node may belong to multiple containers (search
//! results, duplicate groups, ad-hoc filters can graft additional ones).

pub mod aggregate;
pub mod facts;
pub mod file_category;
pub mod html_export;
pub mod layout;
pub mod model;

pub use aggregate::{
    build_filtered_layout_node_with_mode, build_layout_node, build_layout_node_with_mode,
};
pub use facts::DiskUsageFact;
pub use file_category::{FileCategory, classify_extension, classify_path};
pub use html_export::{
    category_color_rgba, category_label, treemap_html_document, treemap_html_fragment,
};
pub use layout::{TreemapRect, compute_treemap, hit_test};
pub use model::{
    DiskUsageLayoutNode, DiskUsageNode, DiskUsageStats, DiskUsageTree, NodeKind, ScanState,
    SizeMode,
};
