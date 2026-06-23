# mpv Video Backend & Layered Chroma-Key Compositing

Plan for replacing the libvlc video backend with **libmpv**, and building a
**multi-layer transparent-color (chroma-key) compositor** on top of it: the
user picks a transparent colour on a video and sees the layer(s) beneath show
through — potentially other videos that are themselves keyed.

← [Feature notes index](README.md) · [Viewer](VIEWER.md) ·
[Architecture](../ARCHITECTURE.md) · [TODO](../../TODO.md)

## Status

**Phase 3 done — compile-green (2026-06-23)** — Phases 0, 1a, 1b, 2 (see
below) plus Phase 3: single-layer chroma key. A "Transparent colour" section
in the adjustments popup (mpv video only) with an on/off toggle, an
**eyedropper swatch** (click it, then click the video to sample the key
colour from the live frame), and **Similarity** (range width) + **Blend**
(edge feather) sliders. The key is pushed live via `set_chroma_key`, so keyed
pixels arrive transparent and the stage background shows through. No new icon
(the swatch is the eyedropper affordance), so `ICONS.md` is untouched. `cargo
check -p feraille-gpui --features vlc` is green.

> **Verified by screenshot + UI reworked.** The adjustments popup renders
> correctly with the full mpv-video control set (a new screenshot-only
> `--viewer-adjust-video` fixture forces the panel without a live stream, since
> the real frame-pull poll never lets a headless render settle). The popup was
> then **reworked to look more professional**: labeled **Colour** / **Enhance**
> / **Transparent colour** sections (replacing bare separators), and the key
> picker now shows a swatch + **hex readout** + a clear **Pick** button (arms
> the eyedropper) instead of a tiny "click swatch" hint. Still pending: a
> screenshot of a *live keyed video* (the keyed pixels actually transparent),
> which needs the poll to run — a follow-up.

Still open: the N-layer stack (Phase 4), docs/icons (Phase 5), and the cosmetic
rename below. Phase 2 recap: removed the VLC-era seamless-reopen apparatus —
`commit_video_enhance` now pushes filters through live `set_enhance`; the
`video_pending_seek`/`video_repause` deferral and the poll's pre-seek-frame
dance are gone (−67 lines in `window.rs`). The decision log is in
[NOTES.md](../../NOTES.md) (2026-06-23 entry).

### Deferred cosmetic rename (pinned by hot `settings.rs`)

The optional provider **is** mpv now (the libvlc crate is gone), but three
user-facing identifiers still read `vlc` because Settings → Plugins
(`settings.rs`) and the persisted [`app_state`] fields pin them, and that file
was under concurrent edit when this landed:

- the cargo feature `vlc` (→ `mpv`),
- the persisted setting `video_backend == "vlc"` and field `vlc_app_path`
  (→ `"mpv"` / `mpv_path`),
- the Settings → Plugins dropdown label "VLC" (→ "mpv").

These are a single mechanical rename to do once `settings.rs`/`app_state.rs`
are free; the implementation behind them is already mpv.

### Phase 0 findings (verified, not assumed)

- **SW render → BGRA pull works**, same shape as libvlc's vmem, so the
  `copy_frame → (w,h,BGRA)` seam is untouched.
- **THE GATE — SW render emits a real alpha channel. PASS.** A live `colorkey`
  filter produced correct per-pixel alpha through SW render (keyed background
  transparent, foreground opaque — `screenshots/mpv-probe-B_alpha_green.png`).
  **Keying lives in mpv's filter chain**, live and off our threads — *not* a
  CPU pass. Recipe: end the vf chain in an alpha format + request `bgra`:
  `vf = lavfi=[…,format=rgba,colorkey=color=0xRRGGBB:similarity=…:blend=…]`.
- **Live `vf` change applies with no re-open** → the VLC reopen apparatus goes.
- **Correction:** mpv's `brightness`/`contrast`/… *equalizer properties* are a
  no-op on the SW-render output (they live in the GPU VO shaders). So colour
  grade routes through a lavfi `eq`/`colorlevels` filter in the same live vf
  chain — **not** `set_adjust`→equalizer-properties. Grade + enhance + key
  unify into one live chain. (`--alpha` option doesn't exist in this build and
  isn't needed.)

Two scope decisions taken up front (user, 2026-06-23):

- **Replace VLC outright.** `feraille-video-vlc` is removed, not kept as a
  fallback — sequenced so mpv reaches frame-pull/seek/grade parity and passes
  its integration test *first*, with VLC deleted in the **same phase**. The
  viewer is never left without a working video path between phases.
- **N-layer stack.** The compositor supports an arbitrary stack of keyed
  layers from the start (not a fixed two). The *data model* is N from day one;
  the *performant ceiling* of the CPU-buffer-pull path is a handful of layers
  at ≤1080p — see [Performance](#performance-the-honest-ceiling).

## Platform tagging convention

Same as [VIEWER.md](VIEWER.md#platform-tagging-convention): **[mac]** =
macOS-only today; **[win-parity]** = the named Windows equivalent for later.
libmpv is itself cross-platform, so most of this is platform-neutral; only the
dylib-discovery and hardware-decode knobs are tagged.

## Why mpv (the motivation, tied to the VLC code we have)

The libvlc backend works, but three properties of libvlc shaped real
complexity that libmpv removes:

1. **libvlc can't change a video filter live.** The whole seamless-reopen
   apparatus in the viewer — `commit_video_enhance`, `video_pending_seek`,
   `video_repause`, the discard-the-pre-seek-frame logic — exists *only* to
   work around that (`video-filter` is an instance arg; changing it means a new
   `libvlc_new`). mpv's `vf` chain is settable at runtime
   (`mpv_command(["vf","set",…])`). That apparatus **deletes**, and — the
   reason it matters here — live filters are the enabling primitive for a
   *live* transparent-colour picker (retune the key while watching it).
2. **libvlc has no reverse frame-step.** `VlcStream::step` fakes backward steps
   by nudging the clock. mpv has `frame-back-step`.
3. **Same windowless pull model, near-zero trait churn.** mpv's *software*
   render API (`mpv_render_context_render` into a CPU buffer) maps onto the
   existing `copy_frame → tightly-packed BGRA` seam exactly like libvlc's vmem
   did. `feraille-video-mpv` is a sibling of `feraille-video-vlc`; the
   `VideoBackend`/`VideoStream` trait barely moves.

Other parity: libmpv is mac/Win/Linux with the same runtime-`dlopen` model and
the same LGPL "dynamic-link only" constraint as libvlc, so the cross-platform
`dynload` module from the VLC crate is reusable nearly verbatim.

### Honest caveats (these shape the plan)

- **No canonical bundled location** like `VLC.app/Contents/MacOS/lib`. libmpv
  ships via Homebrew (`$(brew --prefix mpv)/lib/libmpv.*.dylib`) or inside
  `mpv.app/Contents/Frameworks`. The Settings path field must probe both and
  fail soft. **[mac]**
- ~~**One genuine unknown:** does mpv's software render emit a real alpha
  channel?~~ **Resolved in Phase 0: yes** — keying lives in mpv's filter chain
  (see [findings](#phase-0-findings-verified-not-assumed)).

## Existing anchors (verified 2026-06-23)

| What | Where |
|---|---|
| Video provider seam (`VideoBackend`/`VideoStream`, BGRA pull) | `crates/feraille-core/src/video.rs` |
| VLC backend (hand-written FFI, runtime dlopen, cross-platform `dynload`) | `crates/feraille-video-vlc/src/imp.rs` |
| Backend selection + path pref | `viewer/window.rs` `resolve_vlc_pref` (:406); `viewer/backend_native.rs` `video_backend()` |
| Single-video frame pull (~60 Hz) | `viewer/window.rs` `start_video_poll` (:1028), `video_poll_tick` (:1050) |
| Single-video render (one `img` via `stage::layout`) | `viewer/window.rs` `video_stage` (:1944), `stage_area` (:1991) |
| Seamless-reopen apparatus (VLC-only; **to delete**) | `viewer/window.rs` `commit_video_enhance` (:912), `video_pending_seek` use (:1063) |
| Per-frame CPU grade precedent | `viewer/window.rs` `graded_video` (:1716), `apply_color_adjust` |
| **GPUI already alpha-blends `img` per-pixel transparency** (transparent PNGs over `area.bg`) | `viewer/loader.rs` (RGBA→BGRA) + `stage_area` drawing `img` over `bg` |
| VLC feature wiring (mirror for `mpv`, then remove `vlc`) | `crates/feraille-gpui/Cargo.toml` `vlc` feature + 3 target blocks |
| Spike precedent | `spikes/vlc-probe/` (standalone crate, dlopen FFI, skip-if-absent) |

The last anchor is decisive for compositing: stacking N keyed videos is the
*same* GPU-alpha-blend mechanism the viewer already uses to draw a transparent
PNG over its canvas — not new rendering tech.

## Trait changes (additive, native keeps defaults)

In `feraille-core/src/video.rs`:

```rust
/// A transparent-colour key applied to a layer: pixels within `similarity`
/// of `color` go transparent; `blend` softens the edge. `None` = no key.
pub struct ChromaKey {
    pub color: [u8; 3],     // target RGB
    pub similarity: f32,    // 0..1 — how close counts as "the colour"
    pub blend: f32,         // 0..1 — edge feather
}

pub trait VideoStream {
    // … existing …

    /// Apply/clear a transparent-colour key live. Returns true if the backend
    /// keyed natively (mpv: a live `colorkey` vf) so the viewer skips its CPU
    /// key; false (native player) leaves the CPU key in charge.
    fn set_chroma_key(&mut self, _key: Option<ChromaKey>) -> bool { false }

    /// Change enhancement filters live (mpv). Returns true if applied live;
    /// false (native) means the viewer must re-open to change them — which,
    /// once VLC is gone, no shipped backend needs, so the re-open path goes.
    fn set_enhance(&mut self, _enhance: VideoEnhance) -> bool { false }
}
```

`VideoBackend::open` keeps its signature; mpv ignores the baked-at-open
`enhance` and applies it live via `set_enhance` instead.

**Grade also goes through the live chain.** Phase 0 found mpv's equalizer
*properties* (`brightness`/`contrast`/…) don't affect the SW-render output, so
the mpv backend implements `set_adjust` by composing a lavfi
`eq`/`colorlevels` filter into the same live `vf` chain as enhance and key —
returning `true` so the viewer skips its CPU grade. Grade + enhance + key are
one live filter chain.

## Architecture

New crate `crates/feraille-video-mpv/` — same shape as `feraille-video-vlc`:

```
feraille-video-mpv/
  Cargo.toml          — only feraille-core; libmpv loaded at runtime
  src/lib.rs          — backend(mpv_path) -> Option<Box<dyn VideoBackend>>
  src/imp.rs          — libmpv FFI: create/initialize/render-sw/observe/command;
                        reuses the dynload mac/win/linux pattern from the VLC crate
```

Viewer changes in `crates/feraille-gpui/src/viewer/window.rs`:

- The single `video_overlay: Option<(stream, path)>` becomes
  `layers: Vec<VideoLayer>` (bottom→top), each owning its stream, poll epoch,
  `Option<Arc<RenderImage>>` frame + seq, `Option<ChromaKey>`, and a mute flag.
- `start_video_poll`/`video_poll_tick` generalise to drive every layer; each
  layer pulls and supersedes its own `RenderImage`.
- `stage_area` folds the layers into stacked absolute `img` children, all laid
  out through the **same** `stage::layout` against the shared `StageState`, so
  GPUI's GPU compositor alpha-blends them and zoom/pan/fit stay aligned.
- A `Composite` section in the Adjustments popup: add/remove/reorder layers,
  a transparent-colour **swatch + eyedropper** (click swatch → click stage →
  sample the pixel from the cached top frame, read-only, no I/O), and
  **Similarity / Blend** sliders feeding the live key.
- The VLC-only seamless-reopen machinery is deleted; enhancement sliders route
  to live `set_enhance`.

### Data flow (≤200 words)

Opening a video resolves the active backend (native / mpv) once and opens the
base layer's stream, as today. Each layer in the stack runs its own ~60 Hz
poll that pulls the newest BGRA frame via `copy_frame` and supersedes a
per-layer `RenderImage`. A layer's transparent colour is applied as a **live
mpv `colorkey` filter** (`set_chroma_key`), so keyed pixels arrive with
alpha = 0; if a backend can't key live, a CPU `key_bgra` pass in that layer's
poll does it off the paint path. `stage_area` stacks the layers' absolute
`img` elements bottom→top, each laid out through the same `stage::layout`
against the shared `StageState`, so GPUI alpha-blends them on the GPU and
zoom/pan/fit stay aligned across layers. The eyedropper samples one pixel from
a cached frame (read-only) to set a key colour; similarity/blend sliders
retune the live filter without a re-open. Render reads only cached frames and
state; decode runs on mpv's threads, keying is live-filtered or polled
off-paint, and stale frames drop by epoch — the prime directive holds.

### Prime-directive compliance

- Render reads only cached `RenderImage`s + in-memory layer/stage state — no
  path resolution, stat, SQLite, or process spawn on any render/hover/scroll
  path.
- Decode runs on mpv's own threads; keying is a live mpv filter (off our
  threads) or a CPU pass in the per-layer poll (foreground task, **not** the
  paint path) — mirroring the existing `graded_video`/`apply_color_adjust`
  precedent.
- Eyedropper sampling reads a cached BGRA buffer; no I/O.
- Per-layer epochs drop stale/late frames, same idiom as today.

## Performance (the honest ceiling)

The keying and compositing are nearly free; the cost that scales is the
CPU-buffer pull, and N layers make it linear:

| Stage | Cost | Verdict |
|---|---|---|
| Decode (N streams) | mpv `hwdec=videotoolbox` **[mac]** / `d3d11va` **[win-parity]** — dedicated HW | 2–3× 1080p30 trivial on Apple Silicon |
| Keying | live mpv `colorkey`/`chromakey` lavfi filter, on mpv's threads | free to us; never touches the main thread |
| Compositing | GPUI GPU alpha-blend of stacked `img`s | free — the path transparent PNGs already use |
| **SW readback + `RenderImage` upload, per layer per frame** | ~W·H·4 bytes × fps × N | the real ceiling: comfy to ~2–3 layers @1080p; 4+ or any 4K layer competes with the UI |

**Mitigations (all cheap):** only poll layers that are present; mute +
optionally downscale lower layers; drop late frames by epoch; a **soft
layer-count guard** that `log()`s when the stack exceeds what the CPU path
serves well (no silent truncation).

**The escalation, when measured need arrives:** remove the readback entirely
with GPU surfaces — `gpui::surface(CVPixelBuffer)` (already a deferred item).
That makes N-layer 4K cheap but is a substantial rewrite of the frame path;
per slow-AI "naive version first", it's a **documented follow-up**, not an MVP
blocker. Phase 4 ships on the proven RenderImage-pull path and we escalate only
if real layer counts demand it.

## Phases

Each phase ends green (`cargo check` + `cargo test`), with a NOTES.md entry;
UI phases add a screenshot under `screenshots/`.

0. **`spikes/mpv-probe/`** (throwaway) — pull a SW-rendered frame; set a colour
   property live; change `vf` live; and the gate: **does SW render emit real
   alpha** from a `colorkey` filter? Resolves where keying runs. Delete
   `spikes/` once the binding + alpha decision is recorded in NOTES.md.
1. **`feraille-video-mpv` to parity, then remove VLC** — open/pull/seek/step/
   grade behind the Plugins dropdown; integration test mirroring the VLC one;
   then delete `feraille-video-vlc`, the `vlc` feature, and the seamless-reopen
   apparatus in the same phase.
2. **Live enhance** — denoise/sharpen/deband/grain via live `set_enhance`.
3. **Chroma key, single layer** — `ChromaKey` state + similarity/blend +
   eyedropper; keyed holes show the stage background through. Eyedropper glyph
   added (check the spare Lucide `pipette` first) + ICONS.md.
4. **N-layer stack** — `Vec<VideoLayer>`, per-layer poll + key, stacked
   compositing, soft layer-count guard, layer add/remove/reorder UI.
5. **Docs / icons / cleanup** — finalise this doc, NOTES.md decision log,
   TODO follow-ups (incl. the GPU-surface escalation), ICONS.md.

## Open questions / deferred

- ~~SW-render alpha (Phase 0 decides)~~ — **resolved: alpha survives**, key in
  the mpv filter chain; the CPU `key_bgra` fallback is unneeded.
- GPU-surface compositing (`gpui::surface`) for high layer counts / 4K — the
  performance escalation, deferred until measured.
- libmpv discovery defaults on Windows/Linux **[win-parity]** — mirror the
  mac Homebrew/`mpv.app` probe.
- Per-layer audio policy beyond "mute all but the focused layer" (defaulted).
- Layer sourcing UX (playlist neighbour vs file picker vs drag-in) — settle in
  Phase 4.
