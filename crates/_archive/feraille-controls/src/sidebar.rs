//! Sidebar — fixed Locations list (Home, Documents, Downloads, then any
//! `/Volumes/*` mounts on macOS). 28-DIP rows with icon + label; click
//! emits `SidebarEvent::Navigate(PathBuf)`.
//!
//! **Transient**: replaced by `FileTree` in step 15 of iter-2. Kept
//! deliberately simple — no virtualization, no drag-out, no nested
//! sections — because everything here will be deleted.
//!
//! Spec: `specs/controls/03-explorer-controls.md` §8.

use std::path::PathBuf;

use feraille_design::{FontWeight, Tokens};
use feraille_render::{Point, Rect, Renderer, TextStyle};

const ROW_HEIGHT: f32 = 28.0;
const SECTION_HEADER_HEIGHT: f32 = 24.0;

#[derive(Clone, Debug)]
pub enum SidebarEvent {
    Navigate(PathBuf),
}

#[derive(Clone, Debug)]
pub enum Entry {
    Header(String),
    Item { label: String, path: PathBuf },
}

pub struct Sidebar {
    pub entries: Vec<Entry>,
    pub hover_index: Option<usize>,
    pub selected_path: Option<PathBuf>,
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

impl Sidebar {
    pub fn new() -> Self {
        Self { entries: Vec::new(), hover_index: None, selected_path: None }
    }

    /// Replace entries with the standard macOS layout: Home block + any
    /// `/Volumes/*` mounts.
    pub fn populate_macos(
        &mut self,
        home: PathBuf,
        volumes: Vec<(String, PathBuf)>,
    ) {
        self.entries.clear();
        self.entries.push(Entry::Header("Locations".to_string()));
        let user_label = home
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("Home")
            .to_string();
        self.entries.push(Entry::Item { label: user_label, path: home.clone() });
        for sub in ["Documents", "Downloads", "Desktop", "Pictures"] {
            let p = home.join(sub);
            if p.is_dir() {
                self.entries.push(Entry::Item { label: sub.to_string(), path: p });
            }
        }
        if !volumes.is_empty() {
            self.entries.push(Entry::Header("Volumes".to_string()));
            for (label, path) in volumes {
                self.entries.push(Entry::Item { label, path });
            }
        }
    }

    pub fn paint(&self, bounds: Rect, tokens: &Tokens, painter: &mut dyn Renderer) {
        painter.fill_rect(bounds, tokens.bg.layer2);

        let mut y = bounds.top() + tokens.space.sm;
        for (i, entry) in self.entries.iter().enumerate() {
            match entry {
                Entry::Header(text) => {
                    let row = Rect::new(bounds.left(), y, bounds.size.width, SECTION_HEADER_HEIGHT);
                    let text_y = row.top() + (SECTION_HEADER_HEIGHT - tokens.text.xs) / 2.0 - 1.0;
                    painter.draw_text(
                        Point::new(row.left() + tokens.space.md, text_y),
                        text,
                        TextStyle {
                            size: tokens.text.xs,
                            weight: FontWeight::SemiBold,
                            color: tokens.fg.secondary,
                        },
                    );
                    y += SECTION_HEADER_HEIGHT;
                }
                Entry::Item { label, path } => {
                    let row = Rect::new(bounds.left(), y, bounds.size.width, ROW_HEIGHT);
                    let is_selected = self.selected_path.as_ref() == Some(path);
                    let is_hover = self.hover_index == Some(i);
                    if is_selected {
                        painter.fill_rect(row, tokens.accent.subtle);
                    } else if is_hover {
                        painter.fill_rect(row, tokens.bg.layer3);
                    }
                    // Icon placeholder.
                    let icon_size = 14.0;
                    let icon_x = row.left() + tokens.space.md;
                    let icon_y = row.top() + (ROW_HEIGHT - icon_size) / 2.0;
                    painter.fill_rect(
                        Rect::new(icon_x, icon_y, icon_size, icon_size),
                        tokens.fg.secondary,
                    );
                    let text_x = icon_x + icon_size + tokens.space.sm;
                    let text_y = row.top() + (ROW_HEIGHT - tokens.text.md) / 2.0 - 1.0;
                    painter.draw_text(
                        Point::new(text_x, text_y),
                        label,
                        TextStyle {
                            size: tokens.text.md,
                            weight: FontWeight::Regular,
                            color: tokens.fg.primary,
                        },
                    );
                    y += ROW_HEIGHT;
                }
            }
        }
    }

    /// Update hover state from a pointer position; returns whether hover
    /// changed (so the host can request a redraw).
    pub fn update_hover(&mut self, bounds: Rect, point: Option<Point>) -> bool {
        let new_hover = point.and_then(|p| self.index_at(bounds, p));
        if new_hover != self.hover_index {
            self.hover_index = new_hover;
            true
        } else {
            false
        }
    }

    /// Hit-test a point and return a `SidebarEvent` if it lands on an item.
    pub fn click(&self, bounds: Rect, point: Point) -> Option<SidebarEvent> {
        let idx = self.index_at(bounds, point)?;
        match self.entries.get(idx)? {
            Entry::Item { path, .. } => Some(SidebarEvent::Navigate(path.clone())),
            Entry::Header(_) => None,
        }
    }

    fn index_at(&self, bounds: Rect, point: Point) -> Option<usize> {
        if !bounds.contains(point) {
            return None;
        }
        let mut y = bounds.top() + 8.0;
        for (i, entry) in self.entries.iter().enumerate() {
            let h = match entry {
                Entry::Header(_) => SECTION_HEADER_HEIGHT,
                Entry::Item { .. } => ROW_HEIGHT,
            };
            if point.y >= y && point.y < y + h {
                return Some(i);
            }
            y += h;
        }
        None
    }
}
