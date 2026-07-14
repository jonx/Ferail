# Media Tags

Embedded audio metadata — ID3v1/v2, MP4 atoms, Vorbis comments, APE — plus
decoded audio properties and cover art, read once off the UI thread and surfaced
in Get Info, the Description column, and the preview pane. One parser
([`lofty`](https://crates.io/crates/lofty)) covers every standard container, so
"music or other media files that support the standard" is handled without a
per-format grab-bag of crates.

## Status

**Shipped (2026-07-14):** reader + core model, Get Info **Media** section,
cross-platform cover art in the preview/grid, the rich audio Description line,
and **in-viewer playback** — audio files open in the viewer with their cover on
the stage, a play/pause + mute + loop + seek transport, and unmuted autoplay.

**Deferred (tracked in [TODO.md](../../TODO.md)):** a SoundCloud-style
**waveform** preview adapted to the app's look.

## Why lofty

`lofty` is pure Rust (MIT/Apache-2.0 — clean for the dual-licensed release), reads
tags *and* audio properties (duration, bitrate, sample rate, channels, bit depth)
*and* embedded pictures through one `read_from_path`, across MP3, the ID3 chunks
in WAV/AIFF, MP4/M4A/ALAC, FLAC/OGG/Opus/Speex, APE, WavPack, and Musepack. The
runner-up `id3` crate is ID3-only; `symphonia` is a full decoder framework — kept
in mind for the deferred waveform (lofty does not decode samples), overkill for
tags. lofty's MSRV (1.85) is what set the workspace `rust-version`.

## Architecture

The dependency and the parsing live behind the existing crate boundaries; no new
crate, no new cache, no new worker.

- **`feraille-core::media`** — `MediaTags`, a platform- and UI-free data record
  (codec, title/artist/album/genre, track/disc, year, comment, duration, bitrate,
  sample rate, channels, bit depth) plus pure formatting helpers
  (`description()`, `duration_label()`, `sample_rate_label()`, …). Zero deps, unit
  tested. Cover-art *bytes* are deliberately **not** a field — see below.
- **`feraille-fs-native::media`** — the one place `lofty` is used. Two entry
  points split by cost:
  - `read_media_tags(path)` — tags + audio properties, `read_cover_art(false)`
    so it never pulls a multi-megabyte picture into memory. Safe to call per
    file (Get Info) or per row (prefetch). `guess_file_type()` falls back to
    content sniffing so a mis-named track still reads. Returns `None` for
    non-audio.
  - `read_cover_art(path)` — the expensive read, on demand for the previewed
    file only. Prefers `PictureType::CoverFront`, else the first picture;
    returns the raw encoded (PNG/JPEG) bytes for the host to decode.

### Why cover bytes don't live in `MediaTags`

The file list clones row records freely; a struct that carried an APIC payload
would drag megabytes through every clone. Cover art instead rides its own
channel straight into the host's image cache. Cover *presence* isn't carried
either: `lofty` can only report it by reading the picture bytes (the cost we're
avoiding), so the preview simply attempts the read and shows whatever comes back.

## Surfaces

### Get Info — "Media" section

`entry_info::gather` (already on the background executor) calls `read_media_tags`
for files and appends a **Media** `InfoSection` — Title, Artist, Album, Genre,
Year, Track ("3 of 11"), Disc ("1 of 2"), Duration, Format, Channels, Sample
rate, Bit depth, Bit rate. Non-audio files yield `None`, so the section's rows
are empty and the existing `filter(!rows.is_empty())` drops it. No new render
code — the neutral `InfoValue::Text` rows paint through the same path as every
other Get Info row.

### Preview / grid — cover art

The preview and thumbnail warms already funnel through one choke point,
`video_poster::fetch_content_thumbnail` (Quick Look → poster). A cover-art step
was inserted there: after Quick Look comes up empty (or on a platform without
it), an audio file's embedded picture is read with `lofty`, decoded, and shrunk
through the **same** `PreviewCache` / BGRA `RenderImage` path everything else
uses. macOS Quick Look already extracts audio cover art, so this step only fires
where it must — **Windows/Linux/AROS** — giving album art in the preview pane
and the icon grid on every platform, and the still-image stage in the viewer
draws it for free.

### File list — Description column

`prefetch::run_worker` replaces the generic magic description ("MPEG audio,
layer III") with the rich media line — `"MP3 · stereo · 44.1 kHz · 192 kbps ·
03:24"` — for audio files, only on a fresh derive (the value persists to the
same `description` field in the metadata DB, so a revisit never re-reads tags).
Mirrors the magic-description contract exactly; see
[MAGIC_DESCRIPTION.md](MAGIC_DESCRIPTION.md).

## Key decisions

- **Reuse over invention.** Every surface rides an existing seam (Get Info
  `InfoSection`, the preview cache choke point, the prefetch worker). The only
  new code is the reader and the pure model. This was a standing instruction:
  integrate into what's there, diverge only with a reason.
- **Cost-split reader.** The per-row / per-file path skips cover bytes; the
  full picture read is on-demand for one file. Keeps the Prime Directive
  (no heavy work near paint) intact.
- **Honest formatting, no format-family branching.** The reader stores whatever
  `lofty` reports; `description()` drops empty segments. A PCM WAV that reports a
  1411 kbps bitrate shows it; a lossy MP3 with no bit depth simply omits that
  segment. (Verified: `lofty` computes a bitrate even for uncompressed WAV.)
- **No new icon.** Cover art shows the real album picture; rows use the existing
  `file/audio.svg`; the deferred viewer transport reuses the existing
  play/pause/volume/loop glyphs. Adding a `disc`/`disc-3` glyph would have been
  an unused icon, against the ~1:1 rule in [ICONS.md](ICONS.md).

## In-viewer audio playback

Playback reuses the viewer's `VideoBackend`/`VideoStream` seam — it was never a
new engine. The native macOS `AVPlayer` decodes an audio-only URL (the video
path had been calling `set_muted` on it all along), and libmpv plays audio
natively. What was missing was routing + the stage surface:

- **Routing.** `is_audio_path` (native [`AUDIO_EXTS`] always, plus
  [`MPV_AUDIO_EXTS`] when the mpv backend is selected) sits beside
  `is_video_path`. `sync_video` opens a stream for either. `current_is_playable`
  (`video || audio`) gates the *playback* machinery — stream open/teardown, the
  transport, self-driven slideshow advance — while `current_is_video` stays the
  gate for *frame rendering*. That split is the whole design: audio is a stream
  with no pixels.
- **Stage.** `copy_frame` returns `None` for audio, so `video_frame_image` stays
  empty and the stage falls through to the still path — which draws the cover
  from the preview cache (the same cover the grid/preview show). No frame
  surface, no "undecodable" branding (that check now excludes audio).
- **Transport.** Play/pause, mute, loop, and the seek bar show for any playable
  stream; the frame-step `−1f/+1f` buttons stay video-only. The position poll
  advances the seek bar off `stream.time()` whether or not frames arrive.
- **Autoplay unmuted.** Opening an audio file unmutes the window (`set_muted`
  applied at open); video stays muted-by-default so stacked viewers don't all
  blare. The mute toggle drives the real backend: `AVPlayer setMuted:` on macOS,
  `IMFMediaEngine::SetMuted` on Windows, and libmpv's `mute` property — no
  longer a no-op on the native players.

### Backend coverage

Because routing goes through the *active* backend, an audio file plays on
whichever backend the user runs — mirroring how video already picks native vs.
mpv:

| Backend | Plays | Notes |
| --- | --- | --- |
| Native macOS (AVFoundation) | MP3, AAC/M4A, ALAC, AIFF, WAV, CAF, FLAC | No WMA; no Vorbis/Opus/APE. |
| Native Windows (Media Foundation) | MP3, AAC/M4A, WAV, WMA, FLAC | WMA is native here. |
| Native Linux | — | The shell's video overlay is a stub; needs mpv. |
| mpv (all OSes) | everything, incl. WMA/Vorbis/Opus/APE/… | The uniform option; libmpv must be installed + selected. |

So [`MPV_AUDIO_EXTS`] (ogg/opus/wma/ape/…) only route to the viewer when mpv is
active; without it they stay a static cover/poster, exactly like the
mpv-only video containers.

### Verification

Verified structurally in the headless harness (stream opens, duration +
position advance, cover + transport render, unmuted). Audible output and the
mute toggle's *effect* need a real run — the `setMuted:`/`SetMuted` calls are
wired through the same registry as the working `set_paused`, but silence can't
be observed in a screenshot.

## Deferred: waveform preview

A SoundCloud-style waveform, styled to the app's look (theme tokens, house
stroke). Decode peak buckets off-thread with `symphonia` (lofty doesn't decode
samples), cache the peaks like previews are cached, paint bars in the stage.
Rides the same preview-cache/staleness machinery when it lands. Tracked under
"Preview-pane providers" in [TODO.md](../../TODO.md).

## Tests

- `feraille-core::media` — duration / kHz / track-of / channel formatting and the
  `description()` composition (lossy, lossless, empty-segment cases).
- `feraille-fs-native::media` — real round-trips against a hand-built minimal
  WAV: audio properties, non-audio → `None`, missing file → `None`, and a tagged
  WAV whose two embedded pictures verify front-cover preference in
  `read_cover_art`.
