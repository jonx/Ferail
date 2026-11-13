//! Linux devices reuse mounted-volume rows; unmounted media mounts on activation.

use crate::text::{IconScale, TextScale};
use crate::{icons::IconCache, shell::Shell, sidebar_layout::SidebarSection};
use ferail_shell_linux::volumes::Filesystem;
use gpui::prelude::*;
use gpui::{App, ElementId, SharedString, WeakEntity, div, svg};
use gpui_component::{ActiveTheme, Collapsible, h_flex, sidebar::SidebarItem, v_flex};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
pub struct LinuxVolumesSection {
    pub mounted: Vec<crate::tree::TreeRowSpec>,
    pub unmounted: Vec<Filesystem>,
    pub icons: Rc<RefCell<IconCache>>,
    pub shell: WeakEntity<Shell>,
    pub section_collapsed: bool,
    pub icon_only: bool,
}

impl Collapsible for LinuxVolumesSection {
    fn is_collapsed(&self) -> bool {
        self.icon_only
    }
    fn collapsed(mut self, collapsed: bool) -> Self {
        self.icon_only = collapsed;
        self
    }
}

impl SidebarItem for LinuxVolumesSection {
    fn render(
        self,
        _id: impl Into<ElementId>,
        _window: &mut gpui::Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let header = crate::tree::collapsible_section_header(
            tr!("Volumes"),
            self.section_collapsed,
            self.shell.clone(),
            SidebarSection::Volumes,
            cx,
        );
        let mut section = v_flex()
            .w_full()
            .when(!self.icon_only, |section| section.child(header));
        if self.section_collapsed && !self.icon_only {
            return section.into_any_element();
        }
        for row in self.mounted {
            section = section.child(crate::tree::render_tree_row(
                row,
                self.shell.clone(),
                self.icons.clone(),
                crate::tree::SIDEBAR_ICON_PX,
                self.icon_only,
                cx,
            ));
        }
        for filesystem in self.unmounted {
            let theme = cx.theme();
            let mounting = crate::process_state::process_state(cx)
                .linux_mounting
                .borrow()
                .contains(&filesystem.object_path);
            let shell = self.shell.clone();
            let label = crate::private_mode::present_label(&filesystem.name);
            let tooltip = label.clone();
            let row = h_flex()
                .id(SharedString::from(filesystem.object_path.clone()))
                .w_full()
                .h_9()
                .items_center()
                .rounded(theme.radius)
                .cursor_pointer()
                .text_scale_sm()
                .text_color(theme.sidebar_foreground)
                .when(self.icon_only, |row| {
                    row.justify_center().tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                    })
                })
                .when(!self.icon_only, |row| row.px_2().gap_2())
                .hover(|row| row.bg(theme.sidebar_accent.opacity(0.5)))
                .when(mounting, |row| row.opacity(0.5))
                .child(
                    svg()
                        .path("icons/nav/drive.svg")
                        .icon_px(crate::tree::SIDEBAR_ICON_PX)
                        .text_color(theme.sidebar_foreground),
                )
                .when(!self.icon_only, |row| {
                    row.child(div().flex_1().min_w_0().truncate().child(label))
                })
                .on_click(move |event, window, cx| {
                    if let Some(shell) = shell.upgrade() {
                        shell.update(cx, |shell, cx| {
                            shell.mount_linux_filesystem(
                                filesystem.object_path.clone(),
                                event.modifiers().platform,
                                window,
                                cx,
                            )
                        });
                    }
                });
            section = section.child(row);
        }
        section.into_any_element()
    }
}

impl Shell {
    pub(crate) fn mount_linux_filesystem(
        &mut self,
        object_path: String,
        new_tab: bool,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if !self
            .process
            .linux_mounting
            .borrow_mut()
            .insert(object_path.clone())
        {
            return;
        }
        let process = self.process.clone();
        let tab_id = self.active_tab().id;
        let generation = self.active_tab().load_generation;
        let window = window.window_handle();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let target = object_path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { ferail_shell_linux::volumes::mount(&target) })
                .await;
            process.linux_mounting.borrow_mut().remove(&object_path);
            let _ = window.update(cx, |_, window, cx| {
                let _ = this.update(cx, |shell, cx| {
                    match result {
                        Ok(path)
                            if shell.active_tab().id == tab_id
                                && shell.active_tab().load_generation == generation =>
                        {
                            if new_tab {
                                shell.open_path_in_new_tab(path, window, cx);
                            } else {
                                shell.navigate(path, cx);
                            }
                        }
                        Ok(_) => {}
                        Err(error) => {
                            use crate::overlay::OverlayWindowExt;
                            window.push_notice(
                                crate::overlay::Notification::error(tr!(
                                    "Could not mount volume: {error}",
                                    error = error
                                )),
                                cx,
                            );
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }
}
