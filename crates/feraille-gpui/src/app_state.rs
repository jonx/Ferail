//! Persisted UI state for the GPUI shell — last directory, show-
//! hidden, etc. Mirrors the `key=value` text-file pattern the old
//! `feraille-app::app_prefs` uses, so the two apps can coexist on
//! disk during the migration without colliding.
//!
//! File: `~/Library/Application Support/Feraille/gpui-state.txt`
//! on macOS, `$XDG_CONFIG_HOME/feraille/gpui-state.txt` elsewhere.
//! Unknown keys are ignored so future additions don't break older
//! builds.

use std::path::PathBuf;

const FILENAME: &str = "gpui-state.txt";

#[derive(Clone, Debug, Default)]
pub struct AppState {
    pub last_dir: Option<PathBuf>,
    pub show_hidden: Option<bool>,
    /// "light", "dark", or "system". `None` = follow the system
    /// detection done at startup (Stage 9.a default).
    pub theme_pref: Option<String>,
    /// User UI zoom factor (Cmd+= / Cmd+- / Cmd+0). Clamped at
    /// load to the same `[0.6, 2.0]` range Shell uses.
    pub ui_scale: Option<f32>,
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

pub fn load() -> AppState {
    let Some(dir) = config_dir() else {
        return AppState::default();
    };
    let Ok(text) = std::fs::read_to_string(dir.join(FILENAME)) else {
        return AppState::default();
    };
    let mut out = AppState::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        match key {
            "last_dir" => {
                let path = PathBuf::from(val);
                if path.is_dir() {
                    out.last_dir = Some(path);
                }
            }
            "show_hidden" => {
                out.show_hidden = parse_bool(val);
            }
            "theme_pref" => {
                let v = val.trim().to_lowercase();
                if matches!(v.as_str(), "light" | "dark" | "system") {
                    out.theme_pref = Some(v);
                }
            }
            "ui_scale" => {
                out.ui_scale = val
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .map(|n| n.clamp(0.6, 2.0));
            }
            _ => {}
        }
    }
    out
}

pub fn save(state: &AppState) {
    let Some(dir) = config_dir() else { return };
    if !dir.exists() && std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let mut s = String::new();
    if let Some(p) = &state.last_dir {
        s.push_str(&format!("last_dir={}\n", p.display()));
    }
    if let Some(b) = state.show_hidden {
        s.push_str(&format!("show_hidden={b}\n"));
    }
    if let Some(p) = &state.theme_pref {
        s.push_str(&format!("theme_pref={p}\n"));
    }
    if let Some(z) = state.ui_scale {
        s.push_str(&format!("ui_scale={z}\n"));
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
