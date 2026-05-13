//! Settings-grade widgets — Toggle, SegmentedControl, Slider, plus the
//! composition helpers (settings rows, sidebar nav, preview tiles) that
//! the Settings screen is built from.
//!
//! All widgets are **stateless** paint + hit-test pairs: the caller holds
//! the model and asks the widget where to draw and what was clicked. No
//! mouse capture, no internal animation. This matches the rest of
//! `feraille-controls` and the paint-is-read-only contract.
//!
//! These are deliberately desktop-scaled — toggles are 28×16, not the
//! iOS 38×22 that dominated the previous Settings layout.
//!
//! See the design brief in conversation history for the rationale on
//! every dimension and the IA decisions ("show consequence", "snap
//! stops with disclosure for raw px", etc.).

use feraille_design::{FontWeight, Tokens};
use feraille_render::{Point, Rect, Renderer, TextStyle};

use super::draw::{
    fill_circle, fill_rounded_rect, paint_card, stroke_rounded_rect, text_y_center,
};

// =============================================================================
// Toggle
// =============================================================================

/// Recommended toggle dimensions for desktop UIs. iOS-sized toggles look
/// out of proportion next to text and 28×16 controls.
pub const TOGGLE_W: f32 = 28.0;
pub const TOGGLE_H: f32 = 16.0;

/// Paint an iOS-style toggle, desktop-sized.
///
/// ON: accent capsule with the thumb on the right.
/// OFF: `bg.layer3` capsule with the thumb on the left and a subtle
/// border so it reads on a light card.
pub fn paint_toggle(renderer: &mut dyn Renderer, tokens: &Tokens, rect: Rect, on: bool) {
    let radius = rect.size.height / 2.0;
    if on {
        fill_rounded_rect(renderer, rect, radius, tokens.accent.fill);
    } else {
        stroke_rounded_rect(
            renderer,
            rect,
            radius,
            1.0,
            tokens.border.default,
            tokens.bg.layer3,
        );
    }
    let pad = 2.0;
    let thumb_r = radius - pad;
    let thumb_cy = rect.top() + radius;
    let thumb_cx = if on {
        rect.right() - radius
    } else {
        rect.left() + radius
    };
    // Drop-shadow approximation: 1-DIP-offset darker disc behind the thumb.
    fill_circle(
        renderer,
        Point::new(thumb_cx, thumb_cy + 0.5),
        thumb_r + 0.5,
        feraille_design::Color::rgba(0, 0, 0, 28),
    );
    fill_circle(
        renderer,
        Point::new(thumb_cx, thumb_cy),
        thumb_r,
        tokens.fg.on_accent,
    );
}

pub fn toggle_hit(rect: Rect, p: Point) -> bool {
    rect.contains(p)
}

// =============================================================================
// SegmentedControl
// =============================================================================

/// Compute the rect for each segment given an outer strip rect and a
/// segment count. Equal-width division; the rightmost segment absorbs
/// any rounding slack.
pub fn segment_rects(strip: Rect, count: usize) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    let seg_w = strip.size.width / count as f32;
    (0..count)
        .map(|i| {
            let x = strip.left() + seg_w * i as f32;
            let w = if i == count - 1 {
                strip.right() - x
            } else {
                seg_w
            };
            Rect::new(x, strip.top(), w, strip.size.height)
        })
        .collect()
}

/// Paint a horizontal segmented control. The strip background is
/// `bg.layer3` (subtle inset); the active segment overlays a `bg.layer1`
/// pill so it pops, matching the macOS Big Sur+ look.
pub fn paint_segmented(
    renderer: &mut dyn Renderer,
    tokens: &Tokens,
    strip: Rect,
    labels: &[&str],
    selected: usize,
) {
    fill_rounded_rect(renderer, strip, tokens.radius.sm + 2.0, tokens.bg.layer3);
    let rects = segment_rects(strip, labels.len());
    for (i, (rect, label)) in rects.iter().zip(labels.iter()).enumerate() {
        let is_selected = i == selected;
        if is_selected {
            // Inset the active pill by 2 DIPs so the layer3 strip
            // shows as a 2-DIP frame.
            let pad = 2.0;
            let pill = Rect::new(
                rect.left() + pad,
                rect.top() + pad,
                (rect.size.width - 2.0 * pad).max(0.0),
                (rect.size.height - 2.0 * pad).max(0.0),
            );
            // Soft 1-DIP-offset shadow under the pill — gives the active
            // segment a tactile lift against the inset strip.
            fill_rounded_rect(
                renderer,
                Rect::new(pill.left(), pill.top() + 1.0, pill.size.width, pill.size.height),
                tokens.radius.sm,
                feraille_design::Color::rgba(0, 0, 0, 18),
            );
            fill_rounded_rect(renderer, pill, tokens.radius.sm, tokens.bg.layer1);
        }
        let style = TextStyle {
            size: tokens.text.sm,
            weight: if is_selected {
                FontWeight::SemiBold
            } else {
                FontWeight::Regular
            },
            color: if is_selected {
                tokens.fg.primary
            } else {
                tokens.fg.secondary
            },
        };
        let m = renderer.measure_text(label, style);
        renderer.draw_text(
            Point::new(
                rect.left() + (rect.size.width - m.width) / 2.0,
                text_y_center(*rect, tokens.text.sm),
            ),
            label,
            style,
        );
    }
}

pub fn segmented_hit(strip: Rect, count: usize, p: Point) -> Option<usize> {
    if !strip.contains(p) || count == 0 {
        return None;
    }
    for (i, rect) in segment_rects(strip, count).into_iter().enumerate() {
        if rect.contains(p) {
            return Some(i);
        }
    }
    None
}

// =============================================================================
// Slider
// =============================================================================

/// Recommended track height for a desktop slider. Thumb extends above/
/// below so the visible row needs ~22 DIPs to contain it.
pub const SLIDER_TRACK_H: f32 = 4.0;
pub const SLIDER_THUMB_R: f32 = 7.0;

/// Compute the centre x of the thumb for a normalized `value` in
/// `0.0..=1.0` along `track`.
pub fn slider_thumb_x(track: Rect, value: f32) -> f32 {
    track.left() + track.size.width * value.clamp(0.0, 1.0)
}

/// Paint a horizontal slider. Track is `bg.layer3` capsule; the fill
/// from `track.left()` to the thumb is `accent.fill`. Thumb is a white
/// disc with a 1-DIP subtle border. Optional `snap_stops` (normalized
/// `0..=1`) paint as small notches across the track.
pub fn paint_slider(
    renderer: &mut dyn Renderer,
    tokens: &Tokens,
    track: Rect,
    value: f32,
    snap_stops: &[f32],
) {
    let radius = track.size.height / 2.0;
    fill_rounded_rect(renderer, track, radius, tokens.bg.layer3);
    let filled_w = track.size.width * value.clamp(0.0, 1.0);
    if filled_w > 0.0 {
        fill_rounded_rect(
            renderer,
            Rect::new(track.left(), track.top(), filled_w, track.size.height),
            radius,
            tokens.accent.fill,
        );
    }
    // Optional snap stops as small notches across the track.
    for stop in snap_stops {
        let cx = track.left() + track.size.width * stop.clamp(0.0, 1.0);
        renderer.fill_rect(
            Rect::new(cx - 0.5, track.top() - 2.0, 1.0, track.size.height + 4.0),
            tokens.border.default,
        );
    }
    // Thumb: drop-shadow approximation + white disc + 1-DIP border ring.
    let thumb_cx = slider_thumb_x(track, value);
    let thumb_cy = track.top() + radius;
    fill_circle(
        renderer,
        Point::new(thumb_cx, thumb_cy + 0.5),
        SLIDER_THUMB_R + 0.5,
        feraille_design::Color::rgba(0, 0, 0, 30),
    );
    fill_circle(
        renderer,
        Point::new(thumb_cx, thumb_cy),
        SLIDER_THUMB_R,
        tokens.border.subtle,
    );
    fill_circle(
        renderer,
        Point::new(thumb_cx, thumb_cy),
        SLIDER_THUMB_R - 0.8,
        tokens.bg.layer1,
    );
}

/// Hit-test a click on the slider track. Returns the normalized
/// position `0..=1` for the click, or `None` if outside.
///
/// Generous vertical hit area: `±10` DIPs around the track so the user
/// doesn't have to pixel-aim at a 4-DIP-tall surface.
pub fn slider_track_hit(track: Rect, p: Point) -> Option<f32> {
    let dy = (p.y - (track.top() + track.size.height / 2.0)).abs();
    if dy > 12.0 {
        return None;
    }
    if p.x < track.left() - 8.0 || p.x > track.right() + 8.0 {
        return None;
    }
    let t = ((p.x - track.left()) / track.size.width).clamp(0.0, 1.0);
    Some(t)
}

/// Hit-test a click on the thumb. Forgiving 22-DIP square so the user
/// can grab it without precision aiming.
pub fn slider_thumb_hit(track: Rect, value: f32, p: Point) -> bool {
    let cx = slider_thumb_x(track, value);
    let cy = track.top() + track.size.height / 2.0;
    (p.x - cx).abs() <= 11.0 && (p.y - cy).abs() <= 11.0
}

// =============================================================================
// SettingsRow + SettingsGroup composition
// =============================================================================

/// Layout for a single settings row: title (always), optional
/// description below the title, and a right-aligned control slot.
pub struct RowLayout {
    pub row: Rect,
    pub title_pos: Point,
    /// `None` when the row has no description (rare — the brief says
    /// every setting should have one).
    pub desc_pos: Option<Point>,
    /// Where the control sits — right-aligned inside the row.
    pub control_slot: Rect,
}

/// Standard row metrics. Single-line rows (no description) are 44
/// DIPs tall — desktop comfortable, not iOS-airy. With description,
/// rows are 60 DIPs (room for a 13pt description below a 13pt title).
pub const ROW_H_SINGLE: f32 = 44.0;
pub const ROW_H_DESCRIBED: f32 = 60.0;

/// Compute a row layout inside `row` (full row rect including the
/// horizontal card inset). `control_w` is the visible width of the
/// control widget — the slot is sized to that width, right-aligned.
pub fn compute_settings_row(
    row: Rect,
    tokens: &Tokens,
    has_description: bool,
    control_w: f32,
    inset: f32,
) -> RowLayout {
    let title_pos = if has_description {
        Point::new(row.left() + inset, row.top() + tokens.space.sm + 2.0)
    } else {
        Point::new(
            row.left() + inset,
            text_y_center(row, tokens.text.md),
        )
    };
    let desc_pos = if has_description {
        Some(Point::new(
            row.left() + inset,
            row.top() + tokens.space.sm + tokens.text.md + 6.0,
        ))
    } else {
        None
    };
    let control_h = row.size.height.min(28.0);
    let control_slot = Rect::new(
        row.right() - inset - control_w,
        row.top() + (row.size.height - control_h) / 2.0,
        control_w,
        control_h,
    );
    RowLayout {
        row,
        title_pos,
        desc_pos,
        control_slot,
    }
}

/// Paint a row's text content (title + optional description). The
/// caller paints the control into `layout.control_slot`.
pub fn paint_settings_row_text(
    renderer: &mut dyn Renderer,
    tokens: &Tokens,
    layout: &RowLayout,
    title: &str,
    description: Option<&str>,
) {
    let title_style = TextStyle {
        size: tokens.text.md,
        weight: FontWeight::Medium,
        color: tokens.fg.primary,
    };
    renderer.draw_text(layout.title_pos, title, title_style);
    if let (Some(pos), Some(desc)) = (layout.desc_pos, description) {
        let desc_style = TextStyle {
            size: tokens.text.sm,
            weight: FontWeight::Regular,
            color: tokens.fg.secondary,
        };
        renderer.draw_text(pos, desc, desc_style);
    }
}

/// Paint the card chrome for a settings group. Rows are placed inside
/// by the caller; this draws the surface, group title (above the
/// card), and optional group description.
pub fn paint_group_header(
    renderer: &mut dyn Renderer,
    tokens: &Tokens,
    body_left: f32,
    y: f32,
    title: &str,
) -> f32 {
    let style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::SemiBold,
        color: tokens.fg.primary,
    };
    renderer.draw_text(Point::new(body_left, y), title, style);
    y + tokens.text.sm + 8.0
}

/// Paint the card body — just calls `paint_card`. Wrapper so callers
/// reading `paint_group_card` see the intent.
pub fn paint_group_card(
    renderer: &mut dyn Renderer,
    tokens: &Tokens,
    card: Rect,
) {
    paint_card(renderer, tokens, card);
}

// =============================================================================
// Sidebar nav item
// =============================================================================

/// Paint a single sidebar nav item (icon glyph + label). Active state
/// fills with `accent.subtle` and a 2-DIP leading bar in `accent.fill`.
/// Inactive items hover via `bg.layer3` (caller passes `hovered`).
pub fn paint_sidebar_nav_item(
    renderer: &mut dyn Renderer,
    tokens: &Tokens,
    rect: Rect,
    icon_glyph: &str,
    label: &str,
    selected: bool,
    hovered: bool,
) {
    if selected {
        fill_rounded_rect(renderer, rect, tokens.radius.sm, tokens.accent.subtle);
        // 2-DIP leading accent bar.
        renderer.fill_rect(
            Rect::new(rect.left() + 2.0, rect.top() + 4.0, 2.0, rect.size.height - 8.0),
            tokens.accent.fill,
        );
    } else if hovered {
        fill_rounded_rect(renderer, rect, tokens.radius.sm, tokens.bg.layer3);
    }
    let inset_x = rect.left() + 14.0;
    let label_x = if icon_glyph.is_empty() {
        inset_x
    } else {
        let icon_style = TextStyle {
            size: tokens.text.md,
            weight: FontWeight::Regular,
            color: if selected {
                tokens.accent.fill
            } else {
                tokens.fg.secondary
            },
        };
        renderer.draw_text(
            Point::new(inset_x, text_y_center(rect, tokens.text.md)),
            icon_glyph,
            icon_style,
        );
        inset_x + 22.0
    };
    let label_style = TextStyle {
        size: tokens.text.md,
        weight: if selected {
            FontWeight::SemiBold
        } else {
            FontWeight::Regular
        },
        color: tokens.fg.primary,
    };
    renderer.draw_text(
        Point::new(label_x, text_y_center(rect, tokens.text.md)),
        label,
        label_style,
    );
}

// =============================================================================
// PreviewTile — mini-window swatches for the theme picker
// =============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Light,
    Dark,
    Auto,
}

/// Paint a preview tile — a mini-window-chrome rendering that shows
/// what the theme will produce. Used by the theme picker so the user
/// sees consequence before they commit.
///
/// Selected tiles get a 2-DIP `accent.fill` border ring; unselected
/// tiles get a 1-DIP `border.subtle` outline so they all read as the
/// same control class.
pub fn paint_preview_tile(
    renderer: &mut dyn Renderer,
    tokens: &Tokens,
    rect: Rect,
    kind: PreviewKind,
    selected: bool,
    label: &str,
) {
    // Outer border ring — selected: accent, unselected: subtle.
    let border = if selected {
        tokens.accent.fill
    } else {
        tokens.border.subtle
    };
    let border_w = if selected { 2.0 } else { 1.0 };
    stroke_rounded_rect(
        renderer,
        rect,
        tokens.radius.md,
        border_w,
        border,
        feraille_design::Color::rgba(0, 0, 0, 0),
    );
    // Inner artwork area — clip to the tile minus a generous label
    // strip at the bottom.
    let label_h = tokens.text.sm + tokens.space.sm + 6.0;
    let art = Rect::new(
        rect.left() + border_w + 4.0,
        rect.top() + border_w + 4.0,
        (rect.size.width - 2.0 * (border_w + 4.0)).max(0.0),
        (rect.size.height - 2.0 * (border_w + 4.0) - label_h).max(0.0),
    );
    paint_preview_artwork(renderer, tokens, art, kind);

    // Label band beneath the artwork.
    let label_style = TextStyle {
        size: tokens.text.sm,
        weight: if selected {
            FontWeight::SemiBold
        } else {
            FontWeight::Regular
        },
        color: tokens.fg.primary,
    };
    let m = renderer.measure_text(label, label_style);
    renderer.draw_text(
        Point::new(
            rect.left() + (rect.size.width - m.width) / 2.0,
            rect.bottom() - label_h + (label_h - tokens.text.sm) / 2.0 - 2.0,
        ),
        label,
        label_style,
    );
}

/// Render a tiny stylized "mini Finder window" inside `art`. Light and
/// Dark show their respective palette directly; Auto shows a 50/50
/// split, mirroring the macOS System Settings tile.
fn paint_preview_artwork(
    renderer: &mut dyn Renderer,
    tokens: &Tokens,
    art: Rect,
    kind: PreviewKind,
) {
    use feraille_design::{Color, Theme, Tokens as T};
    let light = T::for_theme(Theme::Light);
    let dark = T::for_theme(Theme::Dark);
    let paint_half = |renderer: &mut dyn Renderer, region: Rect, palette: &T| {
        // Window background
        fill_rounded_rect(renderer, region, tokens.radius.sm, palette.bg.base);
        // Titlebar strip
        let title_h = (region.size.height * 0.18).max(6.0);
        let title_rect =
            Rect::new(region.left(), region.top(), region.size.width, title_h);
        fill_rounded_rect(renderer, title_rect, tokens.radius.sm, palette.bg.layer1);
        // Three traffic light dots inside the titlebar
        let dot_r = (title_h * 0.28).max(1.5);
        let dot_y = title_rect.top() + title_h / 2.0;
        let dot_colors = [
            Color::rgb(0xFF, 0x60, 0x57),
            Color::rgb(0xFF, 0xBD, 0x2E),
            Color::rgb(0x28, 0xC9, 0x40),
        ];
        for (i, c) in dot_colors.iter().enumerate() {
            let dot_x = title_rect.left() + dot_r * (2.5 + 2.6 * i as f32);
            fill_circle(renderer, Point::new(dot_x, dot_y), dot_r, *c);
        }
        // Sidebar slab
        let sidebar_w = region.size.width * 0.30;
        let sidebar_rect = Rect::new(
            region.left(),
            title_rect.bottom(),
            sidebar_w,
            region.bottom() - title_rect.bottom(),
        );
        renderer.fill_rect(sidebar_rect, palette.bg.layer1);
        // A handful of fake rows in the file pane
        let row_h = (region.size.height * 0.10).max(2.0);
        let pane_left = sidebar_rect.right() + 2.0;
        let pane_right = region.right() - 4.0;
        let mut y = title_rect.bottom() + 4.0;
        for i in 0..3 {
            let row_y = y;
            // Selected highlight on the second row
            if i == 1 {
                renderer.fill_rect(
                    Rect::new(pane_left, row_y, pane_right - pane_left, row_h),
                    palette.accent.subtle,
                );
            }
            // Filename ghost — short horizontal bar
            let bar_w = (pane_right - pane_left) * 0.55;
            renderer.fill_rect(
                Rect::new(
                    pane_left + 4.0,
                    row_y + row_h / 2.0 - 0.5,
                    bar_w,
                    1.0,
                ),
                palette.fg.secondary,
            );
            y += row_h + 2.0;
        }
    };
    match kind {
        PreviewKind::Light => paint_half(renderer, art, &light),
        PreviewKind::Dark => paint_half(renderer, art, &dark),
        PreviewKind::Auto => {
            let half_w = art.size.width / 2.0;
            let left = Rect::new(art.left(), art.top(), half_w, art.size.height);
            let right =
                Rect::new(art.left() + half_w, art.top(), art.size.width - half_w, art.size.height);
            paint_half(renderer, left, &light);
            paint_half(renderer, right, &dark);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_rects_cover_strip() {
        let strip = Rect::new(0.0, 0.0, 300.0, 28.0);
        let rects = segment_rects(strip, 3);
        assert_eq!(rects.len(), 3);
        assert!((rects[0].left() - 0.0).abs() < 0.01);
        assert!((rects[2].right() - 300.0).abs() < 0.01);
        let total: f32 = rects.iter().map(|r| r.size.width).sum();
        assert!((total - 300.0).abs() < 0.5);
    }

    #[test]
    fn segmented_hit_returns_correct_index() {
        let strip = Rect::new(0.0, 0.0, 300.0, 28.0);
        assert_eq!(segmented_hit(strip, 3, Point::new(50.0, 14.0)), Some(0));
        assert_eq!(segmented_hit(strip, 3, Point::new(150.0, 14.0)), Some(1));
        assert_eq!(segmented_hit(strip, 3, Point::new(250.0, 14.0)), Some(2));
        assert_eq!(segmented_hit(strip, 3, Point::new(-10.0, 14.0)), None);
    }

    #[test]
    fn slider_thumb_x_clamps_to_track() {
        let track = Rect::new(10.0, 50.0, 200.0, 4.0);
        assert!((slider_thumb_x(track, 0.0) - 10.0).abs() < 0.01);
        assert!((slider_thumb_x(track, 1.0) - 210.0).abs() < 0.01);
        assert!((slider_thumb_x(track, 0.5) - 110.0).abs() < 0.01);
        assert!((slider_thumb_x(track, -0.5) - 10.0).abs() < 0.01);
    }

    #[test]
    fn slider_track_hit_outside_vertical_returns_none() {
        let track = Rect::new(0.0, 100.0, 200.0, 4.0);
        assert_eq!(slider_track_hit(track, Point::new(100.0, 200.0)), None);
        let hit = slider_track_hit(track, Point::new(100.0, 102.0));
        assert!(hit.is_some());
        let t = hit.unwrap();
        assert!((t - 0.5).abs() < 0.05);
    }

    #[test]
    fn row_with_description_is_taller_than_single() {
        assert!(ROW_H_DESCRIBED > ROW_H_SINGLE);
    }
}
