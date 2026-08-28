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

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use ferail_core::media::MediaTags;
use lofty::config::ParseOptions;
use lofty::file::{FileType, TaggedFile, TaggedFileExt};
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
    read_media_tags_with_magic(path, None)
}

/// Cheap scheduling predicate for callers which already have a magic result.
/// It performs no I/O: extensions cover the common path and content covers
/// renamed files. Correctness still lives in [`read_media_tags_with_magic`].
pub fn is_audio_candidate(path: &Path, magic: Option<&crate::MagicInfo>) -> bool {
    audio_type_from_extension(path).is_some() || magic.and_then(file_type_from_magic).is_some()
}

/// Variant for callers which already paid for Ferail's bounded magic sniff.
/// Passing that result avoids opening the file a second time merely to decide
/// whether an extensionless row is audio. The extension remains a fast parser
/// hint, not a correctness requirement.
pub fn read_media_tags_with_magic(
    path: &Path,
    magic: Option<&crate::MagicInfo>,
) -> Option<MediaTags> {
    let tagged = read_tagged(path, false, magic)?;

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

    if tags.is_empty() {
        None
    } else {
        Some(tags)
    }
}

/// Read the embedded cover art for `path`, if any: the front cover when the
/// file labels one, otherwise the first embedded picture. Returns the raw
/// **encoded** image bytes (PNG/JPEG/…) for the host to decode — the format is
/// self-describing, so no separate mime hint is needed.
///
/// This is the expensive read (it pulls the full picture payload), so it is
/// only called on demand for the previewed file, never per row.
pub fn read_cover_art(path: &Path) -> Option<Vec<u8>> {
    let tagged = read_tagged(path, true, None)?;

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

/// Open a supported audio file without giving lofty's deliberately permissive
/// MPEG sync search authority over arbitrary binaries.
///
/// A known extension is the zero-extra-I/O common path: it selects the parser
/// directly. If that parser rejects the bytes (or there is no known
/// extension), Ferail's content detector gets the final say. Its executable,
/// archive and document signatures run before audio, which prevents a random
/// `FF Fx` pair inside a PE file from becoming an invented MP3 duration. This
/// fallback is bounded to the detector's small header window.
fn read_tagged(
    path: &Path,
    read_cover_art: bool,
    known_magic: Option<&crate::MagicInfo>,
) -> Option<TaggedFile> {
    let extension_type = audio_type_from_extension(path);
    if let Some(file_type) = extension_type {
        if let Some(tagged) = read_as(path, file_type, read_cover_art) {
            return Some(tagged);
        }
    }

    let owned_magic;
    let magic = match known_magic {
        Some(info) => info,
        None => {
            owned_magic = crate::detect_magic_info(path)?;
            &owned_magic
        }
    };
    let detected_type = file_type_from_magic(magic).or_else(|| {
        matches!(
            magic.magic_type,
            crate::MagicType::Unknown | crate::MagicType::Binary
        )
        .then(|| strongly_sniff_audio(path))
        .flatten()
    })?;
    if extension_type.is_none()
        && detected_type == FileType::Mpeg
        && !file_has_coherent_mpeg_frames(path)
    {
        return None;
    }
    if extension_type == Some(detected_type) {
        return None;
    }
    read_as(path, detected_type, read_cover_art)
}

/// Extension hint for formats Ferail treats as audio. This deliberately omits
/// video-capable MP4 extensions (`mp4`, `m4v`, `3gp`): those must first be
/// identified as audio-only by content. It is a scheduling/performance hint,
/// never a veto against a renamed file.
fn audio_type_from_extension(path: &Path) -> Option<FileType> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "aac" => Some(FileType::Aac),
        "aiff" | "aif" | "afc" | "aifc" => Some(FileType::Aiff),
        "ape" => Some(FileType::Ape),
        "flac" => Some(FileType::Flac),
        "mp3" | "mp2" | "mp1" => Some(FileType::Mpeg),
        "m4a" | "m4b" => Some(FileType::Mp4),
        "mpc" | "mp+" | "mpp" => Some(FileType::Mpc),
        "opus" => Some(FileType::Opus),
        "ogg" | "oga" => Some(FileType::Vorbis),
        "spx" => Some(FileType::Speex),
        "wav" | "wave" => Some(FileType::Wav),
        "wv" => Some(FileType::WavPack),
        _ => None,
    }
}

/// Last-resort content path for formats outside Ferail's main 4-KiB magic
/// table. The read is capped, signatures must start at a structurally valid
/// location, and MPEG/AAC require three coherent consecutive frames. That is
/// intentionally much stricter than finding one sync word somewhere in a
/// large binary.
fn strongly_sniff_audio(path: &Path) -> Option<FileType> {
    const SNIFF_BYTES: u64 = 64 * 1024;
    let mut bytes = Vec::with_capacity(SNIFF_BYTES as usize);
    std::fs::File::open(path)
        .ok()?
        .take(SNIFF_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;

    let guessed = FileType::from_buffer(&bytes);
    match guessed {
        Some(FileType::Mpeg) if coherent_mpeg_frames(&bytes) => Some(FileType::Mpeg),
        Some(FileType::Aac) if coherent_adts_frames(&bytes) => Some(FileType::Aac),
        Some(FileType::Mpeg | FileType::Aac) => None,
        Some(other) => Some(other),
        None if coherent_mpeg_frames(&bytes) => Some(FileType::Mpeg),
        None if coherent_adts_frames(&bytes) => Some(FileType::Aac),
        None => None,
    }
}

fn file_has_coherent_mpeg_frames(path: &Path) -> bool {
    const FRAME_WINDOW: u64 = 64 * 1024;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut header = [0u8; 10];
    let start = match file.read_exact(&mut header) {
        Ok(()) => id3_payload_end(&header).unwrap_or(0) as u64,
        Err(_) => 0,
    };
    if file.seek(SeekFrom::Start(start)).is_err() {
        return false;
    }
    let mut bytes = Vec::with_capacity(FRAME_WINDOW as usize);
    if file.take(FRAME_WINDOW).read_to_end(&mut bytes).is_err() {
        return false;
    }
    coherent_mpeg_frames(&bytes)
}

fn coherent_mpeg_frames(bytes: &[u8]) -> bool {
    let start = id3_payload_end(bytes).unwrap_or(0);
    let end = bytes.len().saturating_sub(4);
    if start > end || start >= bytes.len() {
        return false;
    }
    (start..=end).any(|offset| {
        let Some((first_len, signature)) = mpeg_frame(&bytes[offset..]) else {
            return false;
        };
        let second = offset.saturating_add(first_len);
        let Some((second_len, second_signature)) = bytes.get(second..).and_then(mpeg_frame) else {
            return false;
        };
        let third = second.saturating_add(second_len);
        let Some((_, third_signature)) = bytes.get(third..).and_then(mpeg_frame) else {
            return false;
        };
        signature == second_signature && signature == third_signature
    })
}

/// `(frame length, version/layer/sample-rate signature)`.
fn mpeg_frame(bytes: &[u8]) -> Option<(usize, u16)> {
    let h = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?);
    if h & 0xffe0_0000 != 0xffe0_0000 {
        return None;
    }
    let version = ((h >> 19) & 0x3) as usize;
    let layer = ((h >> 17) & 0x3) as usize;
    let bitrate_index = ((h >> 12) & 0xf) as usize;
    let sample_index = ((h >> 10) & 0x3) as usize;
    if version == 1 || layer == 0 || bitrate_index == 0 || bitrate_index == 15 || sample_index == 3
    {
        return None;
    }

    const V1_L1: [u16; 16] = [
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448, 0,
    ];
    const V1_L2: [u16; 16] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0,
    ];
    const V1_L3: [u16; 16] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
    ];
    const V2_L1: [u16; 16] = [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256, 0,
    ];
    const V2_L23: [u16; 16] = [
        0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
    ];
    let bitrate_kbps = match (version, layer) {
        (3, 3) => V1_L1[bitrate_index],
        (3, 2) => V1_L2[bitrate_index],
        (3, 1) => V1_L3[bitrate_index],
        (_, 3) => V2_L1[bitrate_index],
        _ => V2_L23[bitrate_index],
    } as usize;
    let base_rate = [44_100usize, 48_000, 32_000][sample_index];
    let sample_rate = match version {
        3 => base_rate,
        2 => base_rate / 2,
        0 => base_rate / 4,
        _ => return None,
    };
    let padding = ((h >> 9) & 1) as usize;
    let frame_len = if layer == 3 {
        (12 * bitrate_kbps * 1000 / sample_rate + padding) * 4
    } else if layer == 1 && version != 3 {
        72 * bitrate_kbps * 1000 / sample_rate + padding
    } else {
        144 * bitrate_kbps * 1000 / sample_rate + padding
    };
    (frame_len >= 4).then_some((
        frame_len,
        ((version << 5 | layer << 3 | sample_index) as u16),
    ))
}

fn coherent_adts_frames(bytes: &[u8]) -> bool {
    let end = bytes.len().saturating_sub(7);
    (0..=end).any(|offset| {
        let Some((first_len, signature)) = adts_frame(&bytes[offset..]) else {
            return false;
        };
        let second = offset.saturating_add(first_len);
        let Some((second_len, second_signature)) = bytes.get(second..).and_then(adts_frame) else {
            return false;
        };
        let third = second.saturating_add(second_len);
        let Some((_, third_signature)) = bytes.get(third..).and_then(adts_frame) else {
            return false;
        };
        signature == second_signature && signature == third_signature
    })
}

fn adts_frame(bytes: &[u8]) -> Option<(usize, u16)> {
    let b = bytes.get(..7)?;
    if b[0] != 0xff || b[1] & 0xf6 != 0xf0 {
        return None;
    }
    let sample_index = (b[2] >> 2) & 0xf;
    if sample_index == 0xf {
        return None;
    }
    let frame_len =
        (((b[3] & 0x03) as usize) << 11) | ((b[4] as usize) << 3) | ((b[5] as usize) >> 5);
    let header_len = if b[1] & 1 == 1 { 7 } else { 9 };
    if frame_len < header_len {
        return None;
    }
    let channels = ((b[2] as u16 & 1) << 2) | ((b[3] as u16 >> 6) & 0x3);
    Some((frame_len, ((sample_index as u16) << 3) | channels))
}

fn id3_payload_end(bytes: &[u8]) -> Option<usize> {
    let header = bytes.get(..10)?;
    if !header.starts_with(b"ID3") || header[6..10].iter().any(|b| b & 0x80 != 0) {
        return None;
    }
    let size = ((header[6] as usize) << 21)
        | ((header[7] as usize) << 14)
        | ((header[8] as usize) << 7)
        | header[9] as usize;
    Some(10usize.saturating_add(size))
}

fn read_as(path: &Path, file_type: FileType, read_cover_art: bool) -> Option<TaggedFile> {
    let probe = Probe::open(path)
        .ok()?
        .options(ParseOptions::new().read_cover_art(read_cover_art))
        .set_file_type(file_type);

    // Ogg is a container shared by Vorbis, Opus and Speex. Ferail's magic
    // result intentionally calls all three `Ogg`, so let lofty distinguish
    // the codec only after our stronger content detector has established that
    // this really is an Ogg stream.
    let probe = if file_type == FileType::Vorbis {
        probe.guess_file_type().ok()?
    } else {
        probe
    };
    probe.read().ok()
}

fn file_type_from_magic(info: &crate::MagicInfo) -> Option<FileType> {
    use crate::MagicType;

    match info.magic_type {
        MagicType::Mp3 => Some(FileType::Mpeg),
        MagicType::Wav => Some(FileType::Wav),
        MagicType::Flac => Some(FileType::Flac),
        MagicType::Ogg => Some(FileType::Vorbis),
        MagicType::Aiff => Some(FileType::Aiff),
        MagicType::M4a => Some(FileType::Mp4),
        _ => None,
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
    fn executable_with_mpeg_sync_words_is_not_audio() {
        // A PE-sized binary will naturally contain MPEG-looking sync words.
        // The old lofty `guess_file_type` path accepted the first one and
        // invented duration/bitrate from the executable's byte length.
        let mut exe = vec![0u8; 4096];
        exe[..2].copy_from_slice(b"MZ");
        for offset in [512, 1024, 1536] {
            exe[offset..offset + 4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        }
        let path = write_temp("false-audio.exe", &exe);
        assert!(read_media_tags(&path).is_none());
        assert!(read_cover_art(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn renamed_audio_uses_content_without_trusting_one_sync_word() {
        let wav = minimal_wav(1, 48_000, 16, 48_000);
        let path = write_temp("renamed.bin", &wav);
        assert_eq!(read_media_tags(&path).map(|t| t.codec), Some("WAV".into()));
        let _ = std::fs::remove_file(&path);

        let mut one_frame = vec![0u8; 2048];
        one_frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        assert!(!coherent_mpeg_frames(&one_frame));
        let path = write_temp("one-frame.bin", &one_frame);
        assert!(read_media_tags(&path).is_none());
        let _ = std::fs::remove_file(&path);

        // MPEG-1 Layer III, 128 kbps, 44.1 kHz: each frame is 417 bytes.
        let mut three_frames = vec![0u8; 417 * 3];
        for offset in [0, 417, 834] {
            three_frames[offset..offset + 4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        }
        assert!(coherent_mpeg_frames(&three_frames));
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
