//! VirtualizedList — fixed-row-height list rendering only the visible
//! window. Speaks `&[FileEntry]` directly; selection lives on the host
//! via the shared `Selection` model.
//!
//! Spec: `specs/controls/03-explorer-controls.md` §1.
//!
//! Iter-3.5 added the column system: clickable header + sort indicator,
//! Name/Size/Kind/Modified columns following Ferail's layout (Magic and
//! Description columns wait for the magic-detection port in iter-7).

use std::cmp::Ordering;

use feraille_core::{EntryKind, FileEntry};
use feraille_design::{Color, FontWeight, Tokens};
use feraille_render::{Bitmap, Point, Rect, Renderer, TextStyle};

use crate::primitives::focus_ring;
use crate::selection::Selection;

const COLUMN_PAD: f32 = 12.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ColumnId {
    Name,
    Size,
    Kind,
    Magic,
    Modified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColumnAlign {
    Left,
    Right,
}

#[derive(Clone, Debug)]
pub struct Column {
    pub id: ColumnId,
    pub label: &'static str,
    /// Fixed width in DIPs. `0.0` = flex (consumes remaining space).
    /// Currently only `Name` is flex; one flex per row is enough.
    pub width: f32,
    pub align: ColumnAlign,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SortKey {
    pub column: ColumnId,
    pub ascending: bool,
}

impl Default for SortKey {
    fn default() -> Self {
        Self { column: ColumnId::Name, ascending: true }
    }
}

pub fn default_columns() -> Vec<Column> {
    vec![
        Column { id: ColumnId::Name, label: "Name", width: 0.0, align: ColumnAlign::Left },
        Column { id: ColumnId::Size, label: "Size", width: 90.0, align: ColumnAlign::Right },
        Column { id: ColumnId::Kind, label: "Kind", width: 80.0, align: ColumnAlign::Left },
        Column { id: ColumnId::Magic, label: "Magic", width: 160.0, align: ColumnAlign::Left },
        Column {
            id: ColumnId::Modified,
            label: "Modified",
            width: 110.0,
            align: ColumnAlign::Left,
        },
    ]
}

/// Sort `entries` in place. Directories always sort before files
/// (Finder/Explorer convention); the sort key only orders within
/// each group.
pub fn sort_entries(entries: &mut [FileEntry], key: SortKey) {
    entries.sort_by(|a, b| {
        let group_order = match (a.kind, b.kind) {
            (EntryKind::Directory, EntryKind::Directory) => Ordering::Equal,
            (EntryKind::Directory, _) => Ordering::Less,
            (_, EntryKind::Directory) => Ordering::Greater,
            _ => Ordering::Equal,
        };
        if group_order != Ordering::Equal {
            return group_order;
        }
        let cmp = match key.column {
            ColumnId::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            ColumnId::Size => a.size.cmp(&b.size),
            ColumnId::Kind => a
                .display_kind
                .to_lowercase()
                .cmp(&b.display_kind.to_lowercase()),
            ColumnId::Magic => a
                .display_magic
                .to_lowercase()
                .cmp(&b.display_magic.to_lowercase()),
            ColumnId::Modified => a.mtime_unix.cmp(&b.mtime_unix),
        };
        if key.ascending {
            cmp
        } else {
            cmp.reverse()
        }
    });
}

/// Carry-over event surface — currently emitted only by the host's
/// keyboard/click handlers, not by the list itself.
#[derive(Clone, Debug)]
pub enum ListEvent {
    Activate(usize),
}

/// Carry-over public type kept to minimize the iter-1 → iter-2 churn.
#[derive(Clone, Debug)]
pub struct ListItem {
    pub primary: String,
    pub secondary: String,
    pub tertiary: String,
}

pub struct VirtualizedList {
    pub row_height: f32,
    /// Column-header row height. Host sets this from `tokens.hit.row`
    /// (or scaled equivalent) so it tracks UI scale.
    pub header_h: f32,
    /// Top of viewport, in DIPs from item 0.
    pub scroll_offset: f32,
    /// Whether this list owns keyboard focus (changes selection-fill color).
    pub focused: bool,
    /// Currently hovered row index, set by host on `CursorMoved`.
    pub hover: Option<usize>,
    /// Currently hovered header column.
    pub header_hover: Option<ColumnId>,
    pub columns: Vec<Column>,
    pub sort: SortKey,
}

impl Default for VirtualizedList {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualizedList {
    pub fn new() -> Self {
        Self {
            row_height: 28.0,
            header_h: 28.0,
            scroll_offset: 0.0,
            focused: true,
            hover: None,
            header_hover: None,
            columns: default_columns(),
            sort: SortKey::default(),
        }
    }

    pub fn header_height(&self) -> f32 {
        self.header_h
    }

    /// Update hover row from a pointer position. Returns whether hover changed.
    pub fn update_hover(
        &mut self,
        bounds: Rect,
        point: Option<Point>,
        item_count: usize,
    ) -> bool {
        let next = point.and_then(|p| self.index_at(bounds, p, item_count));
        if next != self.hover {
            self.hover = next;
            true
        } else {
            false
        }
    }

    pub fn update_header_hover(&mut self, header_bounds: Rect, point: Option<Point>) -> bool {
        let next = point.and_then(|p| self.header_column_at(header_bounds, p));
        if next != self.header_hover {
            self.header_hover = next;
            true
        } else {
            false
        }
    }

    pub fn paint_header(
        &self,
        bounds: Rect,
        tokens: &Tokens,
        painter: &mut dyn Renderer,
    ) {
        painter.fill_rect(bounds, tokens.bg.layer2);
        painter.fill_rect(
            Rect::new(bounds.left(), bounds.bottom() - 1.0, bounds.size.width, 1.0),
            tokens.border.subtle,
        );
        let layout = self.column_layout(bounds);
        let text_size = tokens.text.sm;
        let text_y = bounds.top() + (bounds.size.height - text_size) / 2.0 - 1.0;
        for (id, x, w) in &layout {
            let column = self.columns.iter().find(|c| c.id == *id).unwrap();
            let is_sorted = self.sort.column == *id;
            if self.header_hover == Some(*id) {
                painter.fill_rect(
                    Rect::new(*x - 4.0, bounds.top() + 2.0, w + 8.0, bounds.size.height - 4.0),
                    tokens.bg.layer3,
                );
            }
            let label_w = approx_text_width(column.label, text_size);
            let label_x = match column.align {
                ColumnAlign::Left => *x,
                ColumnAlign::Right => x + w - label_w - 18.0,
            };
            let color = if is_sorted { tokens.fg.primary } else { tokens.fg.secondary };
            let weight = if is_sorted { FontWeight::SemiBold } else { FontWeight::Medium };
            painter.draw_text(
                Point::new(label_x, text_y),
                column.label,
                TextStyle { size: text_size, weight, color },
            );
            if is_sorted {
                // U+25B2 / U+25BC — full BLACK TRIANGLE glyphs ship in Arial;
                // the SMALL TRIANGLE variants (U+25B4 / U+25BE) don't.
                let arrow = if self.sort.ascending { "\u{25B2}" } else { "\u{25BC}" };
                painter.draw_text(
                    Point::new(label_x + label_w + 4.0, text_y + 1.0),
                    arrow,
                    TextStyle {
                        size: tokens.text.xs,
                        weight: FontWeight::Regular,
                        color: tokens.fg.primary,
                    },
                );
            }
        }
    }

    pub fn paint<'a>(
        &self,
        bounds: Rect,
        items: &[FileEntry],
        selection: &Selection,
        icon_for: impl Fn(&FileEntry) -> Option<&'a Bitmap>,
        tokens: &Tokens,
        painter: &mut dyn Renderer,
    ) {
        painter.fill_rect(bounds, tokens.bg.layer1);
        if items.is_empty() || bounds.size.height <= 0.0 {
            return;
        }
        let layout = self.column_layout(bounds);
        let viewport_h = bounds.size.height;
        let count = items.len();
        let first_unbounded = (self.scroll_offset / self.row_height).floor() as i64;
        let last_unbounded =
            ((self.scroll_offset + viewport_h) / self.row_height).ceil() as i64;
        let overscan = 4_i64;
        let first = (first_unbounded - overscan).max(0).min(count as i64) as usize;
        let last = (last_unbounded + overscan).max(0).min(count as i64) as usize;

        painter.push_clip(bounds);

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
            } else if self.hover == Some(i) {
                painter.fill_rect(row_rect, tokens.bg.layer3);
            }

            let icon = icon_for(&items[i]);
            paint_row(&items[i], row_rect, &layout, &self.columns, icon, tokens, painter);
        }

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

    pub fn ensure_visible(&mut self, idx: usize, viewport_height: f32) {
        let cursor_y = (idx as f32) * self.row_height;
        if cursor_y < self.scroll_offset {
            self.scroll_offset = cursor_y;
        } else if cursor_y + self.row_height > self.scroll_offset + viewport_height {
            self.scroll_offset = (cursor_y + self.row_height - viewport_height).max(0.0);
        }
    }

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

    /// Hit-test a point against the header. Returns the column whose
    /// header was clicked, if any.
    pub fn header_column_at(&self, header_bounds: Rect, point: Point) -> Option<ColumnId> {
        if !header_bounds.contains(point) {
            return None;
        }
        let layout = self.column_layout(header_bounds);
        for (id, x, w) in &layout {
            if point.x >= *x - 4.0 && point.x < *x + *w + 4.0 {
                return Some(*id);
            }
        }
        None
    }

    /// Rect of the Name column for `row_idx` given the list bounds and
    /// current `scroll_offset`. Returns `None` when the row is offscreen.
    /// Used by inline rename to anchor a `TextInput` overlay over the
    /// row's name; if the row scrolls out of view the caller can treat
    /// `None` as "auto-cancel."
    pub fn row_name_rect(&self, bounds: Rect, row_idx: usize) -> Option<Rect> {
        let row_top = bounds.top() + (row_idx as f32 * self.row_height) - self.scroll_offset;
        let row_bottom = row_top + self.row_height;
        if row_bottom <= bounds.top() || row_top >= bounds.bottom() {
            return None;
        }
        let layout = self.column_layout(bounds);
        let (_, name_x, name_w) = *layout.first()?;
        Some(Rect::new(name_x, row_top, name_w, self.row_height))
    }

    /// Toggle sort: clicking the active column flips ascending; clicking a
    /// different column makes it the new sort, ascending.
    pub fn toggle_sort(&mut self, id: ColumnId) {
        if self.sort.column == id {
            self.sort.ascending = !self.sort.ascending;
        } else {
            self.sort = SortKey { column: id, ascending: true };
        }
    }

    /// Compute (id, x_start, width) for each column given the list bounds.
    /// Layout reserves `icon_x_end` DIPs at the left for the icon column,
    /// then the Name column flexes between the icon and the rightmost
    /// fixed columns.
    fn column_layout(&self, bounds: Rect) -> Vec<(ColumnId, f32, f32)> {
        let icon_x_end = bounds.left() + 12.0 + 16.0 + 8.0;
        let mut total_fixed = 0.0;
        for c in self.columns.iter().filter(|c| c.id != ColumnId::Name) {
            total_fixed += c.width + COLUMN_PAD;
        }
        let name_x = icon_x_end;
        let name_w = (bounds.right() - icon_x_end - total_fixed - COLUMN_PAD).max(80.0);
        let mut out: Vec<(ColumnId, f32, f32)> = Vec::with_capacity(self.columns.len());
        out.push((ColumnId::Name, name_x, name_w));
        let mut x = name_x + name_w + COLUMN_PAD;
        for c in self.columns.iter().filter(|c| c.id != ColumnId::Name) {
            out.push((c.id, x, c.width));
            x += c.width + COLUMN_PAD;
        }
        out
    }
}

fn paint_row(
    entry: &FileEntry,
    row_rect: Rect,
    layout: &[(ColumnId, f32, f32)],
    columns: &[Column],
    icon: Option<&Bitmap>,
    tokens: &Tokens,
    painter: &mut dyn Renderer,
) {
    let icon_size = tokens.icon.md;
    let icon_x = row_rect.left() + tokens.space.md;
    let icon_y = row_rect.top() + (row_rect.size.height - icon_size) / 2.0;
    let icon_rect = Rect::new(icon_x, icon_y, icon_size, icon_size);
    if let Some(bitmap) = icon {
        painter.draw_bitmap(icon_rect, bitmap);
    } else {
        let icon_color = if matches!(entry.kind, EntryKind::Symlink) {
            tokens.fg.secondary
        } else {
            icon_color_for_file(entry, tokens)
        };
        painter.fill_rect(icon_rect, icon_color);
    }

    let row_h = row_rect.size.height;
    let text_y = row_rect.top() + (row_h - tokens.text.md) / 2.0 - 1.0;
    let text_size = tokens.text.md;

    for (id, x, w) in layout {
        let column = columns.iter().find(|c| c.id == *id).unwrap();
        let value = column_value(*id, entry);
        if value.is_empty() {
            continue;
        }
        let color = match id {
            ColumnId::Name => tokens.fg.primary,
            _ => tokens.fg.secondary,
        };
        let text_x = match column.align {
            ColumnAlign::Left => *x,
            ColumnAlign::Right => x + w - approx_text_width(value, text_size),
        };
        painter.draw_text(
            Point::new(text_x, text_y),
            value,
            TextStyle { size: text_size, weight: FontWeight::Regular, color },
        );
    }
}

fn column_value(id: ColumnId, entry: &FileEntry) -> &str {
    match id {
        ColumnId::Name => &entry.name,
        ColumnId::Size => &entry.display_size,
        ColumnId::Kind => &entry.display_kind,
        ColumnId::Magic => &entry.display_magic,
        ColumnId::Modified => &entry.display_mtime,
    }
}

fn approx_text_width(s: &str, size: f32) -> f32 {
    s.chars().count() as f32 * size * 0.55
}

fn icon_color_for_file(entry: &FileEntry, tokens: &Tokens) -> Color {
    if matches!(entry.kind, EntryKind::Directory) {
        return tokens.accent.fill;
    }
    let ext = entry
        .name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some(
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "c" | "cpp" | "cc" | "h" | "hpp"
            | "swift" | "kt" | "java" | "rb" | "php" | "lua" | "sh" | "zsh" | "bash" | "fish",
        ) => tokens.magic.code,
        Some(
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "tiff" | "ico" | "heic",
        ) => tokens.magic.image,
        Some(
            "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" | "mp3" | "wav" | "flac" | "aac"
            | "ogg",
        ) => tokens.magic.media,
        Some("zip" | "tar" | "gz" | "tgz" | "bz2" | "7z" | "rar" | "xz" | "zst") => {
            tokens.magic.archive
        }
        Some(
            "json" | "yaml" | "yml" | "toml" | "xml" | "csv" | "tsv" | "ini" | "cfg" | "conf"
            | "lock",
        ) => tokens.magic.data,
        Some("md" | "markdown" | "txt" | "pdf" | "doc" | "docx" | "rtf") => tokens.magic.doc,
        _ => tokens.fg.disabled,
    }
}
