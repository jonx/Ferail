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
        self.overflow_hidden().whitespace_nowrap().text_ellipsis_middle()
    }
}

impl<T: Styled + Sized> TruncateMiddle for T {}
