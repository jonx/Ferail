//! Persistent sidebar section order and disclosure state.
//!
//! IDs are platform-neutral and stay in the stored order even when their
//! section is unavailable on the current machine (Windows namespace, WSL,
//! volumes). Unknown IDs are ignored and newly-added IDs are appended.

use std::collections::HashSet;

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
