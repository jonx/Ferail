//! Pure, process-session presentation helpers for Private Mode.
//!
//! Raw filesystem values stay in their owning models.  This module only
//! derives deterministic, non-reversible display values from a per-process
//! random key; it performs no I/O and owns no global enabled/disabled state.

use std::path::{Component, Path};

use uuid::Uuid;

const ADJECTIVES: &[&str] = &[
    "amber", "brisk", "calm", "cobalt", "coral", "crisp", "gentle", "golden", "lunar", "misty",
    "quiet", "rapid", "silver", "solar", "velvet", "vivid",
];
const NOUNS: &[&str] = &[
    "atlas", "beacon", "cedar", "delta", "falcon", "harbor", "island", "maple", "meadow", "orbit",
    "river", "signal", "summit", "timber", "valley", "willow",
];
const COMPOUND_EXTENSIONS: &[&str] = &["tar.gz", "tar.bz2", "tar.xz", "tar.zst"];

/// Semantic value passed by a renderer to the one private-presentation
/// interface. Controls do not try to guess whether arbitrary prose contains a
/// path: the caller states what it is, and the presenter applies the matching
/// policy. This is the Rust/composition equivalent of a `private_mode`
/// interface inherited by every control.
#[derive(Clone, Copy, Debug)]
pub enum PrivateValue<'a> {
    Leaf { raw: &'a str, is_dir: bool },
    Path(&'a Path),
    Label(&'a str),
    Digest { raw: &'a str, width: usize },
}

/// A random disguise namespace whose mapping lasts only for this process.
#[derive(Clone, Debug)]
pub struct PrivateSession {
    key: [u8; 16],
}

impl Default for PrivateSession {
    fn default() -> Self {
        Self::new()
    }
}

impl PrivateSession {
    pub fn new() -> Self {
        Self {
            key: *Uuid::new_v4().as_bytes(),
        }
    }

#[cfg(test)]
    fn with_key(key: [u8; 16]) -> Self {
        Self { key }
    }

    pub fn present(&self, value: PrivateValue<'_>) -> String {
        match value {
            PrivateValue::Leaf { raw, is_dir } => self.leaf(raw, is_dir),
            PrivateValue::Path(path) => self.path(path),
            PrivateValue::Label(raw) => self.leaf(raw, true),
            PrivateValue::Digest { raw, width } => self.digest(raw, width),
        }
    }

    /// Present one file or directory leaf while preserving only a safe,
    /// recognized extension and a leading-dot hidden-file cue.
    pub fn leaf(&self, raw: &str, is_dir: bool) -> String {
        let hidden = raw.starts_with('.') && raw != "." && raw != "..";
        let visible = raw.strip_prefix('.').unwrap_or(raw);
        let extension = (!is_dir).then(|| safe_extension(visible)).flatten();
        let stem = extension
            .and_then(|ext| visible.get(..visible.len().saturating_sub(ext.len() + 1)))
            .unwrap_or(visible);
        let hash = self.hash(0x4c45_4146, raw.as_bytes());
        let mut alias = format!(
            "{}-{}",
            ADJECTIVES[(hash as usize) % ADJECTIVES.len()],
            NOUNS[((hash >> 8) as usize) % NOUNS.len()]
        );
        if stem.chars().count() > 24 {
            alias.push('-');
            alias.push_str(&format!("{:02}", (hash >> 16) % 100));
        }
        if hidden {
            alias.insert(0, '.');
        }
        if let Some(ext) = extension {
            alias.push('.');
            alias.push_str(ext);
        }
        alias
    }

    /// Present a path component-by-component.  Filesystem anchors keep their
    /// syntax, never their identifying labels.
    pub fn path(&self, raw: &Path) -> String {
        let mut out = String::new();
        let mut normal_ix = 0usize;
        for component in raw.components() {
            match component {
                Component::Prefix(_) => out.push_str("X:"),
                Component::RootDir => {
                    if !out.ends_with('/') {
                        out.push('/');
                    }
                }
                Component::CurDir => {
                    if !out.is_empty() && !out.ends_with('/') {
                        out.push('/');
                    }
                    out.push('.');
                }
                Component::ParentDir => {
                    if !out.is_empty() && !out.ends_with('/') {
                        out.push('/');
                    }
                    out.push_str("..");
                }
                Component::Normal(part) => {
                    if !out.is_empty() && !out.ends_with('/') {
                        out.push('/');
                    }
                    let text = part.to_string_lossy();
                    let hash = self.hash(normal_ix as u64, text.as_bytes());
                    out.push_str(ADJECTIVES[(hash as usize) % ADJECTIVES.len()]);
                    out.push('-');
                    out.push_str(NOUNS[((hash >> 8) as usize) % NOUNS.len()]);
                    normal_ix += 1;
                }
            }
        }
        if out.is_empty() {
            "private".into()
        } else {
            out
        }
    }

    /// A same-width hexadecimal placeholder for a checksum or identifier.
    pub fn digest(&self, raw: &str, width: usize) -> String {
        let mut result = String::with_capacity(width);
        let mut counter = 0u64;
        while result.len() < width {
            result.push_str(&format!("{:016x}", self.hash(counter, raw.as_bytes())));
            counter = counter.wrapping_add(1);
        }
        result.truncate(width);
        result
    }

    /// Stable, bounded fake byte count that preserves only an order of
    /// magnitude useful for representative layout.
    pub fn bytes(&self, identity: u64, raw: u64) -> u64 {
        if raw == 0 {
            return 0;
        }
        let magnitude = 10u64.saturating_pow(raw.ilog10());
        let factor = 10 + (self.hash(identity, &raw.to_le_bytes()) % 90);
        magnitude.saturating_mul(factor) / 10
    }

    /// Stable fake timestamp in the recent past. The real instant only feeds
    /// the keyed mapping; no calendar component is retained in the output.
    pub fn timestamp(&self, identity: u64, raw: i64, now: i64) -> i64 {
        if raw <= 0 {
            return raw;
        }
        let hash = self.hash(identity ^ 0x5449_4d45, &raw.to_le_bytes());
        let seconds = 3_600 + (hash % (540 * 24 * 3_600));
        now.saturating_sub(seconds as i64)
    }

    /// Stable, plausible image dimensions with the original orientation but
    /// no retained pixel count.
    pub fn dimensions(&self, identity: u64, raw: (u32, u32)) -> (u32, u32) {
        if raw.0 == 0 || raw.1 == 0 {
            return raw;
        }
        const LONG_EDGES: &[u32] = &[1024, 1280, 1600, 1920, 2048, 2560, 3024, 4032];
        let hash = self.hash(identity ^ 0x4449_4d53, &raw.0.to_le_bytes());
        let long = LONG_EDGES[(hash as usize) % LONG_EDGES.len()];
        let short = ((long as u64 * (55 + ((hash >> 8) % 31))) / 100) as u32;
        if raw.0 >= raw.1 {
            (long, short)
        } else {
            (short, long)
        }
    }

    /// Stable, plausible pixels for a private stand-in thumbnail: a tiny
    /// `size`x`size` RGBA image derived from the session key and `identity`,
    /// with no input from the file it stands for.
    ///
    /// The point of Private Mode is to publish a capture, so the grid has to
    /// look alive without any of it being real. This is the same trick as
    /// [`Self::bytes`], [`Self::timestamp`] and [`Self::dimensions`], one
    /// layer down: stable per identity so a row does not flicker between
    /// frames, plausible enough to read as a photo at thumbnail size, and
    /// carrying nothing back to the original.
    ///
    /// Colours come from a keyed hue with a soft vertical gradient, because
    /// blurred photographs are mostly that: two or three related tones, lighter
    /// at one end. Pure noise would read as television static, not as content.
    pub fn thumb_pixels(&self, identity: u64, size: usize) -> Vec<u8> {
        let seed = self.hash(identity ^ 0x5448_554d, &identity.to_le_bytes());
        // One base hue per file, plus a second one nearby: enough variation
        // between neighbouring rows to look like different pictures.
        let hue = (seed % 360) as f32;
        let drift = 18.0 + ((seed >> 16) % 60) as f32;
        let saturation = 0.25 + ((seed >> 8) % 40) as f32 / 100.0;
        let mut rgba = Vec::with_capacity(size * size * 4);
        for y in 0..size {
            for x in 0..size {
                let across = x as f32 / (size.max(2) - 1) as f32;
                let down = y as f32 / (size.max(2) - 1) as f32;
                // A diagonal light falloff, then a per-cell nudge so the
                // result is not a flat gradient.
                let cell = self.hash(seed ^ (y as u64) << 8 ^ x as u64, &[]);
                let jitter = ((cell % 100) as f32 / 100.0 - 0.5) * 0.18;
                let value = (0.72 - 0.34 * down - 0.08 * across + jitter).clamp(0.05, 0.98);
                let (red, green, blue) =
                    hsv_to_rgb(hue + drift * across, saturation, value);
                rgba.extend_from_slice(&[red, green, blue, 255]);
            }
        }
        rgba
    }

    fn hash(&self, domain: u64, bytes: &[u8]) -> u64 {
        // Keyed FNV-style diffusion is deliberately local and dependency-free.
        // Private Mode is a presentation boundary, not a memory-inspection
        // security boundary; the essential property here is a per-process,
        // non-persisted and stable mapping with no copied source substring.
        let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ domain;
        for byte in self.key.iter().chain(bytes) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
            hash ^= hash >> 32;
        }
        hash
    }
}

fn safe_extension(name: &str) -> Option<&str> {
    let lower = name.to_ascii_lowercase();
    for compound in COMPOUND_EXTENSIONS {
        if lower.ends_with(compound)
            && lower
                .get(..lower.len().saturating_sub(compound.len()))
                .is_some_and(|prefix| prefix.ends_with('.'))
        {
            return name.get(name.len() - compound.len()..);
        }
    }
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty()
        || ext.is_empty()
        || ext.len() > 12
        || !ext.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(ext)
}

/// Minimal HSV to RGB, so the stand-in colours stay in one family instead of
/// landing anywhere in the cube. Local rather than a dependency: this is the
/// only place in the workspace that needs it.
fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> (u8, u8, u8) {
    let hue = hue.rem_euclid(360.0) / 60.0;
    let chroma = value * saturation;
    let second = chroma * (1.0 - (hue % 2.0 - 1.0).abs());
    let (red, green, blue) = match hue as u32 {
        0 => (chroma, second, 0.0),
        1 => (second, chroma, 0.0),
        2 => (0.0, chroma, second),
        3 => (0.0, second, chroma),
        4 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    let base = value - chroma;
    let to_byte = |channel: f32| (((channel + base) * 255.0).round().clamp(0.0, 255.0)) as u8;
    (to_byte(red), to_byte(green), to_byte(blue))
}

#[cfg(test)]
mod tests {

    #[test]
    fn stand_in_pixels_are_stable_keyed_and_carry_nothing_real() {
        let session = PrivateSession::new();
        let one = session.thumb_pixels(11, 6);
        // Stable: a row must not flicker between frames.
        assert_eq!(one, session.thumb_pixels(11, 6));
        assert_eq!(one.len(), 6 * 6 * 4);
        // Different files look like different pictures.
        assert_ne!(one, session.thumb_pixels(12, 6));
        // Keyed per session: the same identity in another session is another
        // picture, so nothing can be correlated across captures.
        assert_ne!(one, PrivateSession::new().thumb_pixels(11, 6));
        // Opaque, and never fully black or fully white: a stand-in has to
        // read as content, not as a broken image.
        assert!(one.chunks(4).all(|pixel| pixel[3] == 255));
        let luma = |p: &[u8]| p[0] as u32 + p[1] as u32 + p[2] as u32;
        assert!(one.chunks(4).any(|pixel| luma(pixel) > 120));
        assert!(one.chunks(4).all(|pixel| luma(pixel) < 750));
    }
    use super::*;

    fn session() -> PrivateSession {
        PrivateSession::with_key([0x42; 16])
    }

    #[test]
    fn aliases_are_stable_and_do_not_copy_the_stem() {
        let private = session();
        let first = private.leaf("Alice Birthday 2026.JPG", false);
        assert_eq!(first, private.leaf("Alice Birthday 2026.JPG", false));
        assert!(first.ends_with(".JPG"));
        assert!(!first.to_lowercase().contains("alice"));
        assert!(!first.contains("2026"));
    }

    #[test]
    fn hidden_and_compound_extensions_are_exact() {
        let private = session();
        assert!(private.leaf(".secret.tar.gz", false).starts_with('.'));
        assert!(private.leaf(".secret.tar.gz", false).ends_with(".tar.gz"));
        let multi = private.leaf("alice.smith.pdf", false);
        assert!(multi.ends_with(".pdf"));
        assert!(!multi.contains("smith"));
    }

    #[test]
    fn session_keys_change_the_mapping() {
        let a = PrivateSession::with_key([1; 16]);
        let b = PrivateSession::with_key([2; 16]);
        assert_ne!(
            a.leaf("private-photo.png", false),
            b.leaf("private-photo.png", false)
        );
    }

    #[test]
    fn paths_keep_shape_without_components() {
        let shown = session().path(Path::new("/Users/alice/Family Photos"));
        assert!(shown.starts_with('/'));
        assert_eq!(shown.matches('/').count(), 3);
        assert!(!shown.contains("Users"));
        assert!(!shown.contains("alice"));
        assert!(!shown.contains("Family"));
    }

    #[test]
    fn digest_has_requested_width_and_is_not_raw() {
        let raw = "0123456789abcdef";
        let shown = session().digest(raw, raw.len());
        assert_eq!(shown.len(), raw.len());
        assert_ne!(shown, raw);
        assert!(shown.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn fake_bytes_are_stable_and_bounded() {
        let private = session();
        let shown = private.bytes(7, 4_123_456);
        assert_eq!(shown, private.bytes(7, 4_123_456));
        assert!((1_000_000..10_000_000).contains(&shown));
        assert_eq!(private.bytes(7, 0), 0);
    }

    #[test]
    fn timestamps_and_dimensions_keep_shape_not_values() {
        let private = session();
        let timestamp = private.timestamp(9, 1_700_000_000, 1_800_000_000);
        assert!(timestamp < 1_800_000_000);
        assert_ne!(timestamp, 1_700_000_000);
        let dims = private.dimensions(9, (4000, 3000));
        assert!(dims.0 > dims.1);
        assert_ne!(dims, (4000, 3000));
    }

    #[test]
    fn semantic_interface_routes_each_sensitive_kind() {
        let private = session();
        assert_eq!(
            private.present(PrivateValue::Leaf {
                raw: "family.jpg",
                is_dir: false,
            }),
            private.leaf("family.jpg", false)
        );
        assert_eq!(
            private.present(PrivateValue::Path(Path::new("/Users/alice"))),
            private.path(Path::new("/Users/alice"))
        );
        assert_eq!(
            private
                .present(PrivateValue::Digest {
                    raw: "abcdef",
                    width: 6,
                })
                .len(),
            6
        );
    }
}
