//! Composite `AssetSource` — our local icon bundle stacks in front of
//! the upstream `gpui_component_assets::Assets`. Lets `gpui_component::Icon`
//! / `gpui::svg().path("icons/file/X.svg")` resolve transparently
//! whether the SVG ships with us or with the upstream library bundle.
//!
//! Our SVGs live under `crates/feraille-gpui/resources/icons/` and
//! the upstream bundle ships under `icons/X.svg`. We mount our bundle
//! at the same `icons/` prefix so both fit a single path namespace.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

#[derive(rust_embed::RustEmbed)]
#[folder = "resources"]
#[include = "icons/**/*.svg"]
struct LocalAssets;

/// Combined `AssetSource` for the Feraille GPUI shell. Lookups try
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
