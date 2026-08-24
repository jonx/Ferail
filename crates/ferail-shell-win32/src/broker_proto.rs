//! Wire format and provider quarantine for the preview broker.
//!
//! The broker (`--preview-broker`, a disposable re-launch of the Ferail
//! binary) renders one third-party `IPreviewHandler` preview and writes
//! a single frame to stdout. This module owns the two pieces that must
//! be trustworthy on the *parent* side of that boundary:
//!
//! - the frame encoding, with strict validation before any byte of
//!   broker output is accepted (WIN-002: never trust dimensions or
//!   lengths from a process that just hosted arbitrary native code);
//! - the session quarantine that stops re-running a provider CLSID
//!   which has already crashed or hung, so one broken codec degrades
//!   to the icon fallback instead of a crash loop.
//!
//! Pure logic, compiled on every host so the unit tests run on
//! non-Windows dev machines too.

use std::collections::HashMap;

/// Frame header magic. Version-bumped if the layout ever changes so a
/// mismatched parent/broker pair fails closed instead of misparsing.
pub const FRAME_MAGIC: &[u8; 4] = b"FPB1";

/// Broker process exit codes. Anything else — including Windows
/// exception codes like `0xC0000005` — counts as a provider failure
/// and feeds the quarantine.
pub const EXIT_OK: i32 = 0;
pub const EXIT_NO_PREVIEW: i32 = 1;
pub const EXIT_USAGE: i32 = 2;

/// Hard ceiling on either frame dimension. The parent never asks for
/// more than ~1k; anything larger is a corrupt or hostile header.
pub const MAX_DIM: u32 = 4096;

/// Encode one RGBA frame: magic, width, height, byte length (all
/// little-endian u32), then the pixels.
pub fn encode_frame(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + rgba.len());
    out.extend_from_slice(FRAME_MAGIC);
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    out.extend_from_slice(&(rgba.len() as u32).to_le_bytes());
    out.extend_from_slice(rgba);
    out
}

/// Parse and validate a frame. Returns `None` for anything that is not
/// exactly a well-formed frame: wrong magic, zero or oversized
/// dimensions, or a byte count that disagrees with `w * h * 4`.
/// Trailing bytes after the payload are rejected too — a well-behaved
/// broker writes one frame and exits.
pub fn parse_frame(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    if bytes.len() < 16 || &bytes[..4] != FRAME_MAGIC {
        return None;
    }
    let word = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
    let (w, h, len) = (word(4), word(8), word(12));
    if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
        return None;
    }
    let expected = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    if len as usize != expected || bytes.len() - 16 != expected {
        return None;
    }
    Some((bytes[16..].to_vec(), w, h))
}

/// Session quarantine for preview-handler CLSIDs.
///
/// A provider that crashes or times out is skipped immediately
/// for the rest of the session; the caller then falls through to
/// `IShellItemImageFactory` / built-in decoding / the generic icon. A
/// success clears the count, so a provider that merely hiccuped under
/// load is not written off forever.
#[derive(Default)]
pub struct Quarantine {
    strikes: HashMap<String, u32>,
}

impl Quarantine {
    /// Native access violations and deadline overruns are not recoverable
    /// transient misses. Retrying the same provider merely repeats a crash or
    /// a six-second stall, so fail closed after the first incident.
    pub const THRESHOLD: u32 = 1;

    pub fn is_quarantined(&self, clsid: &str) -> bool {
        self.strikes.get(clsid).copied().unwrap_or(0) >= Self::THRESHOLD
    }

    /// Record a crash/hang for `clsid`. Returns true when this failure
    /// is the one that quarantined the provider (so the caller can log
    /// the transition exactly once).
    pub fn note_failure(&mut self, clsid: &str) -> bool {
        let count = self.strikes.entry(clsid.to_string()).or_insert(0);
        *count += 1;
        *count == Self::THRESHOLD
    }

    pub fn note_success(&mut self, clsid: &str) {
        self.strikes.remove(clsid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let rgba = vec![7u8; 2 * 3 * 4];
        let encoded = encode_frame(2, 3, &rgba);
        let (bytes, w, h) = parse_frame(&encoded).expect("roundtrip");
        assert_eq!((w, h), (2, 3));
        assert_eq!(bytes, rgba);
    }

    #[test]
    fn frame_rejects_bad_magic() {
        let mut encoded = encode_frame(1, 1, &[0u8; 4]);
        encoded[0] = b'X';
        assert!(parse_frame(&encoded).is_none());
    }

    #[test]
    fn frame_rejects_dimension_length_mismatch() {
        // Header claims 2×2 but carries one pixel.
        let mut encoded = encode_frame(1, 1, &[0u8; 4]);
        encoded[4..8].copy_from_slice(&2u32.to_le_bytes());
        encoded[8..12].copy_from_slice(&2u32.to_le_bytes());
        assert!(parse_frame(&encoded).is_none());
    }

    #[test]
    fn frame_rejects_zero_oversize_and_trailing() {
        assert!(parse_frame(&encode_frame(0, 1, &[])).is_none());
        let mut oversize = encode_frame(1, 1, &[0u8; 4]);
        oversize[4..8].copy_from_slice(&(MAX_DIM + 1).to_le_bytes());
        assert!(parse_frame(&oversize).is_none());
        let mut trailing = encode_frame(1, 1, &[0u8; 4]);
        trailing.push(0);
        assert!(parse_frame(&trailing).is_none());
        // Truncated header.
        assert!(parse_frame(&[0u8; 8]).is_none());
    }

    #[test]
    fn frame_rejects_overflowing_dimensions() {
        // w*h*4 overflowing usize/u32 math must fail, not wrap.
        let mut encoded = encode_frame(1, 1, &[0u8; 4]);
        encoded[4..8].copy_from_slice(&MAX_DIM.to_le_bytes());
        encoded[8..12].copy_from_slice(&MAX_DIM.to_le_bytes());
        encoded[12..16].copy_from_slice(&4u32.to_le_bytes());
        assert!(parse_frame(&encoded).is_none());
    }

    #[test]
    fn quarantine_threshold_and_reset() {
        let mut q = Quarantine::default();
        let clsid = "{aaaa}";
        assert!(!q.is_quarantined(clsid));
        assert!(q.note_failure(clsid)); // first fatal failure → transition
        assert!(q.is_quarantined(clsid));
        assert!(!q.note_failure(clsid)); // already quarantined: no re-log
        q.note_success(clsid);
        assert!(!q.is_quarantined(clsid));
        // Other providers are independent.
        q.note_failure("{bbbb}");
        assert!(q.is_quarantined("{bbbb}"));
        assert!(!q.is_quarantined(clsid));
    }
}
