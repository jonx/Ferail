//! The sidebar's "Locations" section.
//!
//! Locations used to render through gpui-component's `SidebarMenu`, which
//! offers no drop hooks at all — so Downloads, Desktop, and friends silently
//! rejected every drag while Favorites and the Browse tree accepted them. The
//! rows are drawn here instead, as ordinary elements, so a Location is a drop
//! target with the same accent ring every other target uses:
//!
//! * files dragged from the file list / Finder transfer into the folder,
//! * archive members extract into it (typed drag *and* the cross-window
//!   native promise session, which carries no GPUI payload — see
//!   `docs/GPUI-UPSTREAM.md` #11).
//!
//! Visuals stay what `SidebarMenu` gave: the per-location glyph, the iCloud
//! badge for Desktop/Documents, and the §5 favorited star.

use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{AnyElement, App, ElementId, SharedString, WeakEntity, div, px};
use gpui_component::menu::ContextMenuExt as _;
use gpui_component::sidebar::SidebarItem;
use gpui_component::{ActiveTheme, Collapsible, h_flex, v_flex};

use crate::shell::Shell;
use crate::text::{IconScale, TextScale};
use ferail_core::platform_locations::{PathBackedRootState, PlatformRootId};
use ferail_fs_native::CloudState;

/// One Locations row, resolved by the shell at build time. Everything here is
/// cached state — rendering a row never touches the filesystem.
#[derive(Clone)]
pub struct LocationRow {
    pub node_id: ferail_core::NodeId,
    pub path: PathBuf,
    pub label: SharedString,
    /// Bundled SVG for this location (`icons/nav/…`).
    pub icon: &'static str,
    /// This location is the active tab's current folder.
    pub is_active: bool,
    /// §5 favorited indicator: the path is also a user Favorite.
    pub favorited: bool,
    /// `Some` only for iCloud-backed Desktop/Documents; drives the
    /// solid-vs-outline cloud badge. Resolved off-thread at startup.
    pub cloud: Option<CloudState>,
}

#[derive(Clone)]
pub struct LocationsSection {
    label: SharedString,
    rows: Vec<LocationRow>,
    shell: WeakEntity<Shell>,
    ui_scale: f32,
    collapsed: bool,
    section_collapsed: bool,
}

impl LocationsSection {
    pub fn new(
        label: impl Into<SharedString>,
        rows: Vec<LocationRow>,
        shell: WeakEntity<Shell>,
        ui_scale: f32,
        section_collapsed: bool,
    ) -> Self {
        Self {
            label: label.into(),
            rows,
            shell,
            ui_scale,
            collapsed: false,
            section_collapsed,
        }
    }
}

/// One dynamic path-backed platform root (WIN-017: a WSL distribution).
/// The state is a cached process-level snapshot; rendering never probes the
/// registry, launches a process or touches the UNC target.
#[derive(Clone)]
pub struct PlatformLocationRow {
    pub id: PlatformRootId,
    pub label: SharedString,
    pub state: PathBackedRootState,
    pub version: Option<u32>,
    pub is_default: bool,
    pub is_active: bool,
}

#[derive(Clone)]
pub struct PlatformLocationsSection {
    label: SharedString,
    rows: Vec<PlatformLocationRow>,
    shell: WeakEntity<Shell>,
    ui_scale: f32,
    collapsed: bool,
    section_collapsed: bool,
}

impl PlatformLocationsSection {
    pub fn new(
        label: impl Into<SharedString>,
        rows: Vec<PlatformLocationRow>,
        shell: WeakEntity<Shell>,
        ui_scale: f32,
        section_collapsed: bool,
    ) -> Self {
        Self {
            label: label.into(),
            rows,
            shell,
            ui_scale,
            collapsed: false,
            section_collapsed,
        }
    }
}

impl Collapsible for LocationsSection {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }
    fn collapsed(mut self, c: bool) -> Self {
        self.collapsed = c;
        self
    }
}

impl Collapsible for PlatformLocationsSection {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

impl SidebarItem for LocationsSection {
    fn render(
        self,
        _id: impl Into<ElementId>,
        _window: &mut gpui::Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let collapsed = self.collapsed;
        let shell = self.shell.clone();
        let ui_scale = self.ui_scale;
        let rows = self
            .rows
            .into_iter()
            .enumerate()
            .map(|(ix, row)| render_location_row(ix, row, shell.clone(), ui_scale, collapsed, cx))
            .collect::<Vec<AnyElement>>();

        v_flex()
            .w_full()
            // The header disappears in icon-collapse mode, matching how the
            // menu-backed section behaved.
            .when(!collapsed, |this| {
                this.child(crate::tree::collapsible_section_header(
                    self.label.clone(),
                    self.section_collapsed,
                    self.shell.clone(),
                    crate::sidebar_layout::SidebarSection::Locations,
                    cx,
                ))
            })
            .when(!self.section_collapsed || collapsed, |this| {
                this.child(
                    v_flex()
                        .w_full()
                        // Sidebar itself already leaves 8 DIPs around its
                        // 48-DIP icon strip. A second inset made only 20 DIPs
                        // available for our 24-DIP glyph and clipped it.
                        .when(!collapsed, |this| this.px(px(crate::tree::TREE_ROW_INSET)))
                        .children(rows),
                )
            })
    }
}

impl SidebarItem for PlatformLocationsSection {
    fn render(
        self,
        _id: impl Into<ElementId>,
        _window: &mut gpui::Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let collapsed = self.collapsed;
        let shell = self.shell.clone();
        let rows = self
            .rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                render_platform_location_row(
                    index,
                    row,
                    shell.clone(),
                    self.ui_scale,
                    collapsed,
                    cx,
                )
            })
            .collect::<Vec<AnyElement>>();
        v_flex()
            .w_full()
            .when(!collapsed, |this| {
                this.child(crate::tree::collapsible_section_header(
                    self.label.clone(),
                    self.section_collapsed,
                    self.shell.clone(),
                    crate::sidebar_layout::SidebarSection::Linux,
                    cx,
                ))
            })
            .when(!self.section_collapsed || collapsed, |this| {
                this.child(
                    v_flex()
                        .w_full()
                        .when(!collapsed, |this| this.px(px(crate::tree::TREE_ROW_INSET)))
                        .children(rows),
                )
            })
    }
}

#[cfg(windows)]
#[derive(Clone)]
pub struct WindowsNamespaceSection {
    shell: WeakEntity<Shell>,
    ui_scale: f32,
    collapsed: bool,
    section_collapsed: bool,
}

#[cfg(windows)]
impl WindowsNamespaceSection {
    pub fn new(shell: WeakEntity<Shell>, ui_scale: f32, section_collapsed: bool) -> Self {
        Self {
            shell,
            ui_scale,
            collapsed: false,
            section_collapsed,
        }
    }
}

#[cfg(windows)]
impl Collapsible for WindowsNamespaceSection {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

#[cfg(windows)]
impl SidebarItem for WindowsNamespaceSection {
    fn render(
        self,
        _id: impl Into<ElementId>,
        _window: &mut gpui::Window,
        cx: &mut App,
    ) -> impl IntoElement {
        use crate::platform_shell::WindowsNamespaceRoot;
        let rows = [
            (
                tr!("This PC"),
                "icons/nav/drive.svg",
                WindowsNamespaceRoot::ThisPc,
            ),
            (
                tr!("Recycle Bin"),
                "icons/trash.svg",
                WindowsNamespaceRoot::RecycleBin,
            ),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (label, icon, root))| {
            render_windows_namespace_row(
                index,
                label,
                icon,
                root,
                self.shell.clone(),
                self.ui_scale,
                self.collapsed,
                cx,
            )
        })
        .collect::<Vec<_>>();
        v_flex()
            .w_full()
            .when(!self.collapsed, |this| {
                this.child(crate::tree::collapsible_section_header(
                    tr!("Windows"),
                    self.section_collapsed,
                    self.shell.clone(),
                    crate::sidebar_layout::SidebarSection::Windows,
                    cx,
                ))
            })
            .when(!self.section_collapsed || self.collapsed, |this| {
                this.child(
                    v_flex()
                        .w_full()
                        .when(!self.collapsed, |this| {
                            this.px(px(crate::tree::TREE_ROW_INSET))
                        })
                        .children(rows),
                )
            })
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn render_windows_namespace_row(
    index: usize,
    label: SharedString,
    icon: &'static str,
    root: crate::platform_shell::WindowsNamespaceRoot,
    shell: WeakEntity<Shell>,
    icon_px: f32,
    collapsed: bool,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let label: SharedString = crate::private_mode::present_label(label.as_ref()).into();
    let tooltip = label.clone();
    let collapsed_tooltip = label.clone();
    h_flex()
        .id(ElementId::Name(
            format!("windows-namespace-row-{index}").into(),
        ))
        .w_full()
        .h_9()
        .when(collapsed, |this| this.justify_center())
        .when(!collapsed, |this| this.px_2().gap_2())
        .items_center()
        .rounded(theme.radius)
        .cursor_pointer()
        .text_scale_sm()
        .text_color(theme.sidebar_foreground)
        .child(
            gpui::svg()
                .path(icon)
                .icon_px(icon_px)
                .text_color(theme.sidebar_foreground)
                .flex_shrink_0(),
        )
        .when(collapsed, |this| {
            this.tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(collapsed_tooltip.clone()).build(window, cx)
            })
        })
        .when(!collapsed, |this| {
            this.child(
                div()
                    .id(("windows-namespace-label", index))
                    .min_w_0()
                    .truncate()
                    .child(label)
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                    }),
            )
        })
        .hover(|this| this.bg(theme.sidebar_accent.opacity(0.5)))
        .on_click(move |event, window, cx| {
            let Some(shell) = shell.upgrade() else { return };
            shell.update(cx, |shell, cx| {
                shell.open_windows_namespace(root, event.modifiers().platform, window, cx);
            });
        })
        .into_any_element()
}

fn render_platform_location_row(
    index: usize,
    row: PlatformLocationRow,
    shell: WeakEntity<Shell>,
    icon_px: f32,
    collapsed: bool,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let PlatformLocationRow {
        id,
        label,
        state,
        version,
        is_default,
        is_active,
    } = row;
    let label: SharedString = crate::private_mode::present_label(label.as_ref()).into();
    let status = match state {
        PathBackedRootState::Stopped => Some(tr!("Stopped")),
        PathBackedRootState::Starting => Some(tr!("Starting…")),
        PathBackedRootState::Unavailable(_) => Some(tr!("Unavailable")),
        PathBackedRootState::Ready(_) => {
            version.map(|version| SharedString::from(format!("WSL {version}")))
        }
    };
    let shell_for_click = shell.clone();
    let label_tooltip = label.clone();
    let collapsed_tooltip = label.clone();

    let mut element = h_flex()
        .id(ElementId::Name(
            format!("platform-location-row-{index}").into(),
        ))
        .w_full()
        .h_9()
        .when(collapsed, |this| this.justify_center())
        .when(!collapsed, |this| this.px_2().gap_2())
        .items_center()
        .flex_shrink_0()
        .rounded(theme.radius)
        .cursor_pointer()
        .text_scale_sm()
        .text_color(if is_active {
            theme.sidebar_accent_foreground
        } else {
            theme.sidebar_foreground
        })
        .when(is_active, |this| this.bg(theme.sidebar_accent))
        .child(
            gpui::svg()
                .path("icons/nav/drive.svg")
                .icon_px(icon_px)
                .text_color(if is_active {
                    theme.sidebar_accent_foreground
                } else {
                    theme.sidebar_foreground
                })
                .flex_shrink_0(),
        )
        .when(collapsed, |this| {
            this.tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(collapsed_tooltip.clone()).build(window, cx)
            })
        });
    if !collapsed {
        element = element
            .child(
                div()
                    .id(ElementId::Name(
                        format!("platform-location-label-{index}").into(),
                    ))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(label)
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(label_tooltip.clone())
                            .build(window, cx)
                    }),
            )
            .when_some(status, |this, status| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.sidebar_foreground.opacity(0.7))
                        .child(status),
                )
            })
            .when(is_default, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.primary)
                        .child(tr!("Default")),
                )
            });
    }
    let hover_bg = theme.sidebar_accent.opacity(0.5);
    element
        .hover(move |this| if is_active { this } else { this.bg(hover_bg) })
        .on_click(move |event, window, cx| {
            let Some(shell) = shell_for_click.upgrade() else {
                return;
            };
            let id = id.clone();
            let open_in_new_tab = event.modifiers().platform;
            shell.update(cx, |shell, cx| {
                shell.open_path_backed_platform_root(id, open_in_new_tab, window, cx);
            });
        })
        .into_any_element()
}

fn render_location_row(
    index: usize,
    row: LocationRow,
    shell: WeakEntity<Shell>,
    icon_px: f32,
    collapsed: bool,
    cx: &App,
) -> AnyElement {
    // Row builders can run during layout/prepaint, outside `Shell::render`'s
    // own guard — re-enter it so an icon cache miss returns the placeholder
    // instead of a synchronous NSWorkspace fetch (parity with tree.rs).
    let _render_guard = ferail_core::path_guard::enter_render();
    let theme = cx.theme();
    let LocationRow {
        node_id,
        path,
        label,
        icon,
        is_active,
        favorited,
        cloud,
    } = row;
    let label: SharedString = crate::private_mode::present_label(label.as_ref()).into();
    let collapsed_tooltip = label.clone();

    let path_for_click = path.clone();
    let path_for_menu = path.clone();
    let path_for_drop = path.clone();
    let path_for_archive_drop = path.clone();
    let path_for_native_drop = path.clone();
    let shell_for_click = shell.clone();
    let shell_for_menu = shell.clone();
    let shell_for_drop = shell.clone();
    let shell_for_archive_drop = shell.clone();
    let shell_for_native_drop = shell.clone();

    let mut element = h_flex()
        .id(ElementId::Name(format!("location-row-{index}").into()))
        .w_full()
        // 36 DIPs: the height gpui-component's SidebarMenuItem gave these
        // rows, kept so replacing the widget changes no sidebar metrics.
        .h_9()
        .when(collapsed, |this| this.justify_center())
        .when(!collapsed, |this| this.px_2().gap_2())
        .items_center()
        .flex_shrink_0()
        .rounded(theme.radius)
        .cursor_pointer()
        .text_scale_sm()
        .text_color(if is_active {
            theme.sidebar_accent_foreground
        } else {
            theme.sidebar_foreground
        })
        .when(is_active, |this| this.bg(theme.sidebar_accent))
        .child(
            gpui::svg()
                .path(icon)
                .icon_px(icon_px)
                .text_color(if is_active {
                    theme.sidebar_accent_foreground
                } else {
                    theme.sidebar_foreground
                })
                .flex_shrink_0(),
        )
        .when(collapsed, |this| {
            this.tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(collapsed_tooltip.clone()).build(window, cx)
            })
        });

    // Icon-collapse mode shows the glyph alone; the label and badges would
    // not fit the 48-DIP strip.
    if !collapsed {
        element = element
            .child(div().flex_1().min_w_0().truncate().child(label.clone()))
            .when_some(cloud, |this, state| {
                // Solid `cloud-fill` = downloaded; outline `cloud` = set up
                // for iCloud but not downloaded (Finder's distinction).
                let glyph = match state {
                    CloudState::Downloaded => "icons/nav/cloud-fill.svg",
                    CloudState::Placeholder => "icons/nav/cloud.svg",
                };
                this.child(
                    gpui::svg()
                        .path(glyph)
                        .icon_px(14.0)
                        .text_color(theme.sidebar_foreground)
                        .flex_shrink_0(),
                )
            })
            .when(favorited, |this| {
                this.child(
                    gpui::svg()
                        .path("icons/nav/star.svg")
                        .icon_px(11.0)
                        .text_color(theme.primary)
                        .flex_shrink_0(),
                )
            });
    }

    // One `.hover()` per element is all gpui allows (a second is a debug
    // assertion), so the ordinary hover wash and the drop ring share it. The
    // ring is the only feedback a Location can give during a native promise
    // session, where GPUI holds no typed drag and `drag_over` never fires.
    let hover_bg = theme.sidebar_accent.opacity(0.5);
    let drop_border = theme.accent;
    let native_dragging = crate::file_list::native_archive_drag_active();
    element = element.hover(move |this| {
        let this = if is_active { this } else { this.bg(hover_bg) };
        if native_dragging {
            this.border_1()
                .border_color(drop_border)
                .bg(drop_border.opacity(0.12))
        } else {
            this
        }
    });

    element = element
        .on_click(move |event, window, cx| {
            let Some(s) = shell_for_click.upgrade() else {
                return;
            };
            let platform = event.modifiers().platform;
            let path = path_for_click.clone();
            s.update(cx, |shell, cx| {
                if platform {
                    shell.open_path_in_new_tab(path, window, cx);
                } else {
                    shell.navigate_node(node_id, cx);
                }
            });
        })
        // Files dropped on a Location transfer into that folder, through the
        // same engine the file list uses.
        .drag_over::<gpui::ExternalPaths>(move |style, _payload, _window, cx| {
            style
                .border_1()
                .border_color(cx.theme().accent)
                .bg(cx.theme().accent.opacity(0.12))
        })
        .on_drop(move |paths: &gpui::ExternalPaths, window, cx| {
            let Some(s) = shell_for_drop.upgrade() else {
                return;
            };
            let dropped = paths.paths().to_vec();
            let dest = path_for_drop.clone();
            s.update(cx, |shell, cx| {
                shell.handle_external_drop(dropped, dest, window, cx);
            });
        })
        // Archive members extract into the Location.
        .drag_over::<crate::file_list::ArchiveEntryDrag>(move |style, _payload, _window, cx| {
            style
                .border_1()
                .border_color(cx.theme().accent)
                .bg(cx.theme().accent.opacity(0.12))
        })
        .on_drop({
            let dest = path_for_archive_drop.clone();
            move |drag: &crate::file_list::ArchiveEntryDrag, window, cx| {
                cx.stop_propagation();
                let Some(s) = shell_for_archive_drop.upgrade() else {
                    return;
                };
                let dest = dest.clone();
                let archive = drag.archive.clone();
                let entries = drag.entries.clone();
                let password = drag.password.clone();
                s.update(cx, |shell, cx| {
                    shell
                        .extract_archive_entries_into(archive, entries, dest, password, window, cx);
                });
            }
        })
        // Cross-window promise sessions arrive as plain mouse events: repaint
        // so the ring above tracks the row under the pointer, and treat the
        // release as the drop.
        .on_mouse_move(|_event, window, _cx| {
            if crate::file_list::native_archive_drag_active() {
                window.refresh();
            }
        })
        .on_mouse_up(gpui::MouseButton::Left, {
            let dest = path_for_native_drop.clone();
            move |_event, window, cx| {
                if cx.has_active_drag() {
                    return;
                }
                let Some(drag) = crate::file_list::take_native_archive_drag() else {
                    return;
                };
                cx.stop_propagation();
                let Some(s) = shell_for_native_drop.upgrade() else {
                    return;
                };
                let dest = dest.clone();
                crate::log_info!(
                    100,
                    "archive-drag: accepted by location -> {}",
                    dest.display()
                );
                s.update(cx, |shell, cx| {
                    shell.extract_archive_entries_into(
                        drag.archive,
                        drag.entries,
                        dest,
                        drag.password,
                        window,
                        cx,
                    );
                });
            }
        });

    // `.context_menu` wraps the element in a new type, so it closes the chain
    // rather than folding back into `element`.
    element
        .context_menu(move |menu, _window, cx| {
            use crate::shell::{CopyContextPath, OpenContextInNewTab, RevealContextPath};
            // Stash the right-clicked path so the path-aware action handlers
            // know which path the user meant.
            if let Some(s) = shell_for_menu.upgrade() {
                s.update(cx, |shell, _| {
                    shell.context_target = Some(path_for_menu.clone());
                });
            }
            menu.menu(tr!("Open in New Tab"), Box::new(OpenContextInNewTab))
                .separator()
                .menu(
                    crate::i18n::tr_static(ferail_core::commands::REVEAL_LABEL),
                    Box::new(RevealContextPath),
                )
                .menu(tr!("Copy Path"), Box::new(CopyContextPath))
        })
        .into_any_element()
}
