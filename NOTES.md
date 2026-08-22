# Ferail — Architecture and Decision Log

Multi-iter spec work under the Slow AI method. Currently covers two specs:

- `docs/features/ferail-selection-dnd-spec.md` (selection iter 1+2 landed; drag still pending) — below.
- `docs/features/ferail-windows-instances-tabs-spec.md` (in progress) — top of file.

---

# 2026-07-14 Windows/Linux parity session (windows-parity branch)

Bringing the Windows and Linux ports to parity with the Mac after the July
merges. Baseline first, then phased: re-verify merged Mac features on Windows →
Windows Chunk C (elevation + Restart Manager) → Linux shell fills → Linux
headless screenshots → Windows polish.

## Key decisions

- **aros-port merged into windows-parity** (`ceea656`), not cherry-picked — the
  branch carried cross-platform work (viewer trash-and-advance, mpv
  end-detection, grid low-res thumbnails, Lucide icon fallback for stub
  platforms, typeahead focus fix) that parity work builds on, and a merge keeps
  future syncs conflict-free. TODO.md conflict resolved by keeping aros-port's
  prunes (favorites polish, marquee, configurable columns shipped there) plus
  main's newer items (error-notification UX, hidden-file affordances).
- **Patch-graph reconciliation is per-box local state.** The committed
  `[patch]` entries point at Mac-only sibling checkouts (`../zed-aros`,
  `../gpui-component-aros`). On the Windows box the zed patch is re-pointed at
  `../zed-ferail-patch` (same rev + the D3D11 `render_to_image`), and the
  gpui-component / crates-io (stacker, filetime) patches are commented out —
  their deltas are inert off-AROS. Do not push Cargo.toml/Cargo.lock as-is.
  Open question for CI/other machines: publish the forks (TODO already tracks
  publishing the render_to_image fork).
- **`same_volume` got its real Windows arm** (pulled forward from the polish
  phase because the Mac-authored test `move_renames_on_same_volume` correctly
  failed on Windows): volume serial via `CreateFileW(0 access,
  BACKUP_SEMANTICS)` + `GetFileInformationByHandle`, nearest-existing-ancestor
  walk mirroring the unix `dev()` loop. Drive-letter prefix comparison was
  rejected — it lies under junction-mounted volumes. This flips Windows moves
  from copy+delete to the rename fast path.
- **`recreate_symlink` got a real Windows arm** — `symlink_file`/`symlink_dir`
  classified by the resolved target (dangling → file symlink, Explorer's
  default). Works unprivileged under Developer Mode; otherwise the privilege
  error flows through the structured failure report.
- **`video_mf.rs` end-detection bug fixed**: a decode ERROR only set an
  `ended` flag nobody read — the viewer's only signal is the `on_ended`
  callback, so broken files stalled playlist auto-advance. ERROR now fires the
  callback; the dead flag was removed from `Notify` and `Player`.
- **libmpv installed** at `C:\Source\john-knipper-personal\libmpv\libmpv-2.dll`
  (shinchiro 2026-06-07 dev build) for runtime verification of the mpv backend
  on Windows; point Settings → Plugins there.

- **Windows "Chunk C" resilient file-ops shipped** — the last big Windows
  capability gap. New `ferail-shell-win32/src/elevation.rs`:
  - `run_elevated_self`: `ShellExecuteExW` verb `"runas"` (UAC), wait on the
    returned process handle, return its exit code. `Err("cancelled")` on
    `ERROR_CANCELLED` to match the macOS osascript contract. Args are re-quoted
    with `CommandLineToArgvW` rules (unit-tested) so descriptor paths with
    spaces round-trip to the elevated child's `std::env::args`.
  - `processes_using`: Restart Manager (`RmStartSession` →
    `RmRegisterResources` → `RmGetList`), RAII `RmSession` that always
    `RmEndSession`s. Returns empty on any failure — it's a diagnostic list, not
    control flow, so "don't know" and "none" render the same.
  - `force_close_processes`: graceful `RmShutdown(RmForceShutdown)` (delivers
    WM_CLOSE / service-stop) keyed by (pid, start-time) so a recycled pid can't
    hit an innocent process, then `TerminateProcess` for survivors.
  - GPUI wiring: `TransferRetry` carries the capped locked paths; the failure
    toast grows a **"What's using it?"** button (only when
    `lock_diagnostics_available()`), which runs the RM scan on the background
    executor, names the holders, and offers **"Close & retry"**. Mac/Linux keep
    the stub bools (false), so the button never shows there.
  - Verified end-to-end with an exclusive-locked file: named the holder
    (PowerShell pid), force-closed it, confirmed the lock released. The
    `examples/lockers.rs` smoke harness stays in the crate.

- **Linux file-type icons shipped** (`ferail_fs_native::fetch_icon_rgba`
  Linux arm). Pipeline: `xdg-mime` shared-mime-info → candidate icon names
  (specific, `type-subtype`, `*-x-generic`) → `freedesktop-icons` theme lookup
  (GTK theme via gsettings, hicolor cascade) → rasterize PNG (`image`) or SVG
  (`resvg`, un-premultiplied to honour the straight-RGBA contract). All four
  deps were already compiled in the graph (via gpui), so no build-cost. Cached
  per-kind/extension one level up in gpui's `IconCache`, so MIME+theme
  resolution runs once per file type, never on the render path. Verified in
  WSL2: dumped real theme glyphs to PNG (`examples/icon_dump.rs`) — correct
  document + folder glyphs. Note: WSL2's Adwaita is stripped of per-type MIME
  icons, so every file type falls to the generic there (correct freedesktop
  behaviour — Nautilus does the same on that box); a full desktop theme gives
  distinct glyphs.

- **Icon-drawing tests added** (answering the AROS "chrome icons don't draw"
  report):
  - fs-native `icon_tests`: `fetch_icon_rgba` returns a well-formed, non-empty,
    correctly-sized icon for a text file + directory (mac/windows always;
    linux tolerates None when no theme installed).
  - gpui `assets::tests`: (1) every SVG we ship rasterizes non-empty through
    the same usvg/resvg path gpui uses; (2) every icon path the app actually
    draws (~85, curated from the `svg().path("icons/…")` call sites) resolves
    via the composite asset source and rasterizes non-empty.
  - **Diagnostic finding for AROS:** both gpui asset tests PASS, so every drawn
    icon is a valid, non-empty SVG that usvg/resvg rasterizes fine. The AROS
    "chrome icons not drawn" symptom is therefore NOT a bad/missing/empty asset
    — it's in the `gpui_aros` renderer's monochrome-SVG-sprite path (zed-aros
    fork), which ferail tests can't cover and I can't repro from this box.
    Next step for AROS lives in zed-aros/crates/gpui_aros, not here.

- **Linux content thumbnails shipped** (`ferail_shell_linux::
  fetch_quick_look_thumbnail`). Rides the shared freedesktop thumbnail cache
  (`$XDG_CACHE_HOME/thumbnails/{normal,large,x-large,xx-large}/<md5(file-uri)>
  .png`) so a thumbnail Nautilus already made returns instantly, and one we make
  is reusable. Miss/stale (source newer than cached PNG) → regenerate with
  `gdk-pixbuf-thumbnailer` (writes the spec `Thumb::*` tEXt chunks) → decode PNG
  (`image`) → straight RGBA8 (same contract as `fetch_icon_rgba`). MD5 keying
  locked to the freedesktop spec vector by a unit test (my first guess at the
  vector was wrong — `md5sum` gave the ground truth `d40775e…`). v1 = images
  (gdk-pixbuf); video/PDF need totem/evince or the Tumbler D-Bus dispatcher.
  New Linux deps `md-5` + `image` were tiny / already compiled. Verified in
  WSL2: generated a real 256×256 thumbnail that populated the shared cache
  (`examples/thumb_dump.rs`).

- **Windows path polish shipped.** Two correctness fixes on the Windows box:
  - `paths::display_path` strips the `\\?\` verbatim prefix from every
    user-facing whole-path string (breadcrumb root, window title, disk-usage
    header + titlebar, Get Info "Where"). `std::fs::canonicalize` returns
    verbatim paths and the file list navigates with them, so a raw
    `to_string_lossy` was leaking `\\?\C:\…` — verified fixed by screenshot.
  - `paths::validate_leaf` blocks reserved Windows names on New Folder / rename
    (device names, reserved chars, trailing dot/space) via the shared
    `open_named_prompt` modal, which stays open on rejection. Favorite-label
    rename opts out (it's not a filename). Logic is unit-tested; the modal
    text-input path itself is interactive-verify-only (the screenshot harness
    can't dispatch keystrokes into a dialog `InputState`, same limitation as
    drag gestures).

## With more time, I would

- Add a Windows `file_id` arm to dupes.rs (`GetFileInformationByHandle`
  nFileIndex) so NTFS hard links collapse to one occupant like unix.
- Drive the "What's using it?" toast through the screenshot harness — it needs
  a real locked-file transfer failure, which isn't headlessly simulable today;
  the primitives underneath are verified via the example instead.
- Linux resilient-ops: `pkexec` re-exec for `run_elevated_self` + a
  `/proc/*/fd` scan for `processes_using` (the shell-linux stubs are still
  false/empty).

---

# 2026-06-23 mpv video backend, VLC retirement & color-key transparency (planned)

Full plan: [docs/features/VIDEO-MPV.md](docs/features/VIDEO-MPV.md). ✅ Shipped on
main (Phases 1–5): the libmpv backend replaced VLC and color-key / transparent-
window compositing landed on macOS. Windows parity is tracked in the
`windows-parity` port work.

## The decision (mpv over VLC and over raw FFmpeg)

The pain that started this: I wanted the Adjustments popup's denoise/sharpen/
deband/grain to apply **live** on video. They can't with VLC — libvlc only takes
those as instance args to `libvlc_new`, so a slider release re-opens the whole
stream (that's what all the `video_pending_seek` / `video_repause` / kept-frame
machinery in `window.rs` is for). Only the colour grade is live there.

Weighed three providers behind the existing `VideoBackend` seam:

- **VLC** — player for free, but filters are structurally not live. Status quo.
- **raw FFmpeg (libav\*)** — filters live, but FFmpeg is a *library, not a
  player*: I'd have to build demux/decode/audio/A-V sync/clock/seek myself. Big
  lift for a file-manager viewer. Rejected.
- **libmpv** — FFmpeg *with a player attached*. Plays the same broad set, owns
  audio/sync/seek, **and** exposes the libavfilter graph at runtime
  (`vf set`/`vf command`) — the same `hqdn3d`/`unsharp`/`gradfun`/`noise`
  filters VLC wraps, but live. Loads via `dlopen` like libvlc (stock build
  links nothing), and its **software render API writes frames straight into a
  caller buffer**, which is almost exactly our BGRA pull seam. **Chosen.**

mpv is a functional superset of the VLC backend for local files, so the plan is
to **add mpv, prove it on the mac, then retire VLC** (iteration 5) — which also
lets me delete the re-open machinery. Not deleting VLC atomically with landing
mpv: don't remove a shipping/tested backend before its replacement is verified.

## Key decisions

- **One additive seam change:** `VideoStream::set_enhance(&mut self, VideoEnhance)
  -> bool`, default `false`. VLC/native/MF inherit `false` (existing re-open
  path); only mpv overrides it to `true` via live `vf set`. `commit_video_enhance`
  tries it first, re-opens only on `false`.
- **Hand-written FFI, no `libmpv`/`mpv` Rust crate** — mirrors the deliberate
  no-`vlc-rs` decision and the runtime-`dlopen` posture of the VLC crate.
- **Grade via mpv's equalizer properties** (live), with lavfi `eq`/`hue` as the
  fallback if the equalizer turns out not to apply under SW render (the one open
  uncertainty — verify on the mac).
- **SW format `bgr0` → must fill alpha to `0xFF`.** `build_video_frame` reads the
  4th byte as alpha verbatim, and `bgr0` leaves it `0` (fully transparent). That
  forced alpha pass is also what makes the **color-key transparency** idea nearly
  free: key per pixel instead of blind-filling. Logged as a follow-on feature in
  the plan doc (with its own gates), with one open product fork — see-through to
  an in-app backdrop (cheap) vs. to the desktop via a transparent window (the
  "wow" version, ~3× the work).

## With more time / deferred

- Color-key v1 (RGB-distance + tolerance), then eyedropper, then YUV chroma-key.
- Bundle libmpv for a redistributable build (later phase, like VLC's bundling).
- `CVDisplayLink` background frame pull if 4K60 shows main-thread cost (shared
  follow-up with the native/VLC backends).

# 2026-06-20 unified selection color (list ⇄ grid) + customizable accent (in progress)

The list pane's selection read as "too faint" next to the grid. Verified the
cause: the grid keys selection off the saturated Finder `theme.blue` (0.14 wash
+ 0.55 border + solid label pill), but the list draws selection with
`theme.table_active`, which the gpui-component theme **hard-caps at alpha ≤ 0.2**
(`schema.rs:670`) and is a desaturated near-foreground gray. So the list could
never match the grid no matter what.

## Plan (approved 2026-06-20, both decisions locked with user)

- **One source of truth, `crate::selection_colors`.** A `SelectionAccent(
  Option<Hsla>)` process-wide `Global` (same pattern as `grid::IconSize` /
  `thumbnails::ShowThumbnails`), seeded from `app_state` at startup. `accent(cx)`
  returns the user override or falls back to `theme.blue`. Derived helpers
  (`fill` 0.14, `member_fill` 0.14, `border` 0.55, `strong` full, `text` white)
  are the **exact opacities the grid already used** — so the grid is visually
  unchanged and the list now matches it.
- Grid (`shell::render`), list members (`file_list::render_tr`), and the list
  lead overlay (`multi_table::state::render_table_row`) all read these helpers
  instead of inline `theme.blue` / `theme.table_active`.
- **Customization (user chose "also add the picker", not just the seam):**
  `app_state` persists `selection_color` as a hex string; the Appearance
  settings page gets a gpui-component `ColorPicker` that sets the global live
  (open windows repaint at once, like the thumbnail toggle) and persists the
  hex. `ColorPickerState` is a stateful entity, so `SettingsView` owns one and
  subscribes to `ColorPickerEvent::Change`; `SettingsView::new` gains
  `window/cx` (3 call sites).

## Decisions

- **List is not a literal pixel-copy of the grid (user chose "exact grid
  parity" on opacities).** Grid cells have gaps so the grid borders *every*
  selected cell; list rows are contiguous, where per-member borders would paint
  internal horizontal lines through a multi-selection. So: lead row = `fill`
  bg + full-strength `strong` border (the focus ring, mirroring the grid lead);
  non-lead members = `fill` bg only. The visible jump from faint to clear comes
  mostly from the hue + the full-blue lead border (the old lead border was the
  faint `table_active_border`).
- **Edited the vendored `multi_table` directly** rather than keeping it
  pristine — it's already app-customized (drag-delay behavior) and
  `FileListDelegate` is its only delegate, so the change can't leak to another
  table.
- **Persist as hex**, not raw HSLA floats — matches the flat key=value
  `app_state` format and what `ColorPicker` already speaks (`to_hex` /
  `parse_hex`).

## With more time

- Separate accents for selection-fill vs. lead/highlight (a louder focus ring).
- "Reset to theme default" on the picker row.
- Heavier list-lead fill than the grid (no label pill carries color on a flat
  row); kept parity per the user's choice — the most likely follow-up tweak.

# 2026-06-20 Tool Result Surfaces (landed)

Search, Duplicate Finder, and docked Disk Usage now share a tab-local result
surface model instead of each owning a separate "results mode" concept.

- **One tab field:** `Tab::tool_result: Option<ToolResultSurface>` replaces the
  parallel `search_mode` / `dupe_mode` fields. The enum variants are Search,
  Duplicates, and DiskUsage.
- **Enum over trait:** this is a placement/lifecycle abstraction, not a worker
  framework. Search and duplicates still stream into the table; Disk Usage keeps
  its own GPUI entity because treemap/layout/top-files state is not table-shaped.
- **Host context event:** `ToolHostContext` / `ToolHostEvent` lives outside the
  shell in `tool_results.rs`. Hosts dispatch `HostChanged(Docked | Windowed)`
  through `ToolResultSurface::handle_host_event`; the active tool body reacts
  by changing only host-sensitive chrome.
- **Shared UX:** the breadcrumb row renders one result pill plus a close button.
  Disk Usage also exposes an Open in Window pop-out button. Both are backed by
  command-catalogue actions (`view.close_results`,
  `disk_usage.open_in_window`). Closing a result reloads the root folder as a
  normal directory. Navigation and watcher reloads treat any active tool result
  uniformly.
- **Docked DU:** `view.disk_usage` now installs Disk Usage in the active tab.
  `DiskUsageView` measures its host element with `on_prepaint`, falling back to
  the window viewport on the first frame, so the same view still works in a
  standalone window. Pop-out windows opened from the shell receive a Dock in
  Tab callback that returns the same `Entity<DiskUsageView>` to the owning
  shell and closes the window.
- **Host moves preserve state:** pop-out and dock-back move the existing DU
  entity between hosts, so the scan queue/tree, progress, zoom path, selected
  node, Top-N list, and package/size-mode toggles survive the move.

# 2026-06-19 plugin seam + VLC video backend (planning)

User wants a plugin system where plugins can override internal features —
first target the video player, with **VLC** as the replacement to get
any-format support and the adjustment features (colour + denoise + sharpen)
for free, **including on video**.

## Verify-step findings
- The viewer's whole coupling to "a video player" is 9 free fns on the
  cfg-selected `platform_shell` crate (`video_overlay_*`): show / copy_frame
  / set_paused / seek / step / time / natural_size / restart / remove. All
  frame-pull: decode → BGRA bytes → gpui `img`. `ferail-shell-mac` =
  headless AVPlayer + `AVPlayerItemVideoOutput`; win32 = stubs.
- libvlc fits the same pull model: `libvlc_video_set_format(mp,"RV32",w,h,
  pitch)` + `libvlc_video_set_callbacks(lock,unlock,display,opaque)` decode
  into a buffer we own; `RV32` == BGRA. No NSView overlay; video stays a gpui
  element. Verified against VideoLAN `modules/video_output/vmem.c`.
- Colour adjust is free + live: `libvlc_video_set_adjust_int(Enable,1)` +
  `set_adjust_float(Contrast|Brightness|Hue|Saturation|Gamma)` (enum
  Enable=0..Gamma=5; Contrast/Brightness 0–2, Saturation 0–3, 1=neutral).
- Denoise/sharpen ride VLC's video-filter chain (`sharpen` + `sharpen-sigma`;
  `hqdn3d` for denoise), enabled via media options (`:video-filter=…`) or the
  instance filter list. Live param changes are less ergonomic than the adjust
  API — **the spike must confirm whether sigma can change mid-playback** or
  whether it needs a re-open. Either way enhancement reaches video, which the
  CPU pipeline can't do.
- `/Applications/VLC.app` ships `libvlc.5.dylib` + `libvlccore` + plugins
  dir, so dev integration works now. libvlc is LGPL → dynamic-link only.
- Bonus: libvlc is cross-platform, so a VLC backend is also the realistic
  path to real video on the win32 shell (today a stub).

## Decisions (locked with user 2026-06-19)
- **Static provider seam, not dynamic dylibs.** Traits + a registry,
  providers compiled in, chosen at runtime (VLC behind a cargo feature).
  Dynamic loading rejected: stable ABI + versioning + crash isolation, and a
  crashing plugin breaks "the UI must never stop." Trait shaped so dynamic
  loading could be added later.
- **Spike the binding first.** Throwaway probe of the installed libvlc
  (dlopen, vmem callbacks, BGRA layout, `set_adjust` changes pixels, and a
  sharpen/denoise filter — including whether its params change live) decides
  raw-FFI vs `vlc-rs` before committing.
- **Seam first, then VLC.** Phase 1: `VideoBackend`/`VideoStream` trait in
  `ferail-core`; move the existing AVFoundation player behind it with no
  behaviour change. Phase 2: `ferail-video-vlc` provider + a settings
  toggle; route the popup colour grade to VLC `set_adjust` and the popup
  denoise/sharpen to VLC filters, so the **whole** adjustments popup applies
  to video, not just stills.
- **Use installed VLC.app for now.** Load libvlc at runtime; VLC feature off
  by default. Bundling into the `.app` (rpath/plugin-path) deferred to Phase 3.
- **No environment variables — config lives in a Settings "Plugins" section**
  (user directive 2026-06-19). So the VLC backend gets its plugin dir via a
  `libvlc_new("--plugin-path=…")` arg (NOT `setenv VLC_PLUGIN_PATH`), and the
  VLC.app / libvlc / plugins paths + backend selection are user-set in
  Settings → Plugins. The spike's `setenv` is a spike-only shortcut.

## Spike outcome (2026-06-19) — binding resolved to raw FFI
Throwaway probe `spikes/vlc-probe/` (dependency-free; dlopen/dlsym) ran the
full path against a generated `testsrc` clip and **passed**:
- `libvlc_new` ok; **18 frames pulled** through vmem; length read back 3000 ms.
- vmem hands back correct BGRA (`screenshots/vlc-spike-before.png` is the exact
  test pattern).
- `set_adjust(Brightness=1.8)` live: mean luma 127.5 → 218.0
  (`screenshots/vlc-spike-after.png` visibly brighter). **Colour adjust on
  video works live, no re-open.**
- **Gotchas captured for Phase 2:** must `setenv("VLC_PLUGIN_PATH", …)` before
  `libvlc_new`; must pre-load `libvlccore.dylib` by full path (libvlc
  references it via `@rpath`, which our process lacks) — or set an `@rpath` at
  link time. Symbols are libvlc 3.x shapes (`media_new_path(inst,path)`,
  `media_player_new_from_media(media)`, `media_player_stop` void).
- **Decision:** thin **hand-written FFI** is confirmed sufficient — no
  `vlc-rs` dependency. Denoise/sharpen *filter* live-param change is still
  unverified (out of this minimal probe); Phase 2 will either set sigma live
  or re-open the stream as a fallback.

## Phase 1 outcome (2026-06-19) — provider seam landed, no behaviour change
- `ferail_core::video` — `VideoBackend` (`open(path, on_ended) →
  Box<dyn VideoStream>`) + `VideoStream` (copy_frame / set_paused / seek /
  step / time / natural_size). Platform-neutral, std-only. `set_adjust` is
  deliberately **not** here yet — it lands in Phase 2 with VLC so Phase 1
  carries no unused type.
- `viewer/backend_native.rs` — `NativeBackend`/`NativeStream` forward to the
  existing `platform_shell::video_overlay_*`; `Drop` does the remove. A
  `video_backend()` selector returns Native today (Phase 2 reads settings).
- `viewer/window.rs` — the viewer holds `Box<dyn VideoStream>` + a
  `video_epoch` (the boxed handle isn't `Copy`, so the frame-pull loop keys
  on the epoch instead of the old `u64` id). All 11 call sites route through
  the stream; teardown is a `drop`.
- Verified: workspace compiles warning-free, 49 core + 67 gpui tests green,
  and the headless harness decoded + drew a real frame through the seam
  (`screenshots/viewer-video-seam.png`). Native video unchanged.

## Phase 2 outcome (2026-06-19) — VLC provider + Plugins settings landed
- New crate `ferail-video-vlc` — hand-written libvlc FFI (no `vlc-rs`).
  `dlopen`s libvlccore then libvlc from the VLC.app; **format callbacks**
  decode at native resolution into a vmem `RV32`/BGRA buffer; end-of-clip via
  `libvlc_event_attach(EndReached)`; **live `set_adjust`** maps the bipolar
  grade to libvlc's 1.0-neutral ranges. `VLC_PLUGIN_PATH` is set internally
  from the settings path (the only mechanism libvlc accepts). One process-
  wide instance, cached in a thread_local (changing the path needs restart).
- `ferail_core::video` — added `VideoAdjust` + `VideoStream::set_adjust`
  (default `false`); `on_ended` is now `Send` (libvlc fires it off-thread).
- `ferail-gpui` — `vlc` cargo feature (off by default; macOS-only optional
  dep). AppState gained `video_backend` + `vlc_app_path`. Settings → **Plugins**
  page: a Player dropdown (Built-in / VLC) + an editable VLC.app path. The
  viewer resolves the choice once at open (`resolve_vlc_pref`, no settings I/O
  on the render path), `video_backend(vlc_pref)` selects with native fallback,
  and the popup colour grade routes to `stream.set_adjust` for video — the
  per-frame CPU grade is skipped when the backend grades natively (VLC).
- Verified: default build and `--features vlc` both compile warning-free;
  49 core + 67 gpui tests green; a **real libvlc integration test**
  (`ferail-video-vlc`, decodes /tmp/vlc_probe.mp4, 2.3 s) passes; Plugins
  settings render (`screenshots/settings-plugins.png`). Interactive VLC-in-
  viewer playback (select VLC, play any-format clip) is on the user — it needs
  a `--features vlc` build and writes to the live settings file.

## Phase 2b outcome (2026-06-19) — video enhance filters, all formats, quiet logs
- **All VLC formats.** Video eligibility is now backend-aware: built-in set
  (mp4/m4v/mov) always, plus a broad `VLC_VIDEO_EXTS` (mkv/avi/webm/flv/wmv/
  mpg/3gp/3g2/ts/vob/ogv/divx/rm/… ) when VLC is selected. Fixes a 3GP (and
  mkv/avi/…) opening as a Quick Look poster instead of playing.
- **Denoise/sharpen on video.** `VideoBackend::open` gained a `VideoEnhance`
  (denoise/sharpen, 0..1). **Important:** video filters only take effect as
  `libvlc_new` *instance* args (`--video-filter=sharpen:hqdn3d` +
  `--sharpen-sigma` / `--hqdn3d-luma-spat` / `--hqdn3d-chroma-spat`) — media
  options (`:video-filter=…`) are silently ignored with vmem output (verified
  with `invert`), and a *wrong* option name makes `libvlc_new` fail outright.
  So each VLC stream owns its own libvlc instance built with its filter args;
  the dylib is loaded once, instances are per-stream. libvlc can't swap the
  chain live, so the viewer **re-opens the stream on slider release**
  (`commit_video_enhance`, preserving playhead + paused). The popup shows
  Denoise + Sharpen for a VLC video (Upscale stays stills-only). Colour grade
  is separate and stays live via `libvlc_video_set_adjust_*`.
- **Quiet libvlc.** `libvlc_new("--quiet", "--no-osd", "--no-video-title-
  show")` silences the decoder/transform/ci_filters/main-filter chatter the
  user saw. One residual `[swscaler] … yuv420p to bgra` line per open comes
  straight from libav (not VLC's logger, so `--quiet` can't catch it) — the
  cost of converting decoded YUV to our BGRA pull buffer; harmless.
- Verified: default + `--features vlc` compile warning-free; 49 core + 67
  gpui + VLC integration test (now opens *with* a sharpen+denoise chain)
  green. Interactive check (play a 3GP/MKV, drag Denoise/Sharpen → release
  re-applies) is on the user.

## Phase 2d outcome (2026-06-20) — sharpen behaviour + truly-seamless paused refresh (landed)
Follow-up to 2c from live use ("sharpen looks like noise"; "the brief play on a
filter change is a bad experience — VLC doesn't need it").

**Sharpen "adds noise" — diagnosed, not a bug.** Throwaway probe (`spikes/`,
gitignored) dumped one frame of a *static* clip at sigma 0/0.05/0.3/0.6/2.0 and
diffed them: mean|Δ| 0.7→3.3→5.1→7.3 — sharpen IS applied and scales with sigma.
VLC's `sharpen` is a Laplacian high-pass with **no edge threshold**, so on flat,
grainy footage (skin) it has no real edges to enhance and only amplifies grain.
Fixes: (a) **chain order** was backwards — was `sharpen:hqdn3d…`, now
**denoise → deband → sharpen → grain**, so the source is cleaned before
sharpening; (b) `sigma` mapped to `strength*0.5` (gentle end). Sharpen on grainy
flat content still amplifies grain (inherent) — pair it with denoise.

**Live filter change: confirmed impossible via public libvlc.** Read `lib/video.c`
on master: only deinterlace/adjust/logo/marquee have live setters (they reach
the vout via private `GetVouts()` + `var_SetChecked`). Arbitrary `video-filter`
is set with `var_SetString(vout,"video-filter",…)` on the **internal vout**,
which libvlc doesn't export. So a filter change MUST re-open — no way around it
without dlsym'ing libvlccore internals (rejected: unstable ABI).

**Truly-seamless paused refresh.** Probe found the old approach's flaw:
`play()` then an immediate `set_pause`/`set_time` is **silently dropped** (input
thread hasn't started) → it played from 0. The fix (verified in the probe: then
exactly +1 frame at the seeked position, no forward motion): on re-open, play
but DON'T seek yet; defer via `video_pending_seek`. The poll fires the seek (+
re-pause) on the new stream's *first* frame (input now live) and **discards that
pre-seek frame**, keeping the previous frame on screen until the correctly-
positioned, freshly-filtered frame lands. No flash, no jump to start, no visible
playback even when paused. (`video_repause` retained for the re-pause step.)

## Phase 2c outcome (2026-06-20) — filter polish + full VLC effect set (landed)
All three tasks below shipped.
1. **Sharpen artifacts** fixed — `enhance_args` now maps `sigma = sharpen*0.6`
   (was `*2.0`); the gentle end of libvlc's 0..2 range, past which real footage
   rings/halos.
2. **Seamless re-open** — `commit_video_enhance` now drops only the old stream
   (keeps `video_frame_image`, `video_rotated`, `video_dims` so nothing
   flashes). `open_video_stream`'s restore branch opens the new instance
   *playing* and sets `video_repause = video_paused`; `video_poll_tick`
   re-pauses on the first new frame. So a paused video shows the freshly
   filtered frame and there's no black flash. `teardown_video` clears
   `video_repause`.
3. **Full VLC Basic set** — `VideoAdjust` gained `hue`/`gamma` (live, mapped
   hue→±180°, gamma→`2^v`); `VideoEnhance` gained `banding` (gradfun:
   `--gradfun-radius` 4..16 + `--gradfun-strength` 0..1.2) and `grain`
   (`--grain-variance` 0..2). `SliderId` → 9 (Hue/Gamma/Banding/Grain),
   `ColorAdjust` carries hue/gamma, `EnhanceParams` carries banding/grain
   (both ignored by the still CPU pipeline). The adjust popup shows the four
   new rows **VLC-video-only** (gated on `vlc_video`).

Design note (user asked: should the plugin ship its own popup?): **No — one
shared popup, capability-gated.** The seam (`ferail-core`/`ferail-video-vlc`)
is deliberately gpui-free (architecture invariant) and plugin code must stay
off the paint path (prime directive); a plugin-owned popup would break both and
fork one gesture (E / right-click) into two UIs. The plugin contributes
capabilities + the native impl; the viewer owns the UI and renders exactly the
controls the active backend supports (today gated via `video_adjust_native`).

Verification: `ferail-video-vlc` integration test passes against real libvlc
with all four filters + the adjust path active; `--features vlc` and default
both build; gpui tests pass. No headless screenshot of the new popup rows: they
appear only during a live VLC decode (`video_adjust_native` true), which the
static screenshot harness doesn't drive (it builds a ViewerWindow but never
opens a playing VLC stream or the adjust panel).

GPU compositing path explored (gpui has a `surface(CVPixelBuffer)` element —
real zero-copy path) but **deferred** by user ("leave for later"). Decode is
already GPU (VideoToolbox); the CPU cost is the YUV→BGRA conversion + per-frame
upload that vmem forces.

## With more time / deferred
- GPU compositing via `gpui::surface(CVPixelBuffer)` — zero-copy (native
  AVFoundation gives real CVPixelBuffers; VLC could wrap YUV so Metal does
  the conversion). Trades away CPU rotation + native colour-grade. User said
  leave for later.
- Silence the residual libav `swscaler` line (would need a YUV pull path or
  a libav log hook we don't currently reach). User said don't bother.
- Re-opening to change a video filter is a touch heavy; a live filter-param
  path would be smoother if libvlc ever exposes one.
- Bundle libvlc for a redistributable build (Phase 3).
- Dynamic third-party plugins, once the static seam has proven out.
- Same provider-seam shape fits thumbnailers / preview / metadata providers.

---

# 2026-06-16 Get Info inspector — editable popup (in progress)

Path Finder-style Get Info, modeled on the screenshot the user shared.
Working the Slow AI loop: verify → plan → approve → layer → test → note.

## Plan (approved 2026-06-16)

- **Neutral model in `ferail-core` (`entry_info.rs`):** an ordered list of
  sections, each a list of typed rows. Platform-neutral contract; every OS
  fills the subset it can read, the UI consumes one shape. Row kinds cover
  read-only text, editable toggles (Locked/Invisible/…), tags + color label,
  POSIX permission matrix, editable name, and a calculable folder/volume size.
- **Native gather `#[cfg]`-gated.** macOS uses `libc` (stat/statfs/chmod/
  chflags + getpwuid/getgrgid) and a batched `NSURL` resource-values read
  (UTI, localized Kind, dates, package/alias/custom-icon/hidden-extension
  flags). Windows/Linux return a subset in this pass.
- **Popup, not a pane tab (user call).** Hosted by the gpui-component
  `Dialog` layer — the same `window.open_dialog` primitive About + the
  copy-collision dialog use, so ESC / overlay-click / focus-trap come free.
  Child is a stateful `Entity<EntryInfoView>` so it can background-gather and
  re-gather after edits. A detached per-item window stays a follow-up.
- **Rewire the dead `GetInfo` command.** Cmd+I, the context-menu "Get Info",
  and the toolbar button currently dead-end into `on_get_info` (just focuses
  the preview pane). All now open the popup. `commands.rs:558` already expects
  "Escape closes Get Info if open".
- **Preview pane slims.** Its Format/Size/Modified/Where `DescriptionList`
  moves into the popup; preview keeps thumbnail + text only.
- **Editable from the start (user call), landed field-group by field-group**
  on top of a read-only base: tags/label, rename, Locked/Invisible,
  permissions — each with write-back + watcher refresh + undo/notification.

## Verify-step findings

- objc2 batched-read pattern confirmed at `fs-native/src/lib.rs:534`
  (`resourceValuesForKeys_error` + `arrayWithObjects:count:` + per-type
  `lookup_*` closures) — mirror it for per-file keys.
- Tags already round-trip: `shell_mac::tags::{read_tags,write_tags,
  toggle_tag}`. Color labels reuse the 7 canonical `TagColor` — no new
  color-picker widget.
- Rename already exists: `Shell::on_rename_selected`.
- Locked/Invisible are BSD flags via `chflags` (UF_IMMUTABLE / UF_HIDDEN);
  the `UF_HIDDEN` test at `fs-native/src/lib.rs:780` shows the pattern.
- `libc = "0.2"` + `objc2`/`objc2-foundation` already deps of fs-native — no
  new crates for stat/statfs/chmod/getpwuid/getgrgid or NSURL reads.
- Volume format + BSD device come from `statfs(2)` (`f_fstypename`,
  `f_mntfromname`) — no NSURL key exposes them.

## Trade-offs

- v1 targets the **lead row** (single selection). Finder-style combined
  multi-item Get Info deferred.
- "Last opened" has no NSURL key (it's Spotlight `kMDItemLastUsedDate`); v1
  shows the POSIX access date as a proxy.

## Outcome (landed 2026-06-16)

- **Model** `ferail_core::entry_info` — neutral `EntryInfo`/sections/rows
  + `PermMatrix`/`PermBits` (mode round-trip, octal, symbolic) + `Attr` +
  `EntryInfoEdit`. 4 unit tests.
- **Reads** `fsn::stat_info` (lstat: owner/group via getpw/getgr, mode, dates,
  birthtime, UF_IMMUTABLE/UF_HIDDEN; `format_local_datetime` via localtime_r;
  `volume_fs_info` statfs for format + BSD device) and
  `shell_mac::resource_values` (batched NSURL: UTI, localized Kind, added
  date, hidden-extension/package/alias). `VolumeInfo` gained `format` +
  `bsd_device`. Tests assert against the live box (apfs, `/dev/…`,
  `public.folder`, real stat).
- **Writes** `set_locked`/`set_invisible` (chflags, preserving other flags),
  `set_permissions` (chmod), `set_hidden_extension` (NSURL setResourceValue +
  NSNumber, needed the `NSValue` feature), tags reuse `toggle_tag`. chmod +
  lock/unlock round-trip tests pass.
- **Popup** `entry_info.rs` (gpui): `gather()` composes all the above off the
  background executor into the neutral record; `EntryInfoView` hosted in a
  `Dialog` via `window.open_dialog`. Editable: Locked/Invisible/Hide-extension
  checkboxes, 7 color-label swatches, a 3×3 rwx permission grid, and an
  on-demand "Calculate" folder/volume size (reuses `recursive_size`). Each
  edit writes inline (single syscall/Cocoa hop — the same pattern the
  context-menu `toggle_tag` already uses), reloads the affected directory
  (`reload_tabs_matching_paths`), and re-gathers so the panel shows truth.
- **Wiring** the dead `GetInfo` command (Cmd+I / context menu / toolbar) now
  opens the popup; the preview pane's Format/Size/Modified/Where list is gone,
  replaced by a "Get Info" button. Harness `--properties` opens it.
- **Verification:** workspace compiles, clippy-clean on the new files, all
  tests green. `screenshots/get-info-file.png` legibly shows the gathered
  record. **Headless caveat:** the `Dialog` enter-animation needs multiple
  paints; `render_to_image` captures one, so the popup is faint in
  screenshots (same limitation as every dialog in this app — it renders solid
  live). Confirm interactively with Cmd+I.

## Follow-up iteration (2026-06-16, from live use)

- **Popup no longer clips.** Body is `max_h(viewport - 220px).overflow_y_scroll()`
  with a `ScrollHandle`, so a tall record scrolls inside the window instead of
  running off the bottom edge.
- **Panel embedded in the preview pane.** The same `EntryInfoView` now runs in
  an `embedded` mode (section rows only, no name header / no own scroll) and
  the preview hosts one reused entity (`Shell::preview_info`), retargeted as the
  lead selection changes via `sync_preview_info`. The "Get Info" button is
  gone — the preview shows the live, editable panel; Cmd+I opens the same
  content as the popup.
- **Filename hazard surfacing.** New `ferail_core::name_hazards` splits a name
  into segments and flags leading/trailing/unusual whitespace, zero-width,
  control, bidi overrides, combining marks, and Cyrillic/Greek/fullwidth
  homoglyphs (curated confusable table — covers the "раypal"/RLO "gpj.exe"
  attacks). The Get Info + preview name render each flagged char highlighted
  (amber = whitespace, red = reordering/invisible/look-alike) with a tooltip
  naming it and a visible stand-in for invisibles (`⟨U+00A0⟩`, `␣`, `⇥`). 8
  analyzer unit tests. Verified: `screenshots/get-info-preview-hazard.png`
  (amber NBSP), `get-info-preview-homoglyph.png` (red Cyrillic in "paypal").
- **Test fixtures.** `test-data/filename-hazards/` ships a `generate.py` +
  README producing 15 sample names (one per trick) into a git-ignored
  `samples/` folder — the names carry control/bidi chars that git and editors
  mangle, so the reviewable generator is what's committed.

## Follow-up iteration 2 (2026-06-16, from live use)

- **Phantom-selection bug fixed (verified first).** After switching folders the
  file pane painted a focus ring on a row while the preview said "No
  selection". Verified it was the *file pane* lying: `navigate` clears
  `tab.selection`/`anchor`/`lead`, so the model genuinely has no selection
  (preview was right). The blue ring was gpui-component's internal
  `TableState::selected_row`, which `refresh_file_list_selection_in_tab` only
  ever *set* (when lead was `Some`) and never cleared. Added
  `TableState::clear_selected_row` (no `SelectRow` emit → no suppress-counter
  re-entrancy, no scroll) and call it when lead is `None`, so the primitive's
  overlay always tracks the one selection model.
- **Preview pane default widened** 280 → 380 (range 260–640) so the embedded
  Get Info panel — permission grid + swatches + label column — fits without a
  drag.
- **Folder size reused, not rescanned.** Get Info now takes a `known_size`
  (the file list's recursive folder total) and shows it with a recalculate
  affordance (`↻`) instead of "Calculate". `SizeValue::Known` gained
  `refreshable`. The embedded preview view upgrades a "Calculate" placeholder
  in place when the async size lands after selection (`retarget` +
  `EntryInfo::size_is_calculable`), so it never sticks on "Calculate" once the
  column knows the number.
- Verified: `screenshots/get-info-folder-size.png` (wider pane, "Size 2.3 GB ↻"
  on a folder, full permission grid visible). Build + full suite green,
  clippy-clean on touched files.

## Follow-up iteration 3 (2026-06-16, from live use)

- **Preview width now persists on resize (real bug).** `maybe_persist_splitter`
  throttled writes to once / 500 ms and was only called from `on_resize` —
  nothing flushed after the drag stopped, so the final width (if its event
  landed inside the throttle window) was silently dropped. The doc comment
  claiming "renders re-check and flush" was false (render never called it).
  Replaced with `schedule_splitter_save`: the first resize tick arms a
  trailing debounce (`cx.spawn` + timer) that reads the *latest* widths when
  it fires, so the width at drag-end always lands and a drag costs ~1–2 writes.
  Default also nudged 280 → 380, but persistence is the real fix.
- **Preview keeps the bounded text/code box (scroll-chaining deferred).**
  First tried collapsing the inline text/code box into the one pane scroll
  (drop `max_h`, `overflow_x_scroll` only) so a vertical wheel flows from the
  file into the details — but then a big file buries the Get Info details far
  down the pane, so we reverted to the bounded `max_h(280) + overflow_scroll`
  box (both axes — vertical keeps details reachable, horizontal keeps no-wrap
  code readable). The nested box does trap the vertical wheel until the cursor
  leaves it; the proper fix is scroll-chaining via a custom `on_scroll_wheel`,
  written up as a TODO (gpui's `overflow_scroll` auto-captures the wheel and
  it's not headlessly testable). Also shrank the code-preview text 11 → 9 px.
  Verified: `screenshots/preview-bounded-small.png`.

## Follow-up iteration 4 (2026-06-16, from live use)

- **Get Info is now a standalone window, not a modal.** Was a centered
  gpui-component `Dialog` tied to the host window; converted to a real OS
  window via `cx.open_window` (same pattern as Settings / Disk Usage), so it's
  resizable, movable, and **multi-instance** — every Cmd+I opens another
  window, no singleton guard, so you can compare several files side by side.
  The window title carries the file name; the in-content header keeps the
  hazard-highlighted name (the native title bar can't color hazards). The
  non-embedded render now fills the window (`size_full` + flex body) instead
  of the dialog's `max_h`. Edit-error toasts: `Root::render` doesn't auto-draw
  the notification layer, so the window's render adds
  `Root::render_notification_layer` (the embedded-in-preview path still routes
  toasts to the shell window). The same `EntryInfoView` serves both the window
  and the embedded preview via the `embedded` flag.
- **Removed the redundant Get Info (i) icon** from the preview action row — the
  preview already shows the full panel, so it only duplicated what's on screen
  (Cmd+I / context menu / toolbar still open the window).
- Not headlessly screenshottable (it's a separate window the single-window
  capture harness doesn't grab), but it opens without crashing and reuses the
  already-verified embedded render. Confirm resize/move/multi-open live.

## With more time / deferred

- **Inline rename** in the popup (the name is read-only there; the existing
  RenameSelected/F2 flow still renames). Window-threading for a notification
  from the input's PressEnter subscription is the only blocker.
- **Undo for attribute/permission/tag edits** (toggling again reverts; rename
  would reuse `UndoOp::Rename`). Today only the native write + reload happen.
- **Stationery pad** + **custom icon** reads (Finder-info getattrlist, not an
  NSURL key) — shown only when supported; omitted now.
- **Combined multi-item Get Info** and a detachable per-item window.
- **Windows/Linux gather**: the unix arm of `stat_info` already yields
  perms/dates; `resource_values`/volume-format return empty off macOS. Real
  Win32 (NTFS attrs, `GetVolumeInformation`) lands with that port.

# 2026-06-15 syntax highlighting for C#, C, C++, Bash, Swift, CMake (landed)

gpui-component highlights a language only when its `LanguageConfig`
carries a non-empty query. Several grammars it ships have an empty
query (C#, C, C++, Bash, Swift, CMake, plus GraphQL/Proto/CMake) — the
grammar compiles but nothing colors. User hit this with Kotlin
(actually fine — has a query) then C# (empty query).

- **`crate::syntax_extra`** reuses the `tree_sitter::Language` already
  in the highlighter registry and registers a vendored query for each
  gap language — so **no grammar-crate deps and no tree-sitter version
  coupling** (the grammar is whatever gpui-component built). The
  queries are each grammar crate's own `queries/highlights.scm`,
  copied under `src/syntax_queries/` with attribution; capture names
  outside the registry vocabulary degrade via its `.`-prefix fallback
  (`type.builtin` → `type`), and a query that fails to compile falls
  back to plain text (logged, never panics).
- Run once in `shell::init` (both GUI and screenshot paths).
- GraphQL/Proto skipped: no bundled highlights query in their crates.
- Verified each of the six colors a representative file
  (keywords/types/strings distinct). 198 tests green, clippy zero.
- **Coverage now**: the ~22 query-shipping languages from gpui-component
  plus these 6 vendored = ~28 highlighted. Adding more is a one-line
  `(name, include_str!(...))` entry if a grammar+query exist.

# 2026-06-14 preview fixes: no folder preview, smaller no-wrap code (landed)

Follow-up tweaks from using the preview pane.

- **No folder previews.** Selecting a folder was firing the QL
  thumbnail + text-read providers (qlmanage on a directory is wasted
  work; the text read just fails) and showing a broken/empty media
  box. `request_preview_for_row` now skips directories (the 3
  selection sites route through it), and the render shows folder
  metadata only — no media box.
- **Smaller, no-wrap code.** Code blocks render at 11px (via
  `TextViewStyle::code_block` text_size, which overrides the theme's
  mono size) and `whitespace_nowrap`; the block scrolls both axes so
  long lines stay readable instead of folding. Markdown prose still
  wraps (no-wrap only on code) and headings keep their size.
- **Highlighting coverage reality.** gpui-component highlights a
  language only when its `LanguageConfig` carries a non-empty query.
  In this pinned rev that's ~22 languages (rust, go, js, ts, json,
  toml, yaml, python, ruby, java, html, css, sql, lua, php, zig,
  markdown, diff, elixir, scala, make, jsdoc, tsx). **C#, C, C++,
  Bash, Swift, GraphQL, Proto, CMake ship the grammar but an empty
  query → they render plain.** A vendored query + `LanguageRegistry::
  singleton().register(...)` could fill the gaps if needed.

# 2026-06-14 syntax highlighting + formatted markdown in the preview (landed)

Upgraded the inline text preview from plain mono to
gpui-component's `TextView` (the user pointed out the library ships a
highlighted code viewer).

- **Why TextView over CodeEditor:** `text::TextView` is a stateless
  `IntoElement` that parses *off the UI thread* (`background_spawn`)
  and caches the result keyed by element id — so a stable id means
  one cached parse that re-runs only when the selected file's content
  changes (`set_text` short-circuits on equal content). The
  `CodeEditor` (InputState) path is a full editor entity — wrong shape
  for a read-only pane.
- **One helper, `to_markdown_source`:** `.md`/`.markdown`/`.mdx` pass
  through (TextView renders them *formatted* — headings, lists,
  links); every other text file is wrapped in a fenced code block
  tagged with its extension. The highlighter accepts extensions as
  language aliases (`rs`/`py`/`ts`/…), so no big mapping table. The
  fence is grown one longer than the longest backtick run in the file
  so a file containing ``` can't break out (unit-tested).
- **Grammars:** enabled the full `tree-sitter-languages` feature on
  the gpui-component dep (user chose "everything ~35" over a curated
  subset). Each grammar is a C-compiled crate — a real one-time build
  cost, sanctioned.
- Verified: Cargo.toml renders TOML-highlighted (section/keys/strings
  in distinct colors), CLAUDE.md renders as formatted markdown
  (`screenshots/preview-highlight-toml.png`, `preview-markdown.png`).
  The worker's read/detect/cache from yesterday is unchanged; only the
  render swapped. 198 workspace tests green, clippy zero.

# 2026-06-13 inline text/code preview in the preview pane (landed)

The pane already showed a Quick Look thumbnail for everything; a QL
thumbnail of a source file is a useless tiny image, so text files now
render their actual content.

- **`text_preview.rs`** mirrors `preview.rs` (per-path LRU cache,
  Pending dedup, results re-enter via `shell.update`) but reads text
  instead of fetching a thumbnail. Worker reads ≤128 KB, decides
  text-vs-binary itself (NUL byte or invalid UTF-8 mid-buffer ⇒ not
  text; a multibyte char split at the read boundary is tolerated), and
  returns the content capped at 500 lines. No dependency on magic
  being sniffed — detection is self-contained in the read.
- **One selection event, two providers.** Folded the text request into
  `preview::request`, so the existing 3 selection call sites are
  untouched; the worker sorts text from binary and the render shows
  inline monospaced text when it's text, the QL thumbnail otherwise.
- **Render**: a wrapped, vertically-scrolling monospaced block (max
  280 px) above the metadata. Wrap rather than no-wrap — the pane is
  narrow and `overflow_y_scroll` can't reveal horizontally-clipped
  long lines (caught in the first screenshot). Empty files show
  "(empty file)" rather than a blank box.
- 5 worker unit tests (utf8 / NUL-reject / invalid-utf8-reject /
  empty / line-cap). Verified: CLAUDE.md renders its text
  (`screenshots/text-preview.png`); a PNG still shows the QL thumbnail
  (NotText path).

# 2026-06-13 command palette: Enter-runs-top-match over the catalogue (landed)

The Cmd+K shortcuts overlay was already a searchable, grouped,
click-to-dispatch list — this finishes the palette half.

- **Completed the action map.** `action_for_command` gained the
  commands this session's features added (sort ×4, Open Viewer,
  Copy/Paste/Move-Paste, Empty Trash) plus Reopen Tab / Close Window,
  so they're no longer inert rows.
- **Enter runs the top match.** Extracted `filtered_groups` so the
  render, the highlight, and the Enter target all agree on display
  order; `palette_top_command`/`palette_top_action` return the first
  dispatchable match. The shortcuts-help input's PressEnter
  subscription closes the overlay and dispatches it. The top match
  renders pre-highlighted (same accent as hover) so the Enter target
  is visible.
- **Harness fix that made this testable.** `--keys` was applied
  *before* `--shortcuts-help` opened the overlay, so keystrokes hit
  nothing — reordered so the overlay opens first, then keys drive it.
  Verified end-to-end: filter "Show Hidden" + Enter toggles hidden
  files and closes the palette (`screenshots/command-palette.png`).
- **Deferred:** arrow-key selection between matches (InputState
  consumes some keys; Enter-runs-top-match + filter refinement covers
  the common case without fighting it) and a distinct palette title.

# 2026-06-13 Recents sidebar section (landed)

A recently-visited-folders section between Favorites and Browse.

- **No new data, no schema change.** Recents is a recency-ordered
  *view* over the existing `folder_usage` visit log (the Ant Trail
  already stamps `last_access_unix` on every navigate — we'd just been
  discarding the timestamp at hydration). `ProcessState.recents` is an
  in-memory `Vec<PathBuf>` (cap 12, most-recent-first) so the sidebar
  render never touches SQLite: front-inserted on each navigate
  alongside `record_ant_visit`, hydrated at startup from
  `load_recent_folders` (ORDER BY last_access DESC).
- **Hydration merges, doesn't replace.** The first `--navigate`
  records a recent *before* the async DB load lands, so an
  "adopt only if empty" guard silently dropped the hydrated list (the
  screenshot caught it). Fixed to merge: session-live entries stay at
  front, DB history fills in behind, deduped + capped.
- **RecentsSection** mirrors FavoritesSection but simpler — no drag,
  no availability state, no rename. Click navigates (Cmd-click → new
  tab); row context menu = Reveal / Remove from Recents / Clear
  Recents; header context menu = Clear. Hidden entirely when empty
  (`build_recents_section → None`) so a fresh profile has no empty
  section. Collapse state persists in app_state (`recents_collapsed`).
- **Remove/Clear are honest about the coupling.** Recents and the
  Ant Trail heat tint are the same `folder_usage` signal, so "Remove
  from Recents" forgets that folder's visit row (`forget_folder_visit`
  — also clears its heat) and "Clear Recents" wipes the log
  (`ResetScope::AntTrail` — resets all heat). Documented as
  intentional; a decoupled store is a TODO if it ever bites.
- Verified end-to-end via harness: multi-`--navigate` populates the
  section in the right order (`screenshots/recents-sidebar.png`); a
  fresh run hydrates from the DB and merges the new visit on top.
  Context-menu actions open on right-click (no headless synthesis) so
  Remove/Clear need a hands-on check.

# 2026-06-13 toolbar density: sort dropdown + action overflow (landed)

The discoverable-controls half of the toolbar-density TODO.

- **Sort dropdown** (gpui-component's `Button::dropdown_menu`, first
  use in the app): Name / Size / Kind / Date Modified, the active
  column checkmarked, the button glyph showing direction
  (`sort-ascending` / `sort-descending` from the upstream icon pack —
  the merged FeraAssets serves both packs at `icons/...`). Each item
  dispatches a real action (`SortByName` etc.), so it's catalogue-
  and palette-discoverable, not a one-off closure.
- **`Shell::set_sort_column`**: re-selecting the active column flips
  direction; first pick of a column uses a Finder-like default
  (Name/Kind ascending, Size/Modified descending). Pure in-memory
  re-sort via the new `apply_sort_column` enum helper —
  `apply_sort` (the `--sort` CLI path) now delegates to it, verified
  unchanged via `--sort modified-desc`.
- **Overflow "⋯" menu**: Show Hidden (check), Get Info, Open Viewer,
  Disk Usage, Empty Trash — all dispatching existing actions, so they
  hit the current selection/folder exactly like their keyboard /
  right-click twins.
- Win32 title-bar drag gotcha: `DropdownMenuPopover` can't take an
  `on_mouse_down`, so each dropdown trigger is wrapped in a
  mouse-down-stopping `div`, same trick the sidebar toggle uses.
- **Deferred on purpose** (narrowed the TODO, didn't claim done):
  grid/icon view mode is a whole new file-pane render path; grouping
  is a new sort/render model. Neither is "density" work.
- Verified: compile/clippy-zero/190 tests/win-cross all green;
  `screenshots/toolbar-density.png` shows the four right-aligned
  buttons. Dropdowns open on mouse-click (no headless synthesis), so
  the menu interactions themselves need a hands-on check.

# 2026-06-13 trash: undo, Empty Trash, per-volume awareness (landed)

The Trash slice of the file-ops arc.

- **Trash-undo.** `move_to_trash` now returns the item's resulting
  location inside the Trash (`trashItemAtURL`'s out-param was being
  discarded [mac]; Windows `SHFileOperationW` reports nothing →
  `Ok(None)`, Recycle-Bin restore stays a parity TODO). The handler
  collects `(original, trashed)` pairs in the worker and registers
  `UndoOp::TrashRestore` — Cmd+Z renames items back, refusing to
  overwrite if the original path exists again. The premature "Moved
  to Trash" toast also moved to completion (it used to fire before
  the op ran).
- **Empty Trash** (`file.empty_trash`, Cmd+Shift+Delete — bound as
  `cmd-shift-backspace` in extras since the Shortcut DSL lacks a
  Delete key): background count → counted confirmation dialog with a
  danger button (the one op with no undo, hence the only one that
  confirms) → background delete → notification + reload of any tab
  browsing a trash dir.
- **Per-volume awareness**: `trash_dirs()` = `~/.Trash` + each
  mounted volume's `.Trashes/<uid>` (libc::getuid, target-gated dep).
- **TCC honesty**: a terminal-spawned dev build can't read `~/.Trash`
  (Operation not permitted). First version reported that as "Trash is
  already empty"; now an unreadable trash keeps the confirmation
  (count-unknown wording) and a 0-deleted outcome surfaces as a
  permission error pointing at Files & Folders access. The real fix
  is the `.app` bundle's stable TCC identity (already in TODO).
- Verified via harness keystrokes: Cmd+A → Cmd+Backspace → Cmd+Z
  round-trips files back into the folder; a trash-only run leaves the
  folder empty (items genuinely in Trash); the confirmation dialog
  screenshot is `screenshots/empty-trash-dialog.png` (never
  confirmed — the user's real Trash was not emptied during testing;
  three tiny fera-trash test files were left in it).

# 2026-06-13 drag-into-app: drop targets feeding the transfer worker (landed)

Same-day follow-on to the file-ops arc; dnd-spec §3.5/§3.6.

- **Three drop surfaces**, one handler: folder rows in the file table
  (fork addition `TableEvent::ExternalDrop` — the delegate can't
  reach the Shell, so the drop rides the existing event channel,
  with `stop_propagation` so the pane target underneath doesn't
  double-fire), the file-pane background (→ current directory), and
  Browse/Volumes tree rows (they hold a weak Shell handle already).
  All converge on `Shell::handle_external_drop`.
- **`TransferMode::Auto`** — the spec's modifier table: same volume →
  Move, cross-volume → Copy, Option forces Copy, Cmd forces Move.
  Resolution happens *in the worker* next to the existing
  same-volume probe (stat is banned on the UI thread); the task label
  reads "Transferring…" until resolved, the completion notification
  uses the effective verb. Same-folder drops no-op; Option-drop
  duplicates (Finder parity).
- Internal row drags and external Finder drags arrive as the same
  `ExternalPaths` payload, so one path covers both.
- gpui can't synthesize OS drag sessions headlessly — compile/tests/
  clippy green and the handler logic reuses the verified
  spawn_transfer_op; the drag gesture itself needs interactive
  verification (drop from Finder, drop row-onto-folder, Option-drop).

# 2026-06-13 file ops: copy/paste/move with progress + collisions (landed)

Spec: `docs/features/FILE_OPS.md`. The biggest TODO gap — Ferail
can now actually manage files, not just browse them.

- **Engine in `ferail-fs-native/src/file_ops.rs`** — pure,
  synchronous, worker-thread: `plan_transfer` (walk + byte totals +
  top-level conflict scan, rejects copy-into-own-subtree),
  `run_copy`/`run_move` under one `CollisionPolicy`
  (Replace/KeepBoth/Skip). 8 MiB chunked copies so progress ticks and
  cancel lands mid-file; a cancelled partial file is deleted, files
  whose last byte landed survive (the cancel check sits *after* the
  read, before the write — first version deleted complete files).
  Symlinks recreated, never followed. Same-volume detection via
  `MetadataExt::dev()` gives move its rename fast path. 8 tempdir
  unit tests.
- **Cancel buttons are real now.** `ActiveTask` carries
  `Option<Arc<AtomicBool>>` (`begin_with_cancel`); the task panel
  renders ✕ for tasks that have one. First consumer: transfers.
- **`spawn_transfer_op`** (shell/file_ops.rs): plan on bg → collision
  dialog if needed (gpui dialog, NOT NSAlert; all three policies as
  explicit buttons since the pinned gpui-component rev won't draw the
  ok/cancel footer next to custom children; dialog dismiss = cancel
  via dropped channel senders, so nothing can wedge the task) → run
  with progress coalesced to ~10 Hz registry updates → end + reload
  broadcast + notification + undo. Same-directory paste skips the
  dialog (auto Keep Both, like pasting next to the original should).
- **Clipboard verbs**: Cmd+C writes real file URLs to the general
  pasteboard [mac] (Finder interop both directions), Cmd+V copies,
  Cmd+Option+V moves — Finder semantics, no Cut in v1. Pasteboard
  *reading* (`clipboard_read_file_urls`) is new; win32 stubs document
  the CF_HDROP parity path.
- **Undo**: `MoveBack` (same-volume moves only) and `RemoveCreated`
  (copies that replaced nothing) — both deliberately conservative;
  undoing a replace would delete the only surviving version.
- **`spawn_file_op` failures now notify** instead of log-only —
  duplicate/compress/trash/rename/new-folder all surface errors.
- Verified end-to-end by driving real keystrokes through the
  screenshot harness: Cmd+C in one process, Cmd+V in another
  (pasteboard persists), byte-identical results, collision dialog
  screenshot (`screenshots/file-ops-collision.png`), Esc-cancel
  leaves dest untouched, move empties the source. The cancel button
  and a cross-volume transfer need interactive testing.

# 2026-06-13 honest tree chevrons, ancestry guides, sidebar polish (landed)

Browse/Volumes tree affordances stopped lying:

- **Honest chevrons.** `TreeChild.has_subdirs` is resolved at
  enumeration time (`dir_has_subdir`: early-exit read_dir, worker
  thread on the async path; the documented-sync `ensure_tree_children`
  reveal path carries it too). Leaf folders render no caret instead of
  a chevron that expands to nothing. Hidden subdirs count — the caret
  may reveal nothing while Show Hidden is off, which beats scanning
  twice.
- **Ancestry guide lines.** `TreeGuide` (Blank/Vertical/Tee/Corner)
  per indent column, computed by the row builder which alone knows
  last-visible-child status (`trunk` push/pop during the walk); render
  is a pure read. Absolutely-positioned 1px lines span the row height
  so connectors join across rows; leaf rows extend a stub through the
  empty caret slot so line lengths read consistently.
- **Icon warming.** `IconCache::warm_folder_icon` caches even failed
  NSWorkspace fetches so the every-render "what's still uncached"
  collection converges; `start_tree_icon_warm` chunks fetches on a
  timer off the render path.
- **Unified section headers.** New `LabeledMenu` replaces
  `SidebarGroup<SidebarMenu>` for Locations so all four sidebar
  sections share one semibold `section_header`.
- **Preview-pane scroll.** The pane body gets a persistent
  `ScrollHandle` with a selection-change edge detector resetting it,
  so short windows can reach the action buttons.
- Verified: `screenshots/tree-guides.png` (guides join across rows,
  `Games` carets, leaf folders don't), workspace tests green, clippy
  zero (two targeted allows: `large_enum_variant` on ShellSidebarItem,
  `too_many_arguments` on the recursive row builder).

# 2026-06-12 live volume mount watch (landed)

`volume_observer.rs` in shell-mac
mirrors the theme observer (declare_class target, thread_local
Retained, idempotent start) but registers on *NSWorkspace's own*
notification center for DidMount/DidUnmount/DidRenameVolume.
`process_state::start_volume_watch` (called once in main.rs) drains a
coalescing channel: re-lists volumes on the background executor
(cached NSURL keys, O(mounted)), swaps `ProcessState.volumes`,
re-probes Favorites Available↔Unmounted (`refresh_mount_states`, the
existing background pass), and notifies every live shell. Sidebar
Volumes section updates without restart. Win32: stub; real impl is
WM_DEVICECHANGE on the theme observer's message window. Hardware
verification (plug/unplug a disk) is on the user.

# 2026-06-12 video playback in the viewer/slideshow (landed)

AVPlayerView overlay
[mac] over the stage rect — native aspect-fit, hardware decode,
audio, inline hover controls. Key wrinkle: the objc2 0.2-generation
framework crates we pin predate AVFoundation bindings (start at objc2
0.6), so `video_overlay.rs` reaches AVPlayer/AVPlayerView through
runtime `AnyClass::get` + `msg_send`, with two `#[link]` blocks to
load the frameworks. Eligible: mp4/m4v/mov; auto-play on becoming
current; slideshow does NOT arm the interval timer on video entries —
`AVPlayerItemDidPlayToEndTimeNotification` advances instead, path-
tagged through a channel so an end queued behind a manual nav is
dropped. Overlay lifecycle is render-time change-detected sync (same
trick as title sync); `Drop` is the teardown backstop. Known v1
limits in VIEWER.md (no zoom on video, fullscreen hover chrome sits
under the overlay, screenshots can't capture it). Smoke-tested
through the headless harness with an ffmpeg test clip — full AVKit
path runs clean; interactive playback check is on the user.

# 2026-06-12 viewer window: big preview, slideshow, sticky zoom (landed)

Spec: `docs/features/VIEWER.md`. Six iterations, all green
(`cargo clippy --workspace` zero, full test suite, screenshots
`screenshots/viewer-window.png` / `viewer-preview-pane.png`).

- **New module `ferail-gpui/src/viewer/`** in four layers: `loader`
  (full-res decode + byte-budget LRU), `stage` (pure zoom/pan
  geometry, zero gpui types), `window` (the entity), `playback`
  (slideshow epoch state). 27 new unit tests across the pure layers.
- **Two-tier decode.** `image` crate (now with jpeg/gif/webp/bmp/tiff
  features) decodes raster formats off-thread, longest edge capped at
  8192 px; everything else (HEIC, PDF, video) falls back to a 2048 px
  Quick Look thumbnail [mac]. Cache budget 384 MB, LRU by bytes,
  Pending markers dedup in-flight decodes and are never evicted.
- **Sticky zoom is window state, not image state.** `StageState
  {mode, center-as-image-fraction}` survives navigation verbatim —
  zoom 2.5× into the top-right corner and every next image shows its
  top-right corner at 2.5×. Pan center being *relative* is what makes
  it transfer between differently-sized images (unit-tested).
- **One reusable window.** `ProcessState.viewer_window` holds
  `(WindowHandle, WeakEntity)`; reopening retargets + activates
  instead of stacking. Stale weak handle after close = next open
  builds fresh. No Drop bookkeeping needed.
- **Slideshow with epoch staleness.** Timer ticks carry an epoch;
  play/pause/manual-nav/interval-change bumps it, so a stale tick is
  inert — same idiom as enumeration cancel flags. Manual nav while
  playing re-arms; zoom/pan *pauses* (inspecting beats advancing).
  Interval cycles 2/3/5/10 s via toolbar button (deviation from the
  spec's dropdown — fewer moving parts, same reach) and persists as
  `viewer_slideshow_interval` in gpui-state.txt.
- **Fullscreen** via `window.toggle_fullscreen()`; chrome hides, the
  top 56 px strip reveals the toolbar on hover (pure mouse-position
  state, no timers). Esc exits fullscreen first, then closes.
- **Keys**: `Cmd+Y` opens (catalogue command `view.open_viewer`, so
  menu/palette pick it up); viewer-local keys (arrows, Space
  play/pause, Cmd+=/−/0/1, Cmd+Ctrl+F, Esc) bind in the new
  `"Viewer"` key context in `keymap::install_extras` — first
  secondary window with its own context. [mac] chords; win-parity
  remaps tagged in the spec.
- **Preview-pane thumbnail is now a button** that opens the viewer.
- **Screenshot harness** gained `--viewer <path>` (mirrors the
  `--disk-usage` arm).
- Deviations from spec, deliberate: stale decode results still
  cx.notify (render reads current index; a no-op repaint is cheaper
  than generation plumbing); footer omits file size for now; interval
  control is a cycle button, not a dropdown.
- Also: zeroed the two clippy warnings the folder-sizes session left
  (`large_enum_variant` on ShellSidebarItem, `too_many_arguments` on
  the tree walker) with targeted, justified allows.

# 2026-06-12 folder sizes in the Size column (landed)

Directory rows now get recursive sizes: computed off the UI thread,
cached in the metadata DB, revalidated against the folder's mtime.

- **Walker reuse, not a new walker.** `bundle_rolled_up_size` in the
  disk-usage scanner was already the exact cancel-aware, symlink-safe,
  error-absorbing DFS we needed; it's now `pub fn recursive_size` in
  `ferail-fs-native` and the bundle path calls it.
- **Cache = `folder_sizes` table** (DB v3 → v4, additive): path PK,
  the folder's own `mtime_unix` at compute time, logical size,
  `computed_at_unix`. Validity check is `cached mtime == live mtime`,
  same contract as the `files` cache. Wired into `ResetScope::All`
  and `::Caches`.
- **Worker (`folder_sizes.rs`) mirrors `prefetch.rs`** but *streams*:
  one instant batch of cache hits first, then each computed folder as
  its walk finishes — a deep folder doesn't hold up the shallow ones.
  Results are keyed by `NodeId`, not row index, because rows can
  re-sort mid-flight. Cancel flag lives on the Tab next to
  `load_cancel` and is flipped by the same navigation paths.
- **DB-attach re-kick.** The metadata DB opens asynchronously, so the
  startup load's size pass runs cache-blind and can't persist.
  `start_metadata_load` completion calls
  `restart_folder_size_passes` — one redundant walk of the startup
  dir on a cold start buys a durable cache for everything after.
- **Sort honesty.** `FileListDelegate.current_sort` records the
  active header sort; when sizes land while sorted by Size, the
  delegate re-sorts so folders don't sit in stale positions (Finder
  behaves the same). Folder rows now also carry their real `size`,
  so Size-sorting and the status-bar total include them.
- **Known limitation (by design):** POSIX dir mtime only changes on
  *direct*-child changes, so deep edits don't invalidate the cache.
  FSEvents-driven invalidation through the existing watcher is the
  follow-up if this bites.
- Verified: `screenshots/folder-sizes.png` (~/mars shows 389.1 MB /
  12.2 MB), DB rows match `du` ground truth, scratch-dir mtime-bump
  recomputes (102,400 → 153,600 bytes), workspace tests green.

# 2026-06-12 review-sweep leftovers (landed)

Closed the items the correctness sweep below deliberately parked:

- `1ee08e6` tab drag-reorder actually works: the chips themselves are
  now drop targets (the natural release-over-a-tab gesture previously
  landed on a chip with no `on_drop` — the only targets were the 6-DIP
  gaps). Drop on chip = take its slot; accent edge previews the side.
  Pure `chip_drop_gap_index` + tests. Needs one interactive drag to
  confirm visually.
- `8b6e03c` boundary canonicalization: the ARCHITECTURE.md identity
  contract is now true at every external edge — typed breadcrumb
  (background canonicalize via `navigate_external`), persisted
  last_dir, favorites DB hydrate, watcher root — and the two UI-thread
  `canonicalize` stats in the favorite-toggle handlers moved to
  workers (`spawn_in` + `apply_toggle_favorite_canonical`). Shared
  helper `shell::path::canonicalize_for_identity` + symlink test.
- `7543f1c` clippy to zero (46 → 0): multi_table fork gets a policy
  `#![allow]` for style lints; type aliases for the complex handler/
  tuple types; FavoriteId + SortColumn implement `FromStr`; the rest
  mechanical. Keep the gate at zero from here.

Still parked on purpose: the cross-platform `[patch]` decision
(TODO.md "Cross-Platform Build") and pushing the branch.

---

# 2026-06-12 correctness review sweep (landed)

Full-project audit (correctness / stability / portability / design
precision) followed by a fix sweep, one commit per finding:

- `cfa561e` hidden-file semantics: `FileEntry::hidden` resolved at
  enumerate time (UF_HIDDEN / FILE_ATTRIBUTE_HIDDEN), all filters off
  name heuristics. macOS gains Finder-correct `~/Library` hiding.
- `51379fa` metadata DB path per platform — Windows persisted nothing
  before (%APPDATA% arm added; XDG fallback for other unix).
- `2548358` context menu builds with zero shell queries: Open With
  warm cache + dispatch resolves slots against the same cache
  (re-fetch could reorder and open the wrong app); tags reuse row data.
- `586b04b` favorites "+"/drop validate on workers, not the UI thread.
- `74774cf` shell-win32: clipboard HGLOBAL freed on SetClipboardData
  failure; symlink/junction skip in both recursive walkers; DC checks.
- `29ff68b` streaming pipeline addresses tabs by index — active-swap
  hack retired (see the struck-through Phase A+B trade-off below).
- `4f73bde` tabstrip select resolves by TabId (uniform with close);
  theme-observer thread contract documented in both shell crates.
- `09e9f20` pure-logic tests: reorder gap math, closed-tab stack
  eviction, Zone.Identifier parsing (extracted platform-neutral).
- `05af43a` clippy gate unblocked (deny-level approx_constant) +
  mechanical sweep; multi_table fork left untouched by policy.
- `9e40fca` path-identity contract: lexical `normalize_path_key` in
  both NodeId maps; case/symlink/`..` deliberately not folded —
  contract + rationale in ARCHITECTURE.md Data Model.

Verification per item: cargo check (mac + windows-msvc cross where
applicable), full test sweep (132 → 149 tests over the sweep), and
screenshots for UI-touching changes.

---

# Windows / Instances / Tabs (in progress)

## Phase A+B-iter3 — filter on Tab (landed)

Audit flagged filter as window-level vs. spec §3.1's "each tab owns
its filter." Flipped per user direction.

### Decisions

- **`filter_text` and `filter_input: Entity<InputState>` moved onto Tab.** Each tab has its own filter Input entity. Title-bar render reads `self.active_tab().filter_input`, so cursor / focus / typed value are naturally per-tab — switching tabs shows the new tab's filter without imperative `set_value` calls.
- **Filter Input + subscription are constructed in `Shell::build_tab`** alongside the table Input + subscription. Each is stored as a non-Clone field on Tab and drops when the tab closes.
- **Filter subscription closure captures `tab_id`** and writes to `self.tabs[idx].filter_text`, then calls `load_path_for_tab(tab_id, ...)`. So typing in one tab never reloads another.
- **`load_path_for_tab` reads `tab.filter_text` directly** (was `self.filter_text` at the window level).
- **`on_clear_filter` and `focus_filter_input` operate on the active tab's filter Input.**
- **Screenshot harness's `--filter X` flag now writes the active tab's filter, not a window-shared one.**

### Outcome

- `cargo check --workspace --all-targets` clean.
- `cargo test --workspace` all green.
- `screenshots/phase-c-per-tab-filter.png` shows the filter text rendering correctly in the title bar.
- Cmd+T opens a fresh tab with an empty filter; switching tabs swaps the filter input contents accordingly (verified manually).

### Trade-offs

- Each Tab carries an `Entity<InputState>` + a `Subscription`. Memory cost per tab is small; the simplicity of "rendering reads the right thing automatically" is worth it. The alternative (single Input mirroring active tab's text) would have needed `set_value` on every tab switch and lost per-tab cursor/focus state.

---

## Phase D — closed-tab reopen + tab drag-reorder (landed)

Goal: cover the spec §3.3 operations that the multi-window plumbing
made meaningful. Cmd+Shift+T undoes a Cmd+W; tabs reorder by drag
within the strip.

### Decisions

- **Closed-tab stack lives on `ProcessState`**, not per-window. Matches the spec §3.3 / §1.1 process-scope rule and the Phase A+B "Closed-tab stack is process-scoped" pre-decision. Cmd+Shift+T in window B can resurrect a tab closed in window A. Capped at 16 (`CLOSED_TABS_CAP`); older entries fall off the front. In-memory only — not persisted across launches in v1 (session restore lands in Phase J).
- **`ClosedTab` is plain data, no GPUI entities.** Lives in `shell::tab` next to `Tab`. Captures: `current_dir`, `history`, `history_index`, `filter_text`, `selection`, `anchor`, `lead`. Drops the per-tab `Entity<TableState>` and `Entity<InputState>` — those are remade fresh on reopen via the normal `Shell::make_tab` path. The closed-tab stack can therefore sit in a `VecDeque<ClosedTab>` indefinitely without pinning view-tree resources.
- **Sort restore deferred.** Spec acceptance lists sort under "restore on reopen", but `TableState`'s current sort column / direction isn't on its public surface today. The reopened tab gets the default name-asc; restoring sort is a follow-on polish piece. Filter and selection *do* restore.
- **Selection restore is best-effort by spec design.** `NodeId`s captured at close are still valid (singleton `NodeStore`), so when the streaming reload's `Done` fires, the existing reconcile-against-model path filters the stale `NodeId`s out without ceremony. No new reconciliation code needed.
- **Push happens at every close site**, not just `Cmd+W`. The tabstrip's `×` button, `Cmd+W` (both the multi-tab and last-tab→remove-window paths), and `Cmd+Shift+W` (all tabs in left-to-right order) push snapshots. The OS-red-button window close does *not* push — there's no hook for "this window is about to close" with the Phase C process-stays-resident model. Acceptable: closing the window via the title bar is a deliberate "I'm done with this window" gesture; user feedback can promote this if it bites.
- **`ReopenClosedTab` action goes through the catalogue** (`file.reopen_closed_tab` in `ferail-core::commands`), not the keymap-extras list. The extras list is for shortcuts the catalogue can't yet express (modifier chords on Esc, etc.); a vanilla Cmd+Shift+T is exactly what the catalogue is for. Knock-on: the new entry surfaces automatically in the shortcuts/command palette + future menu-bar wiring.
- **Cmd+Shift+T binds in `SHELL_CONTEXT`, not at the App level.** Requires an active window. With Phase C stay-resident-at-zero-windows, a user with no windows open hits Cmd+N first, then Cmd+Shift+T. Safari binds it App-level; we can promote later if zero-window reopen turns out to be a common path. Keeping it shell-scoped now avoids the action-shape complexity Cmd+N has (App `actions!` block, separate `cx.on_action` wiring).
- **Tab drag-reorder uses `TabDragPayload { id: TabId, label }`** following the `FavoriteDragPayload` shape — the payload `impl Render` so it doubles as its own follow-the-cursor drag preview. Source is `TabId` (not index) so a drop arriving after a concurrent close still resolves correctly.
- **Drop targets are 6-DIP-wide gaps interleaved with the chips**, mirroring `favorites_section::render_drop_gap` rotated 90°. Idle: invisible. `drag_over::<TabDragPayload>`: a 2-DIP vertical accent rule shows where the drop will land. Insertion-point pattern over chip-half-zones: more discoverable, hits cleanly, no edge-of-element math.
- **Index math runs on the `Shell::reorder_tab` helper.** Gap positions number `0..=tabs.len()`; the helper resolves the source by `TabId`, rejects no-op drops (`to_pos == from_idx || to_pos == from_idx + 1`), and tracks the active tab by id across the move so `self.active` follows correctly whether the moved tab is the active one or not.
- **Close-button listener now resolves by `TabId`, not by the captured `idx`.** A drag-reorder may have shifted `idx` since the listener closure was constructed; looking up the tab by id at click time keeps the right tab closing.

### Outcome

- `cargo check --workspace --all-targets` clean (1.7s).
- `cargo test --workspace` all green (173+ tests across the workspace).
- `screenshots/phase-d-baseline.png` renders three tabs with the second active — visually identical to Phase A+B's multi-tab screenshot, which is the goal: drag-reorder gaps are invisible at idle.
- Manually verified end-to-end:
  - Cmd+W → Cmd+Shift+T restores the closed tab at the same directory, with its filter text and history intact.
  - Cmd+Shift+W → multiple Cmd+Shift+T's pop the window's tabs in reverse order (rightmost first).
  - Drag-reorder of any tab updates `self.active` correctly whether the moved tab is the active one or another.
  - Stack cap respected — closing 20 tabs leaves the 16 most recent reachable.

### Trade-offs taken

- **Sort isn't preserved on reopen.** Most users hit Cmd+Shift+T to recover from a misclick; the path/filter restoration is the load-bearing piece. Sort restore can land alongside the broader file-table sort persistence work in the TODO.md backlog ("Persist file-table column order after drag reorder, alongside column widths").
- **OS-red-button window close doesn't snapshot tabs.** Phase C dropped the `on_window_closed` handler when it switched to stay-resident; restoring a hook just for closed-tab snapshotting is overkill for v1. Cmd+W and Cmd+Shift+W cover the deliberate close paths.
- **Closed-tab stack is in-memory only.** Persisting it across launches is part of the session-restore work (Phase J). For now a relaunch clears the stack.
- **Drop gaps are present in single-tab strips too.** Cheap (no hit during a node drag — `TabDragPayload` only originates from tab chips), and lets the eventual cross-window tear-off / merge work share the same gap rendering.

---

## Phase C — Cmd+N, second window, stay-resident (landed)

Goal: open a second window that shares the singleton `ProcessState`.
Process stays resident on zero windows (Finder / Safari model).

### Decisions

- **`ProcessState` lives in a GPUI `Global` newtype** (`ProcessStateGlobal(Rc<ProcessState>)`). `Global: 'static` is the only constraint — `Rc<…>` qualifies. Set once at `app.run` start via `cx.set_global(...)` in both `main.rs::run_gui` and the screenshot path. Every `Shell::new` reads it back via `process_state::process_state(cx)`. No Send/Sync gymnastics needed.
- **`Shell::new` now takes `Rc<ProcessState>` by argument** instead of constructing it. Two call sites: `main.rs::open_shell_window_sized` and `screenshot.rs::run` (both read from the global). New helper `Shell::build_process_state(cx)` runs once at startup and returns the Rc.
- **`open_shell_window(cx)`** is the single entry point for spawning a Shell window. Used by the initial-window boot (via `open_shell_window_sized` with size hints) and by the Cmd+N handler. Window options live in this function — there's no longer a single hard-coded `opts` block in `run_gui`. Future Phase C polish (cascade offset, per-window WindowOptions persistence) lands here.
- **`Cmd+N` binds at App level**, not under `SHELL_CONTEXT`. Reasons: (a) Cmd+N must work with zero windows open; (b) it should work regardless of which window holds focus. The action is declared in `main.rs`'s `actions!(app, …)` block alongside `Quit`/`OpenAbout`. The keymap's catalogue walker is told to skip `window.new_window` so main.rs's explicit `cx.bind_keys` is the only binding.
- **Process stays resident at zero windows.** Removed the `cx.on_window_closed` handler that called `cx.quit()`. Quit only via `Cmd+Q` or app-menu Quit. Matches spec §1.2 / §2.2. The dock icon stays visible — Phase I will wire `applicationShouldHandleReopen` so clicking it with no windows open reopens a window.
- **`Cmd+W` on the last tab closes the window**, via `window.remove_window()`. With the stay-resident default this is non-fatal. Same behavior on the tabstrip's `×` close button. Matches spec §3.4.
- **Watcher / reload fan-out now tracks every live tab path in-process.** `FsWatcher` keeps a set of watched directories, and `ProcessState` keeps weak handles for live Shell windows so watcher events and file-op completions reload every matching tab in every window.
- **Later: true OS-level singleton + launch-intent forwarding.** The current work shares one `ProcessState` inside a running process, but a second `ferail-gpui` process launched from CLI/Finder still needs the platform primary/secondary intent channel described in the spec.
- **`MergeAllWindows` / dock menu / cascade offsets deferred** to Phases F / K. Cmd+N opens centered windows; the user can drag them apart. Tear-off (Phase F) needs a position-near-cursor anyway, so cascade lives with that work.

### Outcome

- `cargo check --workspace --all-targets` clean.
- `cargo test --workspace` all green.
- `screenshots/phase-c-baseline.png` renders a single window identically to Phase A+B's baseline (the screenshot harness only captures one window).
- Cmd+N opens a second window sharing process state (favorites, undo, NodeStore, caches, tasks). Closing the last window leaves the process alive. Re-opening via Cmd+N from a zero-window state works.

### Trade-offs taken

- Initial-window size hints (`--width`, `--height`) only apply to the *first* window. Cmd+N windows use defaults (1180 × 760, centered). The size flags exist primarily for screenshots / dev iteration; persisted per-window geometry is Phase J (session restore) work.
- The settings-only boot path (`--settings page`) uses windowed (top-left) bounds rather than centered, because computing centered bounds needs `&mut App` synchronously and the existing code structure spawned that work async. Acceptable — this is a developer / CLI path, not a user-facing default.

---

## Phase A+B — per-tab state + ProcessState extraction (in flight)

Goal: pure refactor, no user-visible behavior change. Foundation for
multi-window, tear-off, and cross-window reload fan-out.

### Decisions agreed before code

- **Per-tab `Entity<TableState>`** — each `Tab` owns its own table state.
  Tab-switching no longer re-enumerates; inactive tabs' enumerations
  keep streaming into their own table.
- **Filter is per-window**, not per-tab — preserves current behavior;
  one less migration surface.
- **Closed-tab stack is process-scoped** (when added in Phase D).
- **Cmd+W closes the active tab; closing the last tab closes the window**
  (today it refuses; flip lands in Phase C alongside multi-window).
- **No lockfile-based singleton** — rely on macOS LaunchServices for the
  shipped .app, accept that `cargo run --bin` from dev can launch multiple.
- **Process state lives in an `Rc<ProcessState>`** held by each window;
  GPUI is single-threaded for entity access so `Rc` is fine. Background
  workers grab `Arc<MetadataDb>` / `Arc<NativeFs>` directly.
- **Phases A and B are landed together** — both touch the same field
  layout on Shell; splitting them is more churn than value.

### Decisions made during Phase A+B

- **`ProcessState` is a plain `Rc<ProcessState>` field on `Shell`** rather than a global / thread-local. Multi-window construction will clone the Rc into each new `Shell`. Background workers don't take the `Rc` — they take `Arc<Mutex<MetadataDb>>` / `Arc<NativeFs>` / `Arc<AtomicBool>` clones, so `Rc` never crosses thread boundaries.
- **`metadata_db` is `RefCell<Option<Arc<Mutex<…>>>>`** because the existing async open path needs to *set* the slot post-construction. Background workers grab `.borrow().clone()` to take a stable handle.
- **`NodeStore` is `RefCell<NodeStore>`.** Every call site now does `.borrow_mut()`. The lifetime cost is one site (`path_for_action` returns `&Path` and can't survive the borrow); rewrote it to use `path_snapshot_for_job` which returns `PathBuf`. Cost: a path-clone per row in `ant_heat`, negligible.
- **`Tab` is no longer `Clone`.** It now owns a `Subscription` (table-event bridge) which isn't Clone. No call sites needed it. Confirmed by grep.
- **Tab construction goes through `Shell::build_tab` / `make_tab`** — both build the `TableState` entity and wire its subscription before handing back a `Tab`. The subscription closure captures the tab's `TabId` so events from a non-active tab are dropped (defence in depth — only the active tab is rendered/hit-tested).
- **`load_path` captures `self.active_tab().id` and delegates to `load_path_for_tab(tab_id, ...)`.** The streaming closure looks up the tab by id, checks *that* tab's generation, then temporarily sets `self.active = idx` before calling the helpers that operate on the active tab. Restored on the way out. This keeps the existing helper signatures (`refresh_file_list_selection`, `restore_filtered_out_against_model`, etc.) unchanged while making the streaming correctly target the loading tab — even when the user has tab-switched mid-load.
- **`Cmd+W` no longer re-enumerates the now-active tab.** Each tab keeps its `TableState` populated from its own prior load. Same for `select_tab`, `on_next_tab`, `on_prev_tab` — pure index swap + `cx.notify()`. The spec calls for this; today's behavior was a forced reload (when there was one shared TableState).
- **`suppress_select_row` stays on `Shell`, not Tab.** It gates programmatic `set_selected_row` calls that fire `SelectRow` events; today only the active tab's mirror calls happen, so one counter suffices. If a future iteration mirrors lead into an inactive tab's TableState, this becomes per-tab.
- **`Shell` rename to `WindowShell` deferred to Phase C.** Cosmetic, and the rename's natural home is the multi-window step. Phase A+B already does the field split; the type name can follow.
- **The screenshot harness now opens new tabs through the window handle** — `handle.update(cx, |_, window, cx| { shell.update(...) })` — because `make_tab` needs `&mut Window` and a bare `Entity::update` doesn't provide one.

### Outcome

- Workspace compiles clean (`cargo check --workspace --all-targets`) — 0 warnings, 0 errors.
- Workspace tests pass (`cargo test --workspace`).
- Screenshots verified:
  - `screenshots/phase-ab-shell.png` (default shell — no visual regression vs. existing baseline).
  - `screenshots/phase-ab-multi-tab.png` (two extra tabs + multi-row selection).
  - `screenshots/phase-ab-selection-multi.png` (selection iter 2 still renders identically).
- Spec §3.6 win landed: tab switching is instant (no re-enumeration); each tab's enumeration streams into its own table; inactive-tab enumerations keep running.

### Trade-offs taken in this phase

- ~~The `self.active = idx; …; self.active = prev_active` swap inside the streaming closure is a hack.~~ **Retired (2026-06-12 review).** The 2026-06 audit found the swap was re-entrancy-fragile: an observer firing synchronously inside the apply (e.g. the favorites `cx.observe` subscription) read `active_tab()` and saw the loading tab instead of the user's. The streaming pipeline now threads the tab index explicitly: `apply_directory_load_msg_in_tab(idx, …)` → `apply_directory_batch_in_tab` / `finish_directory_load_in_tab` → `_in_tab` variants of the selection-reconcile helpers. Gesture-path call sites keep the old names as thin wrappers that pass `self.active`.
- Volumes is `RefCell<Vec<VolumeInfo>>` even though it's only read after construction today. Future-proofs against a Disk Arbitration listener that refreshes it from any thread.
- The undo stack is process-wide. Spec §1.1 implies process-scoped state; a Cmd+Z in any window undoes the most recent op anywhere. If user feedback wants per-window undo, easy to move later.

---

# Selection & DnD (iter 1+2 landed)

## Architecture at a glance
- Selection state is per-tab in `Tab` (file table): `selection: HashSet<NodeId>`, `anchor: Option<NodeId>`, `lead: Option<NodeId>`. The legacy `selected: Option<usize>` is gone; the row index is derived from `lead` against the live delegate entries.
- The gpui-component `TableState`'s built-in `selected_row` stays mirrored to the lead so the primitive's native focus overlay marks it; we paint a softer accent bg in `render_tr` for the rest of the set.
- Selection mutations route through Shell helpers that always (a) update Tab state, (b) call `refresh_selection_parallel_vecs`, (c) push the lead row into the table, (d) `cx.notify()`. Skipping any of these leaves the UI inconsistent.
- Streaming reconciliation hooks the same refresh from `apply_directory_batch` and `finish_directory_load`. On `Done` we drop NodeIds no longer in the model.
- The original `target_row()` chain still works: context_row → lead row. Right-click on a row outside the set replaces selection; on a row inside, leaves it.

## Key decisions

### Layer multi-select over gpui-component instead of forking it
The Table primitive is pinned. Modifier-aware clicks are addressable through `window.modifiers()` at SelectRow time. We pay one extra hop (Shell intercepts SelectRow and re-applies modifier logic) but avoid maintaining a fork. If we ever need more (per-event cell click intercept, drag-select rubber-banding), revisit.

### Selection is `HashSet<NodeId>` only — no parallel ordered vec
Visible-order is the delegate's `entries` order. Recompute when needed (Cmd+A, range computation). Fine at typical folder sizes; revisit if 10k-file folders become real.

### Lead = native overlay; set-only members = our painted bg
Spec §2.3 wants a focus ring distinct from selection fill. The Table primitive already paints a 1-px accent border on `selected_row` — we use that as the focus ring by mirroring lead → `set_selected_row`. Our `render_tr` adds a `theme.accent.opacity(0.18)` bg for set members that aren't the lead. The lead row gets both, which reads naturally ("the focused one of the selected set").

## Trade-offs made under time pressure
- Live Shift-range reconciliation through streaming batches (spec §2.6 last bullet of streaming arrival) deferred to iter 2 — iter 1 freezes the range at click time.
- Tree multi-select left as single-select per spec §2.7 ("optional for v1").
- The existing `on_drag(ExternalPaths(...))` in file_list.rs still carries one path. Iter 1 only changes selection. Iter 3 expands the payload.

## With more time, I would
- Push modifier-aware clicks into gpui-component's TableEvent so other consumers (Disk Usage, settings tables) inherit the same model.
- Add a `Selection` type in `ferail-core` so the model isn't shell-specific.
- Build an integration test harness for selection that synthesizes ClickEvents with modifiers.

## Things to discuss in the walkthrough
- Why the parallel `selected_in_set` / `is_lead` vecs instead of querying the entity from `render_td`: render_td has `&mut Context<TableState>`, not Shell, and crossing that boundary is the kind of thing the Prime Directive warns against. Parallel vecs are the same pattern `heats` and `is_favorited` already use.
- Why we mirror lead → Table's `selected_row` instead of suppressing the native overlay: less to maintain, and the primitive's focus ring is exactly what spec §2.3 describes.
- How right-click targeting works after this change: `context_row` still drives a single-row target (it's set on right-click; first checked, then falls back to lead).
- The `suppress_select_row: u32` counter on Shell: `TableState::set_selected_row` always `cx.emit(SelectRow)`. Without the suppression, our mirror call would re-enter the subscription, hit the plain-click branch with empty modifiers, and collapse a freshly-built multi-selection back to a single row. The counter is bumped before every mirror call and decremented in the subscription. It's a counter not a bool because a render frame can queue multiple mirrors.
- The `pending_select_row(s)` fields on Shell are CLI-screenshot-harness escape hatches. The harness applies `--select-row(s)` before the streaming load delivers any batches; we stash the row indices and consume them on the first batch that resolves all of them to NodeIds. Cleared on navigation so a stale row index can't apply to a different directory.

## Iter 2 outcome
- **Delegate selection state went NodeId-keyed.** The old parallel vecs (`selected_in_set: Vec<bool>`, `is_lead: Vec<bool>`) became `selected_set: HashSet<NodeId>` + `lead: Option<NodeId>`. `render_tr` looks up `entries[row].id` against the set on each frame. Sort can now reorder rows in place without desyncing the selection visuals — the HashSet doesn't care about row order. Same property holds for any future incremental row mutation (rename-stable identity, etc.).
- **`load_path` no longer clears selection.** Clearing moved into `navigate` (and a corresponding seed-then-load happens in the new `restore_from_history` helper). `Refresh`, filter changes, `toggle_hidden`, and the fs watcher all preserve selection now and let `apply_directory_batch` / `finish_directory_load` reconcile it.
- **`HistoryEntry` carries selection per back-stop.** `Tab::history` is `Vec<HistoryEntry>` with `{path, selection, anchor, lead}`. On every `navigate`, the leaving entry is updated with the current snapshot before push. `navigate_back` / `navigate_forward` symmetrically save the current entry's snapshot, step, and restore via `restore_from_history`. The restored selection rides through `load_path` and is then reconciled against the fresh stream on `Done`.
- **`reconcile_done` is the canonical "after the load settles" pass.** It drops NodeIds not in the final visible model, except when a filter is active — those get moved to `Tab::filtered_out` instead so a future filter loosening can lift them back. It also re-seats `anchor` / `lead` if they vanished, and demotes `range_live` to false when its endpoints are gone.
- **Filter holding is implicit via the same path.** Narrowing the filter calls `load_path`; the new model shrinks; `reconcile_done` with filter active moves shrunk-out members to `filtered_out`. Loosening the filter does the inverse — `restore_filtered_out_against_model` runs on every batch + on `Done`, lifting members back as their rows arrive. `clear_active_selection` (Esc) also drains `filtered_out` so a follow-up filter loosen can't resurrect ghosts.
- **Live Shift-range now actually streams.** `range_live: bool` on `Tab` is set by `range_select` (Shift / Cmd+Shift click) and the `move_selection(..., extend=true, ..)` keyboard path; cleared by every non-range gesture (plain click, Cmd-click, plain kbd nav, Cmd+A, Esc, navigation). When set, `recompute_live_range` runs on every batch and at `Done`: if both `anchor` and `lead` are visible, selection is rebuilt as the inclusive anchor→lead span in the current visible order; otherwise it waits for the missing endpoint to arrive.
- **Verified via screenshots** at [docs/images/selection-iter2-multi.png](docs/images/selection-iter2-multi.png) (multi-select identity unchanged after the HashSet refactor) and [docs/images/selection-iter2-sort.png](docs/images/selection-iter2-sort.png) (sort applied with selection still alive).
- **Caveats deferred to later iters:** the spec's "sort change recomputes the span in the new visible order then freezes the range" polish — we keep the range live and rebuild on next batch instead (good enough on real-world flows; the strict freeze can land with a delegate→Shell hook later). DnD §3 and tree multi-select still queued.

## Iter 1 outcome
- All spec §2 file-table behaviors land: single click replace, Cmd-click toggle, Shift-click range, Cmd+Shift additive range, anchor/lead model, plain and Shift-extend keyboard nav, Cmd+A, Esc with filter-vs-selection priority, right-click rule (selected vs unselected).
- Status bar reads from the selection set: count + summed size across visible members.
- Preview pane reads the lead row, not the whole set (matches Finder).
- Spec §2.4 "Click on empty space below rows" not yet wired — the gpui-component table doesn't currently surface an empty-area click. Defer to iter 2 or whenever we tap that primitive.
- Spec §2.4 "Right-click on empty space" same status — not surfaced by the primitive yet.
- Spec §2.6 streaming reconciliation: minimal pass only. `refresh_file_list_selection` runs on every batch + Done so NodeIds in the set rejoin visually as their rows land. Live Shift-range recomputation across batches deferred to iter 2 (range freezes at click time).
- Verified visually: `screenshots/selection-iter1-single.png` (one row, focus ring, "1 of 44 selected"), `screenshots/selection-iter1-multi.png` (four rows, anchor=2, lead=8, "4 of 44 selected · 20.3 KB", lead distinct from set members).

# 2026-06-23 mpv backend + chroma-key compositor (planning + Phase 0 spike)

Plan: rip out the libvlc video backend, replace it with **libmpv**, and build
an **N-layer transparent-colour (chroma-key) compositor** on top — pick a
transparent colour on a video, see the layer(s) beneath show through, where a
lower layer can itself be a keyed video. Full design in
[docs/features/VIDEO-MPV.md](docs/features/VIDEO-MPV.md).

Why mpv over VLC, in one line: libvlc can't change a video filter live, which
is the whole reason for the seamless-reopen apparatus in the viewer
(`commit_video_enhance` / `video_pending_seek` / `video_repause`) — mpv's `vf`
is live, so that apparatus deletes AND live filters are exactly what a *live*
colour-key picker needs.

Two scope calls I made up front:
- **Replace VLC outright** (not keep as a fallback), sequenced so mpv hits
  parity + passes its integration test before VLC is deleted in the same phase
  — never left without working video.
- **N-layer stack** from the start. The data model is N; the honest perf
  ceiling of the CPU-buffer-pull path is a handful of layers at ≤1080p, past
  which the fix is GPU surfaces (`gpui::surface`) — a documented follow-up, not
  an MVP blocker.

**Phase 0 spike (`spikes/mpv-probe/`, dependency-free dlopen FFI) — ran green
against Homebrew libmpv (`/opt/homebrew/opt/mpv/lib/libmpv.2.dylib`).** It
exists to resolve the gating unknowns before any real integration:

- **SW render pulls BGRA frames.** `mpv_render_context_create(..,"sw")` +
  `mpv_render_context_render` with `SW_SIZE/SW_FORMAT="bgra"/SW_STRIDE/
  SW_POINTER` hands back a tightly-packed BGRA buffer at the size we ask —
  same shape as libvlc's vmem, so the `copy_frame → (w,h,BGRA)` seam is
  untouched.
- **THE GATE — SW render emits a REAL alpha channel. PASS.** A `colorkey`
  filter set live produced correct per-pixel alpha through SW render: keying
  the clip's green background made it transparent (`alpha lo=0`), the overlaid
  test box stayed opaque (`hi=255`), ~66k/76.8k px transparent. Eyeballed at
  `screenshots/mpv-probe-B_alpha_green.png` (green→black, box→white). So
  **keying lives in mpv's filter chain** (live, off our threads), not a CPU
  pass. Recipe: end the vf chain in an alpha format and request bgra —
  `vf = lavfi=[...,format=rgba,colorkey=color=0xRRGGBB:similarity=..:blend=..]`.
- **Live vf change works with no re-open.** Setting `vf` *after* playback
  started applied immediately (the green key above was set live, ret=0). This
  is what kills the VLC reopen dance and enables live key + live enhance.
- **Two corrections to fold into the plan:**
  1. **mpv's `brightness` equalizer property is a no-op on the SW-render
     output** (luma 83.8 → 83.8 unchanged). The equalizer lives in the GPU VO
     shaders; SW render doesn't run them. So **colour grade must route through
     a lavfi filter** (`eq`/`colorlevels`) in the live vf chain, not via the
     `brightness/contrast/saturation/gamma/hue` properties — which unifies
     grade + enhance + key into one live chain. (Or keep the existing CPU
     grade; lavfi is cleaner now that the chain is live.)
  2. **The `--alpha` option doesn't exist in this mpv build** (rejected -5).
     Didn't matter — alpha came purely from `format=rgba` in the chain + the
     bgra SW format. One less knob.
  - (Minor: the spike's *second* live retune snapshot was stale — 200 ms
    wasn't enough settle, not a mechanism failure; the real backend uses
    mpv's render update-callback rather than a fixed sleep.)

Net: the architecture holds and the risky unknown is retired green. Next is
Phase 1 — `ferail-video-mpv` to parity, then delete `ferail-video-vlc` +
the `vlc` feature + the reopen apparatus. Delete `spikes/mpv-probe/` once
Phase 1 lands (binding decision now recorded here).

---

# 2026-06-23 resilient file operations — cope with permission/lock failures + report transparently

Problem: a copy/move that hit a permission denial or a locked file (open in
another process) aborted the **whole batch on the first failure**, threw away
the structured cause at the `format!("{path}: {e}")` boundary, and surfaced one
flat string — with no way to *cope* (escalate) or even see which items failed.
Plan: `~/.claude/plans/tidy-dreaming-lobster.md` (approved). Phased: Chunk A =
transparency foundation; Chunk B = retry + elevation vertical slice (macOS-real,
Win/Linux stubbed); Chunk C = Windows-native lock detection + runas (deferred to
the Windows box, since I can't runtime-test Win32 from this Mac).

User decisions: explicit **"Retry as administrator"** button (no auto-escalation);
build the platform-neutral + macOS foundation now; **force-close** of a locking
process is in scope (Windows-native, Chunk C).

## Two deviations from the plan letter (both to match existing conventions)

1. **`FileOpError`/`FileOpErrorKind` live in `ferail-fs-native`, not
   `ferail-core`.** The plan said core "beside `EnumerationError`", but core
   deliberately uses `String` for paths and never imports `std::path` — a
   `PathBuf`-bearing error type doesn't belong there. fs-native is the engine's
   home and where the libc-based classifier must live anyway (errno values vary
   across unix flavours, so the classifier uses `libc::EACCES` etc., not
   literals). gpui reaches them via `ferail_fs_native::file_ops::FileOpError`.
2. **No serde.** The project persists settings as a hand-rolled `key=value`
   format; neither core, fs-native, nor gpui pull serde. So the Chunk B elevated-
   op descriptor will use the same hand-rolled line format, and the platform
   elevation primitive stays a dumb "re-launch self elevated with these args"
   call (it never sees the op type) — which also keeps crate boundaries clean
   (shell crates can't depend on gpui's descriptor type).

## Chunk A — landed (this session)

- `FileOpErrorKind { PermissionDenied | Locked | NotFound | NoSpace | ReadOnly |
  NameTooLong | AlreadyExists | Other }` with `summary()` (plain label),
  `advice()` (centralised — the GPUI notification's old inline string-match table
  moved here), `is_elevation_recoverable()` (PermissionDenied only — elevation
  doesn't release another process's lock or fill a disk) and `is_lock()`.
- `FileOpError { kind, path, raw, os_code }` + `FileOpError::from_io` classifier:
  `ErrorKind` first, then raw OS code (libc on unix, documented winerror.h values
  on the windows arm — pure data, safe to write blind).
- Engine collect-and-continue: `OpOutcome.failed: Vec<FileOpError>`; the per-item
  loops in `run_copy`/`run_move` record a failed item and **keep going** instead
  of `?`-aborting; `resolve_dest` returns a `Resolution` enum and no longer
  mutates the outcome. `run_copy`/`run_move` keep their `Result<_, String>`
  signature but in practice only ever return `Ok` now (per-item errors live in
  `failed`; cancellation stays a distinct early break). All inner tiers
  (`copy_item`, `copy_leaf_file`, `copy_file_chunked`, `recreate_symlink`,
  `mac::copy_file`) now return `FileOpError`.
- Surfacing: `file_op_outcome_summary(verb, &OpOutcome)` →
  "Move: 7 of 10 done · 3 failed" + first 4 items with reasons + dominant-kind
  advice; wired into `spawn_transfer_op` so **failures always surface** (even
  sub-150ms ops) with the existing Copy-to-clipboard action. The old transfer
  error path used a bare `Notification::error` that skipped the advice helper
  entirely — fixed. `file_op_error_notification` now delegates to
  `classify_error_text(...).advice()` so the string-error surfaces (rename,
  duplicate, compress, alias) share the one advice table.
- Tests: `partial_failure_continues_and_is_recorded` (a hand-built plan with a
  ghost second source → first item copies, second recorded as `NotFound`, batch
  not aborted) and `classify_maps_os_errors_to_kinds` (synthetic OS errors →
  kinds + the elevation/lock predicates). All 15 file_ops tests green.

## With more time / deferred

- Chunk B next (this session): elevated-op descriptor + `--elevated-op` worker +
  `platform_shell` elevation/lock surface + Retry / Retry-as-administrator UI.
- Chunk C (Windows box): Restart Manager lockers, RmShutdown/Terminate force-
  close, `runas` re-exec. macOS elevation via osascript is the simplest viable
  mechanism (runs the worker as root, generic auth dialog) — a `SMAppService`
  privileged helper is the upgrade.

## Chunk B — landed (this session): retry + elevation vertical slice

Verified end-to-end on macOS (only the literal auth prompt is the manual step).

- **Descriptor model + worker** (`crates/ferail-gpui/src/elevation.rs`):
  `ElevatedOp { is_move, dest_dir, sources }` + `ElevatedResult`, NUL-separated
  encoding (robust against any path on unix; no serde). `--elevated-op
  <descriptor> --elevated-result <result>` CLI mode (dispatched in main.rs
  before any GUI init) runs the op via the *same engine* the GUI uses, writes
  per-item results, and **exits 0 even when items fail** (failures live in the
  result file) so the osascript wrapper treats "ran" as success. Round-trip
  unit tests + a direct CLI test (copied/moved real files, spaced filenames).
- **Platform surface** (all three shell crates, `platform_shell::*`):
  `elevation_available()`, `run_elevated_self(args)`, `lock_diagnostics_available()`,
  `processes_using(path) -> Vec<LockingProcess>`, `force_close_processes(pids)`.
  The primitive is deliberately dumb — "re-launch THIS exe elevated with these
  args and wait" — so it never sees the op type and the shell crates need no
  dep on gpui's descriptor. macOS `run_elevated_self` = osascript `do shell
  script … with administrator privileges` (two-layer quoting: POSIX
  single-quote per token, then AppleScript string escaping — verified through
  osascript with a spaced filename). Windows/Linux = stubs (Chunk C / pkexec).
- **UI** (`shell.rs` `transfer_failure_notification` + `TransferRetry`;
  `file_ops.rs` `retry_transfer_elevated`): the failure toast offers Copy +
  in-process **Retry** (re-runs just the failed top-level sources) + **Retry as
  administrator…** (gated on `is_elevation_recoverable() && elevation_available()`).
  Elevated retry runs `run_elevated_op` on the executor (the auth dialog
  blocks), reloads, and reports "completed N as administrator" / "M still
  failed". gpui-component `Notification`: `.action` sets the primary button AND
  disables autohide; `.content` renders the secondary button row.

### Decisions / trade-offs

- **macOS PermissionDenied + TCC**: "Retry as administrator" runs the worker as
  root, which fixes Unix-ownership denials but NOT TCC/privacy denials (root is
  still gated on protected folders without Full Disk Access). The follow-up
  toast reports honestly when it still fails. The old mac-specific "grant Full
  Disk Access in System Settings" hint is dropped in favour of one
  platform-neutral advice line; acceptable, revisit if it confuses.
- **Scope held**: only the copy/move/paste/drag transfer path got the retry UI
  this session. The other silent/first-error surfaces (trash, tag-toggle) are
  noted in TODO, not done — they're separate surfaces, and the user was editing
  in parallel (`//`), so I kept the blast radius tight.
- **Clippy**: my additions are warning-clean; the few workspace warnings live in
  files I didn't touch (search.rs, open_text_prompt region) — pre-existing or
  the parallel edits, left alone.

### Chunk C (next, on the Windows box) — see TODO "Resilient file-op coping"

`ShellExecuteExW` runas for `run_elevated_self`; Restart Manager for
`processes_using`; `RmShutdown`/`TerminateProcess` for `force_close_processes`;
then flip the two `*_available()` bools true.

## 2026-08-22 — Grid icon-size slider + thumbnail fit modes

Toolbar gets a continuous icon-size slider and a reset button beside the
existing −/＋ stepper; Settings › Layout gets **Icon fit**, which decides how a
thumbnail fills the square icon slot. Size range widened from 64–256 to
32–512.

### Design choices

- **Five fit modes, three gpui object-fits.** `ThumbFit::object_fit` maps the
  modes onto `gpui::ObjectFit` using the image's own orientation. The slot is
  always square, which is what collapses them: scaling by width is the
  *smaller* scale factor for a landscape image (`Contain`) and the *larger*
  one for a portrait (`Cover`), so "Fit width" is just Contain-or-Cover chosen
  per image. The first design computed the target rect ourselves and drew with
  `ObjectFit::Fill`; letting gpui crop instead keeps the painted element
  exactly slot-sized — a 4:1 panorama in Fill frame would otherwise lay out an
  element several thousand px wide purely to have it clipped back. The mapping
  is a pure function with unit tests; the renderer just asks for it.

- **The size control is hand-rolled, and has no knob.** It is a
  click-anywhere two-tone fill bar: bright left = current size, dim right =
  headroom. gpui-component's `Slider` cannot express that — it always draws a
  thumb, and its thumb ring and track are both tinted from a single colour, so
  there is no way to keep the two-tone bar and lose the knob (only `disabled`
  hides the thumb, and that kills interaction too). Its thumb size and track
  thickness are hardcoded as well. Copies the viewer's `slider_row` shape:
  bounds captured via `canvas`, cursor x mapped back to a value.

  Two things fell out of dropping the component. The value now lives in
  exactly one place (the `IconSize` global), so the entity, the subscription
  and the whole two-writer sync problem are gone — the −/＋ buttons and the
  bar are just two callers of the same setter. And the scrub no longer goes
  through gpui's drag system, so dragging the size no longer makes
  `cx.has_active_drag()` true app-wide (which had been livening up drop
  targets and spring-load for the duration of a gesture).

- **The scrub persists on release, not on move.** Mouse-move only writes the
  live global — that is what makes cells resize under the cursor. Persisting
  per move would enqueue a settings write on every frame of one gesture. The
  drag is tracked on the Shell and serviced from the *window root*, not the
  bar, so it keeps following the cursor past either end of a 96-px track;
  `on_mouse_up_out` commits a release that happens outside the window.

- **The fill keeps a round nub at minimum size.** At 32 px the fill is
  zero-width, and an empty bar reads as a dead control rather than as "all the
  way down". A knobless bar has no other "you are here" cue at the extremes.

- **Steppers now take the next stop *past* the current size**, not the
  nearest. Off-stop sizes used to be impossible and are now the norm; with
  nearest-stop, ＋ from 100 px picks 96 and reads as the button going the wrong
  way.

- **Magnifying fit modes step up one fetch bucket.** Covering scales until the
  short edge fills the slot, so the same thumbnail is stretched further than
  Best fit stretches it. The step reuses the existing 128/256/512 ladder rather
  than adding a 1024 rung — that would cost ~4 MB per entry against a
  512-entry LRU, and only helps at the largest sizes.

- **The window claimed its own titlebar drag** (`shell_window_options`,
  `app_owns_titlebar_drag: true`). This is the bug that made the first cut of
  the slider unusable: dragging it moved the window. Every toolbar control
  already wraps itself in a mouse-down-stopping div for the Win32 title-bar
  drag, and I assumed that covered macOS too. It does not. With gpui's default
  `app_owns_titlebar_drag: false`, `_opaqueRectForWindowMoveWhenInTitlebar`
  reports an empty rect and **AppKit drags the window from the titlebar rect
  below gpui entirely** — `cx.stop_propagation()` cannot reach it, because
  AppKit never asks gpui. gpui-component's `TitleBar` carries an app-side
  window-move (`should_move` → `start_window_move`) that was simply dead code
  here. A *button* never exposes this (a click is not a drag), so the slider is
  the first control in that bar to hit it. Only the shell window sets the flag
  — it is the only one that renders a `gpui_component::TitleBar` and so the
  only one with an app-side move to fall back on; Settings / Get Info / the
  icon picker draw no custom titlebar and must keep AppKit's.

- **The slider hides below a 1060-px window.** The shell title bar has no
  width tiering (the viewer's toolbar does), and without a gate the slider
  pushed the whole right-hand cluster — overflow menu included — off the edge
  of a narrow window. Deliberately one local gate rather than a tier system.

### Trade-offs / with more time

- A 512-px cell shows a 512-px thumbnail at 1:1, not 2× retina. A 1024 bucket
  would fix it but needs `CACHE_CAP` lowered in the same change.
- In Fill frame there is no letterbox margin left, so the favorite star, the
  quarantine badge and the tag dots now sit on image content. `ADORN_MIN_ICON`
  already hides most of that below 96 px; Finder has the same overlap.
- Fit width / Fit height only become distinct *looks* on a non-square slot.
  Aspect-aware (masonry) cells would make them earn their place — separate
  feature, not attempted.
- No "Actual size" (never enlarge) mode; it is one more arm in the same match
  if a small PNG blown up to 512 px turns out to bother anyone.
