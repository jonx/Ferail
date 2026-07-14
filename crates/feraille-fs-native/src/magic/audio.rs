//! Audio format parsers: MP3 (with/without ID3 tag), FLAC, WAV, Ogg.
//!
//! All extract channels + sample rate where the bitstream header
//! fits inside the 4 KB read. MP3 with very large ID3 tags can push
//! the first frame past 4 KB — in that case we report only the type,
//! no audio facts.
//!
//! Ported from bfe-explorer's `sniff_audio_info`, `sniff_mp3_info`,
//! `sniff_mp3_frame_info`, `parse_mp3_frame`, `sniff_flac_info`,
//! `sniff_wav_info`, `sniff_ogg_info`.

use super::types::{MagicInfo, MagicType};

pub(super) fn sniff(buf: &[u8]) -> Option<MagicInfo> {
    if buf.len() >= 10 && buf.starts_with(b"ID3") {
        return Some(sniff_mp3_with_id3(buf));
    }

    // Bare MP3 frame — but only if we're sure it's not a UTF-16 BOM.
    if buf.len() >= 4 && !is_utf16_bom(buf) && is_valid_mp3_frame_header(buf) {
        let mut info = MagicInfo::new(MagicType::Mp3);
        parse_mp3_frame(buf, &mut info);
        return Some(info);
    }

    if buf.len() >= 42 && buf.starts_with(b"fLaC") {
        return Some(sniff_flac(buf));
    }

    if buf.len() >= 44 && buf.starts_with(b"RIFF") && &buf[8..12] == b"WAVE" {
        return Some(sniff_wav(buf));
    }

    // AIFF / AIFF-C: the IFF `FORM` container with an `AIFF` (uncompressed)
    // or `AIFC` (compressed) form type. Without this, uncompressed AIFF has
    // no recognizable leading signature and falls through to "Binary" — which
    // then trips the format-mismatch alert against the `.aiff` extension.
    if buf.len() >= 12 && buf.starts_with(b"FORM") && matches!(&buf[8..12], b"AIFF" | b"AIFC") {
        return Some(sniff_aiff(buf));
    }

    if buf.len() >= 35 && buf.starts_with(b"OggS") {
        return Some(sniff_ogg(buf));
    }

    None
}

fn is_utf16_bom(buf: &[u8]) -> bool {
    buf.len() >= 2 && ((buf[0] == 0xff && buf[1] == 0xfe) || (buf[0] == 0xfe && buf[1] == 0xff))
}

/// Validate an MP3 frame header to reject false positives (FF FE is
/// also a UTF-16 LE BOM; FF FB is a valid MP3 frame; we want only the
/// latter).
fn is_valid_mp3_frame_header(buf: &[u8]) -> bool {
    if buf.len() < 4 {
        return false;
    }
    if buf[0] != 0xff || (buf[1] & 0xe0) != 0xe0 {
        return false;
    }
    let version = (buf[1] >> 3) & 0x03;
    if version == 1 {
        return false;
    }
    let layer = (buf[1] >> 1) & 0x03;
    if layer == 0 {
        return false;
    }
    let bitrate_idx = (buf[2] >> 4) & 0x0f;
    if bitrate_idx == 0x0f || bitrate_idx == 0 {
        return false;
    }
    let sample_rate_idx = (buf[2] >> 2) & 0x03;
    if sample_rate_idx == 0x03 {
        return false;
    }
    let emphasis = buf[3] & 0x03;
    if emphasis == 2 {
        return false;
    }
    true
}

/// ID3v2: "ID3" + ver(2) + flags(1) + syncsafe-size(4). The first MP3
/// frame starts at offset 10 + tag_size.
fn sniff_mp3_with_id3(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::Mp3);
    let id3_size = ((buf[6] as usize & 0x7f) << 21)
        | ((buf[7] as usize & 0x7f) << 14)
        | ((buf[8] as usize & 0x7f) << 7)
        | (buf[9] as usize & 0x7f);
    let frame_start = 10 + id3_size;
    if frame_start + 4 <= buf.len() {
        parse_mp3_frame(&buf[frame_start..], &mut info);
    }
    info
}

fn parse_mp3_frame(buf: &[u8], info: &mut MagicInfo) {
    if buf.len() < 4 || buf[0] != 0xff || (buf[1] & 0xe0) != 0xe0 {
        return;
    }
    let version_bits = (buf[1] >> 3) & 0x03;
    let layer_bits = (buf[1] >> 1) & 0x03;
    let bitrate_idx = (buf[2] >> 4) & 0x0f;
    let sample_rate_idx = (buf[2] >> 2) & 0x03;
    let channel_mode = (buf[3] >> 6) & 0x03;

    let sample_rates: [u32; 4] = match version_bits {
        0 => [11025, 12000, 8000, 0],
        2 => [22050, 24000, 16000, 0],
        3 => [44100, 48000, 32000, 0],
        _ => return,
    };
    if (sample_rate_idx as usize) < 3 {
        info.sample_rate = Some(sample_rates[sample_rate_idx as usize]);
    }

    // Bitrate tables for Layer III (most common). Layer I/II have
    // their own tables — skipped because Layer III dominates real
    // files.
    if version_bits == 3 && layer_bits == 1 && bitrate_idx > 0 && bitrate_idx < 15 {
        const RATES_V1_L3: [u16; 15] = [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ];
        info.bitrate_kbps = Some(RATES_V1_L3[bitrate_idx as usize]);
    } else if (version_bits == 0 || version_bits == 2)
        && layer_bits == 1
        && bitrate_idx > 0
        && bitrate_idx < 15
    {
        const RATES_V2_L3: [u16; 15] =
            [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
        info.bitrate_kbps = Some(RATES_V2_L3[bitrate_idx as usize]);
    }

    info.channels = Some(if channel_mode == 3 { 1 } else { 2 });
}

/// FLAC STREAMINFO block (mandatory, always first):
/// - byte 4: last-block(1) + type(7), type 0 = STREAMINFO
/// - bytes 18-20: 20-bit sample rate (big-endian, top 20 of 24)
/// - byte 20 bits 3-1: channels-1
/// - bytes 21-25: total samples (36 bits across)
fn sniff_flac(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::Flac);
    if (buf[4] & 0x7f) != 0 {
        return info;
    }
    let sr_bytes = &buf[18..21];
    let sample_rate = ((sr_bytes[0] as u32) << 12)
        | ((sr_bytes[1] as u32) << 4)
        | ((sr_bytes[2] as u32) >> 4);
    info.sample_rate = Some(sample_rate);
    info.channels = Some(((sr_bytes[2] >> 1) & 0x07) + 1);

    let total_hi = (buf[21] & 0x0f) as u64;
    let total_lo = u32::from_be_bytes([buf[22], buf[23], buf[24], buf[25]]) as u64;
    let total = (total_hi << 32) | total_lo;
    if sample_rate > 0 && total > 0 {
        info.duration_secs = Some((total / sample_rate as u64) as u32);
    }
    info
}

/// WAV: walk RIFF chunks looking for `fmt ` (format) and `data`
/// (for duration estimation, assuming 16-bit samples).
fn sniff_wav(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::Wav);
    let mut pos = 12;
    while pos + 8 < buf.len() {
        let id = &buf[pos..pos + 4];
        let size =
            u32::from_le_bytes([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]]) as usize;
        if id == b"fmt " && pos + 8 + 16 <= buf.len() {
            let d = &buf[pos + 8..];
            info.channels = Some(u16::from_le_bytes([d[2], d[3]]) as u8);
            info.sample_rate = Some(u32::from_le_bytes([d[4], d[5], d[6], d[7]]));
        } else if id == b"data" {
            if let (Some(sr), Some(ch)) = (info.sample_rate, info.channels) {
                let bytes_per_sample = 2u32 * ch as u32;
                if bytes_per_sample > 0 && sr > 0 {
                    info.duration_secs = Some((size as u32) / (sr * bytes_per_sample));
                }
            }
            break;
        }
        let advance = 8usize.saturating_add(size).saturating_add(size & 1);
        if advance == 0 {
            break;
        }
        pos = pos.saturating_add(advance);
    }
    info
}

/// AIFF: walk the big-endian IFF chunks for `COMM`, which carries channel
/// count, sample-frame count, and the sample rate as an 80-bit IEEE-754
/// extended float. Duration is `numSampleFrames / sampleRate`.
fn sniff_aiff(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::Aiff);
    let mut pos = 12;
    while pos + 8 <= buf.len() {
        let id = &buf[pos..pos + 4];
        let size =
            u32::from_be_bytes([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]]) as usize;
        // COMM: numChannels(i16) frames(u32) sampleSize(i16) sampleRate(f80)
        if id == b"COMM" && pos + 8 + 18 <= buf.len() {
            let d = &buf[pos + 8..];
            info.channels = Some(u16::from_be_bytes([d[0], d[1]]) as u8);
            let frames = u32::from_be_bytes([d[2], d[3], d[4], d[5]]);
            let sr = extended_f80_to_u32(&d[8..18]);
            if sr > 0 {
                info.sample_rate = Some(sr);
                info.duration_secs = Some(frames / sr);
            }
            break;
        }
        // Chunks are padded to an even byte count.
        let advance = 8usize.saturating_add(size).saturating_add(size & 1);
        if advance <= 8 {
            break;
        }
        pos = pos.saturating_add(advance);
    }
    info
}

/// Decode an 80-bit IEEE-754 extended-precision float (big-endian, as AIFF
/// stores the sample rate) to a rounded `u32`. Sample rates are always small
/// positive normals, so the sign and the subnormal/NaN cases collapse to 0.
fn extended_f80_to_u32(b: &[u8]) -> u32 {
    if b.len() < 10 {
        return 0;
    }
    let exponent = (((b[0] & 0x7f) as u32) << 8) | b[1] as u32; // 15-bit, bias 16383
    let mantissa = u64::from_be_bytes([b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9]]);
    if exponent == 0 || exponent == 0x7fff {
        return 0; // zero / subnormal / inf / NaN — not a real rate
    }
    // value = mantissa * 2^(exponent - 16383 - 63); the mantissa's top bit is
    // the explicit integer part of the extended format.
    let value = mantissa as f64 * 2f64.powi(exponent as i32 - 16383 - 63);
    if value.is_finite() && value > 0.0 {
        value.round() as u32
    } else {
        0
    }
}

/// Ogg page header followed by Vorbis identification packet:
/// packet_type(1)=1 + "vorbis"(6) + version(4) + channels(1)
/// + sample_rate(4 LE) + ...
fn sniff_ogg(buf: &[u8]) -> MagicInfo {
    let mut info = MagicInfo::new(MagicType::Ogg);
    let segments = buf[26] as usize;
    let data_start = 27 + segments;
    if data_start + 30 > buf.len() {
        return info;
    }
    if &buf[data_start + 1..data_start + 7] != b"vorbis" {
        return info;
    }
    let v = &buf[data_start..];
    if v[0] != 1 {
        return info;
    }
    info.channels = Some(v[11]);
    info.sample_rate = Some(u32::from_le_bytes([v[12], v[13], v[14], v[15]]));
    info
}
