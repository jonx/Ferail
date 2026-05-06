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
use std::time::{Duration, Instant};

use feraille_core::{EntryKind, FileEntry, NodeId};
use feraille_design::{FontWeight, Tokens};
use feraille_render::{Bitmap, Point, Rect, Renderer, TextStyle};

use crate::primitives::focus_ring;

/// Idle window after which the type-ahead buffer resets. Matches
/// `specs/ux/02-selection.md`.
const TYPE_AHEAD_RESET: Duration = Duration::from_millis(800);

/// Hover dwell before a tooltip appears for a truncated label.
const TOOLTIP_DELAY: Duration = Duration::from_millis(600);

const ROW_HEIGHT: f32 = 24.0;
const HEADER_HEIGHT: f32 = 26.0;
const INDENT_PER_LEVEL: f32 = 16.0;
const CHEVRON_W: f32 = 14.0;
const ICON_SIZE: f32 = 14.0;
const ELLIPSIS: &str = "\u{2026}";
const TOOLTIP_PAD_X: f32 = 8.0;
const TOOLTIP_PAD_Y: f32 = 4.0;

#[derive(Clone, Debug)]
struct Node {
    label: String,
    expanded: bool,
    children_loaded: bool,
    children: Vec<NodeId>,
    /// Recorded on `populate_children` so Left-arrow can walk up the tree
    /// without an extra reverse-index. `None` for section roots.
    parent: Option<NodeId>,
    /// Volume capacity for this node, when it represents a mounted volume
    /// root (e.g. `/`, `/Volumes/Foo`). `None` for ordinary folders.
    capacity: Option<NodeCapacity>,
}

/// Bytes used to paint a Finder-style capacity bar under a Volumes /
/// Locations row. `total == 0` is treated as "no data" and skips paint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeCapacity {
    pub total: u64,
    pub available: u64,
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

/// Section + node identification for the host's right-click menu.
/// `kind` lets the host pick context-menu items (e.g. "Remove from
/// Favorites" only when `Favorites`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeContextTarget {
    pub kind: SectionKind,
    pub id: NodeId,
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
    /// Type-ahead state. Buffer accumulates while keypresses arrive
    /// faster than `TYPE_AHEAD_RESET`; cleared on idle.
    type_ahead: String,
    type_ahead_last: Option<Instant>,
    /// When the cursor first entered `hover_index`. Used to delay
    /// tooltip appearance.
    hover_since: Option<Instant>,
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
            type_ahead: String::new(),
            type_ahead_last: None,
            hover_since: None,
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
        // Volume capacity is metadata that can change between rebuilds
        // (remount, eject); clear and let the host re-attach via
        // set_node_capacity.
        for n in self.nodes.values_mut() {
            n.capacity = None;
        }
        for (mut section, labels) in sections {
            for (id, label) in labels {
                self.nodes.entry(id).or_insert_with(|| Node {
                    label: label.clone(),
                    expanded: false,
                    children_loaded: false,
                    children: Vec::new(),
                    parent: None,
                    capacity: None,
                });
                if !section.entries.contains(&id) {
                    section.entries.push(id);
                }
            }
            self.sections.push(section);
        }
        self.recompute_visible();
    }

    /// Attach (or clear) volume capacity for a node. Painted as a thin
    /// horizontal bar under the row in `paint_row`. No-op if the node
    /// doesn't exist.
    pub fn set_node_capacity(&mut self, id: NodeId, cap: Option<NodeCapacity>) {
        if let Some(n) = self.nodes.get_mut(&id) {
            n.capacity = cap;
        }
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
            self.nodes
                .entry(e.id)
                .and_modify(|n| {
                    if n.parent.is_none() {
                        n.parent = Some(parent);
                    }
                })
                .or_insert_with(|| Node {
                    label: e.name.clone(),
                    expanded: false,
                    children_loaded: false,
                    children: Vec::new(),
                    parent: Some(parent),
                    capacity: None,
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
                    paint_indent_guides(row_rect, *depth, tokens, painter);
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

        // Tooltip overlay for hovered, truncated labels. Drawn outside
        // the tree clip so it can cap at the pane boundary cleanly.
        self.paint_tooltip(bounds, tokens, painter);
    }

    fn paint_tooltip(&self, bounds: Rect, tokens: &Tokens, painter: &mut dyn Renderer) {
        let Some(idx) = self.hover_index else { return };
        let Some(since) = self.hover_since else { return };
        if since.elapsed() < TOOLTIP_DELAY {
            return;
        }
        let row = match self.visible.get(idx).cloned() {
            Some(r) => r,
            None => return,
        };
        let (id, depth, expandable) = match row {
            TreeRow::Node { id, depth, expandable } => (id, depth, expandable),
            TreeRow::Header { .. } => return,
        };
        let Some(node) = self.nodes.get(&id) else { return };
        let Some(row_rect) = self.row_rect(bounds, idx) else { return };

        let style = TextStyle {
            size: tokens.text.md,
            weight: FontWeight::Regular,
            color: tokens.fg.primary,
        };
        let label_x = label_left(row_rect, depth, expandable, tokens);
        let max_w = (row_rect.right() - label_x).max(0.0);
        let full_w = painter.measure_text(&node.label, style).width;
        if full_w <= max_w + 0.5 {
            // Not truncated — no tooltip.
            return;
        }

        // Position tooltip to the right of the row, vertically aligned.
        // Cap right edge at the pane boundary; if it would overflow,
        // fall back to anchoring at the pane's right edge minus the
        // tooltip width so the user still sees the full name.
        let tip_w = full_w + TOOLTIP_PAD_X * 2.0;
        let tip_h = tokens.text.md + TOOLTIP_PAD_Y * 2.0 + 2.0;
        let preferred_x = row_rect.left() + 8.0;
        let mut tip_x = preferred_x;
        if tip_x + tip_w > bounds.right() {
            tip_x = (bounds.right() - tip_w).max(bounds.left());
        }
        let tip_y = (row_rect.bottom() + 2.0).min(bounds.bottom() - tip_h);
        let tip_rect = Rect::new(tip_x, tip_y, tip_w, tip_h);
        painter.fill_rect(tip_rect, tokens.bg.layer1);
        painter.stroke_rect(tip_rect, 1.0, tokens.border.default);
        let text_y = tip_y + (tip_h - tokens.text.md) / 2.0 - 1.0;
        painter.draw_text(
            Point::new(tip_x + TOOLTIP_PAD_X, text_y),
            &node.label,
            style,
        );
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
            self.hover_since = if new_hover.is_some() {
                Some(Instant::now())
            } else {
                None
            };
            true
        } else {
            false
        }
    }

    /// Whether the host should request a redraw after the tooltip-delay
    /// elapses. The host calls this on its event-tick or animation
    /// timer; if `true`, schedule a redraw `TOOLTIP_DELAY` after
    /// `hover_since`.
    pub fn pending_tooltip(&self) -> Option<Instant> {
        self.hover_since.map(|t| t + TOOLTIP_DELAY)
    }

    pub fn scroll_by(&mut self, delta: f32, viewport_h: f32) {
        let max = (self.content_height() - viewport_h).max(0.0);
        self.scroll_offset = (self.scroll_offset + delta).clamp(0.0, max);
    }

    pub fn select(&mut self, id: NodeId) {
        self.selected = Some(id);
    }

    /// Position of the currently-selected node within `visible`, if it
    /// is a `TreeRow::Node` (header rows can never be "selected").
    fn selected_visible_index(&self) -> Option<usize> {
        let id = self.selected?;
        self.visible
            .iter()
            .position(|r| matches!(r, TreeRow::Node { id: nid, .. } if *nid == id))
    }

    /// Find the next `TreeRow::Node` index in `direction` (+1 / -1)
    /// starting from `from`, skipping headers. Returns `None` if there
    /// is no Node row in that direction.
    fn next_node_index(&self, from: Option<usize>, direction: i32) -> Option<usize> {
        if self.visible.is_empty() {
            return None;
        }
        let len = self.visible.len() as i32;
        let mut i = match (from, direction.signum()) {
            (Some(f), 1) => f as i32 + 1,
            (Some(f), -1) => f as i32 - 1,
            (None, 1) | (None, 0) => 0,
            (None, -1) => len - 1,
            _ => 0,
        };
        let step = if direction >= 0 { 1 } else { -1 };
        while i >= 0 && i < len {
            if matches!(self.visible[i as usize], TreeRow::Node { .. }) {
                return Some(i as usize);
            }
            i += step;
        }
        None
    }

    /// Move selection by `delta` rows (skipping headers). Updates scroll
    /// to keep the new selection visible. No-op if `visible` has no
    /// Node rows. Returns `true` if the selection changed.
    pub fn move_cursor(&mut self, delta: i32, viewport_h: f32) -> bool {
        let cur = self.selected_visible_index();
        let target = if delta > 0 {
            self.next_node_index(cur, 1)
        } else if delta < 0 {
            self.next_node_index(cur, -1)
        } else {
            return false;
        };
        match target.and_then(|i| match &self.visible[i] {
            TreeRow::Node { id, .. } => Some(*id),
            _ => None,
        }) {
            Some(id) if Some(id) != self.selected => {
                self.selected = Some(id);
                self.ensure_visible(id, viewport_h);
                true
            }
            _ => false,
        }
    }

    /// Jump to the first / last Node row.
    pub fn move_to_first(&mut self, viewport_h: f32) -> bool {
        if let Some(i) = self.next_node_index(None, 1) {
            if let TreeRow::Node { id, .. } = self.visible[i] {
                self.selected = Some(id);
                self.ensure_visible(id, viewport_h);
                return true;
            }
        }
        false
    }

    pub fn move_to_last(&mut self, viewport_h: f32) -> bool {
        if let Some(i) = self.next_node_index(None, -1) {
            if let TreeRow::Node { id, .. } = self.visible[i] {
                self.selected = Some(id);
                self.ensure_visible(id, viewport_h);
                return true;
            }
        }
        false
    }

    /// Left-arrow semantics (VS Code style): if the selected node is
    /// expanded, collapse it; otherwise jump to its parent. `Activate`
    /// is *not* fired — Left only affects tree shape and cursor.
    pub fn collapse_or_parent(&mut self, viewport_h: f32) -> bool {
        let Some(id) = self.selected else { return false };
        let (expanded, parent) = match self.nodes.get(&id) {
            Some(n) => (n.expanded, n.parent),
            None => return false,
        };
        if expanded {
            if let Some(n) = self.nodes.get_mut(&id) {
                n.expanded = false;
            }
            self.recompute_visible();
            return true;
        }
        if let Some(p) = parent {
            if self.nodes.contains_key(&p) {
                self.selected = Some(p);
                self.ensure_visible(p, viewport_h);
                return true;
            }
        }
        false
    }

    /// Right-arrow semantics: if the selected expandable node is
    /// collapsed, expand it (firing `ExpandRequested` if children aren't
    /// loaded). If already expanded with children, move to first child.
    pub fn expand_or_first_child(&mut self, viewport_h: f32) -> Option<TreeEvent> {
        let id = self.selected?;
        let (expanded, has_children, expandable) = {
            let n = self.nodes.get(&id)?;
            // A node is "expandable" in the keyboard sense if it sits in
            // an expandable section; we approximate by checking the
            // visible row (since `Node.expandable` flag isn't on the
            // struct).
            let expandable = self
                .visible
                .iter()
                .find_map(|r| match r {
                    TreeRow::Node { id: nid, expandable, .. } if *nid == id => Some(*expandable),
                    _ => None,
                })
                .unwrap_or(false);
            (n.expanded, !n.children.is_empty(), expandable)
        };
        if !expandable {
            return None;
        }
        if !expanded {
            return self.toggle_expand(id);
        }
        if has_children {
            let first = self.nodes.get(&id).and_then(|n| n.children.first().copied());
            if let Some(c) = first {
                self.selected = Some(c);
                self.ensure_visible(c, viewport_h);
            }
        }
        None
    }

    /// Enter / Return on the selected node — Activate.
    pub fn activate_selected(&self) -> Option<TreeEvent> {
        self.selected.map(TreeEvent::Activate)
    }

    /// Push one type-ahead character. Returns `true` if a match was
    /// found and selection advanced. Buffer auto-resets after
    /// `TYPE_AHEAD_RESET` of idle time.
    pub fn type_ahead_push(&mut self, ch: char, viewport_h: f32) -> bool {
        let now = Instant::now();
        let stale = self
            .type_ahead_last
            .map(|t| now.duration_since(t) > TYPE_AHEAD_RESET)
            .unwrap_or(true);
        if stale {
            self.type_ahead.clear();
        }
        self.type_ahead_last = Some(now);
        for c in ch.to_lowercase() {
            self.type_ahead.push(c);
        }
        let needle = self.type_ahead.clone();
        let cur = self.selected_visible_index();
        // Start scanning from `cur + 1` for a single new char (cycling
        // through duplicates), or from `cur` when refining a multi-char
        // buffer (so the first additional char can match the same row).
        let start = match (cur, needle.chars().count()) {
            (Some(i), 1) => (i + 1) % self.visible.len().max(1),
            (Some(i), _) => i,
            _ => 0,
        };
        let len = self.visible.len();
        for offset in 0..len {
            let i = (start + offset) % len;
            if let TreeRow::Node { id, .. } = self.visible[i] {
                if let Some(node) = self.nodes.get(&id) {
                    if node.label.to_lowercase().starts_with(&needle) {
                        self.selected = Some(id);
                        self.ensure_visible(id, viewport_h);
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn type_ahead_clear(&mut self) {
        self.type_ahead.clear();
        self.type_ahead_last = None;
    }

    /// Right-click on a tree row: select the node and return the
    /// section + id so the host can build a context menu. Header rows
    /// return `None`. Outside the tree returns `None`.
    pub fn right_click(&mut self, bounds: Rect, point: Point) -> Option<TreeContextTarget> {
        let idx = self.index_at(bounds, point)?;
        let row = self.visible.get(idx)?.clone();
        let TreeRow::Node { id, .. } = row else {
            return None;
        };
        // Find the section this index falls into by walking visible rows.
        let kind = self.section_kind_at(idx)?;
        self.selected = Some(id);
        Some(TreeContextTarget { kind, id })
    }

    fn section_kind_at(&self, idx: usize) -> Option<SectionKind> {
        let mut current: Option<SectionKind> = None;
        for (i, row) in self.visible.iter().enumerate() {
            match row {
                TreeRow::Header { kind, .. } => current = Some(*kind),
                TreeRow::Node { .. } => {
                    if i == idx {
                        // Recents has no header — default to Recents
                        // when no header has been seen yet.
                        return Some(current.unwrap_or(SectionKind::Recents));
                    }
                }
            }
        }
        None
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

/// X coordinate where the label text starts for a row at `depth`.
/// Mirrors the layout in `paint_row` (kept in sync by both reading the
/// same constants).
fn label_left(row: Rect, depth: u8, expandable: bool, tokens: &Tokens) -> f32 {
    let indent = depth as f32 * INDENT_PER_LEVEL;
    let mut x = row.left() + 8.0 + indent;
    if expandable {
        x += CHEVRON_W;
    }
    x += ICON_SIZE + tokens.space.xs;
    x
}

/// Draw 1-DIP vertical guides at each ancestor depth, sitting in the
/// gutter between the indent and the chevron. Color is `border.subtle`
/// — visible but quiet. Skipped at depth 0 (no guide for root rows).
fn paint_indent_guides(row: Rect, depth: u8, tokens: &Tokens, painter: &mut dyn Renderer) {
    if depth == 0 {
        return;
    }
    // Each guide sits at the left edge of *its* indent column. So
    // depth=1's guide is at column 0's right edge, etc. We draw one
    // guide per ancestor (i.e. for depth=N, draw N guides).
    for d in 0..depth {
        let x = row.left() + 8.0 + d as f32 * INDENT_PER_LEVEL + INDENT_PER_LEVEL / 2.0;
        painter.fill_rect(
            Rect::new(x, row.top(), 1.0, row.size.height),
            tokens.border.default,
        );
    }
}

/// Truncate `label` so that `label + "…"` fits within `max_width` DIPs
/// when measured under `style`. Returns `(text_to_draw, was_truncated)`.
/// Uses a coarse byte scan rather than grapheme clusters — fine for
/// filenames where boundary cases are rare.
fn truncate_to_width(
    label: &str,
    max_width: f32,
    style: TextStyle,
    painter: &dyn Renderer,
) -> (String, bool) {
    if max_width <= 0.0 {
        return (String::new(), !label.is_empty());
    }
    let full = painter.measure_text(label, style).width;
    if full <= max_width {
        return (label.to_string(), false);
    }
    let ellipsis_w = painter.measure_text(ELLIPSIS, style).width;
    let target = (max_width - ellipsis_w).max(0.0);
    // Walk char boundaries from start, accumulating until we'd exceed
    // `target`. We re-measure progressively — a few hundred chars at
    // worst, and the cache keeps it cheap.
    let mut cut = 0usize;
    for (i, _) in label.char_indices() {
        let w = painter.measure_text(&label[..i], style).width;
        if w > target {
            break;
        }
        cut = i;
    }
    let mut out = label[..cut].trim_end().to_string();
    out.push_str(ELLIPSIS);
    (out, true)
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

    let style = TextStyle {
        size: tokens.text.md,
        weight: FontWeight::Regular,
        color: tokens.fg.primary,
    };
    let max_w = (row.right() - x - 4.0).max(0.0);
    let (text, _truncated) = truncate_to_width(&node.label, max_w, style, painter);
    painter.draw_text(Point::new(x, text_y), &text, style);

    if let Some(cap) = node.capacity {
        if cap.total > 0 {
            // 3-DIP bar at the bottom of the row, spanning the label
            // area. Track uses `border.default` rather than `subtle` so
            // it stays visible against `bg.layer2` in dark mode. Fill
            // is neutral grey by default and escalates on heavy use.
            let bar_h = 3.0;
            let bar_y = row.bottom() - bar_h - 2.0;
            let bar_left = x;
            let bar_right = (row.right() - 8.0).max(bar_left);
            let bar_w = bar_right - bar_left;
            if bar_w >= 24.0 {
                let track = Rect::new(bar_left, bar_y, bar_w, bar_h);
                painter.fill_rect(track, tokens.border.default);
                let used = cap.total.saturating_sub(cap.available);
                let frac = (used as f64 / cap.total as f64).clamp(0.0, 1.0) as f32;
                let fill_w = (bar_w * frac).clamp(0.0, bar_w);
                if fill_w > 0.0 {
                    let fill_color = if frac >= 0.97 {
                        tokens.status.danger
                    } else if frac >= 0.90 {
                        tokens.status.warning
                    } else {
                        tokens.fg.secondary
                    };
                    painter.fill_rect(Rect::new(bar_left, bar_y, fill_w, bar_h), fill_color);
                }
            }
        }
    }
}
