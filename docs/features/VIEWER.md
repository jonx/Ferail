# Viewer & Slideshow

Design and implementation plan for big-image viewing in Feraille: a dedicated
viewer window with zoom/pan, slideshow playback, and zoom that sticks while
flipping through files. Complements [PREVIEW.md](PREVIEW.md) (the info pane);
this document covers the *large* viewing experience.

← [Feature notes index](README.md) · [Architecture](../ARCHITECTURE.md) ·
[TODO](../../TODO.md)

## Status

**Landed (v1, 2026-06-12)** — all six iterations below shipped; see the
dated NOTES.md entry for the decision log. Deliberate deviations from
this design, kept simple on purpose:

- The slideshow interval control is a cycle button (2 → 3 → 5 → 10 s),
  not a dropdown.
- Stale decode results still `cx.notify()` — render reads the current
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

- **[mac]** — macOS-only today (AppKit, Quick Look, Cmd-key conventions).
- **[win-parity]** — the named Windows equivalent to implement when the
  Windows shell catches up. Untagged design is platform-neutral.

## Goals

1. **Big preview** — see the selected file large, not just the 512 px
   info-pane thumbnail.
2. **Dedicated viewer window** — a secondary window (like Disk Usage), not a
   modal takeover. Less intrusive than macOS Preview/Quick Look: no document
   model, no per-file windows, Esc dismisses, fullscreen optional.
3. **Slideshow** — auto-advance through the current folder's files with
   play/pause and a configurable interval.
4. **Sticky zoom** — the signature feature: when the user zooms/pans, the same
   zoom level and relative pan position apply to the next/previous entry.
   Compare XnView/FastStone behavior; neither Finder nor Preview does this.
5. **Prime directive intact** — every frame renders from cached state; all
   decode/IO is off-thread, cancellable, and stale-dropped.

## Non-goals (v1)

- Editing, rotation, cropping, EXIF panels.
- Video/audio *playback* (a poster frame via Quick Look is enough for v1).
- Text-file viewing in the viewer window (the preview-pane provider system in
  [PREVIEW.md](PREVIEW.md) owns that).
- Live playlist sync with the file list (snapshot at open; deferred).
- Multi-monitor pinning, slideshow shuffle, transitions/cross-fade (animation
  budget review first — see TODO "visual polish").

## Existing anchors (verified 2026-06-12)

| What | Where |
|---|---|
| Info-pane thumbnail pipeline (async, 16-entry LRU, Pending/Loaded/Failed) | `crates/feraille-gpui/src/preview.rs` (`request()` at :93, `build_render_image()` at :136) |
| Quick Look thumbnail fetch, size-parameterized, 8 s timeout | `crates/feraille-shell-mac/src/quick_look.rs:54` (`fetch_thumbnail(path, size_px)`) **[mac]** |
| Secondary-window pattern (own entity, shared `Arc<NativeFs>` + task registry, weak-handle notify) | `crates/feraille-gpui/src/disk_usage.rs:1185` (`open_window`) |
| Fullscreen API | gpui `Window::toggle_fullscreen()` / `is_fullscreen()` (zed checkout `crates/gpui/src/window.rs:4892`, `:2136`) |
| Catalogue-driven keymap (command id → action → binding, context-gated) | `crates/feraille-core/src/commands.rs`, `crates/feraille-gpui/src/keymap.rs` (`install`, `install_extras`) |
| Task registry for status-bar visibility | `crates/feraille-gpui/src/tasks.rs` (`TaskRegistry::begin/update/end`) |
| `image` crate already a dep (v0.25, `png` feature only today) | `crates/feraille-gpui/Cargo.toml:42` |
| Theme tokens | `gpui_component::ActiveTheme` — `cx.theme().background/accent/muted_foreground/…` |

## Architecture

New module: `crates/feraille-gpui/src/viewer/`

```
viewer/
  mod.rs        — public API: open_viewer(...), action decls, shared types
  loader.rs     — full-res decode worker + byte-budget LRU cache
  stage.rs      — pure zoom/pan geometry (no gpui types beyond f32 math)
  window.rs     — ViewerWindow entity: playlist, input, toolbar, render
  playback.rs   — slideshow timer state machine
```

### Data flow (200 words)

Opening the viewer snapshots the active tab's *visible* rows (sorted +
filtered, directories excluded) into an in-memory playlist of
`{path, name}` plus the start index — no filesystem reads at open. The
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
(image dims, viewport dims, zoom mode, relative pan center) — pure math,
unit-testable. Wheel/drag/keyboard mutate StageState and notify; nothing in
the render path touches a file, a process, or SQLite.

### loader.rs — full-resolution pipeline

- `ViewerFrame { image: Arc<RenderImage>, w: u32, h: u32, bytes: usize }`.
- Raster decode path: read bytes + `image::load_from_memory` on the
  background executor → RGBA8 → BGRA swap (same as `build_render_image`).
  Enable `image` features: `jpeg`, `gif`, `webp`, `bmp`, `tiff` (additive to
  `png`). Animated GIF: first frame only (v1).
- **Dimension cap:** downscale longest edge to 8192 px during decode
  (`image::imageops::resize`, triangle filter) — bounds GPU texture size and
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
  the registry only if it takes visible time — v1: skip registry, the
  in-window spinner is the feedback. Prefetches are always silent.

### stage.rs — zoom/pan geometry (pure, tested)

```rust
pub enum ZoomMode { FitDown, Actual, Custom(f32) }   // FitDown = default
pub struct StageState {
    pub mode: ZoomMode,
    pub center: (f32, f32),   // pan position as fraction of image size, (0.5, 0.5) = centered
}
```

- `fit_down_scale(img, view) -> f32` — `min(vw/iw, vh/ih).min(1.0)`; small
  images render at 100 %, never upscaled (decision: blurry upscale by
  default looks broken; users zoom in deliberately).
- `effective_scale(mode, img, view)` — Fit→fit_down, Actual→1.0, Custom→s.
- `layout(img, view, state) -> Rect` — on-screen rect; clamps pan so the
  image can't be dragged fully off-screen; centers when smaller than view.
- `zoom_at(state, cursor, img, view, factor) -> StageState` — wheel zoom
  toward the cursor: the image point under the cursor stays fixed. Scale
  clamps to `[0.05, 32.0]`. Any zoom_at switches mode to `Custom`.
- `pan_by(state, delta_px, img, view)` — drag panning while zoomed.
- All functions are `f32` math over `(w, h)` tuples — unit tests cover fit,
  clamping, anchor invariance, and center round-tripping.

### Sticky zoom semantics (the signature feature)

`StageState` lives on the ViewerWindow, **not** per-image. Navigating to
another entry keeps `{mode, center}` verbatim:

- `FitDown` (default) → next image also fits. Matches slideshow expectations.
- `Custom(2.5)` + center (0.7, 0.3) → next image renders at 2.5× zoomed to
  the same *relative* region — comparing the same corner across screenshots
  or scans "just works".
- `Actual` → next image at 100 % too.
- Double-click toggles `FitDown` ↔ `Actual` (zoom-to-point on the click
  position when going to Actual).
- Explicit reset: `Cmd+0` **[mac key; win-parity Ctrl+0]** → `FitDown`,
  center (0.5, 0.5).

### window.rs — ViewerWindow

- Opened via the Disk Usage pattern: `viewer::open_viewer(playlist, start,
  cx)` from a Shell action handler; 1100×760 centered, themed background
  (`cx.theme().background` darkened canvas behind the image).
- **Single reusable window**: a process-wide `Option<WindowHandle>` —
  invoking Open Viewer while one exists retargets its playlist and
  activates it instead of stacking windows.
- Title: `"<filename> — 3 of 128"`; same counter repeated in the toolbar.
- Toolbar (gpui-component `Button`/`ButtonGroup`, icons consistent with
  `icons.rs`): prev / next, play–pause, interval dropdown (2 s / 3 s / 5 s /
  10 s), zoom out / percent label / zoom in, fit / actual toggle,
  fullscreen, and filename. `Kbd` hints in tooltips once the tooltip sweep
  (TODO) lands.
- Footer/charcoal status strip: index, dimensions, zoom %, file size —
  all from cached entry + frame data.
- **Fullscreen**: `window.toggle_fullscreen()`; binding `Cmd+Ctrl+F`
  **[mac convention; win-parity F11]**. In fullscreen the toolbar hides;
  hovering the top ~48 px strip shows it (pure hover state, no timers).
- **Input map** (context `"Viewer"`, registered in `keymap.rs
  install_extras`, so Shell shortcuts can't fire here):
  - `Left/Right` (also `Up/Down`): previous/next entry
  - `Space`: toggle slideshow play/pause
  - `Esc`: exit fullscreen if fullscreen, else close window
  - `Cmd+=` / `Cmd+-` / `Cmd+0` **[mac; win-parity Ctrl]**: zoom in/out/reset
  - `Cmd+Ctrl+F` **[mac]**: fullscreen
  - Wheel: zoom toward cursor · Drag: pan when zoomed · Double-click:
    fit ↔ actual
- Navigation wraps (last → first), so slideshows loop.
- Window close clears the process-wide handle and drops the cache.

### playback.rs — slideshow

- `Playback { playing: bool, interval: Duration, epoch: u64 }`.
- Play spawns a foreground task loop: `background_executor().timer(interval)`
  → `entity.update`: if still playing and epoch matches, advance to next
  (wrapping) and re-arm. Pause/manual-nav/zoom bumps `epoch`, killing stale
  timers — the same staleness idiom as enumeration cancel flags.
- Manual navigation while playing *re-arms* the timer (doesn't pause);
  zooming pauses playback (user is inspecting — advancing under them is the
  "intrusive" behavior we're avoiding).
- Default interval 3 s; persisted in `gpui-state.txt` as
  `viewer_slideshow_interval`.

### Shell integration

- Command catalogue (`feraille-core/src/commands.rs`): `view.open_viewer`,
  title "Open Viewer", shortcut `Cmd+Y` **[mac; win-parity Ctrl+Y]** —
  Space stays Quick Look **[mac]**.
- Action `OpenViewer` in `shell/actions.rs`; handler snapshots the active
  tab's visible rows (skip directories), resolves start = lead row, calls
  `viewer::open_viewer`. Empty/no-file selection: viewer opens on the first
  file row; folder with zero files → no-op with status-bar notification.
- Preview pane: the thumbnail becomes a button — click (or the new ⤢
  overlay button on hover) dispatches `OpenViewer`. Double-click on an
  image row keeps its current "open with default app" behavior; the viewer
  is deliberate, not a hijack.

### Prime-directive compliance checklist

- Render reads: playlist vec, cache entries, StageState, Playback — all
  in-memory. **No** path resolution, stat, SQLite, or process spawn in any
  render/hover/scroll path.
- Decodes and Quick Look shell-outs run on the background executor;
  results re-enter via `entity.update`; stale generations don't notify.
- Quick Look fallback inherits the existing 8 s kill-timeout **[mac]**.
- Playlist snapshot at open — viewer never re-reads the directory.
- Cache eviction is O(evicted) on insert, never in render.

## Iterations

Each iteration ends green (`cargo check` + `cargo test -p feraille-gpui`),
with a NOTES.md entry; UI iterations add a screenshot under `screenshots/`.

1. **Loader** — `viewer/loader.rs`: ViewerFrame, byte-budget LRU
   (unit-tested eviction), raster decode + dimension cap, Quick Look
   fallback **[mac]**, generation staleness. Enable `image` features.
2. **Stage** — `viewer/stage.rs`: ZoomMode/StageState + the five geometry
   functions, full unit tests (fit, clamp, anchor invariance, wrap).
3. **Window** — `viewer/window.rs` + `mod.rs`: open/reuse window, playlist
   snapshot, prev/next with wrap, title/counter, toolbar v1 (nav + zoom +
   fit/actual + fullscreen), Esc/arrows/zoom keys in `"Viewer"` context,
   catalogue command `view.open_viewer` + `Cmd+Y`, spinner while decoding.
4. **Sticky zoom + prefetch** — StageState persistence across nav (it
   already lives on the window — this iter is *verifying* semantics +
   placeholder swap), instant 512 px placeholder from the shared preview
   cache, prefetch ±1, wheel-zoom-at-cursor and drag-pan wiring.
5. **Slideshow** — `playback.rs`, play/pause button + Space, interval
   dropdown + persistence, epoch staleness, fullscreen chrome auto-hide.
6. **Shell integration & docs** — preview-pane click/⤢ button, empty-folder
   notification, TODO.md/README index/NOTES.md refresh, screenshot sweep.

## Video playback in the slideshow (v1 landed 2026-06-12)

Videos play inside the viewer instead of sitting as static posters.

**Approach — native `AVPlayerView` overlay [mac].** gpui has no video
element, so the viewer overlays an `AVPlayerView` (AVKit) as an
NSView subview of the gpui window's content view (obtained via
`raw_window_handle::HasWindowHandle` → `AppKitWindowHandle.ns_view`),
positioned over the stage rect with native aspect-fit gravity and the
default inline controls (hover to scrub/pause/mute). The alternative —
pumping frames through `AVPlayerItemVideoOutput` into `RenderImage`s —
keeps rendering inside gpui (zoom/pan would work) but costs a per-frame
BGRA copy and a pile of CoreVideo interop; deferred until something
needs it. *[win-parity: Media Foundation / MFPlay overlay]*

Rules:

- **Eligible extensions**: `mp4`, `m4v`, `mov` — formats AVFoundation
  reliably plays. Everything else stays a Quick Look poster.
- **Auto-play on becoming current** (viewing a video = playing it),
  with sound; native inline controls handle pause/scrub/mute.
- **Slideshow**: a video entry does NOT arm the interval timer — the
  video's own end advances the show
  (`AVPlayerItemDidPlayToEndTimeNotification` → channel → step). The
  ended event carries the entry path and is dropped if the user
  already navigated away. Pausing the slideshow stops auto-advance
  but never interrupts the video itself.
- **Lifecycle**: overlay attach/retarget/frame-sync happens in the
  same render-time change-detected sync as the window title (creation
  is cheap — AVFoundation loads media asynchronously; no blocking
  I/O on the UI thread). Window close / entry change tears the
  overlay down; `ViewerWindow::Drop` is the backstop.
- **Known v1 limitations** (accepted, on the follow-up list): zoom/pan
  and sticky zoom don't apply to the native overlay; the fullscreen
  hover-toolbar renders *under* the overlay within the video rect; a
  corrupt eligible file that never reaches its end-notification stalls
  slideshow auto-advance on that entry (arrows/Esc always work);
  screenshots can't capture the overlay (it bypasses gpui's
  framebuffer).

## Windows parity worklist (deferred, tagged above)

- Thumbnail/poster fallback: `IShellItemImageFactory` replaces Quick Look.
- Key conventions: Ctrl+Y / Ctrl+= / Ctrl+0 / F11; Esc identical.
- Fullscreen: gpui `toggle_fullscreen` should work via gpui_windows —
  verify on the Windows box (same machine that owns the `[patch]` decision
  in TODO).
- HEIC decodes via WIC there, not Quick Look.
- Everything in `stage.rs`, `playback.rs`, the cache, and sticky-zoom
  semantics is platform-neutral by construction.

## Open questions / deferred

- Live playlist sync (file deleted mid-slideshow → keep showing cached
  frame until nav; then skip-missing). Deferred to watcher integration.
- `QLThumbnailGenerator` over `qlmanage` shell-out **[mac]** — latency win,
  needs objc2 dependency decision.
- Pinch-to-zoom trackpad gesture mapping. gpui delivers pinch as scroll
  with modifiers on mac — investigate in a later polish iter.
- Slideshow transitions (cross-fade) — blocked on the animation-budget
  review in TODO.
