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
        self.bit_depth.map(|b| format!("{b}-bit")).unwrap_or_default()
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

/// Hz → `"44.1 kHz"` / `"48 kHz"`. One decimal, trailing `.0` stripped.
fn format_khz(hz: u32) -> String {
    // Round to the nearest 100 Hz first so 44100 → 44.1 rather than a noisy
    // 44.099…; audio sample rates are always multiples of at least 100 Hz.
    let tenths = (hz + 50) / 100; // hz in units of 0.1 kHz
    if tenths % 10 == 0 {
        format!("{} kHz", tenths / 10)
    } else {
        format!("{}.{} kHz", tenths / 10, tenths % 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let ch = |n: Option<u8>| MediaTags {
            channels: n,
            ..Default::default()
        }
        .channels_label();
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
        assert_eq!(t.description(), "MP3 · stereo · 44.1 kHz · 192 kbps · 03:24");
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
