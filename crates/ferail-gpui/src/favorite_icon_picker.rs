//! Favorite icon picker window.
//!
//! Lists the bundled Lucide glyphs — the flat `icons/<name>.svg` set, i.e.
//! the upstream `gpui-component-assets` library plus our few top-level adds —
//! in a scrollable grid. Picking one sets the target favorite's `custom_icon`
//! to `FavoriteIcon::Lucide(name)` and closes the window.
//!
//! Replaces the old curated emoji submenu: the emoji clashed with the
//! line-icon language and the curated picks were a placeholder. The picker
//! draws from the same bundle the rest of the app does, so every glyph is an
//! on-style Lucide line icon — see [docs/features/ICONS.md].
//!
//! The favorite is identified by id (resolved once when the menu opens),
//! and the picker holds the shared `Entity<Favorites>` directly — so the
//! selection writes through the same path `Reset Icon` uses, no fragile
//! context state.

use crate::text::TextScale as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Root, Sizable, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use ferail_core::favorites::{FavoriteIcon, FavoriteId};

use crate::favorites::Favorites;

/// The flat, on-style Lucide library: every `icons/<name>.svg` with no
/// further path segment, deduped and sorted. The app's own `nav/` and
/// `file/` semantic variants are intentionally excluded so the picker reads
/// as one consistent set rather than showing near-duplicate folders/stars.
fn lucide_library() -> Vec<SharedString> {
    let mut names: Vec<SharedString> = crate::assets::FeraAssets
        .list("icons/")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| {
            let name = p.strip_prefix("icons/")?.strip_suffix(".svg")?;
            if name.is_empty() || name.contains('/') {
                return None;
            }
            Some(SharedString::from(name.to_owned()))
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

pub struct IconPickerView {
    favorites: Entity<Favorites>,
    target: FavoriteId,
    icons: Vec<SharedString>,
    filter: Entity<InputState>,
    _filter_sub: Subscription,
}

impl IconPickerView {
    pub fn new(
        favorites: Entity<Favorites>,
        target: FavoriteId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("Filter icons\u{2026}")));
        // Re-render the grid on every keystroke so the visible glyphs
        // narrow to the substring match.
        let _filter_sub = cx.subscribe(&filter, |_this, _input, _event: &InputEvent, cx| {
            cx.notify();
        });
        Self {
            favorites,
            target,
            icons: lucide_library(),
            filter,
            _filter_sub,
        }
    }
}

impl Render for IconPickerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title(&tr!("Choose Favorite Icon"));
        let theme = cx.theme();
        let query = self.filter.read(cx).value().trim().to_lowercase();
        let cells: Vec<AnyElement> = self
            .icons
            .iter()
            .filter(|name| query.is_empty() || name.to_lowercase().contains(&query))
            .cloned()
            .map(|name| {
                let path: SharedString = format!("icons/{name}.svg").into();
                let pick = name.clone();
                let tip = name.clone();
                v_flex()
                    .id(ElementId::Name(name.clone()))
                    .w(px(78.0))
                    .h(px(74.0))
                    .px_1()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.secondary))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _e: &ClickEvent, window, cx| {
                        let icon = FavoriteIcon::Lucide(pick.to_string());
                        this.favorites
                            .update(cx, |f, cx| f.set_icon(this.target, Some(icon.clone()), cx));
                        window.remove_window();
                    }))
                    .child(
                        svg()
                            .path(path)
                            .w(px(26.0))
                            .h(px(26.0))
                            .text_color(theme.foreground),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_center()
                            .text_scale_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(name),
                    )
                    .into_any_element()
            })
            .collect();

        let total = self.icons.len();
        let shown = cells.len();
        let count_label: SharedString = if query.is_empty() {
            trn!("{n} glyph", "{n} glyphs", total)
        } else {
            tr!(
                "{shown} of {total}",
                shown = ferail_core::counts::format_count(shown as u64),
                total = ferail_core::counts::format_count(total as u64)
            )
        };
        let content = v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.filter).small()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_scale_xs()
                            .text_color(theme.muted_foreground)
                            .child(count_label),
                    ),
            )
            .child(
                div()
                    .id("favorite-icon-picker-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .child(h_flex().flex_wrap().gap_1().children(cells)),
            )
            .into_any_element();
        crate::private_mode::protect(content, cx)
    }
}

/// Open the picker as a centered window for `target`, writing the chosen
/// glyph back through the shared `favorites` entity.
pub fn open_window(cx: &mut App, favorites: Entity<Favorites>, target: FavoriteId) {
    let title = tr!("Choose Favorite Icon");
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(560.0), px(560.0)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.clone()),
            ..Default::default()
        }),
        ..crate::base_window_options()
    };
    let handle = cx.open_window(opts, |window, cx| {
        let view = cx.new(|cx| IconPickerView::new(favorites, target, window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    });
    if let Ok(handle) = handle {
        crate::process_state::process_state(cx)
            .register_aux_window(handle.into(), title.to_string());
        crate::boot::refresh_window_menu(cx);
    }
}
