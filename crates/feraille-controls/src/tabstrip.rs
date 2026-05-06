//! TabStrip — horizontal tabs of independent navigation contexts.
//!
//! Iter-2 ships: activate, close-on-hover-X, "+" button. Drag-reorder and
//! detach-window slide to iter-3.
//!
//! Spec: `specs/controls/03-explorer-controls.md` §4.

use feraille_design::{FontWeight, Tokens};
use feraille_render::{Point, Rect, Renderer, TextStyle};

const HEIGHT: f32 = 32.0;
const TAB_WIDTH: f32 = 180.0;
const TAB_MIN_WIDTH: f32 = 100.0;
const CLOSE_SIZE: f32 = 16.0;
const NEW_BUTTON_W: f32 = 32.0;

#[derive(Clone, Debug)]
pub struct TabInfo {
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabHit {
    Tab(usize),
    Close(usize),
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabStripEvent {
    Activate(usize),
    Close(usize),
    New,
}

pub struct TabStrip {
    pub hover: Option<TabHit>,
    /// Leading-edge inset (DIPs). Hosts reserve room here for OS
    /// traffic-light buttons on macOS; zero otherwise.
    pub inset_left: f32,
}

impl Default for TabStrip {
    fn default() -> Self {
        Self::new()
    }
}

impl TabStrip {
    pub fn new() -> Self {
        Self { hover: None, inset_left: 0.0 }
    }

    pub fn paint(
        &self,
        bounds: Rect,
        tabs: &[TabInfo],
        active: usize,
        tokens: &Tokens,
        painter: &mut dyn Renderer,
    ) {
        // Strip background.
        painter.fill_rect(bounds, tokens.bg.layer2);
        painter.fill_rect(
            Rect::new(bounds.left(), bounds.bottom() - 1.0, bounds.size.width, 1.0),
            tokens.border.subtle,
        );

        let inner_left = bounds.left() + self.inset_left;
        let inner_width = (bounds.size.width - self.inset_left).max(0.0);
        let tab_w = compute_tab_width(inner_width, tabs.len());
        let mut x = inner_left;
        for (i, tab) in tabs.iter().enumerate() {
            let tab_rect = Rect::new(x, bounds.top(), tab_w, bounds.size.height);
            let is_active = i == active;
            let is_hover_tab = matches!(self.hover, Some(TabHit::Tab(j)) if j == i)
                || matches!(self.hover, Some(TabHit::Close(j)) if j == i);
            self.paint_tab(tab_rect, &tab.label, is_active, is_hover_tab, tokens, painter);
            x += tab_w;
            // Vertical separator between inactive neighbors only.
            if !is_active && i + 1 < tabs.len() {
                let next_active = i + 1 == active;
                if !next_active {
                    painter.fill_rect(
                        Rect::new(x - 1.0, bounds.top() + 6.0, 1.0, bounds.size.height - 12.0),
                        tokens.border.subtle,
                    );
                }
            }
        }
        // "+" button
        let new_rect = Rect::new(x, bounds.top(), NEW_BUTTON_W, bounds.size.height);
        let new_hover = matches!(self.hover, Some(TabHit::New));
        if new_hover {
            painter.fill_rect(new_rect, tokens.bg.layer3);
        }
        painter.draw_text(
            Point::new(
                new_rect.left() + (NEW_BUTTON_W - tokens.text.lg) / 2.0 + 1.0,
                new_rect.top() + (new_rect.size.height - tokens.text.lg) / 2.0,
            ),
            "+",
            TextStyle {
                size: tokens.text.lg,
                weight: FontWeight::Regular,
                color: tokens.fg.secondary,
            },
        );
    }

    fn paint_tab(
        &self,
        rect: Rect,
        label: &str,
        is_active: bool,
        is_hover: bool,
        tokens: &Tokens,
        painter: &mut dyn Renderer,
    ) {
        if is_active {
            painter.fill_rect(rect, tokens.bg.layer1);
        } else if is_hover {
            painter.fill_rect(rect, tokens.bg.layer3);
        }
        let text_y = rect.top() + (rect.size.height - tokens.text.md) / 2.0 - 1.0;
        // Label starts past the (always-reserved) close-button area on
        // the left, so showing/hiding the X doesn't shift the text.
        // 8 pad + close + 4 gap.
        let label_left = rect.left() + 8.0 + CLOSE_SIZE + 4.0;
        painter.draw_text(
            Point::new(label_left, text_y),
            label,
            TextStyle {
                size: tokens.text.md,
                weight: if is_active { FontWeight::Medium } else { FontWeight::Regular },
                color: if is_active { tokens.fg.primary } else { tokens.fg.secondary },
            },
        );
        // Close X (visible only on hover or active)
        if is_active || is_hover {
            let close_rect = close_rect_of(rect);
            let close_hover = matches!(self.hover, Some(TabHit::Close(_)))
                && self.hover_close_matches(rect);
            if close_hover {
                painter.fill_rect(close_rect, tokens.bg.layer3);
            }
            // Glyph: a stylized "x"
            painter.draw_text(
                Point::new(close_rect.left() + 4.0, close_rect.top() + 1.0),
                "\u{00D7}",
                TextStyle {
                    size: tokens.text.md,
                    weight: FontWeight::Regular,
                    color: tokens.fg.secondary,
                },
            );
        }
    }

    fn hover_close_matches(&self, _tab_rect: Rect) -> bool {
        // The hover state already encodes which Close(i) is hovered.
        matches!(self.hover, Some(TabHit::Close(_)))
    }

    pub fn update_hover(
        &mut self,
        bounds: Rect,
        tabs: &[TabInfo],
        point: Option<Point>,
    ) -> bool {
        let new_hover = point.and_then(|p| self.hit_test(bounds, tabs, p));
        if new_hover != self.hover {
            self.hover = new_hover;
            true
        } else {
            false
        }
    }

    pub fn click(
        &self,
        bounds: Rect,
        tabs: &[TabInfo],
        point: Point,
    ) -> Option<TabStripEvent> {
        match self.hit_test(bounds, tabs, point)? {
            TabHit::Tab(i) => Some(TabStripEvent::Activate(i)),
            TabHit::Close(i) => Some(TabStripEvent::Close(i)),
            TabHit::New => Some(TabStripEvent::New),
        }
    }

    fn hit_test(&self, bounds: Rect, tabs: &[TabInfo], point: Point) -> Option<TabHit> {
        if !bounds.contains(point) {
            return None;
        }
        let inner_left = bounds.left() + self.inset_left;
        let inner_width = (bounds.size.width - self.inset_left).max(0.0);
        if point.x < inner_left {
            return None;
        }
        let tab_w = compute_tab_width(inner_width, tabs.len());
        let mut x = inner_left;
        for (i, _tab) in tabs.iter().enumerate() {
            let tab_rect = Rect::new(x, bounds.top(), tab_w, bounds.size.height);
            if tab_rect.contains(point) {
                let close = close_rect_of(tab_rect);
                if close.contains(point) {
                    return Some(TabHit::Close(i));
                }
                return Some(TabHit::Tab(i));
            }
            x += tab_w;
        }
        let new_rect = Rect::new(x, bounds.top(), NEW_BUTTON_W, bounds.size.height);
        if new_rect.contains(point) {
            Some(TabHit::New)
        } else {
            None
        }
    }
}

fn compute_tab_width(strip_width: f32, n: usize) -> f32 {
    if n == 0 {
        return TAB_WIDTH;
    }
    let available = (strip_width - NEW_BUTTON_W).max(0.0);
    let ideal = available / n as f32;
    ideal.clamp(TAB_MIN_WIDTH, TAB_WIDTH)
}

fn close_rect_of(tab_rect: Rect) -> Rect {
    Rect::new(
        tab_rect.left() + 8.0,
        tab_rect.top() + (tab_rect.size.height - CLOSE_SIZE) / 2.0,
        CLOSE_SIZE,
        CLOSE_SIZE,
    )
}

pub fn height() -> f32 {
    HEIGHT
}
