//! Native embedded-tag / audio-property reader — the one place the `lofty`
//! dependency lives.
//!
//! Two entry points, deliberately split by cost:
//!
//! * [`read_media_tags`] is the cheap path (tags + audio properties, **no**
//!   cover-art bytes). It is what the Get Info gather and the file-list
//!   prefetch worker call, one file at a time or per row — so it must never
//!   pull a multi-megabyte embedded picture into memory just to format a
//!   `"MP3 · stereo · …"` line. `ParseOptions::read_cover_art(false)` makes
//!   lofty skip the picture frames entirely.
//!
//! * [`read_cover_art`] is the on-demand path for the preview pane: it reads
//!   the picture bytes and hands back the front cover (or the first picture) as
//!   raw encoded image data for the host to decode into its image cache.
//!
//! Both run off the UI thread (Prime Directive) and both are cross-platform:
//! lofty is pure Rust with no platform backends, so this module has no `#[cfg]`
//! branches. A file lofty can't open (not audio, truncated, unreadable) yields
//! `None` rather than an error — the caller simply shows no Media section.

use std::path::Path;

use ferail_core::media::MediaTags;
use lofty::config::ParseOptions;
use lofty::file::{FileType, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::prelude::{Accessor, AudioFile};
use lofty::probe::Probe;

/// Read embedded tags and decoded audio properties for `path`, **without**
/// reading cover-art bytes. Returns `None` when the file isn't something lofty
/// recognizes as audio, or when it yields nothing worth showing.
///
/// The cover-art skip is what makes this safe to call per row in the prefetch
/// worker: lofty reads only the header/tag regions, not the whole file, and
/// never allocates the picture payload.
pub fn read_media_tags(path: &Path) -> Option<MediaTags> {
    let tagged = Probe::open(path)
        .ok()?
        .options(ParseOptions::new().read_cover_art(false))
        // Fall back to content sniffing when the extension is missing or
        // wrong, so a mis-named `.mp3` (or an extensionless track) still reads.
        .guess_file_type()
        .ok()?
        .read()
        .ok()?;

    let props = tagged.properties();
    let mut tags = MediaTags {
        codec: codec_label(tagged.file_type()),
        duration_secs: props.duration().as_secs(),
        // Audio bitrate is the stream rate; the overall (container) rate is a
        // reasonable fallback for formats that don't separate them.
        bitrate_kbps: props.audio_bitrate().or_else(|| props.overall_bitrate()),
        sample_rate_hz: props.sample_rate(),
        channels: props.channels(),
        bit_depth: props.bit_depth(),
        ..Default::default()
    };

    // Prefer the format's primary tag; fall back to whatever tag exists (a
    // file may carry only a secondary one, e.g. an ID3v1 tail on an MP3).
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        tags.title = text(tag.title());
        tags.artist = text(tag.artist());
        tags.album = text(tag.album());
        tags.genre = text(tag.genre());
        tags.comment = text(tag.comment());
        tags.track = tag.track();
        tags.track_total = tag.track_total();
        tags.disc = tag.disk();
        tags.disc_total = tag.disk_total();
        // Year rides in the tag's combined timestamp (lofty folds ID3v2.3's
        // separate TYER frame into TDRC via its default implicit conversions).
        tags.year = tag.date().map(|d| d.year);
    }

    if tags.is_empty() { None } else { Some(tags) }
}

/// Read the embedded cover art for `path`, if any: the front cover when the
/// file labels one, otherwise the first embedded picture. Returns the raw
/// **encoded** image bytes (PNG/JPEG/…) for the host to decode — the format is
/// self-describing, so no separate mime hint is needed.
///
/// This is the expensive read (it pulls the full picture payload), so it is
/// only called on demand for the previewed file, never per row.
pub fn read_cover_art(path: &Path) -> Option<Vec<u8>> {
    let tagged = Probe::open(path).ok()?.read().ok()?;

    // Pictures can live on any tag the file carries, not just the primary one.
    let pictures = || tagged.tags().iter().flat_map(|t| t.pictures());
    let front = pictures().find(|p| p.pic_type() == PictureType::CoverFront);
    let chosen = front.or_else(|| pictures().next())?;

    let data = chosen.data();
    if data.is_empty() {
        None
    } else {
        Some(data.to_vec())
    }
}

/// Short, human-facing container/codec label for the Description line and the
/// Media section's "Kind" — `FileType` is `#[non_exhaustive]`, hence the
/// catch-all arm.
fn codec_label(ft: FileType) -> String {
    match ft {
        FileType::Aac => "AAC",
        FileType::Aiff => "AIFF",
        FileType::Ape => "APE",
        FileType::Flac => "FLAC",
        // MPEG-1/2 audio in the wild is overwhelmingly MP3.
        FileType::Mpeg => "MP3",
        FileType::Mp4 => "M4A",
        FileType::Mpc => "Musepack",
        FileType::Opus => "Opus",
        FileType::Vorbis => "OGG",
        FileType::Speex => "Speex",
        FileType::Wav => "WAV",
        FileType::WavPack => "WavPack",
        FileType::Custom(s) => return s.to_string(),
        _ => "Audio",
    }
    .to_string()
}

/// Trim a tag string value and drop it if empty, so blank frames don't render
/// as empty Get Info rows.
fn text(value: Option<std::borrow::Cow<'_, str>>) -> String {
    value
        .map(|c| c.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build the bytes of a minimal valid PCM WAV: RIFF/WAVE with a `fmt `
    /// chunk and a silent `data` chunk sized to `frames`. Enough for lofty to
    /// report channels / sample rate / bit depth / duration — no external
    /// fixture file needed.
    fn minimal_wav(channels: u16, sample_rate: u32, bits: u16, frames: u32) -> Vec<u8> {
        let block_align = channels * (bits / 8);
        let byte_rate = sample_rate * block_align as u32;
        let data_len = frames * block_align as u32;
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data_len).to_le_bytes());
        w.extend_from_slice(b"WAVE");
        w.extend_from_slice(b"fmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&sample_rate.to_le_bytes());
        w.extend_from_slice(&byte_rate.to_le_bytes());
        w.extend_from_slice(&block_align.to_le_bytes());
        w.extend_from_slice(&bits.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data_len.to_le_bytes());
        w.resize(w.len() + data_len as usize, 0); // silence
        w
    }

    fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("ferail-media-{}-{name}", std::process::id()));
        let mut f = std::fs::File::create(&path).expect("create temp");
        f.write_all(bytes).expect("write temp");
        path
    }

    #[test]
    fn reads_wav_audio_properties() {
        // 1 second of CD-quality stereo silence: 44100 frames.
        let wav = minimal_wav(2, 44100, 16, 44100);
        let path = write_temp("props.wav", &wav);

        let tags = read_media_tags(&path).expect("wav should parse");
        assert_eq!(tags.codec, "WAV");
        assert_eq!(tags.channels, Some(2));
        assert_eq!(tags.sample_rate_hz, Some(44100));
        assert_eq!(tags.bit_depth, Some(16));
        assert_eq!(tags.duration_secs, 1);
        // Untagged file: no human metadata, but the record isn't "empty".
        assert!(!tags.has_metadata());
        assert!(!tags.is_empty());
        // lofty computes a bitrate even for PCM WAV (44100·2·16/1000 = 1411
        // kbps), so the Description carries it alongside the bit depth.
        assert_eq!(tags.bitrate_kbps, Some(1411));
        assert_eq!(
            tags.description(),
            "WAV · stereo · 44.1 kHz · 16-bit · 1411 kbps · 00:01"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn non_audio_file_yields_none() {
        let path = write_temp("notes.txt", b"this is not audio at all");
        assert!(read_media_tags(&path).is_none());
        // And no cover art to find.
        assert!(read_cover_art(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_yields_none() {
        let path = std::env::temp_dir().join("ferail-media-does-not-exist.mp3");
        assert!(read_media_tags(&path).is_none());
        assert!(read_cover_art(&path).is_none());
    }

    #[test]
    fn wav_has_no_embedded_cover() {
        let wav = minimal_wav(1, 48000, 16, 48000);
        let path = write_temp("nocover.wav", &wav);
        assert!(read_cover_art(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reads_tags_and_prefers_front_cover() {
        use lofty::picture::{MimeType, Picture, PictureType};
        use lofty::prelude::TagExt;
        use lofty::tag::{Tag, TagType};

        // A WAV carrying an ID3v2 chunk with two pictures: a non-cover one
        // first, then the front cover. read_cover_art must return the front
        // cover's bytes, not the first picture. (read_cover_art doesn't decode
        // the image, so arbitrary marker bytes exercise the selection logic.)
        let wav = minimal_wav(2, 44100, 16, 44100);
        let path = write_temp("tagged.wav", &wav);

        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title("Fixture Song".to_string());
        tag.set_artist("Test Artist".to_string());
        tag.push_picture(
            Picture::unchecked(b"OTHER-PIC".to_vec())
                .pic_type(PictureType::Other)
                .mime_type(MimeType::Png)
                .build(),
        );
        tag.push_picture(
            Picture::unchecked(b"FRONT-COVER".to_vec())
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Png)
                .build(),
        );
        tag.save_to_path(&path, lofty::config::WriteOptions::default())
            .expect("write tag");

        let tags = read_media_tags(&path).expect("tagged wav parses");
        assert_eq!(tags.title, "Fixture Song");
        assert_eq!(tags.artist, "Test Artist");
        assert!(tags.has_metadata());

        let cover = read_cover_art(&path).expect("has a cover");
        assert_eq!(cover, b"FRONT-COVER", "front cover wins over the first pic");

        let _ = std::fs::remove_file(&path);
    }
}
