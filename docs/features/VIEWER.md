# Viewer & Slideshow

Design and implementation plan for big-image viewing in Ferail: a dedicated
viewer window with zoom/pan, slideshow playback, and zoom that sticks while
flipping through files. Complements [PREVIEW.md](PREVIEW.md) (the info pane);
this document covers the *large* viewing experience.

← [Feature notes index](README.md) · [Architecture](../ARCHITECTURE.md) ·
[TODO](../../TODO.md)

## Status

**Landed (v1, 2026-06-12)**: all six iterations below shipped; see the
dated NOTES.md entry for the decision log. Deliberate deviations from
this design, kept simple on purpose:

- The slideshow interval control is a cycle button (2 → 3 → 5 → 10 s),
  not a dropdown.
- Stale decode results still `cx.notify()`: render reads the current
  index, so a stale repaint is a no-op and cheaper than generation
  plumbing on every result.
- The footer strip shows index + dimensions + slideshow state; zoom %
  lives in the toolbar, file size is omitted for now.
- `Cmd+1` was added as an explicit Actual-size chord alongside the
  double-click toggle.
- The screenshot harness gained `--viewer <path>` for headless capture.

Open follow-ups are tracked in [TODO.md](../../TODO.md) (viewer bullet).

## Platform tagging convention

Anything not portable is tagged inline:

- **[mac]**: macOS-only today (AppKit, Quick Look, Cmd-key conventions).
- **[win-parity]**: the named Windows equivalent to implement when the
  Windows shell catches up. Untagged design is platform-neutral.

## Goals

1. **Big preview**: see the selected file large, not just the 512 px
   info-pane thumbnail.
2. **Dedicated viewer window**: a secondary window (like Disk Usage), not a
   modal takeover. Less intrusive than macOS Preview/Quick Look: no document
   model, no per-file windows, Esc dismisses, fullscreen optional.
3. **Slideshow**: auto-advance through the current folder's files with
   play/pause and a configurable interval.
4. **Sticky zoom**: the signature feature: when the user zooms/pans, the same
   zoom level and relative pan position apply to the next/previous entry.
   Compare XnView/FastStone behavior; neither Finder nor Preview does this.
5. **Prime directive intact**: every frame renders from cached state; all
   decode/IO is off-thread, cancellable, and stale-dropped.

## Non-goals (v1)

- Editing, rotation, cropping, EXIF panels.
- Video/audio *playback* (a poster frame via Quick Look is enough for v1).
- Text-file viewing in the viewer window (the preview-pane provider system in
  [PREVIEW.md](PREVIEW.md) owns that).
- Live playlist sync with the file list (snapshot at open; deferred).
- Multi-monitor pinning, slideshow shuffle, transitions/cross-fade (animation
  budget review first: see TODO "visual polish").

## Existing anchors (verified 2026-06-12)

| What | Where |
|---|---|
| Info-pane thumbnail pipeline (async, 16-entry LRU, Pending/Loaded/Failed) | `crates/ferail-gpui/src/preview.rs` (`request()` at :93, `build_render_image()` at :136) |
| Quick Look thumbnail fetch, size-parameterized, 8 s timeout | `crates/ferail-shell-mac/src/quick_look.rs:54` (`fetch_thumbnail(path, size_px)`) **[mac]** |
| Secondary-window pattern (own entity, shared `Arc<NativeFs>` + task registry, weak-handle notify) | `crates/ferail-gpui/src/disk_usage.rs:1185` (`open_window`) |
| Fullscreen API | gpui `Window::toggle_fullscreen()` / `is_fullscreen()` (zed checkout `crates/gpui/src/window.rs:4892`, `:2136`) |
| Catalogue-driven keymap (command id → action → binding, context-gated) | `crates/ferail-core/src/commands.rs`, `crates/ferail-gpui/src/keymap.rs` (`install`, `install_extras`) |
| Task registry for status-bar visibility | `crates/ferail-gpui/src/tasks.rs` (`TaskRegistry::begin/update/end`) |
| `image` crate already a dep (v0.25, `png` feature only today) | `crates/ferail-gpui/Cargo.toml:42` |
| Theme tokens | `gpui_component::ActiveTheme`: `cx.theme().background/accent/muted_foreground/…` |

## Architecture

New module: `crates/ferail-gpui/src/viewer/`

```
viewer/
  mod.rs       : public API: open_viewer(...), action decls, shared types
  loader.rs    : full-res decode worker + byte-budget LRU cache
  stage.rs     : pure zoom/pan geometry (no gpui types beyond f32 math)
  window.rs    : ViewerWindow entity: playlist, input, toolbar, render
  playback.rs  : slideshow timer state machine
```

### Data flow (200 words)

Opening the viewer snapshots the active tab's *visible* rows (sorted +
filtered, directories excluded) into an in-memory playlist of
`{path, name}` plus the start index, no filesystem reads at open. The
ViewerWindow entity owns: playlist, current index, a `ViewerCache`
(byte-budget LRU of decoded full-res frames), a zoom/pan `StageState`, a
slideshow `Playback` state, and a `generation: u64`. Navigation bumps the
generation and requests the image for the new index: cache hit renders
immediately; miss spawns a background decode (image crate for raster
formats; Quick Look at 2048 px for everything else **[mac]**), which
re-enters via `entity.update(cx)`, inserts into the cache, and notifies
only if its generation is current. While the full-res frame is in flight,
the shared 512 px info-pane thumbnail (if cached) renders as an instant
placeholder. After each navigation the loader also prefetches index ±1.
Render reads only cached state: `stage.rs` computes the on-screen rect from
(image dims, viewport dims, zoom mode, relative pan center): pure math,
unit-testable. Wheel/drag/keyboard mutate StageState and notify; nothing in
the render path touches a file, a process, or SQLite.

### loader.rs - full-resolution pipeline

- `ViewerFrame { image: Arc<RenderImage>, w: u32, h: u32, bytes: usize }`.
- Raster decode path: read bytes + `image::load_from_memory` on the
  background executor → RGBA8 → BGRA swap (same as `build_render_image`).
  Enable `image` features: `jpeg`, `gif`, `webp`, `bmp`, `tiff` (additive to
  `png`). Animated GIF: first frame only (v1).
- **Dimension cap:** downscale longest edge to 8192 px during decode
  (`image::imageops::resize`, triangle filter): bounds GPU texture size and
  memory.
- Fallback for HEIC, PDF, video, and anything `image` can't parse:
  `fetch_quick_look_thumbnail(path, 2048)` **[mac]**
  *[win-parity: `IShellItemImageFactory::GetImage` for the same role]*.
  A future iter can swap the shell-out for `QLThumbnailGenerator` via
  `objc2-quick-look-thumbnailing` **[mac]**.
- Failure → `Failed` state with a quiet broken-image glyph; never blocks nav.
- `ViewerCache`: HashMap + LRU order, **byte budget 384 MB** (const,
  revisit when real photo libraries are tested). One in-flight decode per
  path (Pending entry), stale results still cached (useful for back-nav)
  but only `cx.notify()` when current.
- Decode of the *current* image registers a `TaskKind::Preview`-style task in
  the registry only if it takes visible time: v1: skip registry, the
  in-window spinner is the feedback. Prefetches are always silent.

### stage.rs - zoom/pan geometry (pure, tested)

```rust
pub enum ZoomMode { Fit, FitDown, Actual, Custom(f32) }   // Fit = default
pub struct StageState {
    pub mode: ZoomMode,
    pub center: (f32, f32),   // pan position as fraction of image size, (0.5, 0.5) = centered
}
```

- `fit_scale(img, view) -> f32`: `min(vw/iw, vh/ih)`; media fills the
  window, small media scales *up* (the default since the
  `viewer_default_zoom` setting landed; the user picks the open-with mode
  in Settings → Layout → Viewer).
- `fit_down_scale(img, view) -> f32`: `fit_scale(...).min(1.0)`; small
  images render at 100 %, never upscaled: the "Fit, never enlarge"
  setting choice for those who find blurry upscale broken-looking.
- `effective_scale(mode, img, view)`: Fit→fit, FitDown→fit_down,
  Actual→1.0, Custom→s.
- `layout(img, view, state) -> Rect`: on-screen rect; clamps pan so the
  image can't be dragged fully off-screen; centers when smaller than view.
- `zoom_at(state, cursor, img, view, factor) -> StageState`: wheel zoom
  toward the cursor: the image point under the cursor stays fixed. Scale
  clamps to `[0.05, 32.0]`. Any zoom_at switches mode to `Custom`.
- `pan_by(state, delta_px, img, view)`: drag panning while zoomed.
- All functions are `f32` math over `(w, h)` tuples: unit tests cover fit,
  clamping, anchor invariance, and center round-tripping.

### Sticky zoom semantics (the signature feature)

`StageState` lives on the ViewerWindow, **not** per-image. Navigating to
another entry keeps `{mode, center}` verbatim:

- `Fit` (default) → next image also fills the window. Matches slideshow
  expectations. The window's default mode comes from the
  `viewer_default_zoom` setting ("fit" / "fit-down" / "actual", resolved
  once at window open, like the video-backend pref).
- `Custom(2.5)` + center (0.7, 0.3) → next image renders at 2.5× zoomed to
  the same *relative* region, comparing the same corner across screenshots
  or scans "just works".
- `Actual` → next image at 100 % too.
- Double-click toggles fit ↔ `Actual` (zoom-to-point on the click position
  when going to Actual). "Fit" here is the user's default mode, unless
  that default *is* `Actual`, in which case the toggle falls back to `Fit`
  so it still has two distinct states.
- Explicit reset: `Cmd+0` **[mac key; win-parity Ctrl+0]** → the default
  mode, center (0.5, 0.5).

### window.rs - ViewerWindow

- Opened via the Disk Usage pattern: `viewer::open_viewer(playlist, start,
  cx)` from a Shell action handler; 1100×760 centered, themed background
  (`cx.theme().background` darkened canvas behind the image).
- **One window per open**: each Open Viewer call opens a new window,
  cascaded from the last (a `VIEWER_CASCADE` counter offsets the centred
  bounds), so multiple files can be viewed side by side. Each window owns
  its own playlist + view state and shares only the process `ProcessState`
  (preview cache, etc.). Closing a window just drops its entity.
- Title: `"<filename>, 3 of 128"`; the toolbar repeats only the counter, not
  the filename.
- Toolbar (gpui-component `Button`/`ButtonGroup`, icons consistent with
  `icons.rs`): prev / next, play–pause, interval dropdown (2 s / 3 s / 5 s /
  10 s), zoom out / percent label / zoom in, fit / actual toggle,
  fullscreen, and filename. `Kbd` hints in tooltips once the tooltip sweep
  (TODO) lands.
- Footer/charcoal status strip: index, dimensions, zoom %, file size,
  all from cached entry + frame data.
- **Fullscreen**: `window.toggle_fullscreen()`; binding `Cmd+Ctrl+F`
  **[mac convention; win-parity F11]**. In fullscreen the toolbar hides;
  hovering the top ~48 px strip shows it (pure hover state, no timers).
- **Input map** (context `"Viewer"`, registered in `keymap.rs
  install_extras`, so Shell shortcuts can't fire here):
  - `Up/Down`: previous/next entry (always)
  - `Left/Right`: previous/next entry on a still; **step one frame
    back/forward** on a video (pausing it). `Up/Down` stay entry
    navigation so a video is still reachable from the keyboard.
  - `Space`: toggle the **video's** play/pause on a video, else toggle
    slideshow play/pause
  - `Esc`: exit fullscreen if fullscreen, else close window
  - `Cmd+=` / `Cmd+-` / `Cmd+0` **[mac; win-parity Ctrl]**: zoom in/out/reset
  - `Cmd+Ctrl+F` **[mac]**: fullscreen
  - `R` / `Shift+R`: rotate clockwise / counter-clockwise (sticky: the
    rotation carries to the next/previous item until the viewer closes)
  - `E`: toggle the **Adjustments** popup (also a right-click on the stage,
    or the palette toolbar button). `Esc` closes the popup first; a click
    elsewhere on the stage dismisses it.
  - `Cmd+Backspace` / `Delete` **[mac canonical / universal alternate]**:
    move the current file to the **Trash** and advance (also the trash
    toolbar button). Cull-while-browsing: the trash call runs on the
    background executor; on success the entry leaves the playlist (the
    following item becomes current, index-keyed bitmap caches are
    invalidated, the browser reloads any tab showing the parent folder) and
    a toast confirms. The last entry closes the window. Always the
    recoverable move-to-Trash: the viewer has no undo stack, so restoring
    means the OS Trash.
  - Wheel: zoom toward cursor · Drag: pan when zoomed · Double-click:
    fit ↔ actual
- **Rotation** is view-only, window-level, and ephemeral: it lives in
  `ViewerWindow::rotation` (a single `u8` of quarter-turns), never touches
  the file, and, like the colour grade, is carried across next/prev so a
  rotation set once applies to every item you flip through. It is dropped
  when the window closes or retargets onto a new playlist.
  - **Images and videos both** CPU-rotate the bitmap (`rotate_render_image`,
    cached in one slot) since gpui can't transform an `img`
    (docs/GPUI-UPSTREAM.md #5). A video frame is just an `img`, so it
    rotates the same way: keyed by frame sequence so each pulled frame
    rotates once, and it rotates live even while paused.
- **Video transport**: since the video is a gpui `img` (no native
  overlay floating on top), the toolbar/seek-bar hit-test normally at any
  rotation. Toolbar play/pause + frame-step (`−1f` / `+1f`, via
  `stepByCount:`) + a **mute toggle** (speaker button, **muted by default**:
  audio is opt-in per window) + a **Loop** checkbox; a custom **seek bar** + elapsed/
  total in the status strip (drag to scrub via `seekToTime:`). `CMTime` is
  mirrored locally for the seek/time calls.
- **In / Out cue points**: the seek bar carries two draggable grips that
  bound playback to `[In, Out]`, with the active region shaded between
  them. Stored as fractions (`cue_in`/`cue_out`) and **reset to 0 / 1
  whenever a clip becomes current** (not remembered). The bar is a small
  custom widget (not the gpui-component slider) so playhead + both cues
  live on one track: it captures its painted bounds via a `canvas`, and a
  press picks the nearer cue grip if within `SEEK_GRAB_PX`, else scrubs
  the playhead: proximity hit-testing, so a grip never steals a scrub
  click. Reaching the **Out** cue: with **Loop** on, jump back to **In**
  and keep playing (the region repeats); with Loop off, pause at Out, or,
  if a slideshow is running, advance to the next clip. A full-length Out
  (`1.0`) is the clip's natural end, left to the end-of-play notification
  so the poll doesn't race it; only a real trim is enforced in the poll.
- **Adjustments popup** (`E` / right-click / palette button): a small
  floating panel with custom-drawn sliders (same hand-rolled track widget
  as the seek bar: label, draggable fill + thumb, bounds captured via a
  `canvas`). Two stages, both view-only and window-level (carried across
  navigation, never written to disk):
  - **Colour grade**: Brightness / Contrast / Color (saturation), each a
    bipolar `[-1, 1]` slider that detents to neutral at centre. Cheap
    per-pixel maths (`grade_bgra`, a brightness+contrast LUT plus optional
    saturation mix over the BGRA buffer). Applies to **stills and video**.
    For an **mpv video** two more bipolar sliders appear: **Hue** and
    **Gamma**: applied live in mpv's filter chain. They're hidden for stills
    and the built-in player, which have no equivalent stage.
  - **Auto-enhance** (the **magic-wand** button in the panel header, beside
    Reset): one click derives an auto-levelled grade from the item's own
    pixels: a 0.5 %-clipped luma-histogram stretch folded into the
    Brightness/Contrast sliders, plus a gentle saturation lift skipped for
    near-monochrome images so a B&W photo isn't tinted (`compute_auto_grade`,
    run off-thread). It only writes the colour fields, so enhancement
    (denoise/sharpen/upscale) is left as the user set it; for a video it reads
    the last pulled frame and pushes the grade live like any other slider.
  - **Enhancement**: Denoise + Sharpen, plus an Upscale `1× / 2× / 4×`
    (SIMD Lanczos3 via `fast_image_resize`, capped at `UPSCALE_MAX_EDGE` and
    never shrinking an already-larger original). For **stills** these run the
    CPU pipeline below; Upscale feeds a higher-res bitmap into the *same*
    layout rect, so the win shows when zoomed past 100 %. For an **mpv video**,
    Denoise/Sharpen are shown too, plus **Debanding** and **Film grain**,
    and apply **live** through mpv's `vf` filter chain (order **denoise →
    deband → sharpen → grain**, so `sharpen` enhances real detail rather than
    amplifying grain), with **no stream re-open** (`set_enhance`). Upscale is
    still-only; the built-in (AVFoundation) player has no filter chain, so the
    section is hidden for it.
  - **Transparent colour**: an mpv-video-only chroma key (a colour **swatch +
    eyedropper**, plus **Similarity** and **Blend** sliders) that makes a
    picked colour transparent, paired with a **Transparent** window toggle so
    keyed videos stack across windows (OS-composited). See
    [VIDEO-MPV.md](VIDEO-MPV.md) for the mpv backend, chroma key, and
    transparent-window details.
  - **Threading**: the still pipeline (grade → denoise → upscale → sharpen
    → rotate, `process_still_pixels`) is convolution/resampling-heavy, so
    it **never runs on the render path**: it's dispatched to the
    background executor (`schedule_process`), keyed by
    `(index, turns, grade, enhance)` into the `processed` one-slot cache,
    with a monotonic token dropping superseded runs. While a fresh result
    computes, the plain rotated original stands in (UI never stalls: the
    prime directive). Video grading stays inline (`graded_video`, per frame
    seq) since it can't be pre-baked off-thread.
  - **Drag responsiveness**: while a slider is being dragged the pipeline
    runs a `preview` pass that **skips the upscale resample**: the one
    genuinely expensive step, and invisible at fit-to-window anyway, so the
    colour/denoise/sharpen change tracks the cursor instantly. The cache
    records whether an entry is a preview; the full-size pass runs once on
    drag release (the `processed` entry's `bool`). The Upscale buttons
    themselves aren't a drag, so they go straight to full quality.
- **Video provider is pluggable** (`ferail_core::video::VideoBackend`,
  see NOTES.md 2026-06-19): the built-in AVFoundation player or, in a
  `--features mpv` build with mpv selected in Settings → Plugins, a libmpv
  backend that plays virtually any container (the eligible-extension set is
  backend-aware: `MPV_VIDEO_EXTS` only counts when mpv is active) and grades
  video natively. Both decode into a BGRA pull buffer drawn as a gpui `img`;
  the viewer never names a concrete player.
- **Stay on top** raises the native window level; on macOS it also opts into
  `CanJoinAllSpaces | FullScreenAuxiliary`, allowing a viewer to overlay
  another app's native full-screen Space. **Transparent** still clears the
  stage/window background for chroma-key composition, while the separate
  **Opacity** scrub fades the entire window (media and chrome together) from
  100% down to a recoverable 20%.
- **Zoom / pan / fit apply to video for free**: the pulled frame is laid
  out through the exact same `stage::layout` call as a still, against the
  frame's own (post-rotation) pixel size. A window resize re-fits both
  (the viewer observes `observe_window_bounds`).
- Navigation wraps (last → first), so slideshows loop.
- Window close clears the process-wide handle and drops the cache.

### playback.rs - slideshow

- `Playback { playing: bool, interval: Duration, epoch: u64 }`.
- Play spawns a foreground task loop: `background_executor().timer(interval)`
  → `entity.update`: if still playing and epoch matches, advance to next
  (wrapping) and re-arm. Pause/manual-nav/zoom bumps `epoch`, killing stale
  timers: the same staleness idiom as enumeration cancel flags.
- Manual navigation while playing *re-arms* the timer (doesn't pause);
  zooming pauses playback (user is inspecting, advancing under them is the
  "intrusive" behavior we're avoiding).
- Default interval 3 s; persisted in `gpui-state.txt` as
  `viewer_slideshow_interval`.

### Shell integration

- Command catalogue (`ferail-core/src/commands.rs`): `view.open_viewer`,
  title "Open Viewer", shortcut `Cmd+Y` **[mac; win-parity Ctrl+Y]**:
  Space stays Quick Look **[mac]**.
- Action `OpenViewer` in `shell/actions.rs`; handler snapshots the active
  tab's visible rows (skip directories), resolves start = lead row, calls
  `viewer::open_viewer`. Empty/no-file selection: viewer opens on the first
  file row; folder with zero files → no-op with status-bar notification.
- Preview pane: the thumbnail becomes a button: click (or the new ⤢
  overlay button on hover) dispatches `OpenViewer`. Double-click on an
  image row keeps its current "open with default app" behavior; the viewer
  is deliberate, not a hijack.

### Prime-directive compliance checklist

- Render reads: playlist vec, cache entries, StageState, Playback, all
  in-memory. **No** path resolution, stat, SQLite, or process spawn in any
  render/hover/scroll path.
- Decodes and Quick Look shell-outs run on the background executor;
  results re-enter via `entity.update`; stale generations don't notify.
- Quick Look fallback inherits the existing 8 s kill-timeout **[mac]**.
- Playlist snapshot at open: viewer never re-reads the directory.
- Cache eviction is O(evicted) on insert, never in render.

## Iterations

Each iteration ends green (`cargo check` + `cargo test -p ferail-gpui`),
with a NOTES.md entry; UI iterations add a screenshot under `screenshots/`.

1. **Loader**: `viewer/loader.rs`: ViewerFrame, byte-budget LRU
   (unit-tested eviction), raster decode + dimension cap, Quick Look
   fallback **[mac]**, generation staleness. Enable `image` features.
2. **Stage**: `viewer/stage.rs`: ZoomMode/StageState + the five geometry
   functions, full unit tests (fit, clamp, anchor invariance, wrap).
3. **Window**: `viewer/window.rs` + `mod.rs`: open/reuse window, playlist
   snapshot, prev/next with wrap, title/counter, toolbar v1 (nav + zoom +
   fit/actual + fullscreen), Esc/arrows/zoom keys in `"Viewer"` context,
   catalogue command `view.open_viewer` + `Cmd+Y`, spinner while decoding.
4. **Sticky zoom + prefetch**: StageState persistence across nav (it
   already lives on the window: this iter is *verifying* semantics +
   placeholder swap), instant 512 px placeholder from the shared preview
   cache, prefetch ±1, wheel-zoom-at-cursor and drag-pan wiring.
5. **Slideshow**: `playback.rs`, play/pause button + Space, interval
   dropdown + persistence, epoch staleness, fullscreen chrome auto-hide.
6. **Shell integration & docs**: preview-pane click/⤢ button, empty-folder
   notification, TODO.md/README index/NOTES.md refresh, screenshot sweep.

## Video playback in the slideshow (v1 2026-06-12; frame-pull 2026-06-18)

Videos play inside the viewer instead of sitting as static posters.

**Approach: windowless `AVPlayer` + frame pull [mac].** gpui has no
video element, but it does have an `img` element backed by `RenderImage`.
So instead of floating a native `AVPlayerView` NSView over the gpui
window (the original v1 design), the viewer drives a *windowless*
`AVPlayer` and pulls decoded frames out of an `AVPlayerItemVideoOutput`
as 32-BGRA pixel buffers (`ferail-shell-mac/src/video_overlay.rs`).
Each frame becomes a `RenderImage` the viewer draws through the **exact
same `stage::layout` + `img` path as still images**, so the video rect
is a real gpui element: it composites in-tree, zoom/pan/fit/rotation are
the shared still path, the gpui transport hit-tests correctly, and the
headless screenshot harness captures it. AVFoundation still owns
decode (hardware), audio, timing, seek, and step, only the *display*
mechanism changed. *[win-parity: Media Foundation source reader feeding
the same frame path.]*

This retired the whole overlay-compositing tax: the clipped wrapper, the
Core Animation rotation transform, the hidden native controls, and the
transparent-layer black-flash hack are all gone (see GPUI-UPSTREAM.md #5,
#5a, now resolved by this design).

Rules:

- **Eligible extensions**: `mp4`, `m4v`, `mov`: formats AVFoundation
  reliably plays. Everything else stays a Quick Look poster.
- **Auto-play on becoming current** (viewing a video = playing it),
  with sound, driven by the gpui transport.
- **Frame pull**: a ~60 Hz foreground task (`start_video_poll`) calls
  `video_overlay_copy_frame` on the main thread (the player registry is
  main-thread-only), builds a `RenderImage`, and supersedes the previous
  frame. `copy_frame` returns `None` between the video's own frames, so
  it's a cheap no-op poll most ticks, and `None` until the first frame
  decodes, which is what keeps the Quick Look poster up during a switch
  (no black flash). Superseded frames are evicted from gpui's sprite
  atlas via `Window::drop_image` at the top of the next render, so a new
  `RenderImage` per frame doesn't grow VRAM.
- **Slideshow**: a video entry does NOT arm the interval timer: the
  video's own end advances the show
  (`AVPlayerItemDidPlayToEndTimeNotification` → channel → step). The
  ended event carries the entry path and is dropped if the user
  already navigated away. Pausing the slideshow stops auto-advance
  but never interrupts the video itself.
- **Lifecycle**: open/teardown happens in the render-time
  change-detected `sync_video` (creation is cheap: AVFoundation loads
  media asynchronously; no blocking I/O on the UI thread). Window close /
  entry change tears the player down and queues the on-screen frame for
  eviction; `ViewerWindow::Drop` is the backstop.
- **Follow-ups** (accepted): the frame copy runs on the main thread:
  fine for typical content, but 4K60 may warrant a `CVDisplayLink`
  background pull (GPUI-UPSTREAM.md); rotating a 4K frame per tick is a
  CPU rotate (only while rotated).

## Windows parity worklist (deferred, tagged above)

- Thumbnail/poster fallback: `IShellItemImageFactory` replaces Quick Look; PDFs render page 1 through `Windows.Data.Pdf` (`pdf_render.rs`) at the viewer edge, so a PDF opens as a real page, not an icon.
- Key conventions: Ctrl+Y / Ctrl+= / Ctrl+0 / F11; Esc identical.
- Fullscreen: gpui `toggle_fullscreen` should work via gpui_windows:
  verify on the Windows box (same machine that owns the `[patch]` decision
  in TODO).
- HEIC decodes via WIC there, not Quick Look.
- Everything in `stage.rs`, `playback.rs`, the cache, and sticky-zoom
  semantics is platform-neutral by construction.

## Open questions / deferred

- Live playlist sync (file deleted mid-slideshow → keep showing cached
  frame until nav; then skip-missing). Deferred to watcher integration.
- `QLThumbnailGenerator` over `qlmanage` shell-out **[mac]**: latency win,
  needs objc2 dependency decision.
- Pinch-to-zoom trackpad gesture mapping. gpui delivers pinch as scroll
  with modifiers on mac: investigate in a later polish iter.
- Slideshow transitions (cross-fade): blocked on the animation-budget
  review in TODO.
