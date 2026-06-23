# mpv Video Backend, VLC Retirement & Color-Key Transparency

Plan for replacing the VLC video provider with an **mpv (libmpv)** backend that
plays the same broad container set *and* applies enhancement filters **live**
(no stream re-open), retiring VLC once mpv is proven, and a follow-on
**color-key transparency** feature that the new alpha-aware frame path makes
nearly free.

← [Feature notes index](README.md) · [Architecture](../ARCHITECTURE.md) ·
[TODO](../../TODO.md) · companion: [VIEWER.md](VIEWER.md)

## Status

**Planned (2026-06-23)** — not started. Implementation begins on **macOS**.
This is an Add-feature plan under the Slow AI method; the four gates below were
approved before any code. The decision rationale (mpv vs VLC vs raw FFmpeg) is
logged in the dated NOTES.md entry.

## Platform tagging convention

Same as VIEWER.md: **[mac]** = macOS-only today, **[win-parity]** = the named
Windows equivalent for later. Untagged design is platform-neutral.

## Why mpv

The viewer's video provider is a pluggable seam
([`feraille_core::video::VideoBackend`](../../crates/feraille-core/src/video.rs)):
the viewer opens a stream and pulls tightly-packed BGRA frames each tick,
drawing them as a gpui `img` through the shared still-image stage path. Today
two providers exist — the platform-native player (AVFoundation **[mac]**, Media
Foundation **[win-parity]**) and an optional libvlc backend.

The motivating problem: **VLC can't change its video-filter chain at runtime.**
libvlc takes denoise/sharpen/deband/grain only as *instance* arguments to
`libvlc_new`, so the Adjustments popup re-opens the whole stream on every slider
release — hence the seamless-reopen / deferred-seek / kept-frame / re-pause
machinery in `viewer/window.rs`. Only the colour grade is live
(`libvlc_video_set_adjust_*`).

The options considered:

| | Player for free | Filters live |
| --- | --- | --- |
| **VLC** (today) | yes | no — re-open |
| **raw FFmpeg** (libav*) | no — we'd build demux/decode/audio/sync/clock/seek | yes |
| **libmpv** | yes | yes |

mpv is FFmpeg-with-a-player-attached: it owns demux, hardware decode, audio,
A/V sync, the clock, seek, and step, **and** exposes the libavfilter graph at
runtime (`vf set`/`vf command`). The same `hqdn3d`/`unsharp`/`gradfun`/`noise`
filters VLC wraps, but **live**. It loads at runtime via `dlopen` exactly like
libvlc (stock build links nothing), and its **software render API** writes
frames straight into a caller buffer — a near-perfect fit for the existing
BGRA pull seam. So mpv is a functional superset of the VLC backend for this
app's use, with no capability gap for local-file playback.

## Verified API surface (checked against current sources, not memory)

libmpv software render (`include/mpv/render.h`):

- `MPV_RENDER_API_TYPE_SW` (`"sw"`).
- `mpv_render_context_render(ctx, params)` with `MPV_RENDER_PARAM_SW_SIZE`
  (`int[2]` w,h), `MPV_RENDER_PARAM_SW_FORMAT` (`char*`),
  `MPV_RENDER_PARAM_SW_STRIDE` (`size_t*`, bytes/line),
  `MPV_RENDER_PARAM_SW_POINTER` (`void*`, first pixel). Accepted formats:
  `rgb0`, `bgr0`, `0bgr`, `0rgb` (4 bytes/px), `rgb24` (slow). **Renders
  directly into our buffer.**
- `mpv_render_context_update()` → bitflags; `MPV_RENDER_UPDATE_FRAME` means a
  new frame is ready — the natural "return `None` between frames" signal.
- `mpv_render_context_create` / `_free`.

Live filters & grade:

- `vf set <chain>` "overwrite the previous filter chain … takes effect
  immediately at runtime"; `vf command` sends a runtime param to an inserted
  filter (the `avfilter_graph_send_command` equivalent). lavfi bridge gives the
  libavfilter filter set. **No file reload.**
- Colour grade: mpv's equalizer (`brightness`/`contrast`/`saturation`/`gamma`/
  `hue`) properties, set live. *Open uncertainty (see Trade-offs): confirm
  these apply through the SW render path; fall back to lavfi `eq`/`hue` in the
  live `vf` chain if not.*

macOS dylib: Homebrew installs `libmpv.2.dylib` at `/opt/homebrew/lib`
(Apple Silicon) or `/usr/local/lib` (Intel); user-pointable in
Settings → Plugins, same as VLC. mpv needs **no external plugin dir**, so the
VLC `VLC_PLUGIN_PATH` dance is not needed.

**Verified gotcha (from our own code).**
[`build_video_frame`](../../crates/feraille-gpui/src/viewer/window.rs) takes the
4th byte of each pixel as alpha verbatim (no force-opaque). mpv's `bgr0` writes
`0` there → fully transparent. So the backend **must fill the alpha byte to
`0xFF`** before returning a frame. This single per-pixel pass is also exactly
where the color-key feature lives (below).

## Seam mapping (mpv → `VideoBackend` / `VideoStream`)

| Trait method | mpv implementation |
| --- | --- |
| `open(path, on_ended, enhance)` | `mpv_create` → pre-init options (`vo=libmpv`, `hwdec=auto`, quiet/no-OSC, initial `vf` lavfi chain from `enhance`) → `mpv_initialize` → create SW render context → `loadfile` (autoplay). |
| `copy_frame()` | Pump events non-blocking (`MPV_EVENT_END_FILE`/EOF → `on_ended`). If `dwidth`/`dheight` known and `mpv_render_context_update()` has `UPDATE_FRAME`: render into our `w*h*4` buffer (`SW_FORMAT=bgr0`), **fill alpha `0xFF`**, return `(w,h,BGRA)`. Else `None`. |
| `set_paused(p)` | set `pause` property. |
| `seek(s)` | `seek <s> absolute` command. |
| `step(n)` | `frame-step` / `frame-back-step` commands; implies pause. |
| `time()` | `time-pos` / `duration` properties. |
| `natural_size()` | `dwidth` / `dheight` properties. |
| `set_adjust(a)` | equalizer properties (live); returns `true` (viewer skips CPU grade). |
| **`set_enhance(e)`** *(new)* | live `vf set` of the lavfi chain; returns `true` — **no re-open**. |

### The one seam change

Add to `feraille_core::video::VideoStream`:

```rust
/// Apply enhancement filters to an *already-open* stream. Returns `true` if
/// the backend changed them live; `false` (the default) means it can't, and
/// the viewer must re-open the stream to apply new filters.
fn set_enhance(&mut self, _enhance: VideoEnhance) -> bool { false }
```

Additive and defaulted, so VLC, the native player, and the MF backend compile
unchanged (they inherit `false` → existing re-open path). Only mpv overrides it.
`commit_video_enhance` then tries `stream.set_enhance(new)` first and only falls
back to the re-open dance when it returns `false`.

## Gate 2 — Change-set

**New crate** (mirrors `feraille-video-vlc` exactly: runtime `dlopen`,
hand-written FFI, **no** `libmpv`/`mpv` Rust crate — matching the deliberate
no-`vlc-rs` decision):

- `crates/feraille-video-mpv/Cargo.toml`
- `crates/feraille-video-mpv/src/lib.rs` — `backend(mpv_path) -> Option<Box<dyn VideoBackend>>`, cfg-gated per OS like the VLC crate; macOS `imp` for v1.
- `crates/feraille-video-mpv/src/imp.rs` — FFI loader, `MpvBackend`, `MpvStream`, render-context lifecycle, the alpha-fill pass, integration test (gated on libmpv + a probe clip, like the VLC test).

**Edited:**

- `crates/feraille-core/src/video.rs` — add `VideoStream::set_enhance` (above).
- `crates/feraille-gpui/Cargo.toml` — optional `feraille-video-mpv` dep + `mpv` feature (mirrors `vlc`).
- `crates/feraille-gpui/src/viewer/backend_native.rs` — `video_backend(...)` becomes provider-aware (`"vlc"` / `"mpv"` / native); add `default_mpv_path()`.
- `crates/feraille-gpui/src/viewer/window.rs` — generalize `vlc_pref` / `resolve_vlc_pref` to (provider, path); extend `is_video_path`'s broad-container set to also fire for mpv; `commit_video_enhance` prefers `set_enhance` (mpv → live), re-open only on `false` (VLC).
- `crates/feraille-gpui/src/settings.rs` — Plugins page: add `"mpv"` radio option + an mpv path field; cfg-gate on the `mpv` feature.
- `crates/feraille-gpui/src/app_state.rs` — accept `"mpv"` for `video_backend`; add an `mpv_path` field (kept separate from `vlc_app_path` for clarity; small state surface, no framework).
- `Cargo.toml` (workspace) — add the new crate to `members`.
- Docs as each iteration lands: this file, `VIEWER.md`, `TODO.md`, `NOTES.md`. No new icons → `ICONS.md` untouched.

## Gate 3 — Data flow

On `open`, the backend creates an mpv handle, sets pre-init options
(`vo=libmpv`, `hwdec=auto`, quiet/no-OSC, an initial `vf` lavfi chain from
`VideoEnhance`), calls `mpv_initialize`, creates a **software render context**,
and `loadfile`s the path (autoplay). mpv owns demux, decode, audio, A/V sync,
the clock, and seek — we build none of it. Each ~60 Hz viewer tick (the existing
`start_video_poll`), `copy_frame` pumps mpv events non-blocking
(`END_FILE`→`on_ended`); if `dwidth`/`dheight` are known and
`mpv_render_context_update()` reports `UPDATE_FRAME`, it renders into our own
`w*h*4` buffer (`SW_FORMAT=bgr0`, `SW_STRIDE`, `SW_POINTER`), fills the alpha
byte to `0xFF`, and returns `(w,h,BGRA)` — the exact contract
`build_video_frame` already consumes, so the frame draws as a gpui `img` through
the shared stage (zoom/pan/fit/rotate for free). `set_paused`/`seek`/`step` map
to the `pause` property and `seek`/`frame-step` commands. `set_adjust` maps the
grade to live equalizer properties. `set_enhance` maps denoise/sharpen/deband/
grain to a live `vf set` lavfi chain — no re-open. Every method runs on the main
thread (the player registry is main-thread-only), same as the native/VLC
backends; the per-frame copy is in-memory (prime-directive clean — no I/O,
Finder, or SQLite).

## Gate 4 — Trade-offs & uncertainties

1. **Equalizer under SW render (the one real uncertainty).** Confirm on the mac
   that the `brightness`/`contrast`/`saturation`/`gamma`/`hue` properties apply
   through the *software* render path (set property → screenshot → compare).
   **Fallback:** drive the grade via lavfi `eq`/`hue` in the same live `vf`
   chain — guaranteed, since `vf` works under SW. Low risk either way.
2. **Alpha fill (verified, handled).** One extra pass over the frame to set
   `0xFF`. SW render writes straight into our buffer, so this re-touches it;
   acceptable. Probe whether a real-alpha internal format name avoids it.
3. **Licensing/distribution.** libmpv is GPLv2+/LGPL (FFmpeg LGPL/GPL) — same
   bracket as VLC. We runtime-`dlopen` a user-pointed/Homebrew lib (no
   build-time link), so a stock build bundles nothing — identical posture to the
   VLC backend. Redistributable bundling is a later phase, exactly like VLC's.
4. **Main-thread frame copy at 4K60.** Same cost and same deferred
   `CVDisplayLink` background-pull follow-up as the VLC/native backends — no new
   ground.

**Defaults deliberately not introduced** (per the method): no heavyweight
`libmpv`/`mpv` Rust crate (hand-written FFI, like the VLC crate); no new
architecture layers (the crate mirrors the existing VLC one).

## Iterations

Each ends green (`cargo check` + `cargo test`), with a NOTES.md entry; UI
iterations add a screenshot under `screenshots/`.

1. **Crate skeleton + playback + frame pull.** FFI loader, `open` / `copy_frame`
   (incl. alpha fill) / `time` / `natural_size` / `Drop`. Integration test gated
   on libmpv + a probe clip. Green when it opens a clip and pulls a frame.
2. **Transport.** `set_paused` / `seek` / `step`; `END_FILE` → `on_ended`.
   Wire into the viewer's existing transport + seek bar.
3. **Live colour grade.** `set_adjust` via equalizer (verify under SW; lavfi
   `eq`/`hue` fallback). Screenshot a graded frame.
4. **Live enhancement filters.** `set_enhance` + live `vf` lavfi chain; wire
   `commit_video_enhance` to prefer live (deleting the re-open *path usage* for
   mpv while the VLC fallback stays); Settings → Plugins `"mpv"` option + path;
   extend `is_video_path`; refresh `VIEWER.md` / `TODO.md`.
5. **Retire VLC** *(gated on 1–4 green and verified on the mac).* Remove the
   `feraille-video-vlc` crate and `vlc` feature; drop the Plugins "VLC" option;
   migrate a saved `video_backend=vlc` pref → `mpv` (or builtin); **delete the
   seamless-reopen / `video_pending_seek` / `video_repause` / kept-frame
   machinery** that existed only for VLC's no-live-filter limitation. Its own
   clean change-set — the simplification payoff of the move.

## Color-key transparency (follow-on feature)

The alpha-fill pass introduced above makes a chroma/color-key feature nearly
free: instead of blindly writing alpha `0xFF`, decide per pixel —

> if the pixel is within *tolerance* of the user's key color → alpha `0`
> (transparent), else `0xFF`.

Same single O(pixels) pass we already run. It operates on the pulled BGRA
buffer, so it's **backend-agnostic** (mpv, VLC while it lasts, and the native
player). gpui already alpha-composites the `img`, so keyed pixels reveal
whatever is behind the video element. Prime-directive clean (in-memory); the
per-frame cost matches the existing grade, with the same 4K60 caveat.

**v1 scope:** an RGB-distance key with a tolerance slider in the Adjustments
popup — matches "specify the transparent color" (a solid plate / GIF-style key).
A "pick color from frame" eyedropper is a natural addition.

**The fork that changes scope — what shows through (needs a product decision
before this feature starts):**

- **In-app backdrop** — keyed pixels reveal the viewer's own canvas, or a
  color/image the user sets behind the video. *Cheap; ships with the key pass.*
- **Desktop / other windows** (true greenscreen floating on the desktop) —
  requires a **transparent GPUI window** (`NSWindow` `isOpaque=false` + clear
  background **[mac]**) and viewer-chrome rework. Roughly triples the work; the
  "wow" version.

**Implementation note.** Do the key as our **CPU pass**, not mpv's lavfi
`colorkey`/`chromakey` filter. The filter is "more correct" (soft edges, GPU)
but depends on mpv preserving alpha through the SW render output — the same
unverified question as Trade-off #1 — so the CPU pass is the robust v1, with the
`vf` filter as a later upgrade. True greenscreen **YUV chroma-key** (keys on
hue, ignores luma/lighting) is the fancier follow-up to the RGB-distance v1.

This feature gets its own four gates when it starts; the above is the scoping
sketch, not the approved plan.
