//! Persisted UI state for the GPUI shell — last directory, show-
//! hidden, etc. A simple `key=value` text file.
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
    /// Whether the file list paints real Quick Look thumbnails instead
    /// of generic type icons. `None` == never set (defaults to `true`,
    /// thumbnails on).
    pub show_thumbnails: Option<bool>,
    /// Default file-pane view mode for newly opened tabs: "list" or
    /// "grid". `None` == never set (defaults to list). View mode is
    /// per-tab at runtime; this is just the seed for new tabs.
    pub view_mode: Option<String>,
    /// Grid icon display size in logical px (longest edge). `None` ==
    /// never set (defaults to [`crate::grid::DEFAULT_ICON_SIZE`]).
    pub icon_size: Option<u32>,
    /// "light", "dark", or "system". `None` = follow the system
    /// detection done at startup (Stage 9.a default).
    pub theme_pref: Option<String>,
    /// Selection / lead-highlight accent, as a `#RRGGBB(AA)` hex string
    /// (the format gpui-component's color picker speaks). `None` ==
    /// never set, in which case the file list and grid fall back to the
    /// theme's blue. See [`crate::selection_colors`].
    pub selection_color: Option<String>,
    /// User UI zoom factor (Cmd+= / Cmd+- / Cmd+0). Clamped at
    /// load to the same `[0.6, 2.0]` range Shell uses.
    pub ui_scale: Option<f32>,
    /// Sidebar width in DIPs (next-level Phase 5). Clamped at load
    /// to the resizable_panel's accepted range so a stale value
    /// can't force the splitter outside its min/max.
    pub sidebar_width: Option<f32>,
    /// Preview pane width in DIPs. Same clamp story.
    pub preview_width: Option<f32>,
    /// Whether the sidebar is collapsed to icons-only. None == the
    /// user has never expressed a preference (defaults to expanded).
    pub sidebar_collapsed: Option<bool>,
    /// Viewer slideshow auto-advance interval in seconds
    /// (docs/features/VIEWER.md). Clamped at load to [1, 60].
    pub viewer_slideshow_interval: Option<u64>,
    /// Recents sidebar section disclosure state. None == never set
    /// (defaults to expanded).
    pub recents_collapsed: Option<bool>,
    /// Ant Trail base tint, as a `#RRGGBB(AA)` hex string (same format
    /// as `selection_color`). `None` == never set, in which case the
    /// list and grid fall back to the original warm orange. The alpha
    /// is ignored — heat drives the tint's translucency. See
    /// [`crate::ant_trail`].
    pub ant_trail_color: Option<String>,
    /// When `true` (the default), reaching a folder by clicking its
    /// favorite is *not* recorded as a visit — it neither bumps the
    /// Ant Trail heat nor pushes the folder into Recents. `None` ==
    /// never set (defaults to `true`). See [`crate::ant_trail`].
    pub exclude_favorites_from_tracking: Option<bool>,

    // ---- Search (docs/features/SEARCH.md) ----
    /// Global-search engine preference: "auto" (Spotlight when
    /// available, else the built-in walker), "spotlight", or "walker".
    /// `None` == auto.
    pub search_engine: Option<String>,
    /// Match the relative path, not just the file name.
    pub search_match_path: Option<bool>,
    /// Include hidden / dot files in search results.
    pub search_include_hidden: Option<bool>,

    // ---- Duplicate finder (docs/features/DUPLICATES.md) ----
    /// How duplicate results are presented: "grouped" (grouped rows in
    /// a results tab) or "panel" (dedicated grouped panel). `None` ==
    /// grouped.
    pub dupe_presentation: Option<String>,
    /// Ignore files smaller than this many MB (0 = compare every
    /// non-empty file). Clamped at load to [0, 4096].
    pub dupe_min_size_mb: Option<u64>,
    /// Skip undownloaded iCloud placeholders (don't trigger a download
    /// to hash). Defaults true.
    pub dupe_skip_cloud: Option<bool>,
    /// Descend into macOS packages (`*.app`, `*.bundle`) and compare
    /// their inner files. Defaults false (packages opaque).
    pub dupe_include_packages: Option<bool>,
    /// Byte-for-byte verify each full-hash group (removes any
    /// hash-collision doubt at the cost of re-reading). Defaults false.
    pub dupe_paranoid: Option<bool>,

    // ---- Plugins (docs/features/VIEWER.md) ----
    /// Video player provider: "builtin" (AVFoundation) or "vlc". `None` ==
    /// builtin. VLC only takes effect in a build with the `vlc` feature.
    pub video_backend: Option<String>,
    /// Path to the VLC.app bundle the VLC provider loads libvlc from.
    /// `None` == the default `/Applications/VLC.app`.
    pub vlc_app_path: Option<String>,
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
                    // Persisted state is an external boundary for the
                    // path-identity contract: re-canonicalize so a
                    // symlinked spelling saved last session can't
                    // mint a second NodeId. `load()` runs at init
                    // (it already stats via `is_dir` above).
                    out.last_dir = Some(crate::shell::canonicalize_for_identity(path));
                }
            }
            "show_hidden" => {
                out.show_hidden = parse_bool(val);
            }
            "show_thumbnails" => {
                out.show_thumbnails = parse_bool(val);
            }
            "view_mode" => {
                out.view_mode = Some(val.trim().to_string());
            }
            "icon_size" => {
                out.icon_size = val.trim().parse::<u32>().ok();
            }
            "theme_pref" => {
                let v = val.trim().to_lowercase();
                if matches!(v.as_str(), "light" | "dark" | "system") {
                    out.theme_pref = Some(v);
                }
            }
            "selection_color" => {
                // Accept only a `#RRGGBB` / `#RRGGBBAA` hex so a garbage
                // value can't poison the live accent global at startup.
                let v = val.trim();
                let body = v.strip_prefix('#').unwrap_or("");
                if matches!(body.len(), 6 | 8) && body.bytes().all(|b| b.is_ascii_hexdigit()) {
                    out.selection_color = Some(format!("#{}", body.to_lowercase()));
                }
            }
            "ui_scale" => {
                out.ui_scale = val.trim().parse::<f32>().ok().map(|n| n.clamp(0.6, 2.0));
            }
            "sidebar_width" => {
                out.sidebar_width = val
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .map(|n| n.clamp(160.0, 400.0));
            }
            "preview_width" => {
                out.preview_width = val
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .map(|n| n.clamp(220.0, 520.0));
            }
            "sidebar_collapsed" => {
                out.sidebar_collapsed = parse_bool(val);
            }
            "viewer_slideshow_interval" => {
                out.viewer_slideshow_interval =
                    val.trim().parse::<u64>().ok().map(|n| n.clamp(1, 60));
            }
            "recents_collapsed" => {
                out.recents_collapsed = parse_bool(val);
            }
            "ant_trail_color" => {
                // Same hex guard as `selection_color` so a garbage value
                // can't poison the live tint global at startup.
                let v = val.trim();
                let body = v.strip_prefix('#').unwrap_or("");
                if matches!(body.len(), 6 | 8) && body.bytes().all(|b| b.is_ascii_hexdigit()) {
                    out.ant_trail_color = Some(format!("#{}", body.to_lowercase()));
                }
            }
            "exclude_favorites_from_tracking" => {
                out.exclude_favorites_from_tracking = parse_bool(val);
            }
            "search_engine" => {
                let v = val.trim().to_lowercase();
                if matches!(v.as_str(), "auto" | "spotlight" | "walker") {
                    out.search_engine = Some(v);
                }
            }
            "search_match_path" => {
                out.search_match_path = parse_bool(val);
            }
            "search_include_hidden" => {
                out.search_include_hidden = parse_bool(val);
            }
            "dupe_presentation" => {
                let v = val.trim().to_lowercase();
                if matches!(v.as_str(), "grouped" | "panel") {
                    out.dupe_presentation = Some(v);
                }
            }
            "dupe_min_size_mb" => {
                out.dupe_min_size_mb = val.trim().parse::<u64>().ok().map(|n| n.min(4096));
            }
            "dupe_skip_cloud" => {
                out.dupe_skip_cloud = parse_bool(val);
            }
            "dupe_include_packages" => {
                out.dupe_include_packages = parse_bool(val);
            }
            "dupe_paranoid" => {
                out.dupe_paranoid = parse_bool(val);
            }
            "video_backend" => {
                let v = val.trim().to_lowercase();
                if matches!(v.as_str(), "builtin" | "vlc") {
                    out.video_backend = Some(v);
                }
            }
            "vlc_app_path" => {
                if !val.trim().is_empty() {
                    out.vlc_app_path = Some(val.trim().to_string());
                }
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
    if let Some(b) = state.show_thumbnails {
        s.push_str(&format!("show_thumbnails={b}\n"));
    }
    if let Some(v) = &state.view_mode {
        s.push_str(&format!("view_mode={v}\n"));
    }
    if let Some(n) = state.icon_size {
        s.push_str(&format!("icon_size={n}\n"));
    }
    if let Some(p) = &state.theme_pref {
        s.push_str(&format!("theme_pref={p}\n"));
    }
    if let Some(c) = &state.selection_color {
        s.push_str(&format!("selection_color={c}\n"));
    }
    if let Some(z) = state.ui_scale {
        s.push_str(&format!("ui_scale={z}\n"));
    }
    if let Some(w) = state.sidebar_width {
        s.push_str(&format!("sidebar_width={w}\n"));
    }
    if let Some(w) = state.preview_width {
        s.push_str(&format!("preview_width={w}\n"));
    }
    if let Some(b) = state.sidebar_collapsed {
        s.push_str(&format!("sidebar_collapsed={b}\n"));
    }
    if let Some(n) = state.viewer_slideshow_interval {
        s.push_str(&format!("viewer_slideshow_interval={n}\n"));
    }
    if let Some(b) = state.recents_collapsed {
        s.push_str(&format!("recents_collapsed={b}\n"));
    }
    if let Some(c) = &state.ant_trail_color {
        s.push_str(&format!("ant_trail_color={c}\n"));
    }
    if let Some(b) = state.exclude_favorites_from_tracking {
        s.push_str(&format!("exclude_favorites_from_tracking={b}\n"));
    }
    if let Some(e) = &state.search_engine {
        s.push_str(&format!("search_engine={e}\n"));
    }
    if let Some(b) = state.search_match_path {
        s.push_str(&format!("search_match_path={b}\n"));
    }
    if let Some(b) = state.search_include_hidden {
        s.push_str(&format!("search_include_hidden={b}\n"));
    }
    if let Some(p) = &state.dupe_presentation {
        s.push_str(&format!("dupe_presentation={p}\n"));
    }
    if let Some(n) = state.dupe_min_size_mb {
        s.push_str(&format!("dupe_min_size_mb={n}\n"));
    }
    if let Some(b) = state.dupe_skip_cloud {
        s.push_str(&format!("dupe_skip_cloud={b}\n"));
    }
    if let Some(b) = state.dupe_include_packages {
        s.push_str(&format!("dupe_include_packages={b}\n"));
    }
    if let Some(b) = state.dupe_paranoid {
        s.push_str(&format!("dupe_paranoid={b}\n"));
    }
    if let Some(v) = &state.video_backend {
        s.push_str(&format!("video_backend={v}\n"));
    }
    if let Some(p) = &state.vlc_app_path {
        s.push_str(&format!("vlc_app_path={p}\n"));
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
