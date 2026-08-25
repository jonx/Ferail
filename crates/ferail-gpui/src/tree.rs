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

use crate::text::{IconScale as _, TextScale as _};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

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

/// Horizontal gutter (px) between the tree rows and the sidebar edges,
/// so a selected row's rounded highlight reads as an inset pill rather
/// than a full-bleed bar.
pub(crate) const TREE_ROW_INSET: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeRowIcon {
    Folder,
    Volume,
    /// A network mount (`is_local == false`). Drawn with a distinct
    /// network glyph so remote shares read differently from local disks.
    Network,
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
    pub node_id: ferail_core::NodeId,
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
    /// `true` for removable/external volume rows that can be unmounted.
    /// Gates the "Eject" entry in the row's context menu. Always `false`
    /// for folders and the boot volume.
    pub ejectable: bool,
}

/// Cached representation of one direct child of an expanded folder.
/// Files are intentionally not included — the tree shows hierarchy;
/// the main pane shows files.
#[derive(Clone, Debug)]
pub struct TreeChild {
    pub node_id: ferail_core::NodeId,
    pub path: PathBuf,
    pub label: String,
    /// Platform hidden semantics resolved at load time by
    /// `ferail_fs_native::entry_is_hidden` (UF_HIDDEN on macOS,
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
        .text_scale_xs()
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
    Locations(crate::locations_section::LocationsSection),
    PlatformLocations(crate::locations_section::PlatformLocationsSection),
    Favorites(crate::favorites_section::FavoritesSection),
    Recents(crate::recents_section::RecentsSection),
    Tree(TreeSection),
}

impl ShellSidebarItem {
    pub fn group(g: LabeledMenu) -> Self {
        ShellSidebarItem::Group(g)
    }
    pub fn locations(l: crate::locations_section::LocationsSection) -> Self {
        ShellSidebarItem::Locations(l)
    }
    pub fn platform_locations(l: crate::locations_section::PlatformLocationsSection) -> Self {
        ShellSidebarItem::PlatformLocations(l)
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
            ShellSidebarItem::Locations(l) => l.is_collapsed(),
            ShellSidebarItem::PlatformLocations(l) => l.is_collapsed(),
            ShellSidebarItem::Favorites(f) => f.is_collapsed(),
            ShellSidebarItem::Recents(r) => r.is_collapsed(),
            ShellSidebarItem::Tree(t) => t.is_collapsed(),
        }
    }
    fn collapsed(self, c: bool) -> Self {
        match self {
            ShellSidebarItem::Group(g) => ShellSidebarItem::Group(g.collapsed(c)),
            ShellSidebarItem::Locations(l) => ShellSidebarItem::Locations(l.collapsed(c)),
            ShellSidebarItem::PlatformLocations(l) => {
                ShellSidebarItem::PlatformLocations(l.collapsed(c))
            }
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
            ShellSidebarItem::Locations(l) => l.render(id, window, cx).into_any_element(),
            ShellSidebarItem::PlatformLocations(l) => l.render(id, window, cx).into_any_element(),
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
            // Inset the tree rows with a small horizontal gutter so a
            // selected row's rounded highlight floats clear of the
            // sidebar edges (Finder-style) instead of running edge to
            // edge. The rows stay `w_full` inside the padded box, so no
            // row overflows and the connector guides shift with them.
            .child(v_flex().w_full().px(px(TREE_ROW_INSET)).children(rows))
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
    let _path_guard = ferail_core::path_guard::enter_render();
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
        ejectable,
    } = spec;
    let theme = cx.theme();
    let row_key: SharedString = format!("tree-row-{}", path.display()).into();
    let caret_key: SharedString = format!("tree-caret-{}", path.display()).into();

    // 8px base + ~14px per depth level — readable indentation
    // without taking too much sidebar width.
    let indent = px(8.0 + 14.0 * depth as f32);

    let drag_path = path.clone();
    let drag_label: SharedString = drag_path
        .file_name()
        // Display leaf on the drag chip (macOS `:` → `/`).
        .map(|n| ferail_fs_native::paths::display_leaf(n.to_string_lossy().as_ref()).into_owned())
        .unwrap_or_else(|| drag_path.display().to_string())
        .into();
    let native_owned = Arc::new(AtomicBool::new(false));
    let native_owned_for_badge = native_owned.clone();
    let native_owned_for_payload = native_owned.clone();
    let mut row = h_flex()
        .id(ElementId::Name(row_key))
        .relative()
        .w_full()
        .pl(indent)
        .pr_2()
        .py_1()
        .gap_1()
        .items_center()
        .text_scale_sm()
        .rounded(theme.radius)
        .cursor_pointer()
        .text_color(if is_active {
            theme.sidebar_accent_foreground
        } else {
            theme.sidebar_foreground
        })
        .on_drag(
            gpui::ExternalPaths(smallvec![drag_path]),
            move |_paths, offset, _window, cx| {
                native_owned_for_badge.store(false, Ordering::Release);
                cx.new(|_| crate::file_list::DragBadge {
                    names: smallvec![drag_label.clone()],
                    icons: smallvec![],
                    count: 1,
                    offset,
                    native_owned: native_owned_for_badge.clone(),
                })
            },
        )
        // Tree rows are always directories; promote the drag to a
        // native session when the pointer leaves the window.
        .external_drag_payload::<gpui::ExternalPaths>(move |paths, _window, _cx| {
            native_owned_for_payload.store(true, Ordering::Release);
            Some(gpui::ExternalDragPayload::Files(gpui::FileDragPaths::new(
                paths.paths().iter().cloned().map(|p| (p, true)),
            )))
        });
    if is_active {
        row = row.bg(theme.sidebar_accent);
    }
    // One `.hover()` per element is all gpui allows (a second is a debug
    // assertion), so the ordinary hover wash and the drop-target ring share
    // it. The ring is what makes a sidebar folder readable as a drop target
    // during a *native* promise session: gpui owns no typed drag then, so
    // `drag_over` below never fires and the row's `on_mouse_move` repaint is
    // what keeps this tracking the row under the pointer.
    {
        let hover_bg = theme.sidebar_accent.opacity(0.5);
        let drop_border = theme.accent;
        let native_dragging = crate::file_list::native_archive_drag_active();
        row = row.hover(move |this| {
            let this = if is_active { this } else { this.bg(hover_bg) };
            if native_dragging {
                this.border_1()
                    .border_color(drop_border)
                    .bg(drop_border.opacity(0.12))
            } else {
                this
            }
        });
    }

    // Tree folders are drop targets (dnd-spec §3.5): Browse/Volumes
    // rows accept OS file drags into the folder they represent. The
    // transfer itself runs through Shell::handle_external_drop —
    // nothing filesystem-y happens here.
    let drop_shell = shell.clone();
    let drop_dest = path.clone();
    let archive_drop_shell = shell.clone();
    let archive_drop_dest = path.clone();
    let native_drop_shell = shell.clone();
    let native_drop_dest = path.clone();
    row = row
        .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(cx.theme().accent.opacity(0.12)))
        .on_drop(move |paths: &ExternalPaths, window, cx| {
            let Some(shell) = drop_shell.upgrade() else {
                return;
            };
            let dropped = paths.paths().to_vec();
            let dest = drop_dest.clone();
            shell.update(cx, |this, cx| {
                this.handle_external_drop(dropped, dest, window, cx);
            });
        })
        .drag_over::<crate::file_list::ArchiveEntryDrag>(|style, _, _, cx| {
            style
                .border_1()
                .border_color(cx.theme().accent)
                .bg(cx.theme().accent.opacity(0.12))
        })
        .on_drop(
            move |drag: &crate::file_list::ArchiveEntryDrag, window, cx| {
                cx.stop_propagation();
                let Some(shell) = archive_drop_shell.upgrade() else {
                    return;
                };
                let dest = archive_drop_dest.clone();
                let archive = drag.archive.clone();
                let entries = drag.entries.clone();
                let password = drag.password.clone();
                shell.update(cx, |this, cx| {
                    this.extract_archive_entries_into(archive, entries, dest, password, window, cx);
                });
            },
        )
        .on_mouse_move(|_event, window, _cx| {
            if crate::file_list::native_archive_drag_active() {
                window.refresh();
            }
        })
        .on_mouse_up(gpui::MouseButton::Left, move |_event, window, cx| {
            if cx.has_active_drag() {
                return;
            }
            let Some(drag) = crate::file_list::take_native_archive_drag() else {
                return;
            };
            cx.stop_propagation();
            let Some(shell) = native_drop_shell.upgrade() else {
                return;
            };
            let dest = native_drop_dest.clone();
            shell.update(cx, |this, cx| {
                this.extract_archive_entries_into(
                    drag.archive,
                    drag.entries,
                    dest,
                    drag.password,
                    window,
                    cx,
                );
            });
        });
    // Spring-load: dwelling a drag over a collapsed expandable folder
    // opens it so the user can drill the tree without releasing.
    if is_expandable && !is_expanded {
        let hover_shell = shell.clone();
        let hover_path = path.clone();
        let archive_hover_shell = shell.clone();
        let archive_hover_path = path.clone();
        row = row
            .on_drag_move(move |e: &gpui::DragMoveEvent<ExternalPaths>, _window, cx| {
                if !e.bounds.contains(&e.event.position) {
                    return;
                }
                let Some(shell) = hover_shell.upgrade() else {
                    return;
                };
                let path = hover_path.clone();
                shell.update(cx, |this, cx| this.tree_drag_hover(&path, cx));
            })
            .on_drag_move(
                move |e: &gpui::DragMoveEvent<crate::file_list::ArchiveEntryDrag>, _window, cx| {
                    if !e.bounds.contains(&e.event.position) {
                        return;
                    }
                    let Some(shell) = archive_hover_shell.upgrade() else {
                        return;
                    };
                    let path = archive_hover_path.clone();
                    shell.update(cx, |this, cx| this.tree_drag_hover(&path, cx));
                },
            );
    }

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
            TreeGuide::Vertical => cell.child(div().ml(px(7.0)).w(px(1.0)).h_full().bg(line)),
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
            .child(
                gpui::svg()
                    .path(if is_expanded {
                        "icons/nav/disclosure-down.svg"
                    } else {
                        "icons/nav/disclosure-right.svg"
                    })
                    .icon_px(9.0)
                    .text_color(theme.muted_foreground),
            )
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
            img(icon).icon_px(SIDEBAR_ICON_PX).into_any_element()
        }
        TreeRowIcon::Volume => svg()
            .path("icons/nav/drive.svg")
            .icon_px(SIDEBAR_ICON_PX)
            .text_color(icon_color)
            .into_any_element(),
        TreeRowIcon::Network => svg()
            .path("icons/network.svg")
            .icon_px(SIDEBAR_ICON_PX)
            .text_color(icon_color)
            .into_any_element(),
    };
    row = row.child(
        div()
            .flex_shrink_0()
            .icon_px(SIDEBAR_ICON_PX)
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
                    .icon_px(12.0)
                    .text_color(cx.theme().primary)
                    .flex_shrink_0(),
            )
        })
        .when(ejectable, |this| {
            // Trailing eject affordance — Finder draws an ⏏ on every
            // removable/external volume row so a drive can be unmounted
            // without opening the context menu. Stops propagation so the
            // click ejects rather than navigating into the volume. The
            // glyph carries its own `text_color` (a raw `svg()` doesn't
            // inherit the parent's currentColor, unlike `Icon`), so the
            // hover affordance is a background highlight instead.
            let eject_shell = shell.clone();
            let eject_path = path.clone();
            let eject_key: SharedString = format!("tree-eject-{}", path.display()).into();
            let glyph = theme.muted_foreground;
            let hover_bg = theme.sidebar_accent;
            this.child(
                h_flex()
                    .id(ElementId::Name(eject_key))
                    .flex_shrink_0()
                    .icon_px(18.0)
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .child(
                        svg()
                            .path("icons/nav/eject.svg")
                            .icon_px(14.0)
                            .text_color(glyph)
                            .flex_shrink_0(),
                    )
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        if let Some(shell) = eject_shell.upgrade() {
                            let path = eject_path.clone();
                            shell.update(cx, |s, cx| s.eject_path(path, window, cx));
                        }
                    }),
            )
        })
        .on_click({
            let path = path.clone();
            move |event, window, cx| {
                if let Some(shell) = shell_for_label.upgrade() {
                    let modifiers = event.modifiers();
                    let double = event.click_count() >= 2;
                    let path = path.clone();
                    shell.update(cx, |s, cx| {
                        if modifiers.platform {
                            s.open_path_in_new_tab(path, window, cx);
                        } else if double && is_expandable {
                            // Double-click a folder that has children:
                            // toggle its fold state while keeping it the
                            // selected/active row. The single-click that
                            // opens the double already navigated to it, so
                            // it stays highlighted; here we just fold or
                            // unfold in place (the caret is the other way
                            // to do this).
                            s.toggle_tree_expand_node(label_node, cx);
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
        tr!("Remove from Favorites")
    } else {
        tr!("Add to Favorites")
    };
    let attach_menu = move |el: gpui::Stateful<gpui::Div>| {
        el.context_menu(move |menu, _window, cx| {
            use crate::shell::{
                CopyContextPath, EjectVolume, GetInfoAtContext, NewFolderHere, OpenContextInNewTab,
                OpenTerminalAtContext, RevealContextPath, ShowLockHoldersAtContext,
                ToggleFavoriteForTarget,
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
            let menu = menu
                .menu(tr!("Open in New Tab"), Box::new(OpenContextInNewTab))
                .separator()
                .menu(tr!("Get Info"), Box::new(GetInfoAtContext))
                .separator()
                .menu(
                    crate::i18n::tr_static(ferail_core::commands::REVEAL_LABEL),
                    Box::new(RevealContextPath),
                )
                .menu(tr!("Copy Path"), Box::new(CopyContextPath))
                .menu(tr!("Open Terminal Here"), Box::new(OpenTerminalAtContext))
                .separator()
                .menu(favorite_label.clone(), Box::new(ToggleFavoriteForTarget))
                .separator()
                .menu(tr!("New Folder Here"), Box::new(NewFolderHere));
            // Eject only for removable/external volumes (the boot
            // volume and folders never get it).
            if ejectable {
                let menu = menu.separator().menu(tr!("Eject"), Box::new(EjectVolume));
                // The pre-emptive "why won't it eject": name the
                // processes with files open on the volume, with
                // force-close buttons. Windows-only lookup today.
                if crate::platform_shell::lock_diagnostics_available() {
                    menu.menu(
                        tr!("What’s Blocking Eject?"),
                        Box::new(ShowLockHoldersAtContext),
                    )
                } else {
                    menu
                }
            } else {
                menu
            }
        })
    };

    // Capacity bar for volume rows. Finder draws this as a thin
    // line under the volume name, with the used portion filled in
    // accent and the rest in muted grey.
    if let Some((total, available)) = capacity {
        if total > 0 {
            let theme = cx.theme();
            let used_fraction =
                ((total.saturating_sub(available)) as f32 / total as f32).clamp(0.0, 1.0);
            // Indent the bar so it sits under the label, skipping
            // caret + icon columns (16 + 4 + 16 + 4 = ~40 DIPs).
            let bar_indent = px(8.0 + 14.0 * depth as f32 + 40.0);
            // Finder draws this bar ~140 DIPs wide. Rather than pin a
            // fixed width that gets clipped when the user drags the
            // sidebar narrow, let the track flex to fill whatever room is
            // left after the indent — capped at 140 so it doesn't sprawl
            // on a wide sidebar, and `min_w_0` so it shrinks instead of
            // overflowing when the panel is tight. The used portion is a
            // `relative` fraction of the track, so it stays correct at any
            // track width.
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
                        .flex_1()
                        .min_w_0()
                        .max_w(px(140.0))
                        .h(px(4.0))
                        .rounded(px(2.0))
                        .bg(track_bg)
                        .child(
                            div()
                                .h_full()
                                .w(relative(used_fraction))
                                .rounded(px(2.0))
                                .bg(fill_bg),
                        ),
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
