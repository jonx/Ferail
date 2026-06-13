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
    sidebar::{SidebarItem, SidebarMenu},
    v_flex,
};
use smallvec::smallvec;

use crate::icons::IconCache;
use crate::shell::Shell;

/// Icon edge length (DIPs) for every sidebar row icon — Locations,
/// Favorites, Browse, and Volumes share it so the sections read as
/// one surface. Finder sizes its sidebar icons noticeably larger
/// than its list-view icons; 24 matches that feel against our 16px
/// file-list icons. Disclosure carets and the section "+" button
/// stay at 16 — they're affordances, not icons.
pub(crate) const SIDEBAR_ICON_PX: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeRowIcon {
    Folder,
    Volume,
}

/// One indent-column of ancestry connector to draw left of a tree
/// row. Index 0 is the outermost ancestor level; the last entry is
/// always the row's own connector (`Tee` or `Corner`). Earlier
/// entries are `Vertical` while that ancestor still has siblings
/// below, `Blank` once its subtree is the trailing one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeGuide {
    /// Empty 14px spacer — ancestor at this level was its parent's
    /// last child, so no line continues through here.
    Blank,
    /// `│` — an outer ancestor's line passing through this row.
    Vertical,
    /// `├` — this row, with more siblings below it.
    Tee,
    /// `└` — this row as its parent's last visible child.
    Corner,
}

/// One row to render in the tree view. Computed by `Shell` (needs
/// access to `expanded`, `tree_children`, `current_dir`), consumed by
/// the `TreeSection::render` impl.
#[derive(Clone, Debug)]
pub struct TreeRowSpec {
    pub node_id: feraille_core::NodeId,
    pub path: PathBuf,
    pub label: SharedString,
    pub depth: usize,
    /// Connector glyphs to draw left of the caret, one per depth
    /// level (`guides.len() == depth`). Computed by the row builder,
    /// which knows each row's last-visible-child status.
    pub guides: Vec<TreeGuide>,
    /// True when the row represents a directory we know contains at
    /// least one subdirectory (resolved at enumeration time on the
    /// worker). Rows without one render no disclosure caret.
    pub is_expandable: bool,
    /// Whether this directory is currently open in `Shell::expanded`.
    pub is_expanded: bool,
    /// Whether this directory equals the active tab's `current_dir`.
    pub is_active: bool,
    /// Optional `(total_bytes, available_bytes)` capacity to render a
    /// Finder-style capacity bar under the label. Populated for
    /// volume rows; `None` for everything else.
    pub capacity: Option<(u64, u64)>,
    pub icon: TreeRowIcon,
    /// §5 favorited indicator: `true` when this path is in the user-
    /// curated Favorites list. Computed from `Shell::favorites` at row
    /// build time; render paints a small accent star trailing the label.
    pub favorited: bool,
}

/// Cached representation of one direct child of an expanded folder.
/// Files are intentionally not included — the tree shows hierarchy;
/// the main pane shows files.
#[derive(Clone, Debug)]
pub struct TreeChild {
    pub node_id: feraille_core::NodeId,
    pub path: PathBuf,
    pub label: String,
    /// Platform hidden semantics resolved at load time by
    /// `feraille_fs_native::entry_is_hidden` (UF_HIDDEN on macOS,
    /// FILE_ATTRIBUTE_HIDDEN on Windows, dot-prefix everywhere) — same
    /// contract as `FileEntry::hidden`.
    pub hidden: bool,
    /// Whether this directory itself contains at least one
    /// subdirectory, resolved at enumeration time (worker thread) by
    /// `shell::loading::dir_has_subdir`. Drives the caret: leaf
    /// folders render no disclosure mark. Hidden subdirectories
    /// count — the chevron may reveal nothing while Show Hidden is
    /// off, which beats scanning twice per child.
    pub has_subdirs: bool,
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

/// Section header shared by every sidebar section (Locations,
/// Favorites, Browse, Volumes) so the labels read as one family.
/// `SidebarGroup`'s built-in header is identical except it lacks the
/// SEMIBOLD weight — which is why Locations uses [`LabeledMenu`]
/// below instead of `SidebarGroup`.
pub(crate) fn section_header(label: SharedString, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .flex_shrink_0()
        .px_2()
        .rounded(theme.radius)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.sidebar_foreground.opacity(0.7))
        .h_8()
        .child(label)
}

/// A flat `SidebarMenu` under a [`section_header`]-styled label.
/// Replaces `SidebarGroup<SidebarMenu>` for the Locations section so
/// its header matches Browse/Volumes (`SidebarGroup` renders its
/// label in regular weight).
#[derive(Clone)]
pub struct LabeledMenu {
    label: SharedString,
    menu: SidebarMenu,
    collapsed: bool,
}

impl LabeledMenu {
    pub fn new(label: impl Into<SharedString>, menu: SidebarMenu) -> Self {
        Self {
            label: label.into(),
            menu,
            collapsed: false,
        }
    }
}

impl Collapsible for LabeledMenu {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }
    fn collapsed(mut self, c: bool) -> Self {
        self.collapsed = c;
        self
    }
}

impl SidebarItem for LabeledMenu {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            // Icon-collapsed sidebar hides section labels, same as
            // `SidebarGroup` does.
            .when(!self.collapsed, |this| {
                this.child(section_header(self.label.clone(), cx))
            })
            .child(
                self.menu
                    .collapsed(self.collapsed)
                    .render(id, window, cx)
                    .into_any_element(),
            )
    }
}

/// Unifies the kinds of section the shell's sidebar contains:
/// flat icon-prefixed menu groups (Locations), the user-curated
/// Favorites section with its own collapse + rendering rules
/// (docs/features/FAVORITES.md), and custom tree sections (Browse,
/// Volumes). Wrapping them in a single `SidebarItem` enum lets
/// `gpui_component::Sidebar<E>` hold the mixed sequence — gpui-
/// component otherwise pins one `E` for all of a sidebar's children.
// The size spread (Group ≈ 700 B vs Tree ≈ 96 B) is fine here: the
// sidebar builds a handful of these per render and drops them with
// the frame — boxing the large variant would add an allocation per
// section per frame for no win.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum ShellSidebarItem {
    Group(LabeledMenu),
    Favorites(crate::favorites_section::FavoritesSection),
    Recents(crate::recents_section::RecentsSection),
    Tree(TreeSection),
}

impl ShellSidebarItem {
    pub fn group(g: LabeledMenu) -> Self {
        ShellSidebarItem::Group(g)
    }
    pub fn favorites(f: crate::favorites_section::FavoritesSection) -> Self {
        ShellSidebarItem::Favorites(f)
    }
    pub fn recents(r: crate::recents_section::RecentsSection) -> Self {
        ShellSidebarItem::Recents(r)
    }
    pub fn tree(t: TreeSection) -> Self {
        ShellSidebarItem::Tree(t)
    }
}

impl Collapsible for ShellSidebarItem {
    fn is_collapsed(&self) -> bool {
        match self {
            ShellSidebarItem::Group(g) => g.is_collapsed(),
            ShellSidebarItem::Favorites(f) => f.is_collapsed(),
            ShellSidebarItem::Recents(r) => r.is_collapsed(),
            ShellSidebarItem::Tree(t) => t.is_collapsed(),
        }
    }
    fn collapsed(self, c: bool) -> Self {
        match self {
            ShellSidebarItem::Group(g) => ShellSidebarItem::Group(g.collapsed(c)),
            ShellSidebarItem::Favorites(f) => ShellSidebarItem::Favorites(f.collapsed(c)),
            ShellSidebarItem::Recents(r) => ShellSidebarItem::Recents(r.collapsed(c)),
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
            ShellSidebarItem::Favorites(f) => f.render(id, window, cx).into_any_element(),
            ShellSidebarItem::Recents(r) => r.render(id, window, cx).into_any_element(),
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
        let header = section_header(self.label.clone(), cx);

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
        guides,
        is_expandable,
        is_expanded,
        is_active,
        capacity,
        icon,
        favorited,
    } = spec;
    let theme = cx.theme();
    let row_key: SharedString = format!("tree-row-{}", path.display()).into();
    let caret_key: SharedString = format!("tree-caret-{}", path.display()).into();

    // 8px base + ~14px per depth level — readable indentation
    // without taking too much sidebar width.
    let indent = px(8.0 + 14.0 * depth as f32);

    let drag_path = path.clone();
    let mut row = h_flex()
        .id(ElementId::Name(row_key))
        .relative()
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
        })
        .on_drag(
            gpui::ExternalPaths(smallvec![drag_path]),
            |paths, _offset, _window, cx| cx.new(|_| paths.clone()),
        );
    if is_active {
        row = row.bg(theme.sidebar_accent);
    } else {
        let hover_bg = theme.sidebar_accent.opacity(0.5);
        row = row.hover(move |this| this.bg(hover_bg));
    }

    // Tree folders are drop targets (dnd-spec §3.5): Browse/Volumes
    // rows accept OS file drags into the folder they represent. The
    // transfer itself runs through Shell::handle_external_drop —
    // nothing filesystem-y happens here.
    let drop_shell = shell.clone();
    let drop_dest = path.clone();
    row = row
        .drag_over::<ExternalPaths>(|style, _, _, cx| {
            style.bg(cx.theme().accent.opacity(0.12))
        })
        .on_drop(move |paths: &ExternalPaths, window, cx| {
            let Some(shell) = drop_shell.upgrade() else { return };
            let dropped = paths.paths().to_vec();
            let dest = drop_dest.clone();
            shell.update(cx, |this, cx| {
                this.handle_external_drop(dropped, dest, window, cx);
            });
        });

    // Ancestry connector lines, absolutely positioned inside the
    // row's left indent so they span the full row height (through
    // the `py_1` padding) and join seamlessly with the rows above
    // and below. One 14px column per depth level mirrors the indent
    // math; each column centres its 1px line at x = 7. The corner /
    // tee elbow is a 16px-tall box (half the 32px row) whose left +
    // bottom borders draw the `└` shape ending at the caret column.
    let line = theme.sidebar_border;
    let guide_count = guides.len();
    for (level, guide) in guides.iter().enumerate() {
        if matches!(guide, TreeGuide::Blank) {
            continue;
        }
        // The row's own connector (last level) normally hands off to
        // the caret right where its column ends. Leaf rows have no
        // caret, so extend the stub through the empty caret slot to
        // where an arrow tip would end (~12px in) — connector lengths
        // read consistently whether or not a row is expandable.
        let stub_extra = if level + 1 == guide_count && !is_expandable {
            12.0
        } else {
            0.0
        };
        let cell = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(8.0 + 14.0 * level as f32))
            .w(px(14.0 + stub_extra))
            .flex()
            .flex_col();
        let elbow = || {
            div()
                .ml(px(7.0))
                .w(px(7.0 + stub_extra))
                .h(px(16.0))
                .border_l_1()
                .border_b_1()
                .border_color(line)
        };
        let cell = match guide {
            TreeGuide::Blank => cell,
            TreeGuide::Vertical => {
                cell.child(div().ml(px(7.0)).w(px(1.0)).h_full().bg(line))
            }
            TreeGuide::Tee => cell
                .child(elbow())
                .child(div().ml(px(7.0)).w(px(1.0)).flex_1().bg(line)),
            TreeGuide::Corner => cell.child(elbow()),
        };
        row = row.child(cell);
    }

    // Caret slot. Reserves the same width for non-expandable rows so
    // labels align across rows that don't have a caret — leaf
    // folders without subdirectories. `▼` / `▶` render larger than
    // the small `▾`/`▸` glyphs at our font size.
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

    // Real icon between the caret and label. Folder descendants use
    // NSWorkspace so custom folder artwork survives; volume roots use
    // our vector drive glyph so the Volumes section reads distinctly
    // even when AppKit returns a generic folder bitmap.
    let icon_color = if is_active {
        theme.sidebar_accent_foreground
    } else {
        theme.sidebar_foreground
    };
    let icon_el = match icon {
        TreeRowIcon::Folder => {
            let icon = icons.borrow_mut().folder_icon_for(&path);
            img(icon)
                .w(px(SIDEBAR_ICON_PX))
                .h(px(SIDEBAR_ICON_PX))
                .into_any_element()
        }
        TreeRowIcon::Volume => svg()
            .path("icons/nav/drive.svg")
            .w(px(SIDEBAR_ICON_PX))
            .h(px(SIDEBAR_ICON_PX))
            .text_color(icon_color)
            .into_any_element(),
    };
    row = row.child(
        div()
            .flex_shrink_0()
            .w(px(SIDEBAR_ICON_PX))
            .h(px(SIDEBAR_ICON_PX))
            .child(icon_el),
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
        .when(favorited, |this| {
            // §5 favorited indicator: trailing accent star. Same
            // glyph used in the file list and breadcrumb so the
            // visual language is consistent across surfaces.
            this.child(
                svg()
                    .path("icons/nav/star.svg")
                    .w(px(12.0))
                    .h(px(12.0))
                    .text_color(cx.theme().primary)
                    .flex_shrink_0(),
            )
        })
        .on_click({
            let path = path.clone();
            move |event, window, cx| {
                if let Some(shell) = shell_for_label.upgrade() {
                    let modifiers = event.modifiers();
                    let path = path.clone();
                    shell.update(cx, |s, cx| {
                        if modifiers.platform {
                            s.open_path_in_new_tab(path, window, cx);
                        } else {
                            s.navigate_node(label_node, cx);
                        }
                    });
                }
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
    let favorite_label = if favorited {
        "Remove from Favorites"
    } else {
        "Add to Favorites"
    };
    let attach_menu = move |el: gpui::Stateful<gpui::Div>| {
        el.context_menu(move |menu, _window, cx| {
            use crate::shell::{
                CopyContextPath, NewFolderHere, OpenContextInNewTab, OpenTerminalAtContext,
                RevealContextPath, ToggleFavoriteForTarget,
            };
            if let Some(shell) = shell_for_menu.upgrade() {
                shell.update(cx, |s, _| {
                    s.context_target = Some(path_for_menu.clone());
                    // Toggle action reads from `favorites_context_path`,
                    // distinct from `context_target` which the Reveal /
                    // Copy / OpenInNewTab handlers consume.
                    s.favorites_context_path = Some(path_for_menu.clone());
                });
            }
            menu.menu("Open in New Tab", Box::new(OpenContextInNewTab))
                .separator()
                .menu(feraille_core::commands::REVEAL_LABEL, Box::new(RevealContextPath))
                .menu("Copy Path", Box::new(CopyContextPath))
                .menu("Open Terminal Here", Box::new(OpenTerminalAtContext))
                .separator()
                .menu(favorite_label, Box::new(ToggleFavoriteForTarget))
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
