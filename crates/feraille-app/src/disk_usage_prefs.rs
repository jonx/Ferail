//! Persistent prefs for the Disk Usage window — geometry only, kept
//! intentionally minimal so we don't yet need serde or a config crate.
//!
//! File: `~/Library/Application Support/Feraille/du_window.txt`
//! (or `$XDG_CONFIG_HOME/feraille/du_window.txt` on Linux). Format is
//! one `key=value` pair per line; unknown keys are ignored so future
//! fields don't break older builds.

use std::path::PathBuf;

const FILENAME: &str = "du_window.txt";

#[derive(Clone, Copy, Debug, Default)]
pub struct DuWindowGeometry {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub topn_width: Option<f32>,
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

pub fn load() -> DuWindowGeometry {
    let Some(dir) = config_dir() else {
        return DuWindowGeometry::default();
    };
    let path = dir.join(FILENAME);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return DuWindowGeometry::default();
    };
    let mut out = DuWindowGeometry::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "width" => out.width = v.trim().parse().ok(),
            "height" => out.height = v.trim().parse().ok(),
            "topn_width" => out.topn_width = v.trim().parse().ok(),
            _ => {}
        }
    }
    out
}

pub fn save(geom: DuWindowGeometry) {
    let Some(dir) = config_dir() else { return };
    if !dir.exists() {
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
    }
    let mut s = String::new();
    if let Some(w) = geom.width {
        s.push_str(&format!("width={w}\n"));
    }
    if let Some(h) = geom.height {
        s.push_str(&format!("height={h}\n"));
    }
    if let Some(tw) = geom.topn_width {
        s.push_str(&format!("topn_width={tw:.1}\n"));
    }
    let _ = std::fs::write(dir.join(FILENAME), s);
}
