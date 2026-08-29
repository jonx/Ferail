//! Platform-neutral media-tag model.
//!
//! [`MediaTags`] is the neutral record the host builds for one audio file from
//! its embedded tags and decoded audio properties. It is filled off the UI
//! thread by the native reader (`ferail_fs_native::media`, which owns the
//! `lofty` dependency) and consumed read-only by the Get Info panel, the
//! preview, and the file-list Description column — in keeping with the Prime
//! Directive: paint never touches I/O and never parses a container.
//!
//! This crate has zero platform and zero UI deps: the model is data plus small
//! pure formatting helpers. The bytes of embedded cover art never live here —
//! they ride a separate channel (`ferail_fs_native::media::read_cover_art`)
//! straight into the host's image cache, so a multi-megabyte APIC frame never
//! sits in a struct the file list clones per row. Cover *presence* isn't
//! carried either: lofty can only report it by reading the picture bytes (the
//! very cost this record avoids), so the preview simply attempts the cover read
//! and shows whatever comes back.

use std::fmt;

/// One audio file's embedded tags and decoded audio properties, pre-parsed off
/// the UI thread. Every text field is already the display string; the numeric
/// fields stay raw so the formatting helpers below can compose different
/// summaries (Get Info rows vs. the one-line Description) without re-reading.
///
/// Absent facts are empty strings / `None` — a file with no tag at all still
/// yields a `MediaTags` carrying just the audio properties (duration, bitrate,
/// …), which is exactly what a freshly-ripped-but-untagged file should show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaTags {
    /// Short container/codec label, e.g. "MP3", "FLAC", "M4A", "WAV". Always
    /// present (derived from the file type, not the tag).
    pub codec: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    /// Track number and, when known, the total (`Some(3)` of `Some(12)`).
    pub track: Option<u32>,
    pub track_total: Option<u32>,
    /// Disc number and total, for multi-disc sets.
    pub disc: Option<u32>,
    pub disc_total: Option<u32>,
    /// Recording/release year from the tag's timestamp, when present.
    pub year: Option<u16>,
    pub comment: String,
    /// Playing time in whole seconds (`0` when the decoder couldn't determine
    /// it, e.g. a truncated stream).
    pub duration_secs: u64,
    /// Overall bitrate in kbps, when the properties reader reports one.
    pub bitrate_kbps: Option<u32>,
    /// Sample rate in Hz (e.g. 44100), when known.
    pub sample_rate_hz: Option<u32>,
    /// Channel count (1 = mono, 2 = stereo), when known.
    pub channels: Option<u8>,
    /// Bit depth for lossless formats (e.g. 16, 24), when known.
    pub bit_depth: Option<u8>,
}

impl MediaTags {
    /// True when there is nothing worth showing — no codec label and no audio
    /// properties. A reader that failed to open the file returns this so the
    /// host can skip the whole Media section rather than render an empty box.
    pub fn is_empty(&self) -> bool {
        self.codec.is_empty() && self.duration_secs == 0 && self.bitrate_kbps.is_none()
    }

    /// True when at least one human-authored tag field is populated (as opposed
    /// to a bare untagged file that only yields audio properties). Lets the
    /// preview decide whether a title/artist header is worth drawing.
    pub fn has_metadata(&self) -> bool {
        !self.title.is_empty()
            || !self.artist.is_empty()
            || !self.album.is_empty()
            || !self.genre.is_empty()
            || self.year.is_some()
            || self.track.is_some()
    }

    /// `"MM:SS"`, or `"H:MM:SS"` past an hour. Empty when the duration is
    /// unknown (`0`). Pure integer arithmetic — safe anywhere.
    pub fn duration_label(&self) -> String {
        format_duration(self.duration_secs)
    }

    /// `"3 of 12"`, `"3"`, or empty — the track position for a Get Info row.
    pub fn track_label(&self) -> String {
        number_of(self.track, self.track_total)
    }

    /// `"1 of 2"`, `"1"`, or empty — the disc position for a Get Info row.
    pub fn disc_label(&self) -> String {
        number_of(self.disc, self.disc_total)
    }

    /// `"stereo"`, `"mono"`, `"6 channels"`, or empty.
    pub fn channels_label(&self) -> String {
        match self.channels {
            Some(1) => tr!("mono").into_string(),
            Some(2) => tr!("stereo").into_string(),
            Some(n) => tr!("{n} channels", n = n).into_string(),
            None => String::new(),
        }
    }

    /// `"44.1 kHz"` / `"48 kHz"`, or empty. Shows one decimal only when the
    /// rate isn't a whole number of kHz, so CD audio reads "44.1 kHz" but a
    /// 48000 Hz file reads the cleaner "48 kHz".
    pub fn sample_rate_label(&self) -> String {
        match self.sample_rate_hz {
            Some(hz) if hz > 0 => format_khz(hz),
            _ => String::new(),
        }
    }

    /// `"192 kbps"`, or empty.
    pub fn bitrate_label(&self) -> String {
        match self.bitrate_kbps {
            Some(kbps) if kbps > 0 => format!("{kbps} kbps"),
            _ => String::new(),
        }
    }

    /// `"16-bit"`, or empty. Lossless formats report a bit depth; lossy ones
    /// don't, so this slot naturally drops out for MP3/AAC and appears for
    /// FLAC/ALAC/WAV.
    pub fn bit_depth_label(&self) -> String {
        self.bit_depth
            .map(|b| format!("{b}-bit"))
            .unwrap_or_default()
    }

    /// The one-line ` · `-joined fact string for the file-list Description
    /// column, e.g. `"MP3 · stereo · 44.1 kHz · 192 kbps · 03:24"` or
    /// `"FLAC · stereo · 44.1 kHz · 16-bit · 882 kbps · 03:24"`. Every segment
    /// is dropped when absent, with no lossy/lossless branching — whatever the
    /// decoder reported is shown, so an untagged WAV still reads cleanly as
    /// `"WAV · stereo · 44.1 kHz · 16-bit"`. Same contract as the magic-derived
    /// descriptions in [`crate::FileEntry::display_description`].
    pub fn description(&self) -> String {
        [
            self.codec.clone(),
            self.channels_label(),
            self.sample_rate_label(),
            self.bit_depth_label(),
            self.bitrate_label(),
            self.duration_label(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" \u{00B7} ")
    }
}

/// `"MM:SS"` / `"H:MM:SS"`, or empty for `0`.
fn format_duration(secs: u64) -> String {
    if secs == 0 {
        return String::new();
    }
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// `"3 of 12"` when both are known, `"3"` when only the number is, else empty.
fn number_of(n: Option<u32>, total: Option<u32>) -> String {
    match (n, total) {
        (Some(n), Some(t)) if t > 0 => tr!("{n} of {t}", n = n, t = t).into_string(),
        (Some(n), _) => format!("{n}"),
        _ => String::new(),
    }
}

/// Portable still-image metadata for Get Info (WIN-014) — dimensions from
/// the image header plus a curated EXIF subset. Filled by
/// `ferail_fs_native::image_meta::read_image_meta` off the UI thread and
/// rendered read-only, like [`MediaTags`].
///
/// String fields are display-ready but deliberately unlocalized here (the
/// `"192 kbps"` precedent above): units and numbers only. Words that need
/// translating — the row labels, the orientation wording, the GPS-presence
/// phrase — belong to the UI layer.
#[derive(Clone, Default, PartialEq)]
pub struct ImageMeta {
    /// Pixel dimensions from the image *header* (never a full decode).
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// EXIF `Make` / `Model`, trimmed. Either may be empty.
    pub camera_make: String,
    pub camera_model: String,
    /// EXIF `LensModel`, when present.
    pub lens_model: String,
    /// `DateTimeOriginal` (falling back to `DateTime`), normalized from
    /// EXIF's `"2023:04:01 12:30:00"` to `"2023-04-01 12:30:00"`. EXIF
    /// datetimes are naive local time — shown as-is, never converted.
    pub taken: String,
    /// Raw EXIF orientation code `1..=8`; `None` when absent or invalid.
    pub orientation: Option<u16>,
    /// Display-ready exposure fragments, e.g. `"1/250 s"`, `"f/2.8"`,
    /// `"35 mm"`. Empty when the tag is absent.
    pub exposure_time: String,
    pub f_number: String,
    pub focal_length: String,
    pub iso: Option<u32>,
    /// The file carries GPS coordinates. Presence only, by design: the
    /// coordinates are never parsed into this DTO, logged, or persisted
    /// (WIN-014's privacy treatment). A deliberate reveal can come later.
    pub gps_present: bool,
}

/// EXIF strings can identify a person, device, or time. Keep them available to
/// the explicit Get Info UI while making accidental diagnostic formatting
/// reveal presence only, never values.
impl fmt::Debug for ImageMeta {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImageMeta")
            .field(
                "has_dimensions",
                &(self.width.is_some() && self.height.is_some()),
            )
            .field(
                "has_camera",
                &(!self.camera_make.is_empty() || !self.camera_model.is_empty()),
            )
            .field("has_lens", &!self.lens_model.is_empty())
            .field("has_taken", &!self.taken.is_empty())
            .field("has_orientation", &self.orientation.is_some())
            .field(
                "has_exposure",
                &(!self.exposure_time.is_empty()
                    || !self.f_number.is_empty()
                    || !self.focal_length.is_empty()
                    || self.iso.is_some()),
            )
            .field("gps_present", &self.gps_present)
            .finish()
    }
}

impl ImageMeta {
    /// True when nothing at all was read — the caller then skips the whole
    /// Image section instead of rendering an empty box.
    pub fn is_empty(&self) -> bool {
        self.width.is_none()
            && self.height.is_none()
            && self.camera_make.is_empty()
            && self.camera_model.is_empty()
            && self.lens_model.is_empty()
            && self.taken.is_empty()
            && self.orientation.is_none()
            && self.exposure_time.is_empty()
            && self.f_number.is_empty()
            && self.focal_length.is_empty()
            && self.iso.is_none()
            && !self.gps_present
    }

    /// `"4032 × 3024"`, or empty when the header yielded no dimensions.
    pub fn dimensions_label(&self) -> String {
        match (self.width, self.height) {
            (Some(w), Some(h)) => format!("{w} \u{00D7} {h}"),
            _ => String::new(),
        }
    }

    /// `"Canon EOS R5"` — make + model, deduplicated when the model already
    /// repeats the make (most vendors do). Empty when neither is known.
    pub fn camera_label(&self) -> String {
        let make = self.camera_make.trim();
        let model = self.camera_model.trim();
        if model.is_empty() {
            return make.to_string();
        }
        if make.is_empty() || model.to_lowercase().contains(&make.to_lowercase()) {
            return model.to_string();
        }
        format!("{make} {model}")
    }

    /// The ` · `-joined exposure line, e.g.
    /// `"1/250 s · f/2.8 · ISO 100 · 35 mm"`. Segments drop out when absent.
    pub fn exposure_label(&self) -> String {
        [
            self.exposure_time.clone(),
            self.f_number.clone(),
            self.iso.map(|n| format!("ISO {n}")).unwrap_or_default(),
            self.focal_length.clone(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" \u{00B7} ")
    }
}

/// Hz → `"44.1 kHz"` / `"48 kHz"`. One decimal, trailing `.0` stripped.
fn format_khz(hz: u32) -> String {
    // Round to the nearest 100 Hz first so 44100 → 44.1 rather than a noisy
    // 44.099…; audio sample rates are always multiples of at least 100 Hz.
    let tenths = (hz + 50) / 100; // hz in units of 0.1 kHz
    if tenths.is_multiple_of(10) {
        format!("{} kHz", tenths / 10)
    } else {
        format!("{}.{} kHz", tenths / 10, tenths % 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_meta_labels() {
        let m = ImageMeta {
            width: Some(4032),
            height: Some(3024),
            camera_make: "Canon".into(),
            camera_model: "Canon EOS R5".into(),
            exposure_time: "1/250 s".into(),
            f_number: "f/2.8".into(),
            iso: Some(100),
            focal_length: "35 mm".into(),
            ..Default::default()
        };
        assert_eq!(m.dimensions_label(), "4032 \u{00D7} 3024");
        // Model already repeats the make — no "Canon Canon EOS R5".
        assert_eq!(m.camera_label(), "Canon EOS R5");
        assert_eq!(
            m.exposure_label(),
            "1/250 s \u{00B7} f/2.8 \u{00B7} ISO 100 \u{00B7} 35 mm"
        );
        assert!(!m.is_empty());

        let distinct = ImageMeta {
            camera_make: "Nikon".into(),
            camera_model: "Z 8".into(),
            ..Default::default()
        };
        assert_eq!(distinct.camera_label(), "Nikon Z 8");

        assert!(ImageMeta::default().is_empty());
        assert_eq!(ImageMeta::default().dimensions_label(), "");
        assert_eq!(ImageMeta::default().exposure_label(), "");
        // GPS presence alone keeps the section alive.
        assert!(!ImageMeta {
            gps_present: true,
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn image_meta_debug_redacts_exif_values() {
        let meta = ImageMeta {
            camera_make: "Private Make".into(),
            camera_model: "Alice's Camera".into(),
            lens_model: "Family Lens".into(),
            taken: "2042-03-04 05:06:07".into(),
            exposure_time: "secret exposure".into(),
            gps_present: true,
            ..ImageMeta::default()
        };
        let debug = format!("{meta:?}");
        for private in ["Private", "Alice", "Family", "2042", "secret"] {
            assert!(!debug.contains(private));
        }
        assert!(debug.contains("has_camera: true"));
        assert!(debug.contains("gps_present: true"));
    }

    #[test]
    fn duration_formats() {
        assert_eq!(format_duration(0), "");
        assert_eq!(format_duration(9), "00:09");
        assert_eq!(format_duration(204), "03:24");
        assert_eq!(format_duration(3600), "1:00:00");
        assert_eq!(format_duration(3725), "1:02:05");
    }

    #[test]
    fn khz_formats() {
        assert_eq!(format_khz(44100), "44.1 kHz");
        assert_eq!(format_khz(48000), "48 kHz");
        assert_eq!(format_khz(96000), "96 kHz");
        assert_eq!(format_khz(22050), "22.1 kHz"); // 22050 rounds to 22.1
        assert_eq!(format_khz(8000), "8 kHz");
    }

    #[test]
    fn number_of_formats() {
        assert_eq!(number_of(Some(3), Some(12)), "3 of 12");
        assert_eq!(number_of(Some(3), None), "3");
        assert_eq!(number_of(Some(3), Some(0)), "3");
        assert_eq!(number_of(None, Some(12)), "");
        assert_eq!(number_of(None, None), "");
    }

    #[test]
    fn channel_labels() {
        let ch = |n: Option<u8>| {
            MediaTags {
                channels: n,
                ..Default::default()
            }
            .channels_label()
        };
        assert_eq!(ch(Some(1)), "mono");
        assert_eq!(ch(Some(2)), "stereo");
        assert_eq!(ch(Some(6)), "6 channels");
        assert_eq!(ch(None), "");
    }

    #[test]
    fn description_lossy_mp3() {
        let t = MediaTags {
            codec: "MP3".into(),
            channels: Some(2),
            sample_rate_hz: Some(44100),
            bitrate_kbps: Some(192),
            duration_secs: 204,
            ..Default::default()
        };
        assert_eq!(
            t.description(),
            "MP3 · stereo · 44.1 kHz · 192 kbps · 03:24"
        );
    }

    #[test]
    fn description_lossless_flac_shows_both_depth_and_bitrate() {
        // FLAC reports a bit depth AND a (large) bitrate; both are shown, in
        // that order, with no format-family branching in the reader.
        let t = MediaTags {
            codec: "FLAC".into(),
            channels: Some(2),
            sample_rate_hz: Some(44100),
            bitrate_kbps: Some(882),
            bit_depth: Some(16),
            duration_secs: 204,
            ..Default::default()
        };
        assert_eq!(
            t.description(),
            "FLAC · stereo · 44.1 kHz · 16-bit · 882 kbps · 03:24"
        );
    }

    #[test]
    fn description_untagged_wav_no_bitrate() {
        // A bare WAV: bit depth but no bitrate — the bitrate slot drops out.
        let t = MediaTags {
            codec: "WAV".into(),
            channels: Some(2),
            sample_rate_hz: Some(44100),
            bit_depth: Some(16),
            ..Default::default()
        };
        assert_eq!(t.description(), "WAV · stereo · 44.1 kHz · 16-bit");
    }

    #[test]
    fn description_drops_empty_segments() {
        // A bare codec label with nothing else known.
        let t = MediaTags {
            codec: "OGG".into(),
            ..Default::default()
        };
        assert_eq!(t.description(), "OGG");
    }

    #[test]
    fn empty_and_metadata_predicates() {
        assert!(MediaTags::default().is_empty());
        assert!(!MediaTags {
            codec: "MP3".into(),
            ..Default::default()
        }
        .is_empty());
        assert!(!MediaTags {
            codec: "MP3".into(),
            ..Default::default()
        }
        .has_metadata());
        assert!(MediaTags {
            title: "Song".into(),
            ..Default::default()
        }
        .has_metadata());
    }
}
