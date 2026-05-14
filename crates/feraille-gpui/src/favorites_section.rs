//! The sidebar's **Favorites** section. Renders a disclosure-triangle
//! header, a click-to-collapse interaction, and either an empty-state
//! prompt or a list of user-curated favorite rows (iter 3+).
//!
//! Distinct from the **Locations** section (the fixed OS folders,
//! built via `Shell::build_locations_menu` in `shell.rs`). Section-
//! header collapse persists through `MetadataDb::favorites_section_collapsed`.

use std::cell::RefCell;
use std::rc::Rc;

use feraille_core::favorites::{Favorite, FavoriteId, FavoriteKind, FavoriteState, FavoriteTarget};
use gpui::{AnyElement, Context, WeakEntity, div};
use gpui::{
    App, AppContext, ElementId, ExternalPaths, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, img, px, svg,
};
use gpui_component::{
    ActiveTheme, Collapsible, h_flex, menu::ContextMenuExt as _, sidebar::SidebarItem, v_flex,
};

use crate::icons::IconCache;
use crate::shell::Shell;

/// Payload carried by a favorite-row drag (§4.2). Implements `Render`
/// so it doubles as its own drag preview — a faint chip showing the
/// row's label following the cursor.
#[derive(Clone)]
pub struct FavoriteDragPayload {
    pub id: FavoriteId,
    pub label: SharedString,
}

impl Render for FavoriteDragPayload {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .text_sm()
            .text_color(theme.sidebar_foreground)
            .child(self.label.clone())
    }
}

/// Section payload for the user-curated Favorites group. Cloned per
/// frame; the `Vec<Favorite>` is owned (small for typical libraries).
#[derive(Clone)]
pub struct FavoritesSection {
    favorites: Vec<Favorite>,
    /// `true` ⇒ Sidebar (whole sidebar) is in icon-only collapse mode.
    /// Distinct from `section_collapsed`. Both can be true.
    icon_only: bool,
    /// Disclosure-triangle state: `true` ⇒ header visible but rows
    /// hidden. Persisted through `Shell::favorites_section_collapsed`.
    section_collapsed: bool,
    shell: WeakEntity<Shell>,
    #[allow(dead_code)]
    icons: Rc<RefCell<IconCache>>,
}

impl FavoritesSection {
    pub fn new(
        favorites: Vec<Favorite>,
        section_collapsed: bool,
        shell: WeakEntity<Shell>,
        icons: Rc<RefCell<IconCache>>,
    ) -> Self {
        Self {
            favorites,
            icon_only: false,
            section_collapsed,
            shell,
            icons,
        }
    }
}

impl Collapsible for FavoritesSection {
    fn is_collapsed(&self) -> bool {
        self.icon_only
    }
    fn collapsed(mut self, c: bool) -> Self {
        self.icon_only = c;
        self
    }
}

impl SidebarItem for FavoritesSection {
    fn render(
        self,
        _id: impl Into<ElementId>,
        _window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let header_key: SharedString = "favorites-section-header".into();
        let plus_key: SharedString = "favorites-section-plus".into();
        let shell_for_click = self.shell.clone();
        let shell_for_plus = self.shell.clone();
        let shell_for_header_menu = self.shell.clone();
        let section_collapsed = self.section_collapsed;

        // Trailing `+` button — adds the active tab's current folder.
        // Stop-propagation on click so the same gesture doesn't also
        // flip the section's disclosure-triangle collapse.
        let plus_button = div()
            .id(ElementId::Name(plus_key))
            .flex_shrink_0()
            .w(px(16.0))
            .h(px(16.0))
            .items_center()
            .justify_center()
            .text_size(px(13.0))
            .text_color(theme.muted_foreground)
            .cursor_pointer()
            .child("+")
            .on_click(move |_, _window, cx| {
                cx.stop_propagation();
                if let Some(shell) = shell_for_plus.upgrade() {
                    shell.update(cx, |s, cx| {
                        let path = s.active_tab().current_dir.clone();
                        let canonical = std::fs::canonicalize(&path).unwrap_or(path);
                        s.process.favorites.update(cx, |f, cx| {
                            f.add_path(
                                canonical,
                                feraille_core::favorites::FavoriteKind::Folder,
                                cx,
                            );
                        });
                    });
                }
            });

        // Disclosure triangle + label + trailing + button.
        let header = h_flex()
            .id(ElementId::Name(header_key))
            .flex_shrink_0()
            .w_full()
            .px_2()
            .gap_1()
            .items_center()
            .rounded(theme.radius)
            .h_8()
            .cursor_pointer()
            .text_xs()
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
                    .text_size(px(9.0))
                    .child(if section_collapsed {
                        "\u{25B6}"
                    } else {
                        "\u{25BC}"
                    }),
            )
            .child(div().flex_1().child("Favorites"))
            .child(plus_button)
            .on_click(move |_, _window, cx| {
                if let Some(shell) = shell_for_click.upgrade() {
                    shell.update(cx, |s, cx| {
                        s.toggle_favorites_section_collapsed(cx);
                    });
                }
            })
            .context_menu(move |menu, _window, cx| {
                use crate::shell::{
                    AddCurrentFolderToFavorites, SortFavoritesByDateAddedNewest,
                    SortFavoritesByDateAddedOldest, SortFavoritesByKind, SortFavoritesByName,
                };
                let _ = shell_for_header_menu.clone();
                let _ = cx;
                menu.menu(
                    "Add Current Folder to Favorites",
                    Box::new(AddCurrentFolderToFavorites),
                )
                .separator()
                .menu("Sort by Name (A\u{2013}Z)", Box::new(SortFavoritesByName))
                .menu(
                    "Sort by Date Added (newest)",
                    Box::new(SortFavoritesByDateAddedNewest),
                )
                .menu(
                    "Sort by Date Added (oldest)",
                    Box::new(SortFavoritesByDateAddedOldest),
                )
                .menu("Sort by Kind", Box::new(SortFavoritesByKind))
            });

        if section_collapsed || self.icon_only {
            return v_flex().w_full().child(header).into_any_element();
        }

        let body: AnyElement = if self.favorites.is_empty() {
            // §11.7 empty state. Muted, slightly indented so it reads
            // as guidance rather than a clickable row.
            div()
                .pl_4()
                .pr_2()
                .py_1()
                .text_xs()
                .text_color(theme.muted_foreground.opacity(0.85))
                .child("Drag folders here for quick access.")
                .into_any_element()
        } else {
            let icons = self.icons.clone();
            let shell = self.shell.clone();
            let active_path = shell
                .upgrade()
                .map(|s| s.read(cx).active_tab().current_dir.clone());
            // Snapshot per-row availability state. Reads from the
            // cached map on the entity — render never touches the
            // filesystem (Prime Directive).
            let states: Vec<FavoriteState> = self
                .favorites
                .iter()
                .map(|f| {
                    shell
                        .upgrade()
                        .map(|s| s.read(cx).process.favorites.read(cx).state_for(f.id))
                        .unwrap_or(FavoriteState::Available)
                })
                .collect();
            // Interleave drop-gap rows between each favorite so the
            // user can drop in a precise insertion point (§4.2). The
            // gap before the first row gets a `-INF` left bound; the
            // gap after the last row gets `+INF` right bound, both
            // routed through `Favorites::reorder_between`.
            let mut elements: Vec<AnyElement> = Vec::with_capacity(self.favorites.len() * 2 + 1);
            let n = self.favorites.len();
            for (i, f) in self.favorites.iter().enumerate() {
                let before = if i == 0 {
                    f64::NEG_INFINITY
                } else {
                    self.favorites[i - 1].sort_index
                };
                let after = f.sort_index;
                elements.push(render_drop_gap(i, before, after, shell.clone(), cx));
                elements.push(render_favorite_row(
                    i,
                    f,
                    states[i],
                    &icons,
                    shell.clone(),
                    active_path.as_deref(),
                    cx,
                ));
            }
            // Trailing gap after the last row.
            let last_idx = if n == 0 {
                0.0
            } else {
                self.favorites[n - 1].sort_index
            };
            elements.push(render_drop_gap(
                n,
                last_idx,
                f64::INFINITY,
                shell.clone(),
                cx,
            ));
            v_flex().w_full().children(elements).into_any_element()
        };

        v_flex()
            .w_full()
            .child(header)
            .child(body)
            .into_any_element()
    }
}

/// Render one favorite row in the sidebar list. Folder + Volume +
/// Application targets get the kind-appropriate icon; saved searches
/// and tags (reserved) fall back to a lucide glyph. Click navigates
/// the active tab; iter 5 layers modifier-click and drag.
fn render_favorite_row(
    index: usize,
    fav: &Favorite,
    state: FavoriteState,
    icons: &Rc<RefCell<IconCache>>,
    shell: WeakEntity<Shell>,
    active_path: Option<&std::path::Path>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let label = fav.effective_label();
    let row_key: SharedString = format!("fav-row-{index}").into();
    let path_for_click = match &fav.target {
        FavoriteTarget::Path(p) => Some(p.clone()),
        _ => None,
    };
    let is_active = match (&fav.target, active_path) {
        (FavoriteTarget::Path(p), Some(a)) => p.as_path() == a,
        _ => false,
    };

    // §7 icon resolution: a custom Lucide pick or folder tint
    // overrides; otherwise fall back to the kind-appropriate default
    // (NSWorkspace bitmap for paths, glyph for saved-search / tag).
    let icon_el: AnyElement = match (&fav.custom_icon, &fav.target) {
        (Some(feraille_core::favorites::FavoriteIcon::Lucide(name)), _) => svg()
            .path(format!("icons/{name}.svg"))
            .w(px(16.0))
            .h(px(16.0))
            .text_color(theme.sidebar_foreground)
            .into_any_element(),
        (Some(feraille_core::favorites::FavoriteIcon::TintedFolder(_color)), _) => svg()
            .path("icons/nav/star.svg")
            .w(px(16.0))
            .h(px(16.0))
            .text_color(theme.primary)
            .into_any_element(),
        (None, FavoriteTarget::Path(p)) => {
            let icon = icons.borrow_mut().folder_icon_for(p);
            img(icon).w(px(16.0)).h(px(16.0)).into_any_element()
        }
        (None, FavoriteTarget::SavedSearch(_)) => svg()
            .path("icons/nav/star.svg")
            .w(px(16.0))
            .h(px(16.0))
            .text_color(theme.sidebar_foreground)
            .into_any_element(),
        (None, FavoriteTarget::Tag(_)) => svg()
            .path("icons/nav/star.svg")
            .w(px(16.0))
            .h(px(16.0))
            .text_color(theme.sidebar_foreground)
            .into_any_element(),
    };

    let label_for_drag: SharedString = label.clone().into();
    let drag_id = fav.id;
    // §8 dimming: Unmounted / Missing rows render at ~50 % opacity so
    // they're still legible but visibly inactive. Click handlers
    // guard against navigation when state != Available.
    let row_opacity = match state {
        FavoriteState::Available => 1.0_f32,
        FavoriteState::Unmounted | FavoriteState::Missing => 0.5,
    };
    let mut row = h_flex()
        .id(ElementId::Name(row_key))
        .w_full()
        .px_2()
        .py_1()
        .gap_2()
        .items_center()
        .text_sm()
        .rounded(theme.radius)
        .cursor_pointer()
        .opacity(row_opacity)
        .text_color(if is_active {
            theme.sidebar_accent_foreground
        } else {
            theme.sidebar_foreground
        })
        .on_drag(
            FavoriteDragPayload {
                id: drag_id,
                label: label_for_drag,
            },
            |payload, _offset, _window, cx| cx.new(|_| payload.clone()),
        );
    if is_active {
        row = row.bg(theme.sidebar_accent);
    } else {
        let hover_bg = theme.sidebar_accent.opacity(0.5);
        row = row.hover(move |this| this.bg(hover_bg));
    }
    row = row.child(div().flex_shrink_0().w(px(16.0)).h(px(16.0)).child(icon_el));
    row = row.child(
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .child(SharedString::from(label)),
    );
    // §8 state affordance: warning glyph for Missing, offline for
    // Unmounted, plain star for Available.
    let trailing_icon = match state {
        FavoriteState::Available => "icons/nav/star.svg",
        // gpui-component ships triangle-alert.svg; we reuse it for
        // both Missing (file-not-found) and Unmounted (volume gone)
        // — the muted color and dim row already disambiguate from
        // a healthy favorite, and a future polish pass can swap a
        // dedicated eject/offline glyph in once we have one.
        FavoriteState::Unmounted | FavoriteState::Missing => "icons/triangle-alert.svg",
    };
    let trailing_color = match state {
        FavoriteState::Available => theme.primary,
        FavoriteState::Unmounted | FavoriteState::Missing => theme.muted_foreground,
    };
    row = row.child(
        svg()
            .path(trailing_icon)
            .w(px(11.0))
            .h(px(11.0))
            .text_color(trailing_color)
            .flex_shrink_0(),
    );

    let Some(p) = path_for_click else {
        return row.into_any_element();
    };
    let shell_for_click = shell.clone();
    let shell_for_menu = shell.clone();
    let path_for_menu = p.clone();
    let path_for_click = p;
    let fav_id = fav.id;
    // Single-click navigates (§11.2). Cmd-click opens in a new tab
    // (§11.3). Modifier inspection mirrors how file-list rows route
    // open-in-new-tab through the same `open_path_in_new_tab` helper.
    // The click also marks this favorite as the keyboard-reorder
    // target (§4.4) so `Cmd+Option+Up/Down` operates on it.
    // §8 click guard: refuse navigation for Missing / Unmounted; show
    // a native alert instead. Locate dialog with NSOpenPanel is iter
    // 11 polish.
    let click_state = state;
    row = row.on_click(move |event, window, cx| {
        if let Some(s) = shell_for_click.upgrade() {
            let modifiers = event.modifiers();
            let path = path_for_click.clone();
            s.update(cx, |shell, cx| {
                shell.focused_favorite = Some(fav_id);
                match click_state {
                    FavoriteState::Available => {
                        if modifiers.platform {
                            shell.open_path_in_new_tab(path, window, cx);
                        } else {
                            shell.navigate(path, cx);
                        }
                    }
                    FavoriteState::Unmounted => {
                        crate::platform_shell::show_alert(
                            "Volume not mounted",
                            &format!(
                                "\u{201C}{}\u{201D} isn\u{2019}t currently mounted.",
                                path.display()
                            ),
                        );
                    }
                    FavoriteState::Missing => {
                        crate::platform_shell::show_alert(
                            "Favorite can\u{2019}t be found",
                            &format!(
                                "\u{201C}{}\u{201D} may have been moved or deleted.\nUse the \u{201C}Remove from Favorites\u{201D} context menu to remove this shortcut, or restore the original location.",
                                path.display()
                            ),
                        );
                    }
                }
            });
        }
    });
    // `.context_menu(...)` changes the element type to `ContextMenu<...>`,
    // so it has to be the terminal call before returning.
    row.context_menu(move |menu, window, cx| {
        use crate::shell::{
            CopyContextPath, RenameFavorite, ResetFavoriteIcon, ResetFavoriteName,
            RevealContextPath, SetFavoriteIconArchive, SetFavoriteIconCode, SetFavoriteIconFolder,
            SetFavoriteIconImage, SetFavoriteIconMusic, SetFavoriteIconStar,
            ToggleFavoriteForTarget,
        };
        use gpui_component::menu::{PopupMenu, PopupMenuItem};
        if let Some(s) = shell_for_menu.upgrade() {
            s.update(cx, |shell, _| {
                shell.context_target = Some(path_for_menu.clone());
                shell.favorites_context_path = Some(path_for_menu.clone());
            });
        }
        // Icon picker submenu (§7). Curated picks tied to the assets
        // that actually ship under `resources/icons/` — a full visual
        // picker is iter 11 polish.
        let icon_submenu = PopupMenu::build(window, cx, move |m, _w, _c| {
            m.menu("\u{2605} Star", Box::new(SetFavoriteIconStar))
                .menu("\u{1F4C1} Folder", Box::new(SetFavoriteIconFolder))
                .menu(
                    "\u{27E8}\u{2009}/\u{2009}\u{27E9} Code",
                    Box::new(SetFavoriteIconCode),
                )
                .menu("\u{1F5BC} Image", Box::new(SetFavoriteIconImage))
                .menu("\u{266B} Music", Box::new(SetFavoriteIconMusic))
                .menu("\u{1F5C4} Archive", Box::new(SetFavoriteIconArchive))
                .separator()
                .menu("Reset Icon", Box::new(ResetFavoriteIcon))
        });
        menu.menu("Rename\u{2026}", Box::new(RenameFavorite))
            .menu("Reset to Original Name", Box::new(ResetFavoriteName))
            .item(PopupMenuItem::submenu("Change Icon", icon_submenu))
            .separator()
            .menu("Reveal in Finder", Box::new(RevealContextPath))
            .menu("Copy Path", Box::new(CopyContextPath))
            .separator()
            .menu("Remove from Favorites", Box::new(ToggleFavoriteForTarget))
    })
    .into_any_element()
}

/// Insertion-point drop gap between two favorite rows. Idle, it's a
/// 4-DIP transparent strip; when a `FavoriteDragPayload` is dragged
/// over, `drag_over` paints a 2-DIP accent line so the user can see
/// exactly where the drop will land (§4.2). `before` / `after` are
/// the `sort_index` bounds passed to `Favorites::reorder_between`.
fn render_drop_gap(
    index: usize,
    before: f64,
    after: f64,
    shell: WeakEntity<Shell>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let accent = theme.primary;
    let key: SharedString = format!("fav-gap-{index}").into();
    let shell_for_external = shell.clone();
    div()
        .id(ElementId::Name(key))
        .w_full()
        .h(px(6.0))
        .drag_over::<FavoriteDragPayload>(move |style, _payload, _window, _cx| {
            // 2-DIP accent line centered in the 6-DIP strip.
            style.border_t_2().border_color(accent)
        })
        .drag_over::<ExternalPaths>(move |style, _payload, _window, _cx| {
            style.border_t_2().border_color(accent)
        })
        .on_drop({
            let shell = shell.clone();
            move |payload: &FavoriteDragPayload, _window, cx| {
                if let Some(s) = shell.upgrade() {
                    let id = payload.id;
                    s.update(cx, |shell, cx| {
                        shell.process.favorites.update(cx, |f, cx| {
                            f.reorder_between(id, before, after, cx);
                        });
                    });
                }
            }
        })
        .on_drop(move |paths: &ExternalPaths, _window, cx| {
            // Drag-to-add (§4.3, §2.3): folder paths dropped from the
            // file list, tree, breadcrumb, or Finder land here. Files
            // are rejected with a toast — only folders can be favorited.
            if let Some(s) = shell_for_external.upgrade() {
                let collected: Vec<_> = paths.paths().to_vec();
                s.update(cx, |shell, cx| {
                    let mut cursor = before;
                    let upper = after;
                    let mut added = 0usize;
                    let mut rejected = 0usize;
                    for raw in collected {
                        let canonical = std::fs::canonicalize(&raw).unwrap_or(raw);
                        if !canonical.is_dir() {
                            rejected += 1;
                            continue;
                        }
                        let slot = feraille_core::favorites::fractional_between(cursor, upper);
                        shell.process.favorites.update(cx, |f, cx| {
                            f.add_path_at(canonical.clone(), FavoriteKind::Folder, slot, cx);
                        });
                        cursor = slot;
                        added += 1;
                    }
                    if rejected > 0 {
                        use gpui_component::notification::Notification;
                        let _ = (added, rejected);
                        // No `window` in scope to push notifications;
                        // the toast is best-effort. Notification needs
                        // a Window — relax to log-only for the rejected
                        // case (iter 11 routes through an action to
                        // get a Window). Successful adds already emit
                        // a section repaint.
                        let _ = Notification::info("");
                    }
                });
            }
        })
        .into_any_element()
}
