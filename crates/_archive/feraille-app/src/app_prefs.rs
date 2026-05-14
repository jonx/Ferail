//! Persistent app preferences. Sits next to [`disk_usage_prefs`] and
//! follows the same intentionally-minimal `key=value` text format —
//! one line per field, unknown keys ignored so future additions
//! don't break older builds.
//!
//! File: `~/Library/Application Support/Feraille/app.txt` on macOS
//! (or `$XDG_CONFIG_HOME/feraille/app.txt` on Linux).

use std::path::PathBuf;

const FILENAME: &str = "app.txt";

#[derive(Clone, Copy, Debug, Default)]
pub struct AppPrefs {
    pub theme_preference: Option<ThemePref>,
    pub show_hidden: Option<bool>,
    pub sidebar_width: Option<f32>,
}

/// Mirror of [`crate::ThemePreference`] — kept separate so this
/// module stays free of cross-module imports and can move later
/// without churning the consumers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemePref {
    Light,
    Dark,
    System,
}

impl ThemePref {
    pub fn as_str(self) -> &'static str {
        match self {
            ThemePref::Light => "light",
            ThemePref::Dark => "dark",
            ThemePref::System => "system",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "light" => Some(ThemePref::Light),
            "dark" => Some(ThemePref::Dark),
            "system" => Some(ThemePref::System),
            _ => None,
        }
    }
}

#[cfg(target_os = "macos")]
fn config_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push("Library/Application Support/Feraille");
    Some(p)
}

#[cfg(not(target_os = "macos"))]
fn config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let mut p = PathBuf::from(xdg);
        p.push("feraille");
        return Some(p);
    }
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push(".config/feraille");
    Some(p)
}

pub fn load() -> AppPrefs {
    let Some(dir) = config_dir() else {
        return AppPrefs::default();
    };
    let path = dir.join(FILENAME);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return AppPrefs::default();
    };
    let mut out = AppPrefs::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "theme" => out.theme_preference = ThemePref::from_str(v.trim()),
            "show_hidden" => out.show_hidden = parse_bool(v.trim()),
            "sidebar_width" => out.sidebar_width = v.trim().parse().ok(),
            _ => {}
        }
    }
    out
}

pub fn save(prefs: AppPrefs) {
    let Some(dir) = config_dir() else { return };
    if !dir.exists() && std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let mut s = String::new();
    if let Some(t) = prefs.theme_preference {
        s.push_str(&format!("theme={}\n", t.as_str()));
    }
    if let Some(b) = prefs.show_hidden {
        s.push_str(&format!("show_hidden={b}\n"));
    }
    if let Some(w) = prefs.sidebar_width {
        s.push_str(&format!("sidebar_width={w:.1}\n"));
    }
    let _ = std::fs::write(dir.join(FILENAME), s);
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_pref_round_trip() {
        for t in [ThemePref::Light, ThemePref::Dark, ThemePref::System] {
            assert_eq!(ThemePref::from_str(t.as_str()), Some(t));
        }
    }

    #[test]
    fn unknown_theme_is_none() {
        assert!(ThemePref::from_str("teal").is_none());
    }

    #[test]
    fn parse_bool_recognises_common_forms() {
        for v in ["true", "1", "yes"] {
            assert_eq!(parse_bool(v), Some(true));
        }
        for v in ["false", "0", "no"] {
            assert_eq!(parse_bool(v), Some(false));
        }
        assert_eq!(parse_bool("maybe"), None);
    }
}
