//! VirtualizedList — fixed-row-height list rendering only the visible
//! window. Speaks `&[FileEntry]` directly; selection lives on the host
//! via the shared `Selection` model.
//!
//! Spec: `specs/controls/03-explorer-controls.md` §1.
//!
//! Iter-2 paint layers (bottom up):
//!   1. background
//!   2. selection fills (`accent.subtle` or `accent.subtle_inactive`)
//!   3. row content (icon + 3 columns)
//!   4. FocusRing overlay on cursor row (last)

use feraille_core::{EntryKind, FileEntry};
use feraille_design::{FontWeight, Tokens};
use feraille_render::{Point, Rect, Renderer, TextStyle};

use crate::primitives::focus_ring;
use crate::selection::Selection;

/// Carry-over event surface — currently emitted only by the host's
/// keyboard/click handlers, not by the list itself. Kept so callers can
/// switch to event-driven semantics in iter-3 without a breaking API.
#[derive(Clone, Debug)]
pub enum ListEvent {
    Activate(usize),
}

/// Carry-over public type — kept (deprecated) so iter-1 examples keep
/// compiling. Internal code now uses `&[FileEntry]` directly.
#[derive(Clone, Debug)]
pub struct ListItem {
    pub primary: String,
    pub secondary: String,
    pub tertiary: String,
}

pub struct VirtualizedList {
    pub row_height: f32,
    /// Top of viewport, in DIPs from item 0.
    pub scroll_offset: f32,
    /// Whether this list owns keyboard focus (changes selection-fill color).
    pub focused: bool,
}

impl Default for VirtualizedList {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualizedList {
    pub fn new() -> Self {
        Self { row_height: 28.0, scroll_offset: 0.0, focused: true }
    }

    pub fn paint(
        &self,
        bounds: Rect,
        items: &[FileEntry],
        selection: &Selection,
        tokens: &Tokens,
        painter: &mut dyn Renderer,
    ) {
        painter.fill_rect(bounds, tokens.bg.layer1);
        if items.is_empty() || bounds.size.height <= 0.0 {
            return;
        }

        let viewport_h = bounds.size.height;
        let count = items.len();
        let first_unbounded = (self.scroll_offset / self.row_height).floor() as i64;
        let last_unbounded =
            ((self.scroll_offset + viewport_h) / self.row_height).ceil() as i64;
        let overscan = 4_i64;
        let first = (first_unbounded - overscan).max(0).min(count as i64) as usize;
        let last = (last_unbounded + overscan).max(0).min(count as i64) as usize;

        painter.push_clip(bounds);

        // Layers 1–3: background, selection fills, row content.
        for i in first..last {
            let row_top = bounds.top() + (i as f32 * self.row_height) - self.scroll_offset;
            let row_rect = Rect::new(bounds.left(), row_top, bounds.size.width, self.row_height);

            if selection.set.contains(i) {
                let fill = if self.focused {
                    tokens.accent.subtle
                } else {
                    tokens.accent.subtle_inactive
                };
                painter.fill_rect(row_rect, fill);
            }

            paint_row(&items[i], row_rect, tokens, painter);
        }

        // Layer 4: FocusRing overlay on the cursor row, painted last so it
        // sits above selection fills + row content.
        if self.focused {
            if let Some(cursor) = selection.cursor() {
                if cursor < count {
                    let row_top =
                        bounds.top() + (cursor as f32 * self.row_height) - self.scroll_offset;
                    let row_rect =
                        Rect::new(bounds.left(), row_top, bounds.size.width, self.row_height);
                    focus_ring::paint(row_rect, tokens, painter);
                }
            }
        }

        painter.pop_clip();
    }

    pub fn scroll_by(&mut self, delta: f32, item_count: usize, viewport_height: f32) {
        let max = ((item_count as f32) * self.row_height - viewport_height).max(0.0);
        self.scroll_offset = (self.scroll_offset + delta).clamp(0.0, max);
    }

    /// Adjust scroll so row `idx` is visible.
    pub fn ensure_visible(&mut self, idx: usize, viewport_height: f32) {
        let cursor_y = (idx as f32) * self.row_height;
        if cursor_y < self.scroll_offset {
            self.scroll_offset = cursor_y;
        } else if cursor_y + self.row_height > self.scroll_offset + viewport_height {
            self.scroll_offset = (cursor_y + self.row_height - viewport_height).max(0.0);
        }
    }

    /// Hit-test a point in *content space* against rows.
    pub fn index_at(&self, bounds: Rect, point: Point, item_count: usize) -> Option<usize> {
        if !bounds.contains(point) {
            return None;
        }
        let local_y = point.y - bounds.top() + self.scroll_offset;
        if local_y < 0.0 {
            return None;
        }
        let idx = (local_y / self.row_height) as usize;
        if idx >= item_count {
            None
        } else {
            Some(idx)
        }
    }
}

fn paint_row(entry: &FileEntry, row_rect: Rect, tokens: &Tokens, painter: &mut dyn Renderer) {
    // Icon placeholder: 16-DIP square. Real icons land in iter-3.
    let icon_size = 16.0;
    let icon_x = row_rect.left() + tokens.space.md;
    let icon_y = row_rect.top() + (row_rect.size.height - icon_size) / 2.0;
    let icon_color = match entry.kind {
        EntryKind::Directory => tokens.accent.fill,
        EntryKind::Symlink => tokens.fg.secondary,
        EntryKind::File => tokens.fg.disabled,
    };
    painter.fill_rect(Rect::new(icon_x, icon_y, icon_size, icon_size), icon_color);

    let row_h = row_rect.size.height;
    let text_y = row_rect.top() + (row_h - tokens.text.md) / 2.0 - 1.0;

    // Primary column.
    let primary_x = icon_x + icon_size + tokens.space.sm;
    painter.draw_text(
        Point::new(primary_x, text_y),
        &entry.name,
        TextStyle {
            size: tokens.text.md,
            weight: FontWeight::Regular,
            color: tokens.fg.primary,
        },
    );

    // Right-aligned columns: secondary (mtime), tertiary reserved.
    let mtime_w_estimate = 90.0;
    let size_w_estimate = 110.0;
    let pad_r = tokens.space.md;
    let mtime_x = row_rect.right() - pad_r - mtime_w_estimate;
    let size_x = mtime_x - size_w_estimate;

    if !entry.display_size.is_empty() {
        painter.draw_text(
            Point::new(size_x, text_y),
            &entry.display_size,
            TextStyle {
                size: tokens.text.md,
                weight: FontWeight::Regular,
                color: tokens.fg.secondary,
            },
        );
    }
    painter.draw_text(
        Point::new(mtime_x, text_y),
        &entry.display_mtime,
        TextStyle {
            size: tokens.text.md,
            weight: FontWeight::Regular,
            color: tokens.fg.secondary,
        },
    );
}
