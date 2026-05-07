//! Treemap visual — stateless renderer for a precomputed
//! `Vec<TreemapRect>` from [`feraille_disk_usage::layout`]. The host
//! owns rect cache, hover, and selection state; this module only
//! paints and hit-tests.
//!
//! Paint contract is read-only. No I/O, no allocations on hot paths
//! beyond the per-rect `format!` for the size suffix when the rect is
//! big enough to show one — and that runs at most a few hundred times
//! per frame even on giant trees because we cap labels by rect size.

use std::collections::HashSet;

use feraille_core::NodeId;
use feraille_design::{Color, FontWeight, Tokens};
use feraille_disk_usage::{FileCategory, NodeKind, ScanState, TreemapRect};
use feraille_render::{Point, Rect, Renderer, TextStyle};

/// How nested cells are tinted on top of the depth-blue base.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum TreemapColoring {
    /// File-category overlay (Image/Video/Audio/Archive/Doc/Exec).
    /// Containers stay neutral; only leaves get tinted.
    #[default]
    Category,
    /// Plain depth-only blue palette, no overlay. Useful for printing
    /// and for screenshots where category color would be noise.
    DepthOnly,
    /// Heatmap by file mtime: recent files glow accent-blue, old
    /// files trend toward warm orange/red. Surfaces stale junk that
    /// the user can probably nuke without looking.
    AgeHeat,
}

/// Pointer event types — the host lifts these into selection /
/// drilldown / context-menu actions. Iter-6.2 wires them into App.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TreemapEvent {
    Click {
        node_id: NodeId,
        modifiers: ClickModifiers,
    },
    DoubleClick {
        node_id: NodeId,
    },
    ContextMenu {
        node_id: NodeId,
        point: Point,
    },
    Hover {
        node_id: Option<NodeId>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClickModifiers {
    pub cmd: bool,
    pub shift: bool,
}

/// Minimum rect width for any text overlay.
const LABEL_MIN_W: f32 = 56.0;
/// Minimum rect height to show a label at all.
const LABEL_MIN_H: f32 = 18.0;
/// Width above which the label gets a "  — 1.2 MB" size suffix.
const LABEL_SIZE_W: f32 = 140.0;
/// Outer-rect inset for the leaf-label paint position.
const LABEL_INSET: f32 = 4.0;

/// Returns the topmost rect under `(px, py)`, walking the slice
/// back-to-front so deepest cells win. Mirrors
/// [`feraille_disk_usage::hit_test`] but exposed at the control layer
/// so callers don't need a transitive `feraille-disk-usage` dep.
pub fn hit_test_at(rects: &[TreemapRect], px: f32, py: f32) -> Option<&TreemapRect> {
    feraille_disk_usage::hit_test(rects, px, py)
}

/// Paint the treemap inside `bounds`. The host clips to `bounds` so
/// rects that extend outside (shouldn't happen, but defensive) don't
/// paint over neighbouring panes.
///
/// `name_for` is invoked at most once per rect that's large enough to
/// show a label. Returning an empty string suppresses the label for
/// that rect — useful when the host doesn't have a name cached yet
/// (e.g. fact stream hasn't reached that node) or wants to hide names
/// for files smaller than some threshold.
pub fn paint(
    rects: &[TreemapRect],
    bounds: Rect,
    hovered: Option<NodeId>,
    selected: &HashSet<NodeId>,
    coloring: TreemapColoring,
    // When `Some`, file rects whose `file_category` doesn't match
    // are dimmed so the matching ones pop. Containers always render
    // at full strength.
    filter_category: Option<FileCategory>,
    tokens: &Tokens,
    renderer: &mut dyn Renderer,
    mut name_for: impl FnMut(NodeId) -> String,
) {
    if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
        return;
    }
    renderer.push_clip(bounds);

    // Background — distinguishes the treemap pane from a transparent
    // backdrop when nothing has been laid out yet.
    renderer.fill_rect(bounds, tokens.bg.layer1);

    if rects.is_empty() {
        renderer.pop_clip();
        return;
    }

    let label_style = TextStyle {
        size: tokens.text.sm,
        weight: FontWeight::SemiBold,
        color: tokens.fg.on_accent,
    };

    for r in rects {
        let rect = Rect::new(r.x, r.y, r.width, r.height);
        if rect.size.width < 1.0 || rect.size.height < 1.0 {
            continue;
        }

        // Fill: depth-blue base, optional category tint for leaves.
        let fill = base_fill(r, coloring, tokens);
        renderer.fill_rect(rect, fill);

        // Border: subtle for normal, default for hovered, focus for selected.
        let is_selected = selected.contains(&r.node_id);
        let is_hovered = hovered == Some(r.node_id);
        let (stroke_w, stroke_color) = if is_selected {
            (2.0, tokens.border.focus)
        } else if is_hovered {
            (1.5, tokens.border.default)
        } else {
            (1.0, tokens.border.subtle)
        };
        renderer.stroke_rect(rect, stroke_w, stroke_color);

        // Scanning overlay — stripes look great but are expensive; iter-6.1
        // ships a single subtle dim instead. Iter-6.3 can revisit.
        if matches!(r.scan_state, ScanState::Scanning) {
            renderer.fill_rect(rect, Color::rgba(0, 0, 0, 28));
        }

        // Category filter dim. Files that don't match the filter get
        // a heavy translucent backdrop so the matching ones visually
        // stand out without being moved or relayouted.
        let is_dimmed = match (filter_category, r.kind) {
            (Some(want), NodeKind::File) => want != r.file_category,
            _ => false,
        };
        if is_dimmed {
            renderer.fill_rect(rect, Color::rgba(0, 0, 0, 140));
        }

        // Label — only when the rect is large enough that text won't
        // overlap its border. Containers get a label; leaves get a
        // label + optional size suffix. Aware of the rect's fill so
        // dark backgrounds still get readable text.
        if rect.size.width >= LABEL_MIN_W && rect.size.height >= LABEL_MIN_H {
            let name = name_for(r.node_id);
            if !name.is_empty() {
                let mut text = name;
                if matches!(r.kind, NodeKind::File) && rect.size.width >= LABEL_SIZE_W {
                    let size = humanize_bytes_short(r.size_bytes);
                    text.push_str("  ");
                    text.push_str(&size);
                }

                let style = label_style_for_fill(label_style, fill);
                let metrics = renderer.measure_text(&text, style);
                let max_text_w = (rect.size.width - LABEL_INSET * 2.0).max(0.0);
                let drawn = if metrics.width <= max_text_w {
                    text
                } else {
                    truncate_with_ellipsis(&text, max_text_w, style, renderer)
                };
                if !drawn.is_empty() {
                    renderer.draw_text(
                        Point::new(rect.left() + LABEL_INSET, rect.top() + LABEL_INSET),
                        &drawn,
                        style,
                    );
                }
            }
        }
    }

    renderer.pop_clip();
}

/// Resolve the base fill for a rect: depth-blue luminance shift on top
/// of the accent color, plus an optional category tint for leaves.
/// Containers stay un-tinted regardless of `coloring` so the nested
/// hierarchy reads as concentric depth.
fn base_fill(r: &TreemapRect, coloring: TreemapColoring, tokens: &Tokens) -> Color {
    let depth_amount = (r.depth.min(6) as f32) * 0.10; // 0.0..=0.6
    let depth_base = lighten_toward_layer1(tokens.accent.fill, tokens, depth_amount);
    match (coloring, r.kind) {
        (TreemapColoring::Category, NodeKind::File) => {
            blend(depth_base, category_tint(r.file_category, tokens), 0.45)
        }
        (TreemapColoring::AgeHeat, NodeKind::File) => {
            // 0.0 = recent (cool, accent.fill); 1.0 = ancient (warm,
            // status.danger). 365 days = halfway through the gradient
            // so a ~2-year-old file lands deep in the warm zone.
            let age = age_factor(r.mtime);
            let cool = tokens.accent.fill;
            let warm = tokens.status.danger;
            let target = blend(cool, warm, age);
            blend(depth_base, target, 0.55)
        }
        _ => depth_base,
    }
}

/// Map an mtime to 0.0 (recent) … 1.0 (ancient) for the heatmap.
/// `None` mtime gets 0.5 — neither hot nor cold — so files we
/// couldn't stat don't pretend to be fresh.
fn age_factor(mtime: Option<std::time::SystemTime>) -> f32 {
    let Some(mt) = mtime else { return 0.5 };
    let elapsed = std::time::SystemTime::now()
        .duration_since(mt)
        .ok()
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    const TWO_YEARS: f64 = 60.0 * 60.0 * 24.0 * 730.0;
    (elapsed / TWO_YEARS).clamp(0.0, 1.0) as f32
}

/// Map a `FileCategory` to a tint color. Reuses `tokens.magic` where
/// the colors line up so categories on the treemap match the icon
/// tints in the file list — visual consistency users can exploit.
fn category_tint(category: FileCategory, tokens: &Tokens) -> Color {
    match category {
        FileCategory::Image => tokens.magic.image,
        FileCategory::Video | FileCategory::Audio => tokens.magic.media,
        FileCategory::Archive => tokens.magic.archive,
        FileCategory::Document => tokens.magic.doc,
        FileCategory::Executable => tokens.magic.code,
        FileCategory::Other => tokens.magic.data,
    }
}

/// Linear blend between `a` (alpha-correct) and `b` by `t` in 0..1.
/// Both inputs should be opaque; returns an opaque color. Used for
/// category-tint mixing where alpha blending against the framebuffer
/// would compound the depth-blue twice.
fn blend(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| -> u8 {
        let v = x as f32 * (1.0 - t) + y as f32 * t;
        v.round().clamp(0.0, 255.0) as u8
    };
    Color::rgba(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b), 0xFF)
}

/// Lighten `c` toward `tokens.bg.layer1` by `t` (0..1). Used to make
/// each nested level a touch paler than its parent so the boundary
/// reads even at narrow strokes.
fn lighten_toward_layer1(c: Color, tokens: &Tokens, t: f32) -> Color {
    blend(c, tokens.bg.layer1, t)
}

/// Pick the label color based on the cell fill — white on dark cells,
/// dark on light cells. Cheap luminance test (Rec.601 Y), no need to
/// be perceptually exact; this is purely for legibility.
fn label_style_for_fill(base: TextStyle, fill: Color) -> TextStyle {
    let lum = 0.299 * fill.r as f32 + 0.587 * fill.g as f32 + 0.114 * fill.b as f32;
    let mut s = base;
    s.color = if lum < 140.0 {
        Color::rgb(0xFF, 0xFF, 0xFF)
    } else {
        Color::rgb(0x10, 0x10, 0x10)
    };
    s
}

/// Truncate `text` to fit `max_w` with a trailing "…". Returns "" if
/// even the ellipsis won't fit. Linear scan; fine for a few hundred
/// labels per frame.
fn truncate_with_ellipsis(
    text: &str,
    max_w: f32,
    style: TextStyle,
    renderer: &mut dyn Renderer,
) -> String {
    const ELLIPSIS: &str = "\u{2026}";
    if max_w <= 0.0 {
        return String::new();
    }
    if renderer.measure_text(ELLIPSIS, style).width > max_w {
        return String::new();
    }
    // Walk char-by-char from the start, stopping when adding the next
    // char would push past `max_w - ellipsis_w`. Slow but correct.
    let ellipsis_w = renderer.measure_text(ELLIPSIS, style).width;
    let budget = (max_w - ellipsis_w).max(0.0);
    let mut acc = String::new();
    for ch in text.chars() {
        let probe = {
            let mut t = acc.clone();
            t.push(ch);
            renderer.measure_text(&t, style).width
        };
        if probe > budget {
            break;
        }
        acc.push(ch);
    }
    if acc.is_empty() {
        return String::new();
    }
    acc.push_str(ELLIPSIS);
    acc
}

/// Compact byte formatting for inline labels. Mirrors the CLI's
/// `humanize` but inlined here so the control crate stays dep-light.
fn humanize_bytes_short(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use feraille_design::Tokens;

    fn nid(raw: u64) -> NodeId {
        NodeId::from_raw(raw).expect("nonzero")
    }

    fn rect(id: u64, x: f32, y: f32, w: f32, h: f32, depth: u32, fc: FileCategory) -> TreemapRect {
        TreemapRect {
            node_id: nid(id),
            x,
            y,
            width: w,
            height: h,
            depth,
            size_bytes: 1024 * (id + 1),
            scan_state: ScanState::Complete,
            has_children: false,
            kind: NodeKind::File,
            file_category: fc,
            mtime: None,
        }
    }

    #[test]
    fn category_tint_maps_each_variant() {
        let t = Tokens::light();
        // Just assert each variant returns a distinct color from at
        // least one other; no need to lock specific RGB values.
        let img = category_tint(FileCategory::Image, &t);
        let aud = category_tint(FileCategory::Audio, &t);
        let exe = category_tint(FileCategory::Executable, &t);
        let oth = category_tint(FileCategory::Other, &t);
        assert_ne!(img, exe);
        assert_ne!(aud, oth);
    }

    #[test]
    fn label_color_inverts_for_light_and_dark_fills() {
        let base = TextStyle {
            size: 12.0,
            weight: FontWeight::Regular,
            color: Color::rgb(0, 0, 0),
        };
        let on_dark = label_style_for_fill(base, Color::rgb(20, 30, 90));
        let on_light = label_style_for_fill(base, Color::rgb(230, 230, 230));
        assert_eq!(on_dark.color, Color::rgb(0xFF, 0xFF, 0xFF));
        assert_eq!(on_light.color, Color::rgb(0x10, 0x10, 0x10));
    }

    #[test]
    fn humanize_bytes_short_matches_cli_humanizer() {
        assert_eq!(humanize_bytes_short(0), "0 B");
        assert_eq!(humanize_bytes_short(1024), "1.0 KB");
        assert_eq!(humanize_bytes_short(1024 * 1024 * 5), "5.0 MB");
    }

    #[test]
    fn blend_50_50_mixes_components_evenly() {
        let red = Color::rgb(0xFF, 0x00, 0x00);
        let blue = Color::rgb(0x00, 0x00, 0xFF);
        let mid = blend(red, blue, 0.5);
        assert!((mid.r as i32 - 128).abs() <= 1);
        assert_eq!(mid.g, 0);
        assert!((mid.b as i32 - 128).abs() <= 1);
    }

    #[test]
    fn hit_test_at_uses_disk_usage_implementation() {
        let rs = vec![
            rect(1, 0.0, 0.0, 100.0, 100.0, 0, FileCategory::Other),
            rect(2, 10.0, 10.0, 20.0, 20.0, 1, FileCategory::Image),
        ];
        // Inside the leaf — must pick the leaf via deepest-first walk.
        assert_eq!(hit_test_at(&rs, 15.0, 15.0).unwrap().node_id, nid(2));
        // Outside the leaf, inside the parent — pick the parent.
        assert_eq!(hit_test_at(&rs, 60.0, 60.0).unwrap().node_id, nid(1));
        // Fully outside — None.
        assert!(hit_test_at(&rs, 200.0, 200.0).is_none());
    }

    /// Recording renderer that captures every Renderer trait call as a
    /// shape-name + rect/text tuple. Lets us assert the paint sequence
    /// without needing a real font or pixel-level golden-image rig.
    #[derive(Default)]
    struct RecRenderer {
        calls: Vec<RecCall>,
        clips: Vec<Rect>,
    }
    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    enum RecCall {
        Fill(Rect, Color),
        Stroke(Rect, f32, Color),
        Text(Point, String, Color),
        PushClip(Rect),
        PopClip,
    }
    impl Renderer for RecRenderer {
        fn viewport(&self) -> feraille_render::Size {
            feraille_render::Size::new(1000.0, 1000.0)
        }
        fn scale_factor(&self) -> f32 {
            1.0
        }
        fn fill_rect(&mut self, rect: Rect, color: Color) {
            self.calls.push(RecCall::Fill(rect, color));
        }
        fn stroke_rect(&mut self, rect: Rect, width: f32, color: Color) {
            self.calls.push(RecCall::Stroke(rect, width, color));
        }
        fn draw_text(&mut self, pos: Point, text: &str, style: TextStyle) {
            self.calls.push(RecCall::Text(pos, text.to_string(), style.color));
        }
        fn measure_text(&self, text: &str, style: TextStyle) -> feraille_render::Size {
            // 7 DIPs per char is a fine-enough stand-in for ab_glyph;
            // the truncation logic only depends on monotonicity.
            feraille_render::Size::new((text.chars().count() as f32) * style.size * 0.55, style.size)
        }
        fn draw_bitmap(&mut self, _rect: Rect, _bitmap: &feraille_render::Bitmap) {}
        fn push_clip(&mut self, rect: Rect) {
            self.clips.push(rect);
            self.calls.push(RecCall::PushClip(rect));
        }
        fn pop_clip(&mut self) {
            self.clips.pop();
            self.calls.push(RecCall::PopClip);
        }
    }

    #[test]
    fn paint_emits_clip_background_and_per_rect_fill_stroke_in_order() {
        let rs = vec![
            rect(1, 0.0, 0.0, 200.0, 100.0, 0, FileCategory::Other),
            rect(2, 0.0, 0.0, 80.0, 100.0, 1, FileCategory::Image),
        ];
        let tokens = Tokens::light();
        let bounds = Rect::new(0.0, 0.0, 200.0, 100.0);
        let mut rec = RecRenderer::default();
        let selected = HashSet::new();

        super::paint(
            &rs,
            bounds,
            None,
            &selected,
            TreemapColoring::Category,
            None,
            &tokens,
            &mut rec,
            |_| String::new(), // suppress labels for this assertion
        );

        // First two calls: push_clip, then bg fill of the bounds.
        match &rec.calls[0] {
            RecCall::PushClip(r) => assert_eq!(*r, bounds),
            other => panic!("expected push_clip, got {other:?}"),
        }
        match &rec.calls[1] {
            RecCall::Fill(r, c) => {
                assert_eq!(*r, bounds);
                assert_eq!(*c, tokens.bg.layer1);
            }
            other => panic!("expected bg fill, got {other:?}"),
        }
        // Two rects → two fills + two strokes.
        let fills = rec
            .calls
            .iter()
            .filter(|c| matches!(c, RecCall::Fill(..)))
            .count();
        let strokes = rec
            .calls
            .iter()
            .filter(|c| matches!(c, RecCall::Stroke(..)))
            .count();
        assert_eq!(fills, 1 + 2, "bg fill plus one fill per rect");
        assert_eq!(strokes, 2);
        // Ends with pop_clip.
        assert!(matches!(rec.calls.last(), Some(RecCall::PopClip)));
    }

    #[test]
    fn paint_thickens_border_for_hover_and_selected() {
        let rs = vec![
            rect(1, 0.0, 0.0, 100.0, 100.0, 0, FileCategory::Other),
            rect(2, 100.0, 0.0, 100.0, 100.0, 0, FileCategory::Other),
            rect(3, 200.0, 0.0, 100.0, 100.0, 0, FileCategory::Other),
        ];
        let tokens = Tokens::light();
        let mut selected = HashSet::new();
        selected.insert(nid(2));

        let mut rec = RecRenderer::default();
        super::paint(
            &rs,
            Rect::new(0.0, 0.0, 300.0, 100.0),
            Some(nid(3)),
            &selected,
            TreemapColoring::DepthOnly,
            None,
            &tokens,
            &mut rec,
            |_| String::new(),
        );

        // Pull stroke widths in encounter order.
        let widths: Vec<f32> = rec
            .calls
            .iter()
            .filter_map(|c| match c {
                RecCall::Stroke(_, w, _) => Some(*w),
                _ => None,
            })
            .collect();
        assert_eq!(widths.len(), 3);
        assert!((widths[0] - 1.0).abs() < f32::EPSILON, "non-hover, non-selected → 1.0");
        assert!((widths[1] - 2.0).abs() < f32::EPSILON, "selected → 2.0");
        assert!((widths[2] - 1.5).abs() < f32::EPSILON, "hovered → 1.5");
    }

    #[test]
    fn paint_skips_label_for_too_small_rect() {
        // Width below LABEL_MIN_W must skip the draw_text call entirely
        // even when a non-empty name is provided.
        let rs = vec![rect(1, 0.0, 0.0, 40.0, 100.0, 0, FileCategory::Other)];
        let tokens = Tokens::light();
        let mut rec = RecRenderer::default();
        super::paint(
            &rs,
            Rect::new(0.0, 0.0, 40.0, 100.0),
            None,
            &HashSet::new(),
            TreemapColoring::Category,
            None,
            &tokens,
            &mut rec,
            |_| "ImportantName.txt".to_string(),
        );
        let texts = rec
            .calls
            .iter()
            .filter(|c| matches!(c, RecCall::Text(..)))
            .count();
        assert_eq!(texts, 0);
    }

    #[test]
    fn paint_renders_label_and_size_suffix_when_rect_wide() {
        // Width above LABEL_SIZE_W should append size string.
        let rs = vec![rect(1, 0.0, 0.0, 220.0, 60.0, 0, FileCategory::Image)];
        let tokens = Tokens::light();
        let mut rec = RecRenderer::default();
        super::paint(
            &rs,
            Rect::new(0.0, 0.0, 220.0, 60.0),
            None,
            &HashSet::new(),
            TreemapColoring::Category,
            None,
            &tokens,
            &mut rec,
            |_| "photo.png".to_string(),
        );
        let drawn: Vec<&str> = rec
            .calls
            .iter()
            .filter_map(|c| match c {
                RecCall::Text(_, t, _) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(drawn.len(), 1, "exactly one label drawn");
        assert!(drawn[0].starts_with("photo.png"));
        assert!(drawn[0].contains("KB") || drawn[0].contains("B"));
    }
}
