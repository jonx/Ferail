//! Stand-in thumbnails for Private Mode.
//!
//! Private Mode exists so a real session can be *published* as a screenshot,
//! which is why [PRIVATE_MODE.md](../../../docs/features/PRIVATE_MODE.md) §4.2
//! forbids painting any content provider's result and §5.3 says real
//! thumbnails are never an escape hatch. A grid of identical grey boxes
//! honours that and makes a poor screenshot: the feature being demonstrated
//! disappears along with the data.
//!
//! So this paints a blur that never came from the file. The pixels are
//! synthesised from the session key and the row's identity
//! ([`PrivateSession::thumb_pixels`]), then pushed through the same ThumbHash
//! round trip a real placeholder would take, so a private capture looks like
//! the real feature rather than like a different one.
//!
//! What that buys, and what it costs, stated plainly:
//!
//! - it reveals **nothing**: no byte of the file is read, and the mapping is
//!   keyed per session, so the same file in two captures is two different
//!   blurs and nothing can be correlated between them;
//! - it is stable within a session, so a row does not flicker as it repaints;
//! - it is **not** a preview. Anyone reading a Private Mode screenshot is
//!   looking at invented colours.
//!
//! Scope: thumbnail surfaces only. Surfaces that deliberately paint nothing at
//! all in Private Mode (the text editor's document stage, for one) stay that
//! way; a stand-in belongs where a picture belongs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui::RenderImage;

/// Side of the synthesised source image. ThumbHash encodes a handful of
/// low-frequency coefficients, so anything larger is thrown away by the
/// encoder: six is enough to produce distinct-looking blurs.
const SOURCE_SIZE: usize = 6;

/// Bound on the per-session cache. Private Mode is used to capture a screen,
/// not to browse, so this only has to cover the rows on screen plus scrolling
/// slack; beyond that, recomputing costs a few microseconds.
const CACHE_CAP: usize = 512;

thread_local! {
    /// Keyed by row identity. Thread-local rather than a global: only the UI
    /// thread paints, so this needs no lock on the render path.
    static CACHE: RefCell<HashMap<u64, Arc<RenderImage>>> = RefCell::new(HashMap::new());
}

/// The stand-in blur for one row, built on first use and reused afterwards.
///
/// `identity` only has to be stable for a row within a session: the node id
/// serves, and it is already what the rest of Private Mode keys its synthetic
/// values on.
pub fn stand_in(identity: u64) -> Option<Arc<RenderImage>> {
    CACHE.with(|cache| {
        if let Some(image) = cache.borrow().get(&identity) {
            return Some(Arc::clone(image));
        }
        let image = Arc::new(build(identity)?);
        let mut cache = cache.borrow_mut();
        // Plain clear rather than an LRU: this is a bounded scratch cache for
        // a mode the user leaves in seconds, and an eviction policy would be
        // more machinery than the thing it manages.
        if cache.len() >= CACHE_CAP {
            cache.clear();
        }
        cache.insert(identity, Arc::clone(&image));
        Some(image)
    })
}

/// Drop everything. Called when Private Mode ends so the synthesised pixels do
/// not outlive the session that invented them.
pub fn clear() {
    CACHE.with(|cache| cache.borrow_mut().clear());
}

fn build(identity: u64) -> Option<RenderImage> {
    let session = crate::private_mode::session();
    let rgba = session.thumb_pixels(identity, SOURCE_SIZE);
    // The round trip through ThumbHash is deliberate, not decoration: a real
    // placeholder will be decoded by exactly this function, so encoding the
    // synthetic pixels the same way guarantees the two look like the same
    // feature. Both directions are pure arithmetic on ~25 bytes.
    let hash = thumbhash::rgba_to_thumb_hash(SOURCE_SIZE, SOURCE_SIZE, &rgba);
    decode(&hash)
}

/// Decode a ThumbHash into something gpui can paint.
///
/// Shared with the real placeholder path when that lands, so a hash from the
/// cache and a synthetic one cannot drift apart visually.
pub fn decode(hash: &[u8]) -> Option<RenderImage> {
    let (width, height, mut rgba) = thumbhash::thumb_hash_to_rgba(hash).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    // gpui's renderer wants BGRA despite the wrapping type being named
    // `RgbaImage`, the same swap `preview::build_render_image` does.
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width as u32, height as u32, rgba)?;
    Some(RenderImage::new(smallvec::SmallVec::from_elem(
        image::Frame::new(buffer),
        1,
    )))
}

#[cfg(test)]
mod tests {
    use super::{build, decode, SOURCE_SIZE};

    #[test]
    fn a_stand_in_decodes_to_a_paintable_image() {
        // Guards the one thing that can silently break: `rgba_to_thumb_hash`
        // asserts on inputs over 100x100, and its output has to survive the
        // decoder that paints it.
        assert!(SOURCE_SIZE <= 100);
        assert!(build(7).is_some());
        assert!(build(u64::MAX).is_some());
    }

    #[test]
    fn a_damaged_hash_is_refused_rather_than_panicking() {
        // Hashes will come from a database in the next step, so the decoder
        // has to treat them as untrusted input, not as something it wrote.
        assert!(decode(&[]).is_none());
        assert!(decode(&[0, 1, 2]).is_none());
    }
}
