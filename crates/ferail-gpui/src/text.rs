//! App typography. The single source of truth for text sizes is
//! [`ferail_design::TextTokens::BASE`]; this module applies those token
//! sizes to GPUI elements **rem-relative**, so UI zoom — the window rem
//! size, driven by [`crate::shell::Shell::ui_scale`] — scales every text
//! tier together.
//!
//! Render code calls the semantic tier methods ([`TextScale::text_scale_xs`],
//! …) instead of gpui's raw `text_xs` / `text_sm`, which bake in a
//! different (looser) scale and can't be retuned in one place. The method
//! names are deliberately distinct from gpui's [`gpui::Styled`] helpers so
//! the two never collide during method resolution.
//!
//! Glyph affordances whose size is tied to a fixed-size box (disclosure
//! triangles, the favorites `+`, the viewer seek grip) and the code-block
//! preview font stay on explicit `px(..)` — they are not part of the UI
//! type scale and must not drift with it.

use ferail_design::TextTokens;
use gpui::{Styled, rems};

pub use ferail_design::TextSize;

/// Baseline rem the token px values are authored against — gpui's default
/// rem size and gpui-component's default `theme.font_size`. Zoom works by
/// scaling that base: `Shell::apply_ui_zoom` sets
/// `theme.font_size = BASE_REM_PX * ui_scale`, and gpui-component's `Root`
/// copies it into the window rem size each frame, so the rem-relative sizes
/// below scale proportionally.
pub const BASE_REM_PX: f32 = 16.0;

/// Resolve a tier to the rem-relative font size used everywhere.
fn tier_rems(size: TextSize) -> gpui::Rems {
    rems(TextTokens::BASE.get(size) / BASE_REM_PX)
}

/// Extension over [`gpui::Styled`] that sizes text from the design-token
/// scale. Blanket-implemented, so it's available anywhere gpui's raw
/// `text_xs` / `text_sm` used to be.
pub trait TextScale: Styled + Sized {
    /// Set the font size from a design-token tier (rem-relative — scales
    /// with the window rem size / UI zoom).
    fn text_token(self, size: TextSize) -> Self {
        self.text_size(tier_rems(size))
    }
    /// Micro labels / dense overlays (`TextSize::Xxs`).
    fn text_scale_xxs(self) -> Self {
        self.text_token(TextSize::Xxs)
    }
    /// Body and metadata — the workhorse tier (`TextSize::Xs`).
    fn text_scale_xs(self) -> Self {
        self.text_token(TextSize::Xs)
    }
    /// Slightly emphasized labels and rows (`TextSize::Sm`).
    fn text_scale_sm(self) -> Self {
        self.text_token(TextSize::Sm)
    }
    /// Default-weight body for roomier surfaces (`TextSize::Md`).
    fn text_scale_md(self) -> Self {
        self.text_token(TextSize::Md)
    }
    /// Section headers (`TextSize::Lg`).
    fn text_scale_lg(self) -> Self {
        self.text_token(TextSize::Lg)
    }
    /// Hero / title text (`TextSize::Xl`).
    fn text_scale_xl(self) -> Self {
        self.text_token(TextSize::Xl)
    }
}

impl<T: Styled + Sized> TextScale for T {}

/// Extension over [`gpui::Styled`] that sizes a glyph/SVG icon
/// **rem-relative**, so chrome icons ride the same UI-zoom rem base as
/// text (sidebar glyphs, the cloud/star/eject accessories, file-list
/// badges). `px_at_base` is the icon's logical px at `ui_scale == 1`.
///
/// Use this for raw `gpui::svg()` / `img()` icons. gpui-component's own
/// [`gpui_component::Icon`] already inherits the (rem-scaled) ambient
/// font size *unless* given an explicit `px` size via `with_size` — in
/// that one case, pre-multiply by `ui_scale` instead.
///
/// Not for: grid thumbnails (their own icon-size axis), glyph
/// affordances pinned to a fixed-size box (disclosure triangles, the
/// favorites `+`), or decorative dots / layout dimensions.
pub trait IconScale: Styled + Sized {
    /// Square icon size from logical px, applied rem-relative.
    fn icon_px(self, px_at_base: f32) -> Self {
        self.size(rems(px_at_base / BASE_REM_PX))
    }
}

impl<T: Styled + Sized> IconScale for T {}

/// Extension mirroring gpui's [`Styled::truncate`] but eliding the
/// **middle** of the text (Finder-style) instead of the end, so a
/// filename keeps both its start and its extension visible: a long
/// "Screen Recording 2026-06-12 at 22.20.44.mov" reads as
/// "Screen Recording 2026-…22.20.44.mov" rather than losing ".mov" off
/// the right edge. Truncation is pixel-accurate (gpui's
/// `TextOverflow::TruncateMiddle`), measured by the text renderer — no
/// per-row allocation or measurement on the paint path, so it's safe in
/// the dense list. Use for filename cells; end-truncation (`.truncate()`)
/// still fits paths and free-form text where the tail is expendable.
pub trait TruncateMiddle: Styled + Sized {
    fn truncate_middle(self) -> Self {
        self.overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis_middle()
    }
}

impl<T: Styled + Sized> TruncateMiddle for T {}

/// Pin the theme's UI and monospace families to fonts that are actually
/// installed, where the platform default is only a *virtual* name.
///
/// gpui-component's theme defaults to `.SystemUIFont`. macOS and Windows
/// resolve that natively; gpui's Linux text system maps it to "IBM Plex
/// Sans" (the family Zed bundles, which Ferail does not), so on a stock
/// Linux box the lookup misses and gpui walks its fallback stack —
/// `.ZedMono`, `.ZedSans`, Helvetica, Segoe UI, Ubuntu, Adwaita Sans,
/// Cantarell, Noto Sans, … — until something like DejaVu Sans answers. gpui
/// caches the miss but re-derives an `anyhow!` error from it on **every**
/// `resolve_font`, i.e. per text run per frame: nine failed lookups, nine
/// error allocations (and, before `obs::init` disabled it, nine backtrace
/// captures) for every string on screen. Resolving once here, at startup,
/// gives every later lookup a first-try cache hit.
///
/// Same story for `"monospace"`: a CSS generic name that fontconfig
/// understands but gpui's family matcher does not, so code previews on
/// Linux silently fell through to a proportional face. Callers should use
/// `cx.theme().mono_font_family` rather than the generic name.
///
/// Call once, right after `gpui_component::init`, before any window opens.
/// No-op on platforms whose `.SystemUIFont` resolves natively.
pub fn install_platform_font_families(cx: &mut gpui::App) {
    #[cfg(target_os = "linux")]
    {
        let text_system = cx.text_system().clone();
        // If the virtual name resolves to a real face (IBM Plex Sans is
        // installed), leave the platform default alone.
        if font_family_installed(&text_system, ".SystemUIFont") {
            return;
        }
        // Order: desktop-native families first (GNOME 47+ Adwaita Sans,
        // Ubuntu, GNOME Cantarell, KDE Noto Sans), then the families every
        // distro ships as dependencies of something, then the metric
        // clones. A KDE session prefers Noto Sans over the Ubuntu brand
        // font even on Kubuntu.
        let kde = std::env::var("XDG_CURRENT_DESKTOP")
            .map(|d| d.to_ascii_uppercase().contains("KDE"))
            .unwrap_or(false);
        let ui_candidates: &[&str] = if kde {
            &["Noto Sans", "Adwaita Sans", "Ubuntu", "Cantarell"]
        } else {
            &["Adwaita Sans", "Ubuntu", "Cantarell", "Noto Sans"]
        };
        let ui_common: &[&str] = &[
            "DejaVu Sans",
            "Liberation Sans",
            "Nimbus Sans",
            "Arial",
            "Helvetica",
        ];
        let mono_candidates: &[&str] = &[
            "Ubuntu Mono",
            "Noto Sans Mono",
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Nimbus Mono PS",
            "Courier New",
        ];
        let ui = ui_candidates
            .iter()
            .chain(ui_common)
            .copied()
            .find(|f| font_family_installed(&text_system, f));
        let mono = mono_candidates
            .iter()
            .copied()
            .find(|f| font_family_installed(&text_system, f));
        let theme = gpui_component::Theme::global_mut(cx);
        if let Some(ui) = ui {
            theme.font_family = ui.into();
        }
        if let Some(mono) = mono {
            theme.mono_font_family = mono.into();
        }
        crate::log_info!(
            90,
            "fonts: .SystemUIFont unresolved on this system; ui={} mono={}",
            ui.unwrap_or("<gpui fallback>"),
            mono.unwrap_or("<theme default>")
        );
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cx;
    }
}

/// Is `family` a real, loadable face on this machine? `resolve_font` never
/// fails (it falls back), so ask which family the resolved id belongs to
/// and compare. Virtual names (`.SystemUIFont`) count as installed when the
/// face they map to exists — the cache maps the virtual name itself to the
/// id, so the round trip returns the virtual name.
#[cfg(target_os = "linux")]
fn font_family_installed(text_system: &gpui::TextSystem, family: &str) -> bool {
    let id = text_system.resolve_font(&gpui::font(family));
    text_system
        .get_font_for_id(id)
        .is_some_and(|f| f.family.as_ref() == family)
}
