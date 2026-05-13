//! Feraille — GPUI shell. Phase 1 of the migration.
//!
//! This is the **empty shell**: a window opens at a sensible size,
//! native macOS chrome (real traffic lights) is provided by GPUI's
//! platform layer, a theme is applied, the window has a sidebar pane
//! and a main pane with the proportions the design brief calls for,
//! and `Cmd+W` closes the window. No domain logic yet — no file
//! system access, no settings, no list pane content. That arrives in
//! Phases 3+ (Settings first, then main surfaces).
//!
//! ## Why "empty" matters
//!
//! The parallel-build strategy requires that `cargo run --bin Feraille`
//! (old, soft renderer) and `cargo run --bin feraille-gpui` (new)
//! both work *every day* of the migration. Loading domain code here
//! before Phase 2 is done — before the domain crates are confirmed
//! UI-agnostic — would create a coupling cycle the moment a domain
//! type bled into the new UI.
//!
//! ## Running it
//!
//! ```
//! cargo run --bin feraille-gpui
//! ```
//!
//! Old app still runs via `cargo run --bin Feraille`. Both binaries
//! coexist until the Phase 6 cutover.

use gpui::*;
use gpui_component::{
    ActiveTheme, Root,
    h_flex,
    sidebar::{Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem},
    v_flex,
};
use gpui_component_assets::Assets;

/// Top-level shell view. Holds no domain state in Phase 1; future
/// phases attach a workspace pane / sidebar selection / etc.
struct Shell;

impl Shell {
    fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self
    }

    /// Scaffold sidebar with placeholder entries. The real categories
    /// (Recents, Favorites, Locations, Volumes — matching the
    /// existing app) land in Phase 4b when the sidebar gets wired to
    /// the domain layer.
    fn nav_menu(&self) -> SidebarMenu {
        SidebarMenu::new().children([
            SidebarMenuItem::new("Recents").active(true),
            SidebarMenuItem::new("Favorites"),
            SidebarMenuItem::new("Locations"),
            SidebarMenuItem::new("Volumes"),
        ])
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu = self.nav_menu();
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                Sidebar::new("shell-sidebar")
                    .w(px(220.0))
                    .header(
                        SidebarHeader::new().child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .child("Feraille"),
                        ),
                    )
                    .child(SidebarGroup::new("").child(menu)),
            )
            .child(
                // Main pane scaffold. In Phase 4 this hosts the
                // tabstrip, breadcrumb, file list, preview pane.
                v_flex()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .p_6()
                    .gap_2()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child("Empty shell"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Phase 1 of the GPUI migration. Sidebar + main pane scaffolding only. \
                                Domain wiring lands in Phase 3 (Settings) and Phase 4 (file list).",
                            ),
                    ),
            )
    }
}

fn main() {
    let app = gpui_platform::application().with_assets(Assets);

    app.run(move |cx| {
        gpui_component::init(cx);

        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1180.0), px(760.0)), cx)),
            ..Default::default()
        };

        cx.spawn(async move |cx| {
            cx.open_window(opts, |window, cx| {
                let view = cx.new(|cx| Shell::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open feraille-gpui window");
        })
        .detach();
    });
}
