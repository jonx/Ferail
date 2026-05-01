//! FileTree — virtualized tree of folders. Lazy children: clicking a
//! chevron emits `TreeEvent::ExpandRequested(NodeId)`; the host calls
//! `populate_children(...)` once enumeration completes.
//!
//! Iter-2 minimal scope: folders only (no files in tree), no animation,
//! no drag-drop hover-expand, no multi-select. Replaces the transient
//! `Sidebar` from step 9.
//!
//! Spec: `specs/controls/03-explorer-controls.md` §2.

use std::collections::HashMap;

use feraille_core::{EntryKind, FileEntry, NodeId};
use feraille_design::{FontWeight, Tokens};
use feraille_render::{Point, Rect, Renderer, TextStyle};

use crate::primitives::focus_ring;

const ROW_HEIGHT: f32 = 24.0;
const INDENT_PER_LEVEL: f32 = 16.0;
const CHEVRON_W: f32 = 14.0;
const ICON_SIZE: f32 = 14.0;

#[derive(Clone, Debug)]
struct Node {
    label: String,
    depth: u8,
    expanded: bool,
    children_loaded: bool,
    children: Vec<NodeId>,
}

#[derive(Clone, Debug)]
pub enum TreeEvent {
    Activate(NodeId),
    ExpandRequested(NodeId),
}

pub struct FileTree {
    nodes: HashMap<NodeId, Node>,
    /// Top-level (depth-0) nodes in display order.
    roots: Vec<NodeId>,
    /// Recomputed flat visible list, in display order.
    visible: Vec<NodeId>,
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
            roots: Vec::new(),
            visible: Vec::new(),
            scroll_offset: 0.0,
            selected: None,
            focused: false,
            hover_index: None,
        }
    }

    /// Replace top-level roots. Does not affect already-loaded children.
    pub fn set_roots(&mut self, roots: Vec<(NodeId, String)>) {
        self.roots.clear();
        for (id, label) in roots {
            if !self.nodes.contains_key(&id) {
                self.nodes.insert(
                    id,
                    Node {
                        label,
                        depth: 0,
                        expanded: false,
                        children_loaded: false,
                        children: Vec::new(),
                    },
                );
            }
            self.roots.push(id);
        }
        self.recompute_visible();
    }

    /// Host calls this after `FsBackend::enumerate(parent)` completes.
    pub fn populate_children(&mut self, parent: NodeId, entries: &[FileEntry]) {
        let depth = match self.nodes.get(&parent) {
            Some(n) => n.depth,
            None => return,
        };
        let mut child_ids: Vec<NodeId> = Vec::new();
        for e in entries {
            if !matches!(e.kind, EntryKind::Directory) {
                continue;
            }
            self.nodes.entry(e.id).or_insert_with(|| Node {
                label: e.name.clone(),
                depth: depth + 1,
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
        let roots = self.roots.clone();
        for r in roots {
            self.visit_node(r);
        }
    }

    fn visit_node(&mut self, id: NodeId) {
        self.visible.push(id);
        let (expanded, children) = match self.nodes.get(&id) {
            Some(n) => (n.expanded, n.children.clone()),
            None => return,
        };
        if expanded {
            for c in children {
                self.visit_node(c);
            }
        }
    }

    pub fn paint(&self, bounds: Rect, tokens: &Tokens, painter: &mut dyn Renderer) {
        painter.fill_rect(bounds, tokens.bg.layer2);
        if self.visible.is_empty() {
            return;
        }
        painter.push_clip(bounds);

        let viewport_h = bounds.size.height;
        let count = self.visible.len();
        let first = ((self.scroll_offset / ROW_HEIGHT).floor() as i64).max(0) as usize;
        let last = ((self.scroll_offset + viewport_h) / ROW_HEIGHT).ceil() as i64;
        let last = last.max(0).min(count as i64) as usize;

        for i in first..last {
            let row_top = bounds.top() + (i as f32 * ROW_HEIGHT) - self.scroll_offset;
            let row_rect = Rect::new(bounds.left(), row_top, bounds.size.width, ROW_HEIGHT);
            let id = self.visible[i];
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
            paint_row(node, row_rect, tokens, painter);
        }

        // FocusRing overlay on selected row when focused.
        if self.focused {
            if let Some(sel_id) = self.selected {
                if let Some(idx) = self.visible.iter().position(|n| *n == sel_id) {
                    let row_top = bounds.top() + (idx as f32 * ROW_HEIGHT) - self.scroll_offset;
                    let row_rect = Rect::new(bounds.left(), row_top, bounds.size.width, ROW_HEIGHT);
                    focus_ring::paint(row_rect, tokens, painter);
                }
            }
        }
        painter.pop_clip();
    }

    /// A click on the chevron toggles expand/collapse only. A click anywhere
    /// else on the row both activates (navigates the host's file pane) AND
    /// expands the node — Finder/Explorer convention. May return up to two
    /// events: an `ExpandRequested` (if children aren't loaded) followed by
    /// an `Activate`. The host fires them in order.
    pub fn click(&mut self, bounds: Rect, point: Point) -> Vec<TreeEvent> {
        let Some(idx) = self.index_at(bounds, point) else { return Vec::new() };
        let Some(&id) = self.visible.get(idx) else { return Vec::new() };
        let row_top = bounds.top() + (idx as f32 * ROW_HEIGHT) - self.scroll_offset;
        let row_rect = Rect::new(bounds.left(), row_top, bounds.size.width, ROW_HEIGHT);
        let depth = self.nodes.get(&id).map(|n| n.depth).unwrap_or(0);
        let chevron_x = row_rect.left() + 8.0 + depth as f32 * INDENT_PER_LEVEL;
        let chevron_rect = Rect::new(chevron_x, row_rect.top(), CHEVRON_W, row_rect.size.height);

        if chevron_rect.contains(point) {
            // Chevron-only: toggle without navigating.
            return self.toggle_expand(id).into_iter().collect();
        }

        // Row click: select + navigate + (auto-)expand if collapsed.
        self.selected = Some(id);
        let mut events = Vec::with_capacity(2);
        let needs_expand = self
            .nodes
            .get(&id)
            .map(|n| !n.expanded)
            .unwrap_or(false);
        if needs_expand {
            if let Some(ev) = self.toggle_expand(id) {
                events.push(ev);
            }
        }
        events.push(TreeEvent::Activate(id));
        events
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
        let max = ((self.visible.len() as f32) * ROW_HEIGHT - viewport_h).max(0.0);
        self.scroll_offset = (self.scroll_offset + delta).clamp(0.0, max);
    }

    pub fn select(&mut self, id: NodeId) {
        self.selected = Some(id);
    }

    pub fn content_height(&self) -> f32 {
        self.visible.len() as f32 * ROW_HEIGHT
    }

    fn index_at(&self, bounds: Rect, point: Point) -> Option<usize> {
        if !bounds.contains(point) {
            return None;
        }
        let local_y = point.y - bounds.top() + self.scroll_offset;
        if local_y < 0.0 {
            return None;
        }
        let idx = (local_y / ROW_HEIGHT) as usize;
        if idx >= self.visible.len() {
            None
        } else {
            Some(idx)
        }
    }
}

fn paint_row(node: &Node, row: Rect, tokens: &Tokens, painter: &mut dyn Renderer) {
    let indent = node.depth as f32 * INDENT_PER_LEVEL;
    let mut x = row.left() + 8.0 + indent;
    let text_y = row.top() + (ROW_HEIGHT - tokens.text.md) / 2.0 - 1.0;

    // Chevron (only if has children or unloaded).
    // Use BLACK TRIANGLE code points (U+25B6 / U+25BC) which Arial
    // ships with; the small triangles (U+25B8 / U+25BE) aren't always
    // present and silently fall back to .notdef.
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

    // Folder icon.
    let icon_y = row.top() + (ROW_HEIGHT - ICON_SIZE) / 2.0;
    painter.fill_rect(Rect::new(x, icon_y, ICON_SIZE, ICON_SIZE), tokens.accent.fill);
    x += ICON_SIZE + tokens.space.xs;

    // Label.
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
