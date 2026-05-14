//! BreadcrumbBar — segmented path display with optional edit mode.
//!
//! Iter-2 shipped read-only segments; iter-2.5 adds edit mode wired to
//! `TextInput`. Ctrl+L (handled by the host) toggles into edit mode with
//! the current path pre-filled and selected for replacement.
//!
//! Spec: `specs/controls/03-explorer-controls.md` §3.

use std::path::{Component, Path, PathBuf};

use feraille_design::{FontWeight, Tokens};
use feraille_render::{Point, Rect, Renderer, TextStyle};

use crate::primitives::text_input::{TextInput, TextInputEvent, TextInputKey};

const HEIGHT: f32 = 32.0;
const SEGMENT_PAD_X: f32 = 6.0;
const SEPARATOR_W: f32 = 14.0;
const SEPARATOR_GLYPH: &str = ">";

#[derive(Clone, Debug)]
pub struct Segment {
    pub label: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub enum BreadcrumbEvent {
    Navigate(PathBuf),
}

pub enum BreadcrumbMode {
    Segments,
    Editing(TextInput),
}

pub struct BreadcrumbBar {
    pub segments: Vec<Segment>,
    pub hover_index: Option<usize>,
    pub mode: BreadcrumbMode,
}

impl Default for BreadcrumbBar {
    fn default() -> Self {
        Self::new()
    }
}

impl BreadcrumbBar {
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
            hover_index: None,
            mode: BreadcrumbMode::Segments,
        }
    }

    pub fn is_editing(&self) -> bool {
        matches!(self.mode, BreadcrumbMode::Editing(_))
    }

    /// Replace segments to reflect `path`. Cancels edit mode if active.
    pub fn set_path(&mut self, path: &Path) {
        self.segments.clear();
        self.hover_index = None;
        let mut accum = PathBuf::new();
        for comp in path.components() {
            match comp {
                Component::RootDir => {
                    accum.push("/");
                    self.segments.push(Segment {
                        label: "/".to_string(),
                        path: accum.clone(),
                    });
                }
                Component::Normal(s) => {
                    accum.push(s);
                    self.segments.push(Segment {
                        label: s.to_string_lossy().into_owned(),
                        path: accum.clone(),
                    });
                }
                _ => {}
            }
        }
        self.mode = BreadcrumbMode::Segments;
    }

    pub fn enter_edit_mode(&mut self, current_path: &Path) {
        let initial = current_path.to_string_lossy().into_owned();
        self.mode = BreadcrumbMode::Editing(TextInput::new(&initial));
    }

    pub fn exit_edit_mode(&mut self) {
        self.mode = BreadcrumbMode::Segments;
    }

    pub fn handle_text(&mut self, text: &str) {
        if let BreadcrumbMode::Editing(input) = &mut self.mode {
            input.handle_text(text);
        }
    }

    pub fn handle_key(&mut self, key: TextInputKey) -> Option<BreadcrumbEvent> {
        let BreadcrumbMode::Editing(input) = &mut self.mode else { return None };
        match input.handle_key(key) {
            Some(TextInputEvent::Submit(s)) => {
                let path = expand_user_path(&s);
                self.exit_edit_mode();
                Some(BreadcrumbEvent::Navigate(path))
            }
            Some(TextInputEvent::Cancel) => {
                self.exit_edit_mode();
                None
            }
            None => None,
        }
    }

    pub fn paint(&self, bounds: Rect, tokens: &Tokens, painter: &mut dyn Renderer) {
        painter.fill_rect(bounds, tokens.bg.layer1);
        painter.fill_rect(
            Rect::new(bounds.left(), bounds.bottom() - 1.0, bounds.size.width, 1.0),
            tokens.border.subtle,
        );

        match &self.mode {
            BreadcrumbMode::Editing(input) => {
                let inset = 6.0;
                let input_rect = Rect::new(
                    bounds.left() + inset,
                    bounds.top() + 4.0,
                    bounds.size.width - inset * 2.0,
                    bounds.size.height - 8.0,
                );
                input.paint(input_rect, true, tokens, painter);
            }
            BreadcrumbMode::Segments => self.paint_segments(bounds, tokens, painter),
        }
    }

    fn paint_segments(&self, bounds: Rect, tokens: &Tokens, painter: &mut dyn Renderer) {
        let text_size = tokens.text.md;
        let text_y = bounds.top() + (bounds.size.height - text_size) / 2.0 - 1.0;
        let mut x = bounds.left() + tokens.space.md;
        for (i, seg) in self.segments.iter().enumerate() {
            let label_w = approx_text_width(&seg.label, text_size);
            let segment_w = label_w + SEGMENT_PAD_X * 2.0;
            let segment_rect =
                Rect::new(x, bounds.top() + 4.0, segment_w, bounds.size.height - 8.0);
            if self.hover_index == Some(i) {
                painter.fill_rect(segment_rect, tokens.bg.layer3);
            }
            painter.draw_text(
                Point::new(x + SEGMENT_PAD_X, text_y),
                &seg.label,
                TextStyle {
                    size: text_size,
                    weight: FontWeight::Medium,
                    color: tokens.fg.primary,
                },
            );
            x += segment_w;
            if i + 1 < self.segments.len() {
                painter.draw_text(
                    Point::new(x + (SEPARATOR_W - 6.0) / 2.0, text_y),
                    SEPARATOR_GLYPH,
                    TextStyle {
                        size: text_size,
                        weight: FontWeight::Regular,
                        color: tokens.fg.disabled,
                    },
                );
                x += SEPARATOR_W;
            }
        }
    }

    pub fn update_hover(&mut self, bounds: Rect, point: Option<Point>) -> bool {
        if self.is_editing() {
            return false;
        }
        let new_hover = point.and_then(|p| self.index_at(bounds, p));
        if new_hover != self.hover_index {
            self.hover_index = new_hover;
            true
        } else {
            false
        }
    }

    pub fn click(&self, bounds: Rect, point: Point) -> Option<BreadcrumbEvent> {
        if self.is_editing() {
            return None;
        }
        let idx = self.index_at(bounds, point)?;
        Some(BreadcrumbEvent::Navigate(self.segments[idx].path.clone()))
    }

    fn index_at(&self, bounds: Rect, point: Point) -> Option<usize> {
        if !bounds.contains(point) {
            return None;
        }
        let mut x = bounds.left() + 12.0;
        let text_size = 13.0;
        for (i, seg) in self.segments.iter().enumerate() {
            let segment_w = approx_text_width(&seg.label, text_size) + SEGMENT_PAD_X * 2.0;
            let segment_rect =
                Rect::new(x, bounds.top() + 4.0, segment_w, bounds.size.height - 8.0);
            if segment_rect.contains(point) {
                return Some(i);
            }
            x += segment_w + SEPARATOR_W;
        }
        None
    }
}

/// Minimal path expansion: trims whitespace, replaces a leading `~` with
/// `$HOME`. Real env-var expansion lands when the macOS shell crate ships
/// in iter-3.
fn expand_user_path(input: &str) -> PathBuf {
    let trimmed = input.trim();
    if let Some(rest) = trimmed.strip_prefix('~') {
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = PathBuf::from(home);
            let suffix = rest.strip_prefix('/').unwrap_or(rest);
            if !suffix.is_empty() {
                p.push(suffix);
            }
            return p;
        }
    }
    PathBuf::from(trimmed)
}

fn approx_text_width(s: &str, size: f32) -> f32 {
    s.chars().count() as f32 * size * 0.55
}

pub fn height() -> f32 {
    HEIGHT
}
