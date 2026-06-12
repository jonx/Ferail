//! Favorite shortcut data model — pure data, no UI, no filesystem.
//!
//! A favorite is the user's pinned reference to a location: folder,
//! volume, application bundle, and (reserved) saved searches and
//! tags. Identity is a stable UUID that survives renames, moves,
//! and "Locate..." repointing. Ordering is fractional so a single
//! reorder writes one row instead of the whole list. Persistence is
//! handled by `feraille-meta`; in-memory observation and the §5
//! favorites index live in `feraille-gpui::favorites`.

use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Stable identity of a favorite. Persisted across launches.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FavoriteId(pub Uuid);

impl FavoriteId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl std::str::FromStr for FavoriteId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self)
    }
}

impl Default for FavoriteId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for FavoriteId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FavoriteKind {
    Folder,
    Volume,
    Application,
    /// Reserved for a future iteration — saved search queries.
    SavedSearch,
    /// Reserved for a future iteration — Finder tag favorites.
    Tag,
}

impl FavoriteKind {
    pub fn as_db_code(self) -> i64 {
        match self {
            Self::Folder => 1,
            Self::Volume => 2,
            Self::Application => 3,
            Self::SavedSearch => 4,
            Self::Tag => 5,
        }
    }
    pub fn from_db_code(code: i64) -> Option<Self> {
        match code {
            1 => Some(Self::Folder),
            2 => Some(Self::Volume),
            3 => Some(Self::Application),
            4 => Some(Self::SavedSearch),
            5 => Some(Self::Tag),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FavoriteTarget {
    /// Canonical (symlink-resolved) absolute path. Resolved at add-time;
    /// subsequent on-disk renames do not re-canonicalize, so the favorite
    /// can detect Missing state by comparing against this stored path.
    Path(PathBuf),
    /// Reserved — saved-search query string.
    SavedSearch(String),
    /// Reserved — Finder tag identifier.
    Tag(String),
}

impl FavoriteTarget {
    pub fn to_db(&self) -> (&'static str, String) {
        match self {
            Self::Path(p) => ("path", p.to_string_lossy().into_owned()),
            Self::SavedSearch(q) => ("search", q.clone()),
            Self::Tag(t) => ("tag", t.clone()),
        }
    }
    pub fn from_db(tag: &str, value: &str) -> Option<Self> {
        match tag {
            "path" => Some(Self::Path(PathBuf::from(value))),
            "search" => Some(Self::SavedSearch(value.to_string())),
            "tag" => Some(Self::Tag(value.to_string())),
            _ => None,
        }
    }
    pub fn as_path(&self) -> Option<&Path> {
        match self {
            Self::Path(p) => Some(p.as_path()),
            _ => None,
        }
    }
}

/// User-overridable icon. `None` on a favorite means "default for kind +
/// target": folder icon, NSWorkspace volume icon, application bundle icon.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FavoriteIcon {
    /// Named Lucide glyph, e.g. "star", "briefcase".
    Lucide(String),
    /// Tinted default-folder icon by named accent color.
    TintedFolder(String),
}

impl FavoriteIcon {
    pub fn to_db(&self) -> String {
        match self {
            Self::Lucide(name) => format!("lucide:{name}"),
            Self::TintedFolder(c) => format!("tint:{c}"),
        }
    }
    pub fn from_db(s: &str) -> Option<Self> {
        if let Some(name) = s.strip_prefix("lucide:") {
            Some(Self::Lucide(name.to_string()))
        } else { s.strip_prefix("tint:").map(|color| Self::TintedFolder(color.to_string())) }
    }
}

/// Runtime availability state of a favorite. Derived from the filesystem
/// and volume mount notifications; never persisted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FavoriteState {
    Available,
    Unmounted,
    Missing,
}

#[derive(Clone, Debug)]
pub struct Favorite {
    pub id: FavoriteId,
    pub kind: FavoriteKind,
    pub target: FavoriteTarget,
    pub display_name: Option<String>,
    pub custom_icon: Option<FavoriteIcon>,
    pub sort_index: f64,
    pub date_added: i64,
}

impl Favorite {
    pub fn folder(target: PathBuf, sort_index: f64, now_unix: i64) -> Self {
        Self {
            id: FavoriteId::new(),
            kind: FavoriteKind::Folder,
            target: FavoriteTarget::Path(target),
            display_name: None,
            custom_icon: None,
            sort_index,
            date_added: now_unix,
        }
    }

    /// Effective label: custom name if set, else basename of the target
    /// path. Volume / saved-search / tag favorites with no custom name
    /// fall back to the raw string the caller passed at add-time —
    /// the runtime layer is responsible for substituting the volume's
    /// localized name before render.
    pub fn effective_label(&self) -> String {
        if let Some(name) = &self.display_name {
            return name.clone();
        }
        match &self.target {
            FavoriteTarget::Path(p) => p
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| p.to_string_lossy().into_owned()),
            FavoriteTarget::SavedSearch(q) => q.clone(),
            FavoriteTarget::Tag(t) => t.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FavoriteSort {
    NameAsc,
    DateAddedNewest,
    DateAddedOldest,
    Kind,
}

/// Pick a `sort_index` strictly between `a` and `b`. `f64::NEG_INFINITY`
/// stands for "no left neighbor"; `f64::INFINITY` for "no right neighbor".
/// On a fully empty list both bounds are infinite and the result is 0.0.
pub fn fractional_between(a: f64, b: f64) -> f64 {
    match (a.is_finite(), b.is_finite()) {
        (false, false) => 0.0,
        (false, true) => b - 1024.0,
        (true, false) => a + 1024.0,
        (true, true) => {
            let mid = (a + b) / 2.0;
            if mid <= a || mid >= b {
                // Precision exhausted in this slot — caller should
                // schedule a renormalize pass. Returning a value
                // equal to `a` keeps the entry from being lost.
                a
            } else {
                mid
            }
        }
    }
}

/// Rewrite `(id, sort_index)` pairs in place with clean 0/1024/2048…
/// values, preserving sort order. Used by the background renormalize
/// pass and by one-shot section sorts.
pub fn renormalize_sort_indices(values: &mut [(FavoriteId, f64)]) {
    values.sort_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal));
    for (i, (_, idx)) in values.iter_mut().enumerate() {
        *idx = (i as f64) * 1024.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_open_list_anchors_at_zero() {
        assert_eq!(fractional_between(f64::NEG_INFINITY, f64::INFINITY), 0.0);
    }

    #[test]
    fn fractional_at_start_steps_left() {
        assert!(fractional_between(f64::NEG_INFINITY, 0.0) < 0.0);
    }

    #[test]
    fn fractional_at_end_steps_right() {
        assert!(fractional_between(0.0, f64::INFINITY) > 0.0);
    }

    #[test]
    fn fractional_between_two_picks_midpoint() {
        assert_eq!(fractional_between(0.0, 1024.0), 512.0);
    }

    #[test]
    fn fractional_subdivides() {
        let (mut a, b) = (0.0_f64, 1.0_f64);
        for _ in 0..40 {
            let mid = fractional_between(a, b);
            assert!(mid > a && mid < b, "mid {mid} not in ({a}, {b})");
            a = mid;
        }
    }

    #[test]
    fn renormalize_sorts_and_steps_evenly() {
        let a = FavoriteId::new();
        let b = FavoriteId::new();
        let c = FavoriteId::new();
        // Arbitrary mid value — NOT 3.14, which trips the deny-level
        // clippy::approx_constant and fails `cargo clippy --all-targets`.
        let mut v = vec![(a, 2.5), (b, -1.0), (c, 999.0)];
        renormalize_sort_indices(&mut v);
        assert_eq!(v[0], (b, 0.0));
        assert_eq!(v[1], (a, 1024.0));
        assert_eq!(v[2], (c, 2048.0));
    }

    #[test]
    fn kind_db_round_trip() {
        for k in [
            FavoriteKind::Folder,
            FavoriteKind::Volume,
            FavoriteKind::Application,
            FavoriteKind::SavedSearch,
            FavoriteKind::Tag,
        ] {
            assert_eq!(FavoriteKind::from_db_code(k.as_db_code()), Some(k));
        }
        assert!(FavoriteKind::from_db_code(99).is_none());
    }

    #[test]
    fn icon_db_round_trip() {
        let lucide = FavoriteIcon::Lucide("star".into());
        let tint = FavoriteIcon::TintedFolder("red".into());
        assert_eq!(FavoriteIcon::from_db(&lucide.to_db()), Some(lucide));
        assert_eq!(FavoriteIcon::from_db(&tint.to_db()), Some(tint));
        assert!(FavoriteIcon::from_db("nonsense").is_none());
    }

    #[test]
    fn target_db_round_trip() {
        let p = FavoriteTarget::Path(PathBuf::from("/Users/me/Code"));
        let (k, v) = p.to_db();
        assert_eq!(FavoriteTarget::from_db(k, &v), Some(p));
        let s = FavoriteTarget::SavedSearch("kind:pdf".into());
        let (k, v) = s.to_db();
        assert_eq!(FavoriteTarget::from_db(k, &v), Some(s));
    }

    #[test]
    fn id_string_round_trip() {
        let id = FavoriteId::new();
        let s = id.to_string();
        assert_eq!(s.parse::<FavoriteId>().ok(), Some(id));
        assert!("not-a-uuid".parse::<FavoriteId>().is_err());
    }

    #[test]
    fn effective_label_prefers_custom_name() {
        let f = Favorite {
            id: FavoriteId::new(),
            kind: FavoriteKind::Folder,
            target: FavoriteTarget::Path(PathBuf::from("/a/b/Projects")),
            display_name: Some("Work".into()),
            custom_icon: None,
            sort_index: 0.0,
            date_added: 0,
        };
        assert_eq!(f.effective_label(), "Work");
    }

    #[test]
    fn effective_label_falls_back_to_basename() {
        let f = Favorite::folder(PathBuf::from("/a/b/Projects"), 0.0, 0);
        assert_eq!(f.effective_label(), "Projects");
    }
}
