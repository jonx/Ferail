//! Composite `AssetSource` — our local icon bundle stacks in front of
//! the upstream `gpui_component_assets::Assets`. Lets `gpui_component::Icon`
//! / `gpui::svg().path("icons/file/X.svg")` resolve transparently
//! whether the SVG ships with us or with the upstream library bundle.
//!
//! Our SVGs live under `crates/ferail-gpui/resources/icons/` and
//! the upstream bundle ships under `icons/X.svg`. We mount our bundle
//! at the same `icons/` prefix so both fit a single path namespace.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

#[derive(rust_embed::RustEmbed)]
#[folder = "resources"]
#[include = "icons/**/*.svg"]
struct LocalAssets;

/// Combined `AssetSource` for the Ferail GPUI shell. Lookups try
/// our local `icons/file/*.svg` bundle first, then fall back to the
/// upstream `gpui_component_assets::Assets`. Listings are merged.
#[derive(Clone, Copy, Default)]
pub struct FeraAssets;

impl AssetSource for FeraAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        if let Some(file) = LocalAssets::get(path) {
            return Ok(Some(file.data));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut merged: Vec<SharedString> = LocalAssets::iter()
            .filter_map(|p| p.starts_with(path).then(|| p.into()))
            .collect();
        if let Ok(upstream) = gpui_component_assets::Assets.list(path) {
            for p in upstream {
                if !merged.contains(&p) {
                    merged.push(p);
                }
            }
        }
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rasterize `svg_bytes` the way gpui's `svg()` element does — parse with
    /// usvg, render to an alpha mask with resvg — and return the count of
    /// pixels with non-zero coverage. Zero means "parses but draws nothing",
    /// the failure mode behind an icon that silently doesn't appear.
    fn coverage(svg_bytes: &[u8], label: &str) -> usize {
        use resvg::{tiny_skia, usvg};
        let tree = usvg::Tree::from_data(svg_bytes, &usvg::Options::default())
            .unwrap_or_else(|e| panic!("{label}: not parseable as SVG: {e}"));
        let size = 48u32;
        let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("alloc pixmap");
        let ts = tree.size();
        let scale = size as f32 / ts.width().max(ts.height());
        let tx = (size as f32 - ts.width() * scale) / 2.0;
        let ty = (size as f32 - ts.height() * scale) / 2.0;
        let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty);
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        pixmap.data().chunks_exact(4).filter(|px| px[3] != 0).count()
    }

    /// Every SVG we ship must parse and rasterize to a non-empty mask. This is
    /// the dynamic guard — it auto-covers any icon added under `resources/`.
    #[test]
    fn local_bundle_icons_all_rasterize() {
        let mut n = 0;
        for path in LocalAssets::iter() {
            if !path.ends_with(".svg") {
                continue;
            }
            let bytes = LocalAssets::get(&path)
                .unwrap_or_else(|| panic!("{path}: listed but not loadable"))
                .data;
            let cov = coverage(&bytes, &path);
            assert!(cov > 0, "{path}: rasterized to an empty (all-transparent) mask");
            n += 1;
        }
        assert!(n >= 40, "expected the local icon bundle, found only {n} SVGs");
    }

    /// Every icon path the app actually draws must resolve through the
    /// composite asset source (local bundle *or* upstream fallback) and
    /// rasterize non-empty. Catches a renamed/removed asset that would make a
    /// live command's glyph silently blank. Keep this list in step with the
    /// `svg().path("icons/…")` / `Icon` call sites when adding a command.
    #[test]
    fn referenced_icons_all_resolve_and_rasterize() {
        // Grouped by surface for readability; deduplicated across the app.
        const REFERENCED: &[&str] = &[
            // Sidebar nav / locations
            "icons/nav/home.svg", "icons/nav/apps.svg", "icons/nav/desktop.svg",
            "icons/nav/documents.svg", "icons/nav/downloads.svg", "icons/nav/trash.svg",
            "icons/nav/movies.svg", "icons/nav/music.svg", "icons/nav/pictures.svg",
            "icons/nav/cloud.svg", "icons/nav/cloud-fill.svg", "icons/nav/drive.svg",
            "icons/nav/eject.svg", "icons/nav/star.svg", "icons/nav/tag.svg",
            "icons/nav/search.svg", "icons/nav/plus.svg", "icons/nav/package.svg",
            "icons/nav/folder.svg", "icons/nav/refresh.svg", "icons/nav/show-desktop.svg",
            "icons/nav/chevron-left.svg", "icons/nav/chevron-right.svg",
            "icons/nav/chevrons-left.svg", "icons/nav/chevrons-right.svg",
            // File-type glyphs (fallback when no OS icon)
            "icons/folder.svg", "icons/file/generic.svg", "icons/file/symlink.svg",
            "icons/file/pdf.svg", "icons/file/html.svg", "icons/file/spreadsheet.svg",
            "icons/file/image.svg", "icons/file/video.svg", "icons/file/audio.svg",
            "icons/file/text.svg", "icons/file/code.svg", "icons/file/archive.svg",
            "icons/file/disk.svg", "icons/file/app.svg",
            // Toolbar / shell chrome (upstream bundle)
            "icons/close.svg", "icons/minimize.svg", "icons/maximize.svg",
            "icons/arrow-up.svg", "icons/panel-right-close.svg", "icons/panel-right-open.svg",
            "icons/sort-ascending.svg", "icons/sort-descending.svg", "icons/view-list.svg",
            "icons/view-grid.svg", "icons/ellipsis.svg", "icons/dock.svg", "icons/dock-left.svg",
            "icons/dock-right.svg", "icons/undock.svg", "icons/external-link.svg",
            "icons/folder-open.svg", "icons/copy.svg", "icons/network.svg",
            // Status / adornments
            "icons/circle-x.svg", "icons/circle-help.svg", "icons/circle-check.svg",
            "icons/triangle-alert.svg", "icons/inbox.svg",
            // Settings pages
            "icons/activity.svg", "icons/search.svg", "icons/palette.svg", "icons/cpu.svg",
            "icons/settings.svg", "icons/settings-2.svg", "icons/keyboard.svg", "icons/info.svg",
            // Viewer controls
            "icons/chevron-left.svg", "icons/chevron-right.svg", "icons/pause.svg",
            "icons/play.svg", "icons/minus.svg", "icons/plus.svg", "icons/redo.svg",
            "icons/trash.svg", "icons/volume-x.svg", "icons/volume-2.svg",
            "icons/wand-sparkles.svg",
        ];
        let assets = FeraAssets;
        for path in REFERENCED {
            let bytes = assets
                .load(path)
                .ok()
                .flatten()
                .unwrap_or_else(|| panic!("{path}: not found in local bundle or upstream assets"));
            let cov = coverage(&bytes, path);
            assert!(cov > 0, "{path}: rasterized to an empty (all-transparent) mask");
        }
    }
}
