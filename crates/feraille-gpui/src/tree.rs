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
    sidebar::SidebarItem, ActiveTheme, Collapsible, h_flex, v_flex,
};

use crate::icons::IconCache;
use crate::shell::Shell;

/// One row to render in the tree view. Computed by `Shell` (needs
/// access to `expanded`, `tree_children`, `current_dir`), consumed by
/// the `TreeSection::render` impl.
#[derive(Clone, Debug)]
pub struct TreeRowSpec {
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
}

/// Cached representation of one direct child of an expanded folder.
/// Files are intentionally not included — the tree shows hierarchy;
/// the main pane shows files.
#[derive(Clone, Debug)]
pub struct TreeChild {
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

        v_flex().w_full().child(header).child(v_flex().w_full().children(rows))
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
    let TreeRowSpec {
        path,
        label,
        depth,
        is_expandable,
        is_expanded,
        is_active,
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
        let caret_path = path.clone();
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
                    let p = caret_path.clone();
                    shell.update(cx, |s, cx| {
                        s.toggle_tree_expand(&p, cx);
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

    let label_path = path.clone();
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
                let p = label_path.clone();
                shell.update(cx, |s, cx| {
                    s.navigate(p, cx);
                });
            }
        });

    row.into_any_element()
}
