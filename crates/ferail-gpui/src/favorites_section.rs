//! The sidebar's **Favorites** section. Renders a disclosure-triangle
//! header, a click-to-collapse interaction, and either an empty-state
//! prompt or a list of user-curated favorite rows (iter 3+).
//!
//! Distinct from the **Locations** section (the fixed OS folders,
//! built via `Shell::build_locations_menu` in `shell.rs`). Section-
//! header collapse persists through `MetadataDb::favorites_section_collapsed`.

use crate::text::{IconScale as _, TextScale as _};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use ferail_core::favorites::{Favorite, FavoriteId, FavoriteKind, FavoriteState, FavoriteTarget};
use gpui::{Animation, AnimationExt as _, AnyElement, Context, FocusHandle, WeakEntity, div};
use gpui::{
    App, AppContext, ElementId, ExternalPaths, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window,
    ease_out_quint, img, px, svg,
};
use gpui_component::{
    ActiveTheme, Collapsible, h_flex, menu::ContextMenuExt as _, sidebar::SidebarItem, v_flex,
};

use crate::icons::IconCache;
use crate::shell::Shell;

/// Key context applied to the focused Favorites section so Up / Down /
/// Enter / Delete route to the favorites focus actions instead of the
/// file list (§11.4). More specific than `SHELL_CONTEXT`.
pub const FAVORITES_CONTEXT: &str = "FerailFavorites";

/// Per-add fade-in duration (§2.2), dedup-pulse flash, and the §3.2
/// collapse-on-remove fade. `COLLAPSE_MS` must match the Shell's
/// `FAV_COLLAPSE_MS` so the row is dropped from the entity exactly when
/// its fade finishes.
const APPEAR_MS: u64 = 150;
const PULSE_MS: u64 = 450;
const COLLAPSE_MS: u64 = 150;

/// Which one-shot animation, if any, a row should play this frame.
#[derive(Clone, Copy)]
enum RowAnim {
    None,
    /// Fade/scale-in on a freshly added favorite (§2.2).
    Appear,
    /// Dedup-pulse flash, keyed by generation so a repeat re-triggers.
    Pulse(u32),
    /// Fade + collapse on a favorite being removed (§3.2). The entity
    /// keeps the row present for the animation window, then drops it.
    Collapse,
}

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
            .text_scale_sm()
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
    /// Keyboard-focused favorite (§11.4) — drives the focus ring.
    focused: Option<FavoriteId>,
    /// Focus handle the section tracks so `FAVORITES_CONTEXT` bindings
    /// fire only while the section is focused.
    focus_handle: FocusHandle,
    /// Ids that should play the §2.2 fade-in this frame.
    appear: HashSet<FavoriteId>,
    /// Dedup-pulse generation per id (§2.2).
    pulse: HashMap<FavoriteId, u32>,
    /// Ids being removed that should play the §3.2 collapse fade this
    /// frame (still present in `favorites` until the fade finishes).
    removing: HashSet<FavoriteId>,
}

impl FavoritesSection {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        favorites: Vec<Favorite>,
        section_collapsed: bool,
        shell: WeakEntity<Shell>,
        icons: Rc<RefCell<IconCache>>,
        focused: Option<FavoriteId>,
        focus_handle: FocusHandle,
        appear: HashSet<FavoriteId>,
        pulse: HashMap<FavoriteId, u32>,
        removing: HashSet<FavoriteId>,
    ) -> Self {
        Self {
            favorites,
            icon_only: false,
            section_collapsed,
            shell,
            icons,
            focused,
            focus_handle,
            appear,
            pulse,
            removing,
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
            .flex()
            .flex_shrink_0()
            .w(px(16.0))
            .h(px(16.0))
            .items_center()
            .justify_center()
            .cursor_pointer()
            // House-style `nav/plus.svg` (Lucide outline, stroke 1.75) so it
            // matches the sidebar icon family rather than a thin font "+".
            .child(
                gpui::svg()
                    .path("icons/nav/plus.svg")
                    .icon_px(13.0)
                    // Match the black Locations/Favorites row icons, not the
                    // muted grey of secondary text.
                    .text_color(theme.sidebar_foreground),
            )
            .on_click(move |_, _window, cx| {
                cx.stop_propagation();
                let Some(shell) = shell_for_plus.upgrade() else {
                    return;
                };
                // Prime directive: `canonicalize` stats the filesystem
                // (unbounded on network volumes) — run it on a worker
                // and apply the favorite on completion. The click
                // itself stays I/O-free.
                let path = shell.read(cx).active_tab().current_dir.clone();
                let shell_weak = shell.downgrade();
                cx.spawn(async move |cx| {
                    let canonical = {
                        let p = path.clone();
                        cx.background_executor()
                            .spawn(async move { crate::shell::canonicalize_for_identity(p) })
                            .await
                    };
                    if let Some(shell) = shell_weak.upgrade() {
                        shell.update(cx, |s, cx| {
                            s.process.favorites.update(cx, |f, cx| {
                                f.add_path(
                                    canonical,
                                    ferail_core::favorites::FavoriteKind::Folder,
                                    cx,
                                );
                            });
                        });
                    }
                })
                .detach();
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
            // as guidance rather than a clickable row. It is ALSO the
            // drop target the text advertises: with no rows there are no
            // inter-row gaps, so without this a folder dragged onto an
            // empty section would have nowhere to land. `-INF`/`+INF`
            // bounds place the first favorite at a valid mid-slot.
            let shell = self.shell.clone();
            let accent = theme.sidebar_accent.opacity(0.6);
            div()
                .id("favorites-empty-drop")
                .flex()
                .items_center()
                // A comfortable drop zone, not just the text's line box —
                // the empty section is the only place to drop the first
                // favorite, so the target should be easy to hit. The
                // resting dashed outline + faint fill mark it as a drop
                // well at all times (not only mid-drag), so it reads as
                // "put something here"; `drag_over` then deepens the fill.
                .min_h(px(40.0))
                .mx_2()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_dashed()
                .border_color(theme.sidebar_border)
                .bg(theme.sidebar_accent.opacity(0.35))
                .text_scale_xs()
                .text_color(theme.muted_foreground.opacity(0.85))
                .child("Drag folders here for quick access.")
                .drag_over::<ExternalPaths>(move |style, _payload, _window, _cx| {
                    style.bg(accent)
                })
                .on_drop(move |paths: &ExternalPaths, _window, cx| {
                    add_dropped_folders(paths, f64::NEG_INFINITY, f64::INFINITY, &shell, cx);
                })
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
                let anim = if self.removing.contains(&f.id) {
                    // Removal wins over add/pulse: a row toggled off mid-
                    // fade should collapse, not keep fading in.
                    RowAnim::Collapse
                } else if self.appear.contains(&f.id) {
                    RowAnim::Appear
                } else if let Some(seq) = self.pulse.get(&f.id).copied() {
                    RowAnim::Pulse(seq)
                } else {
                    RowAnim::None
                };
                elements.push(render_drop_gap(i, before, after, shell.clone(), cx));
                elements.push(render_favorite_row(
                    i,
                    f,
                    states[i],
                    self.focused == Some(f.id),
                    anim,
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

        // The section tracks a focus handle and declares
        // `FAVORITES_CONTEXT` so arrow/Enter/Delete bindings fire only
        // while it's focused (§11.4). The action handlers themselves
        // live on the Shell root and are reached by bubbling.
        v_flex()
            .id("favorites-section")
            .key_context(FAVORITES_CONTEXT)
            .track_focus(&self.focus_handle)
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
#[allow(clippy::too_many_arguments)]
fn render_favorite_row(
    index: usize,
    fav: &Favorite,
    state: FavoriteState,
    is_focused: bool,
    anim: RowAnim,
    icons: &Rc<RefCell<IconCache>>,
    shell: WeakEntity<Shell>,
    active_path: Option<&std::path::Path>,
    cx: &App,
) -> AnyElement {
    // Row builders can run during layout/prepaint, outside the scope of
    // `Shell::render`'s own guard — re-enter it so a favorites icon
    // cache miss returns the blank placeholder instead of a synchronous
    // NSWorkspace fetch (same parity as tree.rs / file_list.rs row
    // builders; the sidebar's background icon warm fills the cache).
    let _render_guard = ferail_core::path_guard::enter_render();
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
        (Some(ferail_core::favorites::FavoriteIcon::Lucide(name)), _) => svg()
            .path(format!("icons/{name}.svg"))
            .icon_px(crate::tree::SIDEBAR_ICON_PX)
            .text_color(theme.sidebar_foreground)
            .into_any_element(),
        (None, FavoriteTarget::Path(p)) => {
            let icon = icons.borrow_mut().folder_icon_for(p);
            img(icon)
                .icon_px(crate::tree::SIDEBAR_ICON_PX)
                .into_any_element()
        }
        (None, FavoriteTarget::SavedSearch(_)) => svg()
            .path("icons/nav/search.svg")
            .icon_px(crate::tree::SIDEBAR_ICON_PX)
            .text_color(theme.sidebar_foreground)
            .into_any_element(),
        (None, FavoriteTarget::Tag(_)) => svg()
            .path("icons/nav/tag.svg")
            .icon_px(crate::tree::SIDEBAR_ICON_PX)
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
        .text_scale_sm()
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
    // §11.4 keyboard focus ring — distinct from the §5 favorited
    // indicator, hover, and the active/selected background. A 1-DIP
    // inset ring keeps the row from shifting (the border replaces a
    // transparent default rather than adding width).
    row = row.border_1().border_color(if is_focused {
        theme.ring
    } else {
        gpui::transparent_black()
    });
    row = row.child(
        div()
            .flex_shrink_0()
            .icon_px(crate::tree::SIDEBAR_ICON_PX)
            .child(icon_el),
    );
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
        // Missing (file-not-found) keeps the warning triangle — it's a
        // genuine error. Unmounted (volume offline) uses circle-x so it
        // reads as "disconnected/unavailable" rather than "broken"; a
        // dedicated eject glyph can replace circle-x in a later pass.
        FavoriteState::Unmounted => "icons/circle-x.svg",
        FavoriteState::Missing => "icons/triangle-alert.svg",
    };
    let trailing_color = match state {
        FavoriteState::Available => theme.primary,
        FavoriteState::Unmounted | FavoriteState::Missing => theme.muted_foreground,
    };
    row = row.child(
        svg()
            .path(trailing_icon)
            .icon_px(11.0)
            .text_color(trailing_color)
            .flex_shrink_0(),
    );

    // §11.5 drop a file/folder *onto* a favorite row = move/copy into
    // that folder (distinct from dropping *between* rows, which the
    // drop gaps handle as a reorder / add). The whole row highlights as
    // the drop target. Only `Available` path favorites accept the drop;
    // dropping into a Missing / Unmounted target would fail in the
    // worker, so those rows decline gracefully (no highlight, no drop).
    if let (FavoriteState::Available, Some(dest)) = (state, path_for_click.clone()) {
        let drop_accent = theme.sidebar_accent.opacity(0.6);
        let shell_for_drop = shell.clone();
        row = row
            .drag_over::<ExternalPaths>(move |style, _payload, _window, _cx| style.bg(drop_accent))
            .on_drop(move |paths: &ExternalPaths, window, cx| {
                let Some(s) = shell_for_drop.upgrade() else {
                    return;
                };
                let collected: Vec<_> = paths.paths().to_vec();
                let dest = dest.clone();
                // `handle_external_drop` reads the live modifiers
                // (Option → copy) and resolves same- vs cross-volume on
                // a worker, so this stays the same engine the file list
                // uses (Prime Directive: no stat on the drop thread).
                s.update(cx, |shell, cx| {
                    shell.handle_external_drop(collected, dest, window, cx);
                });
            });
    }

    // Tag favorites (§9): clicking runs a Finder-tag search in the active
    // tab; a small context menu offers removal. Other non-path targets
    // (saved search, reserved) fall through to the inert early-return.
    if let FavoriteTarget::Tag(tag_name) = &fav.target {
        let tag = tag_name.clone();
        let fav_id = fav.id;
        let shell_for_tag = shell.clone();
        let shell_for_tagmenu = shell.clone();
        row = row.on_click(move |_event, window, cx| {
            if let Some(s) = shell_for_tag.upgrade() {
                let tag = tag.clone();
                s.update(cx, |shell, cx| {
                    shell.focused_favorite = Some(fav_id);
                    window.focus(&shell.favorites_focus, cx);
                    shell.navigate_from_tag_favorite(tag, window, cx);
                });
            }
        });
        return apply_row_anim(
            row.context_menu(move |menu, _window, cx| {
                use crate::shell::DeleteFavorite;
                // Stash the target so the bubbled DeleteFavorite action
                // removes this row (with the §3.2 collapse + undo).
                if let Some(s) = shell_for_tagmenu.upgrade() {
                    s.update(cx, |shell, _| {
                        shell.focused_favorite = Some(fav_id);
                    });
                }
                menu.menu("Remove from Favorites", Box::new(DeleteFavorite))
            })
            .into_any_element(),
            anim,
            fav.id,
            theme.primary,
        );
    }

    let Some(p) = path_for_click else {
        return apply_row_anim(row.into_any_element(), anim, fav.id, theme.primary);
    };
    let shell_for_click = shell.clone();
    let shell_for_menu = shell.clone();
    let path_for_menu = p.clone();
    let path_for_click = p;
    let fav_id = fav.id;
    // Single-click navigates (§11.2). Cmd-click opens a new tab, and
    // Cmd+Option-click a new window (§11.3) — mirroring the file list's
    // modifier vocabulary. The click also focuses the section (so the
    // arrow/Delete keys take over, §11.4) and marks this favorite as the
    // keyboard target for reorder / delete / activate. §8 click guard:
    // a Missing / Unmounted row surfaces the Locate/Remove/Keep dialog
    // instead of navigating into the void.
    let click_state = state;
    row = row.on_click(move |event, window, cx| {
        if let Some(s) = shell_for_click.upgrade() {
            let modifiers = event.modifiers();
            let path = path_for_click.clone();
            s.update(cx, |shell, cx| {
                shell.focused_favorite = Some(fav_id);
                window.focus(&shell.favorites_focus, cx);
                match click_state {
                    FavoriteState::Available => {
                        if modifiers.platform && modifiers.alt {
                            crate::shell::open_window_at(cx, path);
                        } else if modifiers.platform {
                            shell.open_path_in_new_tab(path, window, cx);
                        } else {
                            shell.navigate_from_favorite(path, cx);
                        }
                    }
                    other => {
                        shell.show_missing_favorite_dialog(fav_id, path, other, window, cx);
                    }
                }
            });
        }
    });
    // `.context_menu(...)` changes the element type to `ContextMenu<...>`,
    // so it has to be the terminal call before returning.
    let menu_anim = anim;
    let menu_fav_id = fav.id;
    let menu_accent = theme.primary;
    apply_row_anim(
        row.context_menu(move |menu, _window, cx| {
            use crate::shell::{
                CopyContextPath, LocateFavorite, OpenFavoriteIconPicker, RenameFavorite,
                ResetFavoriteIcon, ResetFavoriteName, RevealContextPath, ToggleFavoriteForTarget,
            };
            if let Some(s) = shell_for_menu.upgrade() {
                s.update(cx, |shell, _| {
                    shell.context_target = Some(path_for_menu.clone());
                    shell.favorites_context_path = Some(path_for_menu.clone());
                });
            }
            // §7 "Change Icon…" opens the Lucide icon-picker window
            // (favorite_icon_picker); "Reset Icon" clears back to the
            // kind+target default. The old curated emoji submenu is gone.
            menu.menu("Rename\u{2026}", Box::new(RenameFavorite))
                .menu("Reset to Original Name", Box::new(ResetFavoriteName))
                .menu("Change Icon\u{2026}", Box::new(OpenFavoriteIconPicker))
                .menu("Reset Icon", Box::new(ResetFavoriteIcon))
                .menu("Locate\u{2026}", Box::new(LocateFavorite))
                .separator()
                .menu(
                    ferail_core::commands::REVEAL_LABEL,
                    Box::new(RevealContextPath),
                )
                .menu("Copy Path", Box::new(CopyContextPath))
                .separator()
                .menu("Remove from Favorites", Box::new(ToggleFavoriteForTarget))
        })
        .into_any_element(),
        menu_anim,
        menu_fav_id,
        menu_accent,
    )
}

/// Wrap a favorite row in its one-shot §2.2 animation, if any. `Appear`
/// fades the row in over ~150ms (a freshly added favorite); `Pulse`
/// flashes the row background to draw the eye to an existing entry on a
/// duplicate-add. `with_animation` plays once per element-id lifetime,
/// so a stable id replays nothing; the pulse id carries the generation
/// so a *repeat* dedup re-triggers. `None` returns the row untouched.
fn apply_row_anim(
    row: AnyElement,
    anim: RowAnim,
    id: FavoriteId,
    accent: gpui::Hsla,
) -> AnyElement {
    match anim {
        RowAnim::None => row,
        RowAnim::Appear => {
            let key: SharedString = format!("fav-appear-{id}").into();
            div()
                .w_full()
                .child(row)
                .with_animation(
                    ElementId::Name(key),
                    Animation::new(Duration::from_millis(APPEAR_MS)).with_easing(ease_out_quint()),
                    |this, delta| this.opacity(delta),
                )
                .into_any_element()
        }
        RowAnim::Pulse(seq) => {
            let key: SharedString = format!("fav-pulse-{id}-{seq}").into();
            div()
                .w_full()
                .child(row)
                .with_animation(
                    ElementId::Name(key),
                    Animation::new(Duration::from_millis(PULSE_MS)),
                    move |this, delta| {
                        // Flash to the accent, then fade back to clear.
                        this.bg(accent.opacity((1.0 - delta) * 0.5))
                    },
                )
                .into_any_element()
        }
        RowAnim::Collapse => {
            let key: SharedString = format!("fav-collapse-{id}").into();
            // Fade out and pull the rows below up (negative bottom margin
            // easing to a full row height) so the removal reads as a
            // collapse rather than a pop. The entity drops the row exactly
            // when this finishes, so the tiny end-state snap is invisible.
            div()
                .w_full()
                .overflow_hidden()
                .child(row)
                .with_animation(
                    ElementId::Name(key),
                    Animation::new(Duration::from_millis(COLLAPSE_MS))
                        .with_easing(ease_out_quint()),
                    |this, delta| {
                        this.opacity(1.0 - delta)
                            .mb(px(-FAV_ROW_COLLAPSE_H * delta))
                    },
                )
                .into_any_element()
        }
    }
}

/// Approximate rendered height (DIP at `ui_scale == 1`) of a favorite
/// row, used only to drive the §3.2 collapse's negative-margin slide.
/// An estimate is fine: the row is removed the instant the fade ends, so
/// any small mismatch resolves while the row is already at zero opacity.
const FAV_ROW_COLLAPSE_H: f32 = 28.0;

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
        .on_drop({
            let shell = shell_for_external.clone();
            move |paths: &ExternalPaths, _window, cx| {
                add_dropped_folders(paths, before, after, &shell, cx);
            }
        })
        .into_any_element()
}

/// Drag-to-add (§4.3, §2.3): favorite folders dropped from the file
/// list, tree, breadcrumb, or Finder. Only folders can be favorited;
/// the surviving folders are inserted in drop order at fractional slots
/// between `before` and `after` (`-INF` / `+INF` for the ends, so an
/// empty section drops at a valid mid-slot).
///
/// Prime directive: `canonicalize` + `is_dir` are stat calls — a
/// 50-item drop from a network volume could block the UI for seconds.
/// The whole batch is validated on a worker, then the survivors are
/// applied on completion. Shared by the inter-row gaps and the
/// empty-state placeholder so both honour the "Drag folders here"
/// affordance identically.
fn add_dropped_folders(
    paths: &ExternalPaths,
    before: f64,
    after: f64,
    shell: &WeakEntity<Shell>,
    cx: &mut App,
) {
    let collected: Vec<_> = paths.paths().to_vec();
    if collected.is_empty() {
        return;
    }
    let shell_weak = shell.clone();
    cx.spawn(async move |cx| {
        let (valid, rejected): (Vec<std::path::PathBuf>, usize) = cx
            .background_executor()
            .spawn(async move {
                let mut valid = Vec::with_capacity(collected.len());
                let mut rejected = 0usize;
                for raw in collected {
                    let canonical = crate::shell::canonicalize_for_identity(raw);
                    if canonical.is_dir() {
                        valid.push(canonical);
                    } else {
                        rejected += 1;
                    }
                }
                (valid, rejected)
            })
            .await;
        if rejected > 0 {
            // Toast needs a Window we don't have in a drop closure;
            // log-only until iter 11 routes this through an action
            // (pre-existing limitation).
            crate::log_warn!(90, "favorites drop: rejected {rejected} non-folder item(s)");
        }
        let Some(shell) = shell_weak.upgrade() else {
            return;
        };
        shell.update(cx, |shell, cx| {
            let mut cursor = before;
            let upper = after;
            for canonical in valid {
                let slot = ferail_core::favorites::fractional_between(cursor, upper);
                shell.process.favorites.update(cx, |f, cx| {
                    f.add_path_at(canonical.clone(), FavoriteKind::Folder, slot, cx);
                });
                cursor = slot;
            }
        });
    })
    .detach();
}
