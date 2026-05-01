//! Software (CPU) renderer backed by a flat ARGB pixel buffer.
//!
//! Iter-2 added a glyph cache: `draw_text` no longer re-rasterizes glyphs
//! every frame. Each `(GlyphId, px_size_q8)` is rasterized once and the
//! coverage bitmap is reused. Eviction is FIFO at 2048 entries; LRU is
//! correct but unnecessary at our working-set size.

use crate::{Point, Rect, Renderer, Size, TextStyle};
use ab_glyph::{Font, FontVec, Glyph, GlyphId, PxScale, ScaleFont};
use feraille_design::Color;
use std::collections::{HashMap, VecDeque};

const GLYPH_CACHE_CAPACITY: usize = 2048;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct GlyphKey {
    glyph_id: GlyphId,
    /// Scale (physical pixels per em) quantized to 1/256 — covers any
    /// fractional DPI without thrashing the cache on micro-changes.
    px_size_q8: u32,
}

struct CachedGlyph {
    /// Coverage values (0–255), row-major. Empty for whitespace glyphs.
    bitmap: Box<[u8]>,
    width: u32,
    height: u32,
    /// Offset from the glyph's pen origin to the bitmap's top-left, in
    /// physical pixels. `bx` is typically small positive (left-bearing);
    /// `by` is typically negative (ascender above baseline).
    bx: i32,
    by: i32,
}

struct GlyphCache {
    cache: HashMap<GlyphKey, CachedGlyph>,
    fifo: VecDeque<GlyphKey>,
    capacity: usize,
}

impl GlyphCache {
    fn new(capacity: usize) -> Self {
        Self {
            cache: HashMap::with_capacity(capacity),
            fifo: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn ensure(&mut self, font: &FontVec, glyph_id: GlyphId, scale: PxScale) -> GlyphKey {
        let px_size_q8 = (scale.x * 256.0).round() as u32;
        let key = GlyphKey { glyph_id, px_size_q8 };
        if self.cache.contains_key(&key) {
            return key;
        }
        while self.cache.len() >= self.capacity {
            if let Some(old) = self.fifo.pop_front() {
                self.cache.remove(&old);
            } else {
                break;
            }
        }
        let cached = rasterize(font, glyph_id, scale);
        self.cache.insert(key, cached);
        self.fifo.push_back(key);
        key
    }

    fn get(&self, key: &GlyphKey) -> Option<&CachedGlyph> {
        self.cache.get(key)
    }
}

fn rasterize(font: &FontVec, glyph_id: GlyphId, scale: PxScale) -> CachedGlyph {
    let scaled = font.as_scaled(scale);
    let glyph = Glyph {
        id: glyph_id,
        scale,
        position: ab_glyph::point(0.0, 0.0),
    };
    let Some(outlined) = scaled.outline_glyph(glyph) else {
        return CachedGlyph { bitmap: Box::new([]), width: 0, height: 0, bx: 0, by: 0 };
    };
    let b = outlined.px_bounds();
    let bx = b.min.x.floor() as i32;
    let by = b.min.y.floor() as i32;
    let w = (b.max.x.ceil() as i32 - bx).max(0) as u32;
    let h = (b.max.y.ceil() as i32 - by).max(0) as u32;
    if w == 0 || h == 0 {
        return CachedGlyph { bitmap: Box::new([]), width: 0, height: 0, bx, by };
    }
    let mut bitmap = vec![0u8; (w as usize) * (h as usize)];
    let stride = w as usize;
    let h_us = h as usize;
    outlined.draw(|gx, gy, coverage| {
        let bxx = gx as usize;
        let byy = gy as usize;
        if bxx < stride && byy < h_us {
            bitmap[byy * stride + bxx] = (coverage * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    });
    CachedGlyph { bitmap: bitmap.into_boxed_slice(), width: w, height: h, bx, by }
}

pub struct SoftRenderer {
    width: u32,
    height: u32,
    scale_factor: f32,
    pixels: Vec<u32>,
    clip_stack: Vec<Rect>,
    font: FontVec,
    glyph_cache: GlyphCache,
}

impl SoftRenderer {
    pub fn new(width: u32, height: u32, scale_factor: f32, font_bytes: Vec<u8>) -> Self {
        let font = FontVec::try_from_vec(font_bytes).expect("font load");
        Self {
            width,
            height,
            scale_factor,
            pixels: vec![0xFF00_0000; (width as usize) * (height as usize)],
            clip_stack: Vec::new(),
            font,
            glyph_cache: GlyphCache::new(GLYPH_CACHE_CAPACITY),
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
        self.pixels = vec![0xFF00_0000; (self.width as usize) * (self.height as usize)];
    }

    pub fn set_scale_factor(&mut self, scale_factor: f32) {
        self.scale_factor = scale_factor.max(0.5);
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    fn current_clip(&self) -> Rect {
        self.clip_stack.last().copied().unwrap_or_else(|| {
            Rect::new(
                0.0,
                0.0,
                (self.width as f32) / self.scale_factor,
                (self.height as f32) / self.scale_factor,
            )
        })
    }
}

impl Renderer for SoftRenderer {
    fn viewport(&self) -> Size {
        Size::new(
            (self.width as f32) / self.scale_factor,
            (self.height as f32) / self.scale_factor,
        )
    }

    fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    fn fill_rect(&mut self, rect: Rect, color: Color) {
        if color.a == 0 {
            return;
        }
        let clipped = rect.intersect(self.current_clip());
        if clipped.size.width <= 0.0 || clipped.size.height <= 0.0 {
            return;
        }
        let s = self.scale_factor;
        let l = (clipped.left() * s).round().max(0.0) as i32;
        let t = (clipped.top() * s).round().max(0.0) as i32;
        let r = (clipped.right() * s).round().min(self.width as f32) as i32;
        let b = (clipped.bottom() * s).round().min(self.height as f32) as i32;
        if r <= l || b <= t {
            return;
        }
        let packed = color.pack_argb();
        let opaque = color.a == 0xFF;
        let stride = self.width as usize;
        for y in t..b {
            let row = (y as usize) * stride;
            for x in l..r {
                let idx = row + (x as usize);
                if opaque {
                    self.pixels[idx] = packed;
                } else {
                    self.pixels[idx] = blend_argb(self.pixels[idx], packed);
                }
            }
        }
    }

    fn stroke_rect(&mut self, rect: Rect, width: f32, color: Color) {
        let w = width;
        // Insets fully inside the rect (matches a "border-box" stroke).
        self.fill_rect(Rect::new(rect.left(), rect.top(), rect.size.width, w), color);
        self.fill_rect(
            Rect::new(rect.left(), rect.bottom() - w, rect.size.width, w),
            color,
        );
        self.fill_rect(Rect::new(rect.left(), rect.top(), w, rect.size.height), color);
        self.fill_rect(
            Rect::new(rect.right() - w, rect.top(), w, rect.size.height),
            color,
        );
    }

    fn draw_text(&mut self, pos: Point, text: &str, style: TextStyle) {
        if text.is_empty() {
            return;
        }
        let s = self.scale_factor;
        let scale = PxScale::from(style.size * s);
        let baseline_y = pos.y * s + self.font.as_scaled(scale).ascent();
        let baseline_y_int = baseline_y.round() as i32;

        // Pass 1: layout + ensure cached. Builds (key, x_pen) for each char.
        let mut positioned: Vec<(GlyphKey, i32)> =
            Vec::with_capacity(text.len().min(256));
        let mut x = pos.x * s;
        let mut last: Option<GlyphId> = None;
        for c in text.chars() {
            let id = self.font.glyph_id(c);
            let scaled = self.font.as_scaled(scale);
            if let Some(p) = last {
                x += scaled.kern(p, id);
            }
            let advance = scaled.h_advance(id);
            let key = self.glyph_cache.ensure(&self.font, id, scale);
            positioned.push((key, x.round() as i32));
            x += advance;
            last = Some(id);
        }

        // Pass 2: blit. Split-borrow self.glyph_cache (immut) + self.pixels (mut).
        let clip = self.current_clip();
        let clip_l_px = clip.left() * s;
        let clip_t_px = clip.top() * s;
        let clip_r_px = clip.right() * s;
        let clip_b_px = clip.bottom() * s;
        let glyph_cache = &self.glyph_cache;
        let pixels = &mut self.pixels;
        let width_u = self.width;
        let height_u = self.height;
        let stride = width_u as usize;
        let color = style.color;

        for (key, pen_x_int) in positioned {
            let Some(cached) = glyph_cache.get(&key) else { continue };
            if cached.width == 0 || cached.height == 0 {
                continue;
            }
            let target_l = pen_x_int + cached.bx;
            let target_t = baseline_y_int + cached.by;
            let cw = cached.width as i32;
            let ch = cached.height as i32;
            // Bounds-cull against window + clip in physical pixels.
            let l = target_l.max(0).max(clip_l_px.ceil() as i32);
            let t = target_t.max(0).max(clip_t_px.ceil() as i32);
            let r = (target_l + cw).min(width_u as i32).min(clip_r_px.floor() as i32);
            let b = (target_t + ch).min(height_u as i32).min(clip_b_px.floor() as i32);
            if r <= l || b <= t {
                continue;
            }
            let bm = &cached.bitmap;
            let bm_stride = cached.width as usize;
            let style_a = color.a as u32;
            let style_r = (color.r as u32) << 16;
            let style_g = (color.g as u32) << 8;
            let style_b = color.b as u32;
            for y in t..b {
                let py = y as usize;
                let glyph_row = (y - target_t) as usize * bm_stride;
                let pixel_row = py * stride;
                for x in l..r {
                    let coverage = bm[glyph_row + (x - target_l) as usize] as u32;
                    if coverage == 0 {
                        continue;
                    }
                    let mixed_a = style_a * coverage / 255;
                    if mixed_a == 0 {
                        continue;
                    }
                    let glyph_packed = (mixed_a << 24) | style_r | style_g | style_b;
                    let idx = pixel_row + x as usize;
                    pixels[idx] = blend_argb(pixels[idx], glyph_packed);
                }
            }
        }
    }

    fn measure_text(&self, text: &str, style: TextStyle) -> Size {
        let s = self.scale_factor;
        let scale = PxScale::from(style.size * s);
        let scaled = self.font.as_scaled(scale);
        let mut width_px = 0.0_f32;
        let mut last: Option<GlyphId> = None;
        for c in text.chars() {
            let id = self.font.glyph_id(c);
            if let Some(p) = last {
                width_px += scaled.kern(p, id);
            }
            width_px += scaled.h_advance(id);
            last = Some(id);
        }
        let line_height_px = scaled.ascent() - scaled.descent() + scaled.line_gap();
        Size::new(width_px / s, line_height_px / s)
    }

    fn push_clip(&mut self, rect: Rect) {
        let clipped = if let Some(cur) = self.clip_stack.last().copied() {
            rect.intersect(cur)
        } else {
            rect
        };
        self.clip_stack.push(clipped);
    }

    fn pop_clip(&mut self) {
        self.clip_stack.pop();
    }
}

#[inline]
fn blend_argb(dst: u32, src: u32) -> u32 {
    let sa = (src >> 24) & 0xFF;
    if sa == 0 {
        return dst;
    }
    if sa == 0xFF {
        return src;
    }
    let sr = (src >> 16) & 0xFF;
    let sg = (src >> 8) & 0xFF;
    let sb = src & 0xFF;
    let dr = (dst >> 16) & 0xFF;
    let dg = (dst >> 8) & 0xFF;
    let db = dst & 0xFF;
    let inv = 255 - sa;
    let r = (sr * sa + dr * inv) / 255;
    let g = (sg * sa + dg * inv) / 255;
    let b = (sb * sa + db * inv) / 255;
    0xFF00_0000 | (r << 16) | (g << 8) | b
}
