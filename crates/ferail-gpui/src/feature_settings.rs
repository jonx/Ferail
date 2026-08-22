//! Typed accessors for the search, duplicate-finder, and terminal
//! preferences stored in [`crate::app_state`]. One place resolves the
//! persisted string/bool fields into the concrete config the workers
//! want, so the settings UI and the shell never disagree about a default.

use ferail_core::terminal::{TerminalMode, TerminalSpec, split_args};
use ferail_fs_native::{DupeMode, DupeOpts, SearchQuery};

use crate::app_state::{self, AppState};

/// Which engine answers a global search.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchEnginePref {
    /// Spotlight when available, else the built-in recursive walker.
    Auto,
    /// Always Spotlight (falls back to the walker only if unavailable).
    Spotlight,
    /// Always the built-in recursive walker.
    Walker,
}

impl SearchEnginePref {
    pub fn as_str(self) -> &'static str {
        match self {
            SearchEnginePref::Auto => "auto",
            SearchEnginePref::Spotlight => "spotlight",
            SearchEnginePref::Walker => "walker",
        }
    }
    fn from_str(s: &str) -> Self {
        match s {
            "spotlight" => SearchEnginePref::Spotlight,
            "walker" => SearchEnginePref::Walker,
            _ => SearchEnginePref::Auto,
        }
    }
}

/// How duplicate results are shown.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DupePresentation {
    /// Grouped, adjacent rows in a results tab (reuses the file list).
    Grouped,
    /// Dedicated grouped panel (future; falls back to grouped until built).
    Panel,
}

impl DupePresentation {
    pub fn as_str(self) -> &'static str {
        match self {
            DupePresentation::Grouped => "grouped",
            DupePresentation::Panel => "panel",
        }
    }
    fn from_str(s: &str) -> Self {
        match s {
            "grouped" => DupePresentation::Grouped,
            // Panel is the default — the dedicated card view with
            // group-level cleanup is the one we want people to land in.
            _ => DupePresentation::Panel,
        }
    }
}

/// Resolved search preferences.
#[derive(Clone, Debug)]
pub struct SearchConfig {
    pub engine: SearchEnginePref,
    pub match_path: bool,
    pub include_hidden: bool,
}

impl SearchConfig {
    pub fn from_state(s: &AppState) -> Self {
        Self {
            engine: s
                .search_engine
                .as_deref()
                .map(SearchEnginePref::from_str)
                .unwrap_or(SearchEnginePref::Auto),
            match_path: s.search_match_path.unwrap_or(false),
            // Falls back to the global show-hidden preference so search
            // matches the listing the user already sees.
            include_hidden: s.search_include_hidden.or(s.show_hidden).unwrap_or(false),
        }
    }

    pub fn load() -> Self {
        Self::from_state(&app_state::load())
    }

    /// Build a [`SearchQuery`] for `needle` from these preferences.
    pub fn query(&self, needle: impl Into<String>) -> SearchQuery {
        SearchQuery {
            needle: needle.into(),
            match_path: self.match_path,
            include_hidden: self.include_hidden,
            expr: None,
        }
    }
}

/// Resolved duplicate-finder preferences.
#[derive(Clone, Debug)]
pub struct DupeConfig {
    pub presentation: DupePresentation,
    pub min_size_mb: u64,
    pub skip_cloud: bool,
    pub include_packages: bool,
    pub paranoid: bool,
}

impl DupeConfig {
    pub fn from_state(s: &AppState) -> Self {
        Self {
            presentation: s
                .dupe_presentation
                .as_deref()
                .map(DupePresentation::from_str)
                .unwrap_or(DupePresentation::Panel),
            min_size_mb: s.dupe_min_size_mb.unwrap_or(0),
            skip_cloud: s.dupe_skip_cloud.unwrap_or(true),
            include_packages: s.dupe_include_packages.unwrap_or(false),
            paranoid: s.dupe_paranoid.unwrap_or(false),
        }
    }

    pub fn load() -> Self {
        Self::from_state(&app_state::load())
    }

    /// Build [`DupeOpts`] from these preferences. `min_size` is at least
    /// 1 byte — 0-byte files are never duplicates worth surfacing.
    pub fn opts(&self) -> DupeOpts {
        let min_size = self.min_size_mb.saturating_mul(1024 * 1024).max(1);
        DupeOpts {
            mode: DupeMode::Exact,
            paranoid: self.paranoid,
            scan_cloud: !self.skip_cloud,
            follow_packages: self.include_packages,
            min_size,
        }
    }
}

/// Resolved terminal-launch preferences ("Open Terminal Here").
#[derive(Clone, Debug)]
pub struct TerminalConfig {
    pub path: Option<String>,
    pub args: Option<String>,
    pub mode: TerminalMode,
}

impl TerminalConfig {
    pub fn from_state(s: &AppState) -> Self {
        Self {
            path: s
                .terminal_path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string),
            args: s
                .terminal_args
                .as_deref()
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map(str::to_string),
            mode: s
                .terminal_mode
                .as_deref()
                .map(TerminalMode::from_str)
                .unwrap_or_default(),
        }
    }

    pub fn load() -> Self {
        Self::from_state(&app_state::load())
    }

    /// The launch request `platform_shell::open_terminal_with` consumes.
    pub fn spec(&self) -> TerminalSpec {
        TerminalSpec {
            program: self.path.clone(),
            args: self.args.as_deref().map(split_args).unwrap_or_default(),
            mode: self.mode,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let s = AppState::default();
        let search = SearchConfig::from_state(&s);
        assert_eq!(search.engine, SearchEnginePref::Auto);
        assert!(!search.match_path);

        let dupe = DupeConfig::from_state(&s);
        assert_eq!(dupe.presentation, DupePresentation::Panel);
        assert!(dupe.skip_cloud);
        let opts = dupe.opts();
        assert!(!opts.scan_cloud, "skip_cloud=true → scan_cloud=false");
        assert_eq!(opts.min_size, 1, "0 MB floor is 1 byte");
    }

    #[test]
    fn search_include_hidden_falls_back_to_show_hidden() {
        let mut s = AppState {
            show_hidden: Some(true),
            ..AppState::default()
        };
        assert!(SearchConfig::from_state(&s).include_hidden);
        s.search_include_hidden = Some(false);
        assert!(
            !SearchConfig::from_state(&s).include_hidden,
            "explicit search pref overrides the global fallback"
        );
    }

    #[test]
    fn terminal_defaults_and_custom() {
        let cfg = TerminalConfig::from_state(&AppState::default());
        assert_eq!(cfg.path, None);
        assert_eq!(cfg.mode, TerminalMode::Standard);
        assert_eq!(cfg.spec(), TerminalSpec::default());

        let s = AppState {
            terminal_path: Some("  /usr/bin/kitty ".into()),
            terminal_args: Some("-d \"{dir}\"".into()),
            terminal_mode: Some("admin".into()),
            ..AppState::default()
        };
        let spec = TerminalConfig::from_state(&s).spec();
        assert_eq!(spec.program(), Some("/usr/bin/kitty"));
        assert_eq!(spec.args, vec!["-d", "{dir}"]);
        assert!(spec.admin());
    }

    #[test]
    fn min_size_mb_converts_to_bytes() {
        let s = AppState {
            dupe_min_size_mb: Some(5),
            ..AppState::default()
        };
        assert_eq!(DupeConfig::from_state(&s).opts().min_size, 5 * 1024 * 1024);
    }
}
