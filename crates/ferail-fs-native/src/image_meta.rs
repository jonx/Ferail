//! Portable still-image metadata reader (WIN-014) — the one place the
//! `kamadak-exif` dependency lives.
//!
//! Fills [`ImageMeta`] for Get Info's Image section: pixel dimensions from
//! the image *header* (`image::image_dimensions` reads a few hundred bytes,
//! never a full decode) and a curated EXIF subset (camera, date taken,
//! orientation, exposure, GPS **presence**). Anything unreadable — not an
//! image, no EXIF, truncated, malformed — degrades to fewer fields or
//! `None`; this reader must never error a Get Info open.
//!
//! Privacy: GPS coordinates are detected but deliberately not parsed,
//! logged, or persisted (WIN-014's treatment). Only the boolean leaves
//! this module.
//!
//! Runs off the UI thread (Prime Directive): both probes open the file.
//! Cross-platform, pure Rust, no platform services — the Windows
//! `IPropertyStore` provider planned by WIN-014 is additive, not a
//! replacement for this.

use std::io::BufReader;
use std::path::Path;

use exif::{In, Tag, Value};
use ferail_core::media::ImageMeta;

/// Extensions worth probing. Gate up front so Get Info on arbitrary files
/// doesn't open them twice for nothing. `heic`/`heif`/`avif` have EXIF but
/// no header support in `image` — they simply yield no dimensions.
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "png", "tif", "tiff", "webp", "heic", "heif", "avif",
];

/// Read image metadata for `path`. `None` when the extension isn't an
/// image type or nothing at all could be read.
pub fn read_image_meta(path: &Path) -> Option<ImageMeta> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !IMAGE_EXTS.contains(&ext.as_str()) {
        return None;
    }

    let mut meta = ImageMeta::default();
    if let Ok((w, h)) = image::image_dimensions(path) {
        meta.width = Some(w);
        meta.height = Some(h);
    }
    if let Ok(file) = std::fs::File::open(path) {
        if let Ok(exif) = exif::Reader::new().read_from_container(&mut BufReader::new(file)) {
            apply_exif(&mut meta, &exif);
        }
    }
    if meta.is_empty() {
        None
    } else {
        Some(meta)
    }
}

/// Copy the curated EXIF subset into `meta`. Separated from the file I/O so
/// tests can drive it from a raw in-memory TIFF buffer.
fn apply_exif(meta: &mut ImageMeta, exif: &exif::Exif) {
    let ascii = |tag| {
        exif.get_field(tag, In::PRIMARY)
            .and_then(|f| ascii_value(&f.value))
    };
    // display_value carries the human form kamadak already knows how to
    // build ("1/250 s", "f/2.8", "35 mm") — no rational math to re-do here.
    let display = |tag| {
        exif.get_field(tag, In::PRIMARY)
            .map(|f| f.display_value().with_unit(exif).to_string())
    };

    meta.camera_make = ascii(Tag::Make).unwrap_or_default();
    meta.camera_model = ascii(Tag::Model).unwrap_or_default();
    meta.lens_model = ascii(Tag::LensModel).unwrap_or_default();
    meta.taken = ascii(Tag::DateTimeOriginal)
        .or_else(|| ascii(Tag::DateTime))
        .map(|s| normalize_exif_datetime(&s))
        .unwrap_or_default();
    meta.orientation = exif
        .get_field(Tag::Orientation, In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .and_then(|v| u16::try_from(v).ok())
        .filter(|o| (1..=8).contains(o));
    meta.exposure_time = display(Tag::ExposureTime).unwrap_or_default();
    meta.f_number = display(Tag::FNumber).unwrap_or_default();
    meta.focal_length = display(Tag::FocalLength).unwrap_or_default();
    meta.iso = exif
        .get_field(Tag::PhotographicSensitivity, In::PRIMARY)
        .and_then(|f| f.value.get_uint(0));
    // Presence only — the coordinate values are deliberately not read.
    meta.gps_present = exif.get_field(Tag::GPSLatitude, In::PRIMARY).is_some()
        || exif.get_field(Tag::GPSLongitude, In::PRIMARY).is_some();
}

/// First ASCII string of an EXIF value, trimmed of the padding and stray
/// whitespace vendors love. `None` for empty or non-ASCII values.
fn ascii_value(value: &Value) -> Option<String> {
    match value {
        Value::Ascii(strings) => {
            let first = strings.first()?;
            let s = String::from_utf8_lossy(first);
            let s = s.trim_matches(['\0', ' ', '\t']).to_string();
            (!s.is_empty()).then_some(s)
        }
        _ => None,
    }
}

/// `"2023:04:01 12:30:00"` → `"2023-04-01 12:30:00"` — only the date part's
/// colons change. Anything not shaped like an EXIF datetime passes through.
fn normalize_exif_datetime(s: &str) -> String {
    match s.split_once(' ') {
        Some((date, time)) if date.len() == 10 => {
            format!("{} {}", date.replace(':', "-"), time)
        }
        _ => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_normalizes() {
        assert_eq!(
            normalize_exif_datetime("2023:04:01 12:30:00"),
            "2023-04-01 12:30:00"
        );
        // Not EXIF-shaped → untouched.
        assert_eq!(normalize_exif_datetime("yesterday"), "yesterday");
    }

    #[test]
    fn non_image_extensions_are_gated() {
        assert!(read_image_meta(Path::new("notes.txt")).is_none());
        assert!(read_image_meta(Path::new("archive.zip")).is_none());
        assert!(read_image_meta(Path::new("no-extension")).is_none());
    }

    #[test]
    fn dimensions_without_exif() {
        // A generated PNG has a header but no EXIF: dimensions only.
        let dir = std::env::temp_dir().join("ferail-image-meta-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plain.png");
        image::RgbaImage::new(6, 4).save(&path).unwrap();
        let meta = read_image_meta(&path).expect("dimensions should read");
        assert_eq!((meta.width, meta.height), (Some(6), Some(4)));
        assert!(meta.camera_label().is_empty());
        assert!(!meta.gps_present);
        let _ = std::fs::remove_file(&path);
    }

    // ---- raw-TIFF EXIF fixture ------------------------------------------

    /// Minimal little-endian TIFF/EXIF builder: enough IFD plumbing to give
    /// `apply_exif` a real parsed structure without a binary fixture.
    struct Ifd {
        entries: Vec<(u16, u16, u32, EntryData)>,
    }

    enum EntryData {
        Inline(u32),
        OutOfLine(Vec<u8>),
    }

    impl Ifd {
        fn new() -> Self {
            Self {
                entries: Vec::new(),
            }
        }
        fn ascii(&mut self, tag: u16, s: &str) {
            let mut bytes = s.as_bytes().to_vec();
            bytes.push(0);
            let count = bytes.len() as u32;
            if bytes.len() <= 4 {
                let mut v = [0u8; 4];
                v[..bytes.len()].copy_from_slice(&bytes);
                self.entries
                    .push((tag, 2, count, EntryData::Inline(u32::from_le_bytes(v))));
            } else {
                self.entries
                    .push((tag, 2, count, EntryData::OutOfLine(bytes)));
            }
        }
        fn short(&mut self, tag: u16, v: u16) {
            self.entries.push((tag, 3, 1, EntryData::Inline(v as u32)));
        }
        fn long(&mut self, tag: u16, v: u32) {
            self.entries.push((tag, 4, 1, EntryData::Inline(v)));
        }
        fn rationals(&mut self, tag: u16, pairs: &[(u32, u32)]) {
            let mut bytes = Vec::new();
            for (n, d) in pairs {
                bytes.extend_from_slice(&n.to_le_bytes());
                bytes.extend_from_slice(&d.to_le_bytes());
            }
            self.entries
                .push((tag, 5, pairs.len() as u32, EntryData::OutOfLine(bytes)));
        }
        /// Serialize at `at` (byte offset inside the TIFF), returning the
        /// IFD bytes followed by its out-of-line data.
        fn build(&self, at: u32) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
            let data_start = at + 2 + 12 * self.entries.len() as u32 + 4;
            let mut data: Vec<u8> = Vec::new();
            for (tag, ty, count, payload) in &self.entries {
                out.extend_from_slice(&tag.to_le_bytes());
                out.extend_from_slice(&ty.to_le_bytes());
                out.extend_from_slice(&count.to_le_bytes());
                match payload {
                    EntryData::Inline(v) => out.extend_from_slice(&v.to_le_bytes()),
                    EntryData::OutOfLine(bytes) => {
                        let off = data_start + data.len() as u32;
                        out.extend_from_slice(&off.to_le_bytes());
                        data.extend_from_slice(bytes);
                    }
                }
            }
            out.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
            out.extend_from_slice(&data);
            out
        }
        fn len(&self, _at: u32) -> u32 {
            let data: usize = self
                .entries
                .iter()
                .map(|(_, _, _, p)| match p {
                    EntryData::Inline(_) => 0,
                    EntryData::OutOfLine(b) => b.len(),
                })
                .sum();
            2 + 12 * self.entries.len() as u32 + 4 + data as u32
        }
    }

    fn exif_fixture() -> exif::Exif {
        // IFD0 (camera + pointers) at 8; Exif IFD and GPS IFD follow it.
        let mut ifd0 = Ifd::new();
        ifd0.ascii(0x010F, "Canon"); // Make
        ifd0.ascii(0x0110, "Canon EOS R5"); // Model
        ifd0.short(0x0112, 6); // Orientation: rotate 90 CW
                               // Pointer values need the layout; compute after sizing IFD0 with
                               // placeholder pointers of the same encoded size.
        let mut sized = Ifd::new();
        sized.ascii(0x010F, "Canon");
        sized.ascii(0x0110, "Canon EOS R5");
        sized.short(0x0112, 6);
        sized.long(0x8769, 0); // Exif IFD pointer
        sized.long(0x8825, 0); // GPS IFD pointer
        let exif_at = 8 + sized.len(8);

        let mut exif_ifd = Ifd::new();
        exif_ifd.ascii(0x9003, "2023:04:01 12:30:00"); // DateTimeOriginal
        exif_ifd.short(0x8827, 100); // ISO
        exif_ifd.rationals(0x829A, &[(1, 250)]); // ExposureTime
        exif_ifd.rationals(0x829D, &[(28, 10)]); // FNumber
        let gps_at = exif_at + exif_ifd.len(exif_at);

        let mut gps_ifd = Ifd::new();
        gps_ifd.rationals(0x0002, &[(48, 1), (51, 1), (24, 1)]); // GPSLatitude

        ifd0.long(0x8769, exif_at);
        ifd0.long(0x8825, gps_at);

        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&ifd0.build(8));
        assert_eq!(tiff.len() as u32, exif_at);
        tiff.extend_from_slice(&exif_ifd.build(exif_at));
        assert_eq!(tiff.len() as u32, gps_at);
        tiff.extend_from_slice(&gps_ifd.build(gps_at));

        exif::Reader::new()
            .read_raw(tiff)
            .expect("hand-built TIFF should parse")
    }

    #[test]
    fn exif_subset_reads() {
        let mut meta = ImageMeta::default();
        apply_exif(&mut meta, &exif_fixture());
        assert_eq!(meta.camera_label(), "Canon EOS R5");
        assert_eq!(meta.taken, "2023-04-01 12:30:00");
        assert_eq!(meta.orientation, Some(6));
        assert_eq!(meta.iso, Some(100));
        assert!(
            meta.exposure_time.contains("1/250"),
            "{}",
            meta.exposure_time
        );
        assert!(meta.f_number.contains("2.8"), "{}", meta.f_number);
        // GPS: presence detected, coordinates never surfaced anywhere.
        assert!(meta.gps_present);
        let line = meta.exposure_label();
        assert!(line.contains("ISO 100"), "{line}");
    }
}
