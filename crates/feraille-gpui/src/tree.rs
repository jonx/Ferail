//! Hierarchical tree view rendered inside the sidebar.
//!
//! Harvest Stage 9.c. Replaces the flat `SidebarMenuItem` list with a
//! recursively-expandable folder tree. Each section (Locations,
//! Volumes) renders as a `TreeSection` — a thin `SidebarItem` wrapper
//! around a pre-computed `Vec<TreeRowSpec>` so the build step (which
//! needs `&Shell` state) stays inside the Shell view, while the render
//! step satisfies gpui-component's `SidebarItem` trait without needing
//! a mutable Shell borrow.
//!
//! State that drives the visible rows lives on `Shell`:
//! - `expanded: HashSet<PathBuf>` — which folders are currently open.
//! - `tree_children: HashMap<PathBuf, Vec<TreeChild>>` — per-path
//!   child cache (folders only; files are not shown in the tree).
//!
//! Click semantics mirror Finder:
//! - Label click → navigate.
//! - Caret click → toggle expand/collapse (with
//!   `cx.stop_propagation()` so the row's navigate handler doesn't
//!   also fire).
//!
//! Lazy enumeration: `Shell::ensure_tree_children` runs
//! `std::fs::read_dir` once per expanded path, caches the result. The
//! current implementation is synchronous because the existing
//! `Shell::load_path` is too — moving both to a background-executor
//! pipeline is a unified follow-on (streaming enumeration).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Collapsible, h_flex,
    menu::ContextMenuExt as _,
    sidebar::{SidebarGroup, SidebarItem, SidebarMenu},
    v_flex,
};

use crate::icons::IconCache;
use crate::shell::Shell;

/// One row to render in the tree view. Computed by `Shell` (needs
/// access to `expanded`, `tree_children`, `current_dir`), consumed by
/// the `TreeSection::render` impl.
#[derive(Clone, Debug)]
pub struct TreeRowSpec {
    pub node_id: feraille_core::NodeId,
    pub path: PathBuf,
    pub label: SharedString,
    pub depth: usize,
    /// True when the row represents a directory we know can be
    /// expanded (everything in our tree is a directory today; kept
    /// as a flag so a future "tree shows files too" mode is one
    /// boolean away).
    pub is_expandable: bool,
    /// Whether this directory is currently open in `Shell::expanded`.
    pub is_expanded: bool,
    /// Whether this directory equals the active tab's `current_dir`.
    pub is_active: bool,
    /// Optional `(total_bytes, available_bytes)` capacity to render a
    /// Finder-style capacity bar under the label. Populated for
    /// volume rows; `None` for everything else.
    pub capacity: Option<(u64, u64)>,
}

/// Cached representation of one direct child of an expanded folder.
/// Files are intentionally not included — the tree shows hierarchy;
/// the main pane shows files.
#[derive(Clone, Debug)]
pub struct TreeChild {
    pub node_id: feraille_core::NodeId,
    pub path: PathBuf,
    pub label: String,
}

/// Pre-built tree section: a header label + a flat list of rows in
/// display order (deeper rows already interleaved with their parents
/// by the builder). Implements `SidebarItem` so it can be passed
/// straight into `gpui_component::Sidebar`.
#[derive(Clone)]
pub struct TreeSection {
    label: SharedString,
    rows: Vec<TreeRowSpec>,
    shell: WeakEntity<Shell>,
    icons: Rc<RefCell<IconCache>>,
    collapsed: bool,
}

impl TreeSection {
    pub fn new(
        label: impl Into<SharedString>,
        rows: Vec<TreeRowSpec>,
        shell: WeakEntity<Shell>,
        icons: Rc<RefCell<IconCache>>,
    ) -> Self {
        Self {
            label: label.into(),
            rows,
            shell,
            icons,
            collapsed: false,
        }
    }
}

impl Collapsible for TreeSection {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }
    fn collapsed(mut self, c: bool) -> Self {
        self.collapsed = c;
        self
    }
}

/// Unifies the two kinds of section the shell's sidebar contains
/// (flat icon-prefixed menu groups for Favorites, custom tree
/// sections for Browse / Volumes) into a single `SidebarItem` so
/// `gpui_component::Sidebar<E>` can hold a mixed sequence — gpui-
/// component otherwise pins one `E` for all of a sidebar's children.
#[derive(Clone)]
pub enum ShellSidebarItem {
    Group(SidebarGroup<SidebarMenu>),
    Tree(TreeSection),
}

impl ShellSidebarItem {
    pub fn group(g: SidebarGroup<SidebarMenu>) -> Self {
        ShellSidebarItem::Group(g)
    }
    pub fn tree(t: TreeSection) -> Self {
        ShellSidebarItem::Tree(t)
    }
}

impl Collapsible for ShellSidebarItem {
    fn is_collapsed(&self) -> bool {
        match self {
            ShellSidebarItem::Group(g) => g.is_collapsed(),
            ShellSidebarItem::Tree(t) => t.is_collapsed(),
        }
    }
    fn collapsed(self, c: bool) -> Self {
        match self {
            ShellSidebarItem::Group(g) => ShellSidebarItem::Group(g.collapsed(c)),
            ShellSidebarItem::Tree(t) => ShellSidebarItem::Tree(t.collapsed(c)),
        }
    }
}

impl SidebarItem for ShellSidebarItem {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        match self {
            ShellSidebarItem::Group(g) => g.render(id, window, cx).into_any_element(),
            ShellSidebarItem::Tree(t) => t.render(id, window, cx).into_any_element(),
        }
    }
}

impl SidebarItem for TreeSection {
    fn render(
        self,
        _id: impl Into<ElementId>,
        _window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let header = h_flex()
            .flex_shrink_0()
            .px_2()
            .rounded(theme.radius)
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.sidebar_foreground.opacity(0.7))
            .h_8()
            .child(self.label);

        let shell = self.shell.clone();
        let icons = self.icons.clone();
        let rows = self
            .rows
            .into_iter()
            .map(|spec| render_tree_row(spec, shell.clone(), icons.clone(), cx))
            .collect::<Vec<AnyElement>>();

        v_flex()
            .w_full()
            .child(header)
            .child(v_flex().w_full().children(rows))
    }
}

/// Render one tree row. The caret + label split is deliberate so
/// click-on-name navigates while click-on-caret only toggles
/// expansion — matches Finder's sidebar. A small NSWorkspace-fetched
/// folder/volume icon renders between the caret and the label, with
/// the icon cache keyed by path so /Volumes/Foo gets its custom icon
/// rather than the generic blue-folder.
fn render_tree_row(
    spec: TreeRowSpec,
    shell: WeakEntity<Shell>,
    icons: Rc<RefCell<IconCache>>,
    cx: &mut App,
) -> AnyElement {
    let _path_guard = feraille_core::path_guard::enter_render();
    let TreeRowSpec {
        node_id,
        path,
        label,
        depth,
        is_expandable,
        is_expanded,
        is_active,
        capacity,
    } = spec;
    let theme = cx.theme();
    let row_key: SharedString = format!("tree-row-{}", path.display()).into();
    let caret_key: SharedString = format!("tree-caret-{}", path.display()).into();

    // 8px base + ~14px per depth level — readable indentation
    // without taking too much sidebar width.
    let indent = px(8.0 + 14.0 * depth as f32);

    let mut row = h_flex()
        .id(ElementId::Name(row_key))
        .w_full()
        .pl(indent)
        .pr_2()
        .py_1()
        .gap_1()
        .items_center()
        .text_sm()
        .rounded(theme.radius)
        .cursor_pointer()
        .text_color(if is_active {
            theme.sidebar_accent_foreground
        } else {
            theme.sidebar_foreground
        });
    if is_active {
        row = row.bg(theme.sidebar_accent);
    } else {
        let hover_bg = theme.sidebar_accent.opacity(0.5);
        row = row.hover(move |this| this.bg(hover_bg));
    }

    // Caret slot. Reserves the same width for non-expandable rows so
    // labels align across rows that don't have a caret (none today,
    // but the layout supports it). `▼` / `▶` render larger than the
    // small `▾`/`▸` glyphs at our font size.
    if is_expandable {
        let caret_node = node_id;
        let shell_for_caret = shell.clone();
        let caret = h_flex()
            .id(ElementId::Name(caret_key))
            .flex_shrink_0()
            .w(px(16.0))
            .h(px(16.0))
            .items_center()
            .justify_center()
            .text_color(theme.muted_foreground)
            .child(if is_expanded { "\u{25BC}" } else { "\u{25B6}" })
            .text_size(px(9.0))
            .on_click(move |_, _window, cx| {
                // Caret toggles expand-collapse only. Suppress
                // bubbling so the row's navigate handler doesn't run
                // — clicking the caret should not change the
                // current directory.
                cx.stop_propagation();
                if let Some(shell) = shell_for_caret.upgrade() {
                    shell.update(cx, |s, cx| {
                        s.toggle_tree_expand_node(caret_node, cx);
                    });
                }
            });
        row = row.child(caret);
    } else {
        row = row.child(div().flex_shrink_0().w(px(16.0)));
    }

    // Real folder/volume icon between the caret and the label. The
    // first call per path costs one NSWorkspace fetch (~1ms); the
    // shared cache means subsequent renders are a HashMap hit.
    let icon = icons.borrow_mut().folder_icon_for(&path);
    row = row.child(
        div()
            .flex_shrink_0()
            .w(px(16.0))
            .h(px(16.0))
            .child(img(icon).w(px(16.0)).h(px(16.0))),
    );

    let label_node = node_id;
    let shell_for_label = shell.clone();
    row = row
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(label)
                .when(is_active, |this| this.font_weight(FontWeight::SEMIBOLD)),
        )
        .on_click(move |_, _window, cx| {
            if let Some(shell) = shell_for_label.upgrade() {
                shell.update(cx, |s, cx| {
                    s.navigate_node(label_node, cx);
                });
            }
        });

    // Phase 6 (next-level): closure that adds the right-click menu
    // to whichever final element the row renders into (the row
    // alone, or the row+capacity-bar wrapper for volumes). Lives
    // outside the row-building chain because `.context_menu(...)`
    // changes the element's type to `ContextMenu<E>` — incompatible
    // with the `row = row.child(...)` accumulator pattern above.
    let shell_for_menu = shell.clone();
    let path_for_menu = path.clone();
    let attach_menu = move |el: gpui::Stateful<gpui::Div>| {
        el.context_menu(move |menu, _window, cx| {
            use crate::shell::{
                CopyContextPath, NewFolderHere, OpenContextInNewTab, RevealContextPath,
            };
            if let Some(shell) = shell_for_menu.upgrade() {
                shell.update(cx, |s, _| {
                    s.context_target = Some(path_for_menu.clone());
                });
            }
            menu.menu("Open in New Tab", Box::new(OpenContextInNewTab))
                .separator()
                .menu("Reveal in Finder", Box::new(RevealContextPath))
                .menu("Copy Path", Box::new(CopyContextPath))
                .separator()
                .menu("New Folder Here", Box::new(NewFolderHere))
        })
    };

    // Capacity bar for volume rows. Finder draws this as a thin
    // line under the volume name, with the used portion filled in
    // accent and the rest in muted grey. Matches the
    // `feraille-controls::filetree` NodeCapacity shape from the
    // old app.
    if let Some((total, available)) = capacity {
        if total > 0 {
            let theme = cx.theme();
            let used_fraction =
                ((total.saturating_sub(available)) as f32 / total as f32).clamp(0.0, 1.0);
            // Indent the bar so it sits under the label, skipping
            // caret + icon columns (16 + 4 + 16 + 4 = ~40 DIPs).
            let bar_indent = px(8.0 + 14.0 * depth as f32 + 40.0);
            let bar_w = px(140.0);
            let fill_w = bar_w * used_fraction;
            let track_bg = theme.muted_foreground.opacity(0.25);
            let fill_bg = theme.muted_foreground.opacity(0.85);
            let bar = div()
                .flex()
                .items_center()
                .pl(bar_indent)
                .pr_2()
                .pb_1()
                .child(
                    div()
                        .w(bar_w)
                        .h(px(4.0))
                        .rounded(px(2.0))
                        .bg(track_bg)
                        .child(div().h_full().w(fill_w).rounded(px(2.0)).bg(fill_bg)),
                );
            return v_flex()
                .id(("tree-row-capacity", node_id.as_raw() as usize))
                .w_full()
                .child(attach_menu(row))
                .child(bar)
                .into_any_element();
        }
    }

    attach_menu(row).into_any_element()
}
