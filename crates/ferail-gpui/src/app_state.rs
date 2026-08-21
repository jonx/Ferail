//! Persisted UI state for the GPUI shell — last directory, show-
//! hidden, etc. A simple `key=value` text file.
//!
//! File: `~/Library/Application Support/Ferail/gpui-state.txt`
//! on macOS, `$XDG_CONFIG_HOME/ferail/gpui-state.txt` elsewhere.
//! Unknown keys are ignored so future additions don't break older
//! builds.
//!
//! ## Caching contract (Prime Directive)
//!
//! [`load`] serves from an in-memory cache after the first disk read,
//! and [`save`] updates the cache synchronously then hands the disk
//! write to a coalescing writer thread. Callers may therefore use
//! `load()`/`save()` freely from click handlers and render-time value
//! getters — the previous implementation re-read the file (and
//! stat'ed `last_dir`, hanging on dead network mounts) on every call,
//! which turned sidebar clicks, splitter drags, new tabs, and the
//! settings window's getters into filesystem I/O on the UI thread.
//! The on-disk file is written atomically (temp + rename) so a crash
//! mid-write can't destroy all settings.

use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};

const FILENAME: &str = "gpui-state.txt";

/// Process-wide cache of the last loaded/saved state. `None` until
/// the first [`load`].
fn cache() -> &'static Mutex<Option<AppState>> {
    static CACHE: OnceLock<Mutex<Option<AppState>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Lazily-spawned writer thread. Bursts of saves (splitter drags)
/// coalesce: the thread drains the queue and writes only the newest
/// serialization.
fn writer() -> &'static Sender<String> {
    static WRITER: OnceLock<Sender<String>> = OnceLock::new();
    WRITER.get_or_init(|| {
        let (tx, rx) = channel::<String>();
        let spawned = std::thread::Builder::new()
            .name("app-state-writer".into())
            .spawn(move || {
                while let Ok(mut latest) = rx.recv() {
                    while let Ok(newer) = rx.try_recv() {
                        latest = newer;
                    }
                    write_atomic(&latest);
                }
            })
            .is_ok();
        if !spawned {
            // Writer thread failed to spawn (resource exhaustion):
            // fall back to synchronous writes by keeping a detached
            // receiver-less channel — sends fail, and save() writes
            // inline below via the send error path.
        }
        tx
    })
}

fn write_atomic(contents: &str) {
    let Some(dir) = config_dir() else { return };
    // Component-wise mkdir: plain create_dir_all can't create
    // `SYS:/.config/ferail` on AROS (emul-handler missing-parent IoErr
    // bug, UPSTREAM-NOTES item 40) — which silently disabled ALL settings
    // persistence there (column widths, theme, toggles reset every run).
    if !dir.exists() && ferail_meta::create_dir_all_compat(&dir).is_err() {
        return;
    }
    // Temp + rename: a crash mid-write leaves either the old file or
    // the new one, never a truncated half.
    let tmp = dir.join(format!("{FILENAME}.tmp"));
    if std::fs::write(&tmp, contents).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(FILENAME));
    }
}

/// Full path to the settings file (config dir + filename), or `None` when the
/// platform's config directory can't be resolved. Exposed for the diagnostics
/// health check.
pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join(FILENAME))
}

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
    /// Grid selection-gutter size in logical px. `None` == never set
    /// (defaults to [`crate::grid::DEFAULT_CELL_GAP`]).
    pub cell_gap: Option<f32>,
    /// Whether the background folder-size walker runs (Performance).
    /// `None` == never set (defaults to `true`, sizing on).
    pub folder_sizing: Option<bool>,
    /// Whether per-row magic sniffing + Finder-tag reads run
    /// (Performance). `None` == never set (defaults to `true`, on).
    pub file_detail_scan: Option<bool>,
    /// File-table column order + widths + visibility, as
    /// `key:width:vis` tuples in display order (e.g.
    /// `name:360:1,size:100:1,format:220:0,...`). `vis` is `1` visible /
    /// `0` hidden; a legacy `key:width` token is read as visible. One key
    /// covers drag-reorder, drag-resize, and header show/hide. `None` ==
    /// defaults. Unknown/missing column keys are reconciled at load by
    /// [`crate::file_list::split_persisted_columns`], so schema drift
    /// can't wedge the table.
    pub list_columns: Option<String>,
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
    /// Height of the preview pane's thumbnail box in DIPs, set by
    /// dragging the resize grip under the image. Same clamp story.
    pub preview_thumb_height: Option<f32>,
    /// Whether the sidebar is collapsed to icons-only. None == the
    /// user has never expressed a preference (defaults to expanded).
    pub sidebar_collapsed: Option<bool>,
    /// Viewer slideshow auto-advance interval in seconds
    /// (docs/features/VIEWER.md). Clamped at load to [1, 60].
    pub viewer_slideshow_interval: Option<u64>,
    /// Whether the Recents feature is on at all. `None` == never set
    /// (defaults to `true`). Off hides the sidebar section and stops
    /// pushing folders into the recents cache; the Ant Trail keeps its
    /// own visit log either way (they share `folder_usage`). See
    /// [`crate::recents_section`].
    pub recents_enabled: Option<bool>,
    /// Recents sidebar section disclosure state. None == never set
    /// (defaults to expanded).
    pub recents_collapsed: Option<bool>,
    /// Whether the Ant Trail heat tint is painted at all. `None` ==
    /// never set (defaults to `true`). Off hides the tint but still
    /// records visits, so Recents keeps working. See [`crate::ant_trail`].
    pub ant_trail_enabled: Option<bool>,
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

    // ---- Viewer (docs/features/VIEWER.md) ----
    /// Zoom a viewer window opens with (and returns to on zoom reset):
    /// "fit" (fill the window, upscaling small media), "fit-down" (fit
    /// without enlarging past 100 %), or "actual" (1:1). `None` == fit.
    pub viewer_default_zoom: Option<String>,

    // ---- Plugins (docs/features/VIEWER.md) ----
    /// Video player provider: "builtin" (AVFoundation) or "mpv". `None` ==
    /// builtin. mpv only takes effect in a build with the `mpv` feature.
    pub video_backend: Option<String>,
    /// Path the mpv provider loads libmpv from (the dylib, a directory, or
    /// `mpv.app`). `None` == the platform default (Homebrew on macOS).
    pub mpv_path: Option<String>,

    // ---- Terminal (docs/features/CONTEXT_MENU.md, "Open Terminal Here") ----
    /// Terminal program the "Open Terminal Here" command launches: an
    /// absolute path, a `.app` bundle (macOS), or a bare `PATH` command.
    /// `None` == the platform default (Terminal.app / `wt.exe` / the
    /// Linux detection chain).
    pub terminal_path: Option<String>,
    /// Extra launch arguments, one params string (split shell-style at
    /// use; `{dir}` expands to the target folder). `None` == the default
    /// arguments for the chosen terminal.
    pub terminal_args: Option<String>,
    /// "standard" or "admin" (elevated: UAC on Windows, a sudo root
    /// shell on macOS/Linux). `None` == standard.
    pub terminal_mode: Option<String>,

    // ---- Diagnostics privacy (docs/features/DIAGNOSTICS.md) ----
    /// When `true` (the default), the diagnostics bundle, "Copy report", and the
    /// in-app activity trail replace every file/folder name with `…` so a shared
    /// report reveals nothing about the user's files. `None` == never set
    /// (defaults to `true`). See [`crate::redact`].
    pub redact_diagnostics: Option<bool>,

    // ---- Language (docs/features/LOCALIZATION.md) ----
    /// UI language: `"system"` (follow the OS), `"en"`, or a language-pack
    /// code (`"fr"`, `"pt-BR"`). `None` == never set == system.
    pub language: Option<String>,

    // ---- Updates ----
    /// Opt-in automatic update check: once a day, ask GitHub Releases
    /// whether a newer Ferail exists (docs/features/UPDATES.md). `None` ==
    /// never set, which means **off** — no background network traffic
    /// unless the user asked for it. The menu's manual Check for
    /// Updates… works regardless of this flag.
    pub update_check: Option<bool>,

    // ---- Sidebar Locations (Windows / OneDrive) ----
    /// Which root the sidebar's special folders resolve against when
    /// OneDrive has moved them: "auto" (shell default), "local"
    /// (`%USERPROFILE%`), or "onedrive". `None` == auto. Windows-only;
    /// ignored elsewhere. See [`ferail_fs_native::paths::SpecialFolderMode`].
    pub special_folder_mode: Option<String>,
}

#[cfg(target_os = "macos")]
pub fn config_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push("Library/Application Support/Ferail");
    Some(p)
}

#[cfg(target_os = "windows")]
pub fn config_dir() -> Option<PathBuf> {
    // Windows has no $HOME — per-user app config lives under %APPDATA%
    // (Roaming). Without this the whole settings store silently no-ops:
    // save() bails when config_dir() is None and load() returns defaults, so
    // every toggle/dropdown "snaps back" because nothing is ever persisted.
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let mut p = PathBuf::from(appdata);
        p.push("Ferail");
        return Some(p);
    }
    let profile = std::env::var_os("USERPROFILE")?;
    let mut p = PathBuf::from(profile);
    p.push("AppData");
    p.push("Roaming");
    p.push("Ferail");
    Some(p)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let mut p = PathBuf::from(xdg);
        p.push("ferail");
        return Some(p);
    }
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push(".config/ferail");
    Some(p)
}

/// Current state — from the in-memory cache after the first call
/// (see the module docs' caching contract). Cheap enough for click
/// handlers and render-time getters.
pub fn load() -> AppState {
    if let Some(cached) = cache().lock().ok().and_then(|guard| guard.clone()) {
        return cached;
    }
    let loaded = load_from_disk();
    if let Ok(mut guard) = cache().lock() {
        *guard = Some(loaded.clone());
    }
    loaded
}

fn load_from_disk() -> AppState {
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
                // Stored raw. Validation (is_dir) + canonicalization
                // happen at the single startup consumer (Shell::new) —
                // stat'ing here made EVERY load() a filesystem touch,
                // and a dead network mount in last_dir hung the UI on
                // each one.
                out.last_dir = Some(PathBuf::from(val));
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
            "cell_gap" => {
                out.cell_gap = val.trim().parse::<f32>().ok();
            }
            "folder_sizing" => {
                out.folder_sizing = parse_bool(val);
            }
            "file_detail_scan" => {
                out.file_detail_scan = parse_bool(val);
            }
            "list_columns" if !val.trim().is_empty() => {
                out.list_columns = Some(val.trim().to_string());
            }
            "language" => {
                let v = val.trim();
                if !v.is_empty() {
                    out.language = Some(v.to_string());
                }
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
            "preview_thumb_height" => {
                out.preview_thumb_height = val
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .map(|n| n.clamp(120.0, 600.0));
            }
            "sidebar_collapsed" => {
                out.sidebar_collapsed = parse_bool(val);
            }
            "viewer_slideshow_interval" => {
                out.viewer_slideshow_interval =
                    val.trim().parse::<u64>().ok().map(|n| n.clamp(1, 60));
            }
            "recents_enabled" => {
                out.recents_enabled = parse_bool(val);
            }
            "recents_collapsed" => {
                out.recents_collapsed = parse_bool(val);
            }
            "ant_trail_enabled" => {
                out.ant_trail_enabled = parse_bool(val);
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
            "viewer_default_zoom" => {
                let v = val.trim().to_lowercase();
                if matches!(v.as_str(), "fit" | "fit-down" | "actual") {
                    out.viewer_default_zoom = Some(v);
                }
            }
            "video_backend" => {
                let v = val.trim().to_lowercase();
                if matches!(v.as_str(), "builtin" | "mpv") {
                    out.video_backend = Some(v);
                }
            }
            "mpv_path" if !val.trim().is_empty() => {
                out.mpv_path = Some(val.trim().to_string());
            }
            "terminal_path" if !val.trim().is_empty() => {
                out.terminal_path = Some(val.trim().to_string());
            }
            "terminal_args" if !val.trim().is_empty() => {
                out.terminal_args = Some(val.trim().to_string());
            }
            "terminal_mode" => {
                let v = val.trim().to_lowercase();
                if matches!(v.as_str(), "standard" | "admin") {
                    out.terminal_mode = Some(v);
                }
            }
            "redact_diagnostics" => {
                out.redact_diagnostics = parse_bool(val);
            }
            "update_check" => {
                out.update_check = parse_bool(val);
            }
            "special_folder_mode" => {
                let v = val.trim().to_lowercase();
                if matches!(v.as_str(), "auto" | "local" | "onedrive") {
                    out.special_folder_mode = Some(v);
                }
            }
            _ => {}
        }
    }
    out
}

/// Update the cache immediately and queue an atomic disk write on
/// the coalescing writer thread (see the module docs).
pub fn save(state: &AppState) {
    if let Ok(mut guard) = cache().lock() {
        *guard = Some(state.clone());
    }
    let serialized = serialize(state);
    if writer().send(serialized.clone()).is_err() {
        // Writer thread unavailable — degrade to a synchronous
        // atomic write rather than dropping the save.
        write_atomic(&serialized);
    }
}

fn serialize(state: &AppState) -> String {
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
    if let Some(g) = state.cell_gap {
        s.push_str(&format!("cell_gap={g}\n"));
    }
    if let Some(b) = state.folder_sizing {
        s.push_str(&format!("folder_sizing={b}\n"));
    }
    if let Some(b) = state.file_detail_scan {
        s.push_str(&format!("file_detail_scan={b}\n"));
    }
    if let Some(c) = &state.list_columns {
        s.push_str(&format!("list_columns={c}\n"));
    }
    if let Some(p) = &state.theme_pref {
        s.push_str(&format!("theme_pref={p}\n"));
    }
    if let Some(l) = &state.language {
        s.push_str(&format!("language={l}\n"));
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
    if let Some(h) = state.preview_thumb_height {
        s.push_str(&format!("preview_thumb_height={h}\n"));
    }
    if let Some(b) = state.sidebar_collapsed {
        s.push_str(&format!("sidebar_collapsed={b}\n"));
    }
    if let Some(n) = state.viewer_slideshow_interval {
        s.push_str(&format!("viewer_slideshow_interval={n}\n"));
    }
    if let Some(b) = state.recents_enabled {
        s.push_str(&format!("recents_enabled={b}\n"));
    }
    if let Some(b) = state.recents_collapsed {
        s.push_str(&format!("recents_collapsed={b}\n"));
    }
    if let Some(b) = state.ant_trail_enabled {
        s.push_str(&format!("ant_trail_enabled={b}\n"));
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
    if let Some(z) = &state.viewer_default_zoom {
        s.push_str(&format!("viewer_default_zoom={z}\n"));
    }
    if let Some(v) = &state.video_backend {
        s.push_str(&format!("video_backend={v}\n"));
    }
    if let Some(p) = &state.mpv_path {
        s.push_str(&format!("mpv_path={p}\n"));
    }
    if let Some(p) = &state.terminal_path {
        s.push_str(&format!("terminal_path={p}\n"));
    }
    if let Some(a) = &state.terminal_args {
        s.push_str(&format!("terminal_args={a}\n"));
    }
    if let Some(m) = &state.terminal_mode {
        s.push_str(&format!("terminal_mode={m}\n"));
    }
    if let Some(b) = state.redact_diagnostics {
        s.push_str(&format!("redact_diagnostics={b}\n"));
    }
    if let Some(b) = state.update_check {
        s.push_str(&format!("update_check={b}\n"));
    }
    if let Some(m) = &state.special_folder_mode {
        s.push_str(&format!("special_folder_mode={m}\n"));
    }
    s
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}
