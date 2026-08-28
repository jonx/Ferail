//! Persistent sidebar section order and disclosure state.
//!
//! IDs are platform-neutral and stay in the stored order even when their
//! section is unavailable on the current machine (Windows namespace, WSL,
//! volumes). Unknown IDs are ignored and newly-added IDs are appended.

use std::collections::HashSet;

/// Icon-only sidebar geometry. The ordinary strip is deliberately narrower
/// than gpui-component's fixed 48-DIP default, and can surrender another
/// eight DIPs as the application window becomes exceptionally narrow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CollapsedSidebarGeometry {
    pub width: f32,
    /// Negative horizontal offset applied to gpui-component's fixed-width
    /// inner sidebar so Ferail's nested row gutters do not become a large
    /// blank margin before the icon.
    pub content_shift: f32,
}

const COLLAPSED_NOMINAL_WIDTH: f32 = 40.0;
const COLLAPSED_MIN_WIDTH: f32 = 32.0;
const COLLAPSED_MAX_VIEWPORT_FRACTION: f32 = 0.10;
// In the unshifted component, a 24-DIP Location icon is centred at x=34:
// 8 DIPs from gpui-component's content inset, 6 from Ferail's section inset,
// 8 from its row padding, then half the icon. Shifting this centre to half the
// effective panel width keeps the glyph centred while the window is resized.
const COLLAPSED_SOURCE_ICON_CENTER: f32 = 34.0;

/// Compute icon-only sidebar geometry from the current viewport on every
/// render. It stays 40 DIPs in ordinary windows, scales down over the
/// 320–400-DIP range, and never squeezes a 24-DIP glyph below four DIPs of
/// space on either side.
pub fn collapsed_sidebar_geometry(viewport_width: f32) -> CollapsedSidebarGeometry {
    let viewport_width = if viewport_width.is_finite() {
        viewport_width.max(0.0)
    } else {
        COLLAPSED_NOMINAL_WIDTH / COLLAPSED_MAX_VIEWPORT_FRACTION
    };
    let width = (viewport_width * COLLAPSED_MAX_VIEWPORT_FRACTION)
        .clamp(COLLAPSED_MIN_WIDTH, COLLAPSED_NOMINAL_WIDTH);
    CollapsedSidebarGeometry {
        width,
        content_shift: COLLAPSED_SOURCE_ICON_CENTER - width / 2.0,
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SidebarSection {
    Locations,
    Windows,
    Linux,
    Favorites,
    Recents,
    Browse,
    Volumes,
}

pub const DEFAULT_ORDER: [SidebarSection; 7] = [
    SidebarSection::Locations,
    SidebarSection::Windows,
    SidebarSection::Linux,
    SidebarSection::Favorites,
    SidebarSection::Recents,
    SidebarSection::Browse,
    SidebarSection::Volumes,
];

impl SidebarSection {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Locations => "locations",
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Favorites => "favorites",
            Self::Recents => "recents",
            Self::Browse => "browse",
            Self::Volumes => "volumes",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value.trim() {
            "locations" => Self::Locations,
            "windows" => Self::Windows,
            "linux" => Self::Linux,
            "favorites" => Self::Favorites,
            "recents" => Self::Recents,
            "browse" => Self::Browse,
            "volumes" => Self::Volumes,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SidebarLayout {
    pub order: Vec<SidebarSection>,
    collapsed: HashSet<SidebarSection>,
}

impl Default for SidebarLayout {
    fn default() -> Self {
        Self {
            order: DEFAULT_ORDER.to_vec(),
            collapsed: HashSet::new(),
        }
    }
}

impl SidebarLayout {
    pub fn from_persisted(order: Option<&str>, collapsed: Option<&str>) -> Self {
        let mut seen = HashSet::new();
        let mut parsed = order
            .into_iter()
            .flat_map(|value| value.split(','))
            .filter_map(SidebarSection::parse)
            .filter(|section| seen.insert(*section))
            .collect::<Vec<_>>();
        for section in DEFAULT_ORDER {
            if seen.insert(section) {
                parsed.push(section);
            }
        }
        let collapsed = collapsed
            .into_iter()
            .flat_map(|value| value.split(','))
            .filter_map(SidebarSection::parse)
            .collect();
        Self {
            order: parsed,
            collapsed,
        }
    }

    pub fn is_collapsed(&self, section: SidebarSection) -> bool {
        self.collapsed.contains(&section)
    }

    pub fn toggle(&mut self, section: SidebarSection) {
        if !self.collapsed.remove(&section) {
            self.collapsed.insert(section);
        }
    }

    pub fn move_before(&mut self, moving: SidebarSection, before: SidebarSection) {
        if moving == before {
            return;
        }
        self.order.retain(|section| *section != moving);
        let index = self
            .order
            .iter()
            .position(|section| *section == before)
            .unwrap_or(self.order.len());
        self.order.insert(index, moving);
    }

    pub fn move_to_end(&mut self, moving: SidebarSection) {
        self.order.retain(|section| *section != moving);
        self.order.push(moving);
    }

    pub fn reset_order(&mut self) {
        self.order = DEFAULT_ORDER.to_vec();
    }

    pub fn order_string(&self) -> String {
        self.order
            .iter()
            .map(|s| s.id())
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn collapsed_string(&self) -> String {
        DEFAULT_ORDER
            .into_iter()
            .filter(|section| self.collapsed.contains(section))
            .map(SidebarSection::id)
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_only_geometry_tracks_narrow_window_resizes() {
        assert_eq!(
            collapsed_sidebar_geometry(1_000.0),
            CollapsedSidebarGeometry {
                width: 40.0,
                content_shift: 14.0,
            }
        );
        assert_eq!(
            collapsed_sidebar_geometry(360.0),
            CollapsedSidebarGeometry {
                width: 36.0,
                content_shift: 16.0,
            }
        );
        assert_eq!(
            collapsed_sidebar_geometry(200.0),
            CollapsedSidebarGeometry {
                width: 32.0,
                content_shift: 18.0,
            }
        );
    }

    #[test]
    fn icon_only_geometry_handles_invalid_viewport_width() {
        assert_eq!(
            collapsed_sidebar_geometry(f32::NAN),
            collapsed_sidebar_geometry(1_000.0)
        );
    }

    #[test]
    fn persisted_order_is_reconciled() {
        let layout = SidebarLayout::from_persisted(
            Some("volumes,unknown,locations,volumes"),
            Some("browse,unknown"),
        );
        assert_eq!(
            layout.order[0..2],
            [SidebarSection::Volumes, SidebarSection::Locations]
        );
        assert_eq!(layout.order.len(), DEFAULT_ORDER.len());
        assert!(layout.is_collapsed(SidebarSection::Browse));
    }

    #[test]
    fn reorder_and_reset_are_lossless() {
        let mut layout = SidebarLayout::default();
        layout.move_before(SidebarSection::Volumes, SidebarSection::Locations);
        assert_eq!(layout.order[0], SidebarSection::Volumes);
        layout.reset_order();
        assert_eq!(layout.order, DEFAULT_ORDER);
    }
}
