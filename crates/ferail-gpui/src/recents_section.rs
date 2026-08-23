//! The sidebar's **Recents** section: recently-visited folders, most-
//! recent first (docs/features — Recents).
//!
//! A live view over the same `folder_usage` visit log that powers the
//! Ant Trail heat tint — the section reads the in-memory
//! `ProcessState::recents` cache (front-inserted on every navigate,
//! hydrated from the DB at startup ordered by last access), so render
//! never touches SQLite. Modeled on [`crate::favorites_section`] but
//! simpler: no drag-reorder, no per-row availability state, no rename.

use crate::text::{IconScale as _, TextScale as _};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, WeakEntity, Window, div, img, px,
};
use gpui_component::{
    ActiveTheme, Collapsible, h_flex, menu::ContextMenuExt as _, sidebar::SidebarItem, v_flex,
};

use crate::icons::IconCache;
use crate::shell::Shell;

/// Master switch for the Recents feature. Default `true`. When off, the
/// sidebar section is hidden and navigation stops pushing folders into
/// [`crate::process_state::ProcessState::recents`]. The Ant Trail is
/// unaffected — the two share the `folder_usage` visit log but each has
/// its own switch ([`crate::ant_trail::AntTrailEnabled`]). Seeded from
/// persisted settings at startup and set live by the settings toggle;
/// reading [`recents_enabled`] during render subscribes the window so
/// flipping it shows/hides the section without a relaunch.
#[derive(Clone, Copy)]
pub struct RecentsEnabled(pub bool);

impl gpui::Global for RecentsEnabled {}

/// Whether the Recents feature is on. Defaults to `true` when the global
/// hasn't been seeded yet.
pub fn recents_enabled(cx: &App) -> bool {
    cx.try_global::<RecentsEnabled>()
        .map(|g| g.0)
        .unwrap_or(true)
}

/// Section payload for the Recents group. Cloned per frame; the
/// `Vec<PathBuf>` is a bounded snapshot (`RECENTS_CAP`).
#[derive(Clone)]
pub struct RecentsSection {
    recents: Vec<PathBuf>,
    /// `true` ⇒ whole sidebar is in icon-only collapse mode.
    icon_only: bool,
    /// Disclosure-triangle state (header visible, rows hidden).
    section_collapsed: bool,
    shell: WeakEntity<Shell>,
    icons: Rc<RefCell<IconCache>>,
}

impl RecentsSection {
    pub fn new(
        recents: Vec<PathBuf>,
        section_collapsed: bool,
        shell: WeakEntity<Shell>,
        icons: Rc<RefCell<IconCache>>,
    ) -> Self {
        Self {
            recents,
            icon_only: false,
            section_collapsed,
            shell,
            icons,
        }
    }
}

impl Collapsible for RecentsSection {
    fn is_collapsed(&self) -> bool {
        self.icon_only
    }
    fn collapsed(mut self, c: bool) -> Self {
        self.icon_only = c;
        self
    }
}

/// Last path component for the row label, or "/" for the filesystem
/// root. Pure display — never resolves the path.
fn row_label(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

impl SidebarItem for RecentsSection {
    fn render(
        self,
        _id: impl Into<ElementId>,
        _window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let section_collapsed = self.section_collapsed;
        let shell_for_header = self.shell.clone();

        // Disclosure triangle + label. Header context menu offers
        // Clear Recents.
        let header = h_flex()
            .id(ElementId::Name("recents-section-header".into()))
            .flex_shrink_0()
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .rounded(theme.radius)
            .h_8()
            .cursor_pointer()
            .text_scale_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.sidebar_foreground.opacity(0.7))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .w(px(12.0))
                    .h(px(12.0))
                    .items_center()
                    .justify_center()
                    .text_color(theme.muted_foreground)
                    .child(
                        gpui::svg()
                            .path(if section_collapsed {
                                "icons/nav/disclosure-right.svg"
                            } else {
                                "icons/nav/disclosure-down.svg"
                            })
                            .icon_px(9.0)
                            .text_color(theme.muted_foreground),
                    ),
            )
            .child(div().flex_1().child(tr!("Recents")))
            .on_click(move |_, _window, cx| {
                if let Some(shell) = shell_for_header.upgrade() {
                    shell.update(cx, |s, cx| s.toggle_recents_section_collapsed(cx));
                }
            })
            .context_menu(move |menu, _window, _cx| {
                menu.menu(
                    tr!("Clear Recents\u{2026}"),
                    Box::new(crate::shell::ClearRecents),
                )
            });

        if section_collapsed || self.icon_only {
            return v_flex().w_full().child(header).into_any_element();
        }

        let active_path = self
            .shell
            .upgrade()
            .map(|s| s.read(cx).active_tab().current_dir.clone());
        let icons = self.icons.clone();
        let rows: Vec<AnyElement> = self
            .recents
            .iter()
            .enumerate()
            .map(|(i, path)| {
                render_recent_row(
                    i,
                    path,
                    &icons,
                    self.shell.clone(),
                    active_path.as_deref(),
                    cx,
                )
            })
            .collect();

        v_flex()
            .w_full()
            .child(header)
            .child(v_flex().w_full().children(rows))
            .into_any_element()
    }
}

/// One Recents row: folder icon + basename. Click navigates (Cmd-click
/// opens a new tab); the context menu offers Reveal / Remove / Clear.
fn render_recent_row(
    index: usize,
    path: &std::path::Path,
    icons: &Rc<RefCell<IconCache>>,
    shell: WeakEntity<Shell>,
    active_path: Option<&std::path::Path>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let label = row_label(path);
    let is_active = active_path == Some(path);
    let row_key: SharedString = format!("recent-row-{index}").into();
    let icon = icons.borrow_mut().folder_icon_for(path);

    let mut row = h_flex()
        .id(ElementId::Name(row_key))
        .w_full()
        .px_2()
        .py_1()
        .gap_2()
        .items_center()
        .text_scale_sm()
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
    row = row
        .child(
            div()
                .flex_shrink_0()
                .icon_px(crate::tree::SIDEBAR_ICON_PX)
                .child(img(icon).icon_px(crate::tree::SIDEBAR_ICON_PX)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(SharedString::from(label)),
        );

    let shell_for_click = shell.clone();
    let shell_for_menu = shell.clone();
    let path_for_click = path.to_path_buf();
    let path_for_menu = path.to_path_buf();
    row = row.on_click(move |event, window, cx| {
        if let Some(s) = shell_for_click.upgrade() {
            let modifiers = event.modifiers();
            let path = path_for_click.clone();
            s.update(cx, |shell, cx| {
                if modifiers.platform {
                    shell.open_path_in_new_tab(path, window, cx);
                } else {
                    shell.navigate(path, cx);
                }
            });
        }
    });
    // `.context_menu(...)` changes the element type, so it's terminal.
    row.context_menu(move |menu, _window, cx| {
        use crate::shell::{ClearRecents, RemoveFromRecents, RevealContextPath};
        if let Some(s) = shell_for_menu.upgrade() {
            s.update(cx, |shell, _| {
                shell.context_target = Some(path_for_menu.clone());
            });
        }
        menu.menu(
            crate::i18n::tr_static(ferail_core::commands::REVEAL_LABEL),
            Box::new(RevealContextPath),
        )
        .separator()
        .menu(tr!("Remove from Recents"), Box::new(RemoveFromRecents))
        .menu(tr!("Clear Recents\u{2026}"), Box::new(ClearRecents))
    })
    .into_any_element()
}
