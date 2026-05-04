//! Controls implementing the spec in `specs/controls/`.
//!
//! Iter-2 layout: `primitives/` for atomic pieces (focus ring, scrollbar,
//! splitter, label, button); top-level files for explorer-specific
//! controls (virtualized_list, sidebar, tabstrip, breadcrumb, filetree).

pub mod breadcrumb;
pub mod filetree;
pub mod primitives;
pub mod selection;
pub mod sidebar;
pub mod tabstrip;
pub mod virtualized_list;

pub use breadcrumb::{BreadcrumbBar, BreadcrumbEvent};
pub use filetree::{FileTree, Section, SectionKind, TreeContextTarget, TreeEvent};
pub use selection::{Selection, SelectionSet};
pub use sidebar::{Sidebar, SidebarEvent};
pub use tabstrip::{TabHit, TabInfo, TabStrip, TabStripEvent};
pub use virtualized_list::{
    default_columns, sort_entries, Column, ColumnAlign, ColumnId, ListEvent, ListItem, SortKey,
    VirtualizedList,
};
