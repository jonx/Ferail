//! FileTree — virtualized tree of folders, organized into sections.
//!
//! Iter-5.1 introduced sections (Recents / Favorites / Locations / Volumes)
//! mirroring Finder's sidebar layout. The visible list interleaves
//! `TreeRow::Header` rows (non-interactive uppercase labels) with
//! `TreeRow::Node` rows (interactive folders with chevron + icon).
//!
//! Lazy children: clicking a chevron emits `TreeEvent::ExpandRequested(NodeId)`;
//! the host calls `populate_children(...)` once enumeration completes.
//!
//! Spec: `specs/controls/03-explorer-controls.md` §2.

use std::collections::HashMap;

use feraille_core::{EntryKind, FileEntry, NodeId};
use feraille_design::{FontWeight, Tokens};
use feraille_render::{Bitmap, Point, Rect, Renderer, TextStyle};

use crate::primitives::focus_ring;

const ROW_HEIGHT: f32 = 24.0;
const HEADER_HEIGHT: f32 = 26.0;
const INDENT_PER_LEVEL: f32 = 16.0;
const CHEVRON_W: f32 = 14.0;
const ICON_SIZE: f32 = 14.0;

#[derive(Clone, Debug)]
struct Node {
    label: String,
    expanded: bool,
    children_loaded: bool,
    children: Vec<NodeId>,
}

impl SectionKind {
    /// Whether nodes in this section show a chevron and recurse to
    /// expanded children. Recents / Favorites are flat shortcut lists.
    /// Locations / Volumes are real tree roots.
    fn is_expandable(self) -> bool {
        matches!(self, SectionKind::Locations | SectionKind::Volumes)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum SectionKind {
    /// Top section without a visible header — recent folders.
    Recents,
    /// User-pinned folders.
    Favorites,
    /// Standard locations (iCloud Drive, Home, Macintosh HD, Trash).
    Locations,
    /// External / non-boot volumes.
    Volumes,
}

#[derive(Clone, Debug)]
pub struct Section {
    /// Uppercase header label, or `None` for a header-less section
    /// (the Recents section sits at the top with no label, like Finder).
    pub header: Option<String>,
    /// Used by iter-5.2 to differentiate click behavior across sections
    /// (e.g. Pin/Unpin shows in Favorites context menus only).
    #[allow(dead_code)]
    pub kind: SectionKind,
    /// Top-level entries in display order. Each may be expanded with
    /// its own children below.
    pub entries: Vec<NodeId>,
}

impl Section {
    pub fn new(kind: SectionKind, header: Option<&str>, entries: Vec<NodeId>) -> Self {
        Self { header: header.map(str::to_string), kind, entries }
    }
}

/// One renderable row in the flattened visible list.
#[derive(Clone, Debug)]
#[allow(dead_code)]
enum TreeRow {
    Header { label: String, kind: SectionKind },
    Node { id: NodeId, depth: u8, expandable: bool },
}

#[derive(Clone, Debug)]
pub enum TreeEvent {
    Activate(NodeId),
    ExpandRequested(NodeId),
}

pub struct FileTree {
    nodes: HashMap<NodeId, Node>,
    sections: Vec<Section>,
    /// Flat list of rows in display order, recomputed when sections /
    /// expanded state change.
    visible: Vec<TreeRow>,
    pub scroll_offset: f32,
    pub selected: Option<NodeId>,
    pub focused: bool,
    pub hover_index: Option<usize>,
}

impl Default for FileTree {
    fn default() -> Self {
        Self::new()
    }
}

impl FileTree {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            sections: Vec::new(),
            visible: Vec::new(),
            scroll_offset: 0.0,
            selected: None,
            focused: false,
            hover_index: None,
        }
    }

    /// Replace the section list. Each section's entries are inserted as
    /// depth-0 nodes; existing nodes already loaded from prior sections
    /// keep their `children_loaded` / `expanded` state.
    pub fn set_sections(&mut self, sections: Vec<(Section, Vec<(NodeId, String)>)>) {
        // The caller passes `(Section, [(NodeId, label_for_each_entry)])`.
        // Each NodeId may already exist (e.g. Documents appears in both
        // FAVORITES and as a child of Home). We never override an
        // existing label/state — the first source wins.
        self.sections.clear();
        for (mut section, labels) in sections {
            for (id, label) in labels {
                self.nodes.entry(id).or_insert_with(|| Node {
                    label: label.clone(),
                    expanded: false,
                    children_loaded: false,
                    children: Vec::new(),
                });
                if !section.entries.contains(&id) {
                    section.entries.push(id);
                }
            }
            self.sections.push(section);
        }
        self.recompute_visible();
    }

    /// Host calls this after `FsBackend::enumerate(parent)` completes.
    pub fn populate_children(&mut self, parent: NodeId, entries: &[FileEntry]) {
        if !self.nodes.contains_key(&parent) {
            return;
        }
        let mut child_ids: Vec<NodeId> = Vec::new();
        for e in entries {
            if !matches!(e.kind, EntryKind::Directory) {
                continue;
            }
            self.nodes.entry(e.id).or_insert_with(|| Node {
                label: e.name.clone(),
                expanded: false,
                children_loaded: false,
                children: Vec::new(),
            });
            child_ids.push(e.id);
        }
        if let Some(n) = self.nodes.get_mut(&parent) {
            n.children = child_ids;
            n.children_loaded = true;
            n.expanded = true;
        }
        self.recompute_visible();
    }

    /// Toggle expand/collapse. Returns `Some(TreeEvent::ExpandRequested)` if
    /// the node needs its children loaded.
    fn toggle_expand(&mut self, id: NodeId) -> Option<TreeEvent> {
        let need_load = {
            let n = self.nodes.get_mut(&id)?;
            if n.expanded {
                n.expanded = false;
                self.recompute_visible();
                return None;
            }
            n.expanded = true;
            !n.children_loaded
        };
        self.recompute_visible();
        if need_load {
            Some(TreeEvent::ExpandRequested(id))
        } else {
            None
        }
    }

    fn recompute_visible(&mut self) {
        self.visible.clear();
        let sections = self.sections.clone();
        for section in sections {
            if let Some(label) = &section.header {
                self.visible.push(TreeRow::Header {
                    label: label.clone(),
                    kind: section.kind,
                });
            }
            let expandable = section.kind.is_expandable();
            for entry in &section.entries {
                if expandable {
                    self.visit_expandable(*entry, 0);
                } else {
                    // Flat shortcut: no chevron, no recursion.
                    self.visible.push(TreeRow::Node {
                        id: *entry,
                        depth: 0,
                        expandable: false,
                    });
                }
            }
        }
    }

    fn visit_expandable(&mut self, id: NodeId, depth: u8) {
        self.visible.push(TreeRow::Node { id, depth, expandable: true });
        let (expanded, children) = match self.nodes.get(&id) {
            Some(n) => (n.expanded, n.children.clone()),
            None => return,
        };
        if expanded {
            for c in children {
                self.visit_expandable(c, depth + 1);
            }
        }
    }

    pub fn paint(
        &self,
        bounds: Rect,
        tokens: &Tokens,
        painter: &mut dyn Renderer,
        heat_for: impl Fn(NodeId) -> f32,
        dir_icon: Option<&Bitmap>,
    ) {
        painter.fill_rect(bounds, tokens.bg.layer2);
        if self.visible.is_empty() {
            return;
        }
        painter.push_clip(bounds);

        // Variable row heights mean we can't index by simple division;
        // walk row offsets sequentially. At sub-100 rows this is fine.
        let mut y = bounds.top() - self.scroll_offset;
        let mut focus_paint: Option<Rect> = None;
        for (i, row) in self.visible.iter().enumerate() {
            let h = row_height(row);
            let row_top = y;
            y += h;
            let row_rect = Rect::new(bounds.left(), row_top, bounds.size.width, h);
            // Cull rows fully above or below the viewport.
            if row_rect.bottom() <= bounds.top() || row_rect.top() >= bounds.bottom() {
                continue;
            }

            match row {
                TreeRow::Header { label, .. } => {
                    paint_header(label, row_rect, tokens, painter);
                }
                TreeRow::Node { id, depth, expandable } => {
                    let id = *id;
                    let Some(node) = self.nodes.get(&id) else { continue };
                    let is_selected = self.selected == Some(id);
                    let is_hover = self.hover_index == Some(i);
                    if is_selected {
                        let bg = if self.focused {
                            tokens.accent.subtle
                        } else {
                            tokens.accent.subtle_inactive
                        };
                        painter.fill_rect(row_rect, bg);
                    } else if is_hover {
                        painter.fill_rect(row_rect, tokens.bg.layer3);
                    }
                    let heat = heat_for(id);
                    if heat > 0.0 {
                        let alpha = (heat * 180.0).clamp(20.0, 200.0) as u8;
                        let strip = feraille_design::Color {
                            r: tokens.accent.fill.r,
                            g: tokens.accent.fill.g,
                            b: tokens.accent.fill.b,
                            a: alpha,
                        };
                        painter.fill_rect(
                            Rect::new(row_rect.left(), row_rect.top(), 2.0, row_rect.size.height),
                            strip,
                        );
                    }
                    paint_row(node, row_rect, *depth, *expandable, dir_icon, tokens, painter);

                    if self.focused && is_selected {
                        focus_paint = Some(row_rect);
                    }
                }
            }
        }

        if let Some(rect) = focus_paint {
            focus_ring::paint(rect, tokens, painter);
        }
        painter.pop_clip();
    }

    /// Click handling. Headers are no-ops; node clicks behave as before
    /// (chevron-only toggles, row-click selects + activates + expands).
    pub fn click(&mut self, bounds: Rect, point: Point) -> Option<Vec<TreeEvent>> {
        let idx = self.index_at(bounds, point)?;
        let row = self.visible.get(idx)?.clone();
        let (id, depth, expandable) = match row {
            TreeRow::Node { id, depth, expandable } => (id, depth, expandable),
            TreeRow::Header { .. } => {
                // Click on a header — return Some(empty) so the host knows we
                // handled it (and redraws if needed) without firing an event.
                return Some(Vec::new());
            }
        };
        let row_rect = self.row_rect(bounds, idx)?;

        // Flat-shortcut sections (Recents / Favorites): always activate,
        // no chevron, no expand.
        if !expandable {
            self.selected = Some(id);
            return Some(vec![TreeEvent::Activate(id)]);
        }

        let chevron_x = row_rect.left() + 8.0 + depth as f32 * INDENT_PER_LEVEL;
        let chevron_rect = Rect::new(chevron_x, row_rect.top(), CHEVRON_W, row_rect.size.height);
        if chevron_rect.contains(point) {
            return Some(self.toggle_expand(id).into_iter().collect());
        }

        self.selected = Some(id);
        let mut events = Vec::with_capacity(2);
        let needs_expand = self.nodes.get(&id).map(|n| !n.expanded).unwrap_or(false);
        if needs_expand {
            if let Some(ev) = self.toggle_expand(id) {
                events.push(ev);
            }
        }
        events.push(TreeEvent::Activate(id));
        Some(events)
    }

    /// Expand the node by NodeId — used by the host when the user navigates
    /// through other paths (breadcrumb, file-pane Enter) and the tree
    /// should reflect the new state.
    pub fn ensure_expanded(&mut self, id: NodeId) -> Option<TreeEvent> {
        let needs_load = {
            let Some(n) = self.nodes.get_mut(&id) else { return None };
            if n.expanded {
                return None;
            }
            n.expanded = true;
            !n.children_loaded
        };
        self.recompute_visible();
        if needs_load {
            Some(TreeEvent::ExpandRequested(id))
        } else {
            None
        }
    }

    pub fn update_hover(&mut self, bounds: Rect, point: Option<Point>) -> bool {
        let new_hover = point.and_then(|p| self.index_at(bounds, p));
        if new_hover != self.hover_index {
            self.hover_index = new_hover;
            true
        } else {
            false
        }
    }

    pub fn scroll_by(&mut self, delta: f32, viewport_h: f32) {
        let max = (self.content_height() - viewport_h).max(0.0);
        self.scroll_offset = (self.scroll_offset + delta).clamp(0.0, max);
    }

    pub fn select(&mut self, id: NodeId) {
        self.selected = Some(id);
    }

    pub fn is_loaded(&self, id: NodeId) -> bool {
        self.nodes.get(&id).map(|n| n.children_loaded).unwrap_or(false)
    }

    pub fn invalidate(&mut self, id: NodeId) {
        if let Some(n) = self.nodes.get_mut(&id) {
            n.children.clear();
            n.children_loaded = false;
        }
        self.recompute_visible();
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    /// Adjust scroll so `id` is visible in `viewport_h` DIPs of viewport.
    pub fn ensure_visible(&mut self, id: NodeId, viewport_h: f32) {
        let Some(idx) = self
            .visible
            .iter()
            .position(|r| matches!(r, TreeRow::Node { id: nid, .. } if *nid == id))
        else {
            return;
        };
        let mut y = 0.0_f32;
        for row in &self.visible[..idx] {
            y += row_height(row);
        }
        let row_h = ROW_HEIGHT;
        if y < self.scroll_offset {
            self.scroll_offset = y;
        } else if y + row_h > self.scroll_offset + viewport_h {
            self.scroll_offset = (y + row_h - viewport_h).max(0.0);
        }
    }

    pub fn content_height(&self) -> f32 {
        self.visible.iter().map(row_height).sum()
    }

    fn row_rect(&self, bounds: Rect, idx: usize) -> Option<Rect> {
        let mut y = bounds.top() - self.scroll_offset;
        for (i, row) in self.visible.iter().enumerate() {
            let h = row_height(row);
            if i == idx {
                return Some(Rect::new(bounds.left(), y, bounds.size.width, h));
            }
            y += h;
        }
        None
    }

    fn index_at(&self, bounds: Rect, point: Point) -> Option<usize> {
        if !bounds.contains(point) {
            return None;
        }
        let mut y = bounds.top() - self.scroll_offset;
        for (i, row) in self.visible.iter().enumerate() {
            let h = row_height(row);
            if point.y >= y && point.y < y + h {
                return Some(i);
            }
            y += h;
        }
        None
    }
}

fn row_height(row: &TreeRow) -> f32 {
    match row {
        TreeRow::Header { .. } => HEADER_HEIGHT,
        TreeRow::Node { .. } => ROW_HEIGHT,
    }
}

fn paint_header(label: &str, row: Rect, tokens: &Tokens, painter: &mut dyn Renderer) {
    // No background fill — uses the section's own bg.layer2. Subtle
    // separator at the top of the header gives visual breathing room
    // between sections.
    let text_y = row.top() + (row.size.height - tokens.text.xs) / 2.0 + 2.0;
    painter.draw_text(
        Point::new(row.left() + 12.0, text_y),
        label,
        TextStyle {
            size: tokens.text.xs,
            weight: FontWeight::SemiBold,
            color: tokens.fg.secondary,
        },
    );
}

fn paint_row(
    node: &Node,
    row: Rect,
    depth: u8,
    expandable: bool,
    dir_icon: Option<&Bitmap>,
    tokens: &Tokens,
    painter: &mut dyn Renderer,
) {
    let indent = depth as f32 * INDENT_PER_LEVEL;
    let mut x = row.left() + 8.0 + indent;
    let text_y = row.top() + (ROW_HEIGHT - tokens.text.md) / 2.0 - 1.0;

    if expandable {
        let chevron_x = x;
        let chevron_glyph = if node.expanded { "\u{25BC}" } else { "\u{25B6}" };
        painter.draw_text(
            Point::new(chevron_x + 1.0, text_y + 1.0),
            chevron_glyph,
            TextStyle {
                size: tokens.text.sm,
                weight: FontWeight::Regular,
                color: tokens.fg.secondary,
            },
        );
        x += CHEVRON_W;
    }

    let icon_y = row.top() + (ROW_HEIGHT - ICON_SIZE) / 2.0;
    let icon_rect = Rect::new(x, icon_y, ICON_SIZE, ICON_SIZE);
    if let Some(bitmap) = dir_icon {
        painter.draw_bitmap(icon_rect, bitmap);
    } else {
        painter.fill_rect(icon_rect, tokens.accent.fill);
    }
    x += ICON_SIZE + tokens.space.xs;

    painter.draw_text(
        Point::new(x, text_y),
        &node.label,
        TextStyle {
            size: tokens.text.md,
            weight: FontWeight::Regular,
            color: tokens.fg.primary,
        },
    );
}
