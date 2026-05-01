//! Renderer abstraction. Backends implement the `Renderer` trait. Controls call
//! into it via `&mut dyn Renderer`. The soft backend is in `soft.rs`; a future
//! Direct2D backend will live alongside it.
//!
//! Coordinate system: all `Renderer` calls are in **DIPs** (device-independent
//! pixels). The backend owns the conversion to physical pixels.

pub mod soft;

use feraille_design::{Color, FontWeight};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

impl Size {
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            origin: Point::new(x, y),
            size: Size::new(w, h),
        }
    }
    pub fn left(self) -> f32 {
        self.origin.x
    }
    pub fn top(self) -> f32 {
        self.origin.y
    }
    pub fn right(self) -> f32 {
        self.origin.x + self.size.width
    }
    pub fn bottom(self) -> f32 {
        self.origin.y + self.size.height
    }
    pub fn contains(self, p: Point) -> bool {
        p.x >= self.left() && p.x < self.right() && p.y >= self.top() && p.y < self.bottom()
    }
    pub fn intersect(self, other: Rect) -> Rect {
        let l = self.left().max(other.left());
        let t = self.top().max(other.top());
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        if r <= l || b <= t {
            Rect::new(0.0, 0.0, 0.0, 0.0)
        } else {
            Rect::new(l, t, r - l, b - t)
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TextStyle {
    pub size: f32,
    pub weight: FontWeight,
    pub color: Color,
}

impl TextStyle {
    pub const fn new(size: f32, color: Color) -> Self {
        Self { size, weight: FontWeight::Regular, color }
    }
}

pub trait Renderer {
    fn viewport(&self) -> Size;
    fn scale_factor(&self) -> f32;
    fn fill_rect(&mut self, rect: Rect, color: Color);
    fn stroke_rect(&mut self, rect: Rect, width: f32, color: Color);
    fn draw_text(&mut self, pos: Point, text: &str, style: TextStyle);
    fn measure_text(&self, text: &str, style: TextStyle) -> Size;
    fn push_clip(&mut self, rect: Rect);
    fn pop_clip(&mut self);
}

pub use soft::SoftRenderer;
