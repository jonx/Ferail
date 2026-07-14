# AROS port (branch `aros-port`)

> Building this from scratch? **[aros-building.md](aros-building.md)** is
> the step-by-step guide (prerequisites, checkout layout, the whole chain).
> This document is the map: what lives where and why.

Feraille on AROS (the open-source AmigaOS), targeting the user's hosted
`darwin-aarch64` AROS (AROS running as a macOS process — see
`~/Source/aros-aarch64`). Local-only work: absolute paths to sibling
checkouts are by design, mirroring the `[patch]` sections in `Cargo.toml`.

## Status

- **Sidebar + chrome polish pass, 2026-07-14 (verified live).** After the icon
  fix below, a round of AROS-specific UI fixes, all confirmed on a booted screenshot:
  - **Font-glyph tofu boxes replaced by SVG icons.** The disclosure triangles
    (`\u{25B6}`/`\u{25BC}`) in the Favorites/Recents section headers and the tree-row
    carets, plus the filter placeholder's return symbol (`\u{23CE}`), rendered as
    tofu on AROS — IBM Plex Sans has no geometric-shapes/symbol glyphs and the AROS
    font stack has no fallback. Carets are now `nav/disclosure-{right,down}.svg`
    (new assets); the filter placeholder uses the word "Enter" on AROS.
  - **Window title.** The OS title bar is drawn by Intuition's topaz bitmap font
    (ASCII only), so the `\u{2014}` em-dash separator was garbage. AROS uses " - ".
  - **AROS-appropriate sidebar Locations.** The Unix "home + Desktop/Documents/
    Downloads" scheme listed folders that don't exist on a stock AROS volume
    (and clicking a missing one tripped a `posixc` open() fault). AROS now lists
    System (`SYS:`), Ram Disk (`RAM:`), and the standard `SYS:` drawers
    (Prefs/Utilities/Tools/Storage) that actually exist, filtered by `exists()`.
    (`feraille-fs-native/src/paths.rs`.)
  - **Volumes as drives.** The Volumes section was empty on AROS (`list_volumes`
    had no arm). It now surfaces the default mounts — `SYS:`, `RAM:`, `MacRO:`,
    `MacRW:` — probed by `exists()` (no `Work:`-style named volume, which would pop
    the AmigaOS insert-media requester). (`feraille-fs-native/src/volumes.rs`.)
  - **Open — tree expand freezes the app.** Expanding the Browse › Home tree row
    (recursive `SYS:` child enumeration) hangs the app (no crash dialog, no trap in
    the log — a freeze, not a bus fault). Likely the same AROS metadata-walker
    frontier as the gated folder-size walker (emul-handler `DoExamineNext`) or a
    dos read_dir stall. Tree connector lines (issue: "Home should draw lines")
    are unverified as a result — they only appear once a node is expanded.
  - **Open — `posixc.library __open` bus-faults on a missing path.** Clicking a
    nonexistent folder (the old phantom `Downloads`) bus-faulted a worker thread
    inside `posixc __open` (Error 0x80000002) rather than returning ENOENT. The
    Locations fix removes the common trigger, but this is a latent landmine (any
    typed bad path). The real fix is hardening `posixc __open` in aros-upstream.
- **Chrome / bundled SVG icons now draw, 2026-07-14.** The long-standing "icons
  don't render on AROS" bug is fixed: rust-embed was in debug filesystem-passthrough
  mode, so the icon SVG bytes were never in the binary (one-line `debug-embed`
  feature fix). Also added an AROS-only diagnostic sink (`obs::init` bridges the
  `log` crate into `MacRW:feraille-log.txt`) that made it findable. Full writeup:
  the "Chrome / bundled SVG icons" section below.
- **Rebuilt + re-verified on-device, 2026-07-13.** Both AROS binaries rebuild
  from clean after two regressions were fixed:
  - `aros-aarch64/hosted/rust/compat/include/{endian.h,sys/ioctl.h}` — the host
    libc compat shim that `.cargo/config.toml` references via
    `-DHAVE_ENDIAN_H -I .../compat/include`. It had been gitignored as
    "experimental/unwired" but is required: a newer tree-sitter calls `le16toh`,
    which is undeclared without it (build now tracks the shim).
  - `crates/feraille-aros-app/link-aros.sh` (and the zed-aros twin) now compile
    and link `aros_env_glue.c`; the std shim gained `aros_env_enum`
    (`std::env::vars`) after the scripts were written, so the link failed on an
    undefined `aros_env_enum`.
  - **Runs live:** `C:Feraille` (203 MB stripped) LoadSegs and opens its window
    on the Wanderer desktop — the dark theme renders, the `SYS:` listing shows
    30 items with Name/Size/Format columns and the sidebar, `crash=none`,
    rendering clean (no dirty-rect smearing). **Memory:** the 203 MB binary needs
    more than the default 256 MB hosted RAM to LoadSeg — boot with
    `AROS_HOST_MEMORY=1280` (or `memory 1280` in `aros-host.conf`), else the load
    silently fails and no window appears. **Launch:** the app must start *before*
    the desktop Startup-Sequence's `EndCLI` (a launch appended after `EndCLI`
    never runs); `feraille.startup`'s `Stack 16000000` + `SetEnv HOME` still apply.
- **On-device battery, 2026-07-06** (booted via `graft/aros-ctl`, `C:Feraille`
  rebuilt with the native-shell + dirty-rect backend):
  - **Self-guard boot WORKS** — launched with no `Stack` line, the wrapper
    relaunches its own seglist at 16 MB and Feraille boots to a full themed
    SYS: listing (30 items), sidebar, columns. `crash=none`.
  - **Native Intuition menu bar WORKS** — the gadtools `SetMenuStrip` populated
    from gpui `set_menus` shows `Feraille | File | Edit | Go | View | Window`
    with the app menu (About / Settings / Quit). Verified with `aros-ctl menu`.
  - **Open finding — selecting a menu item that opens a second window traps**
    (About Feraille → new window → `Trap signal 5/11`, thin backtrace). The menu
    strip renders/populates fine; the fault is in the pick→action→window-open
    path (multi-window is untested on AROS). Containment keeps the desktop alive.
  - **emul-handler: ExWalk PASS** (`C:ExWalk 8x25 EXALL` over SYS:C, 24600
    entries, 0 errors) — the stack fix holds. **But** Feraille's recursive SYS:
    folder-size walk trips a *distinct* `DoExamineNext` bus fault (0x80000002),
    so the walker is re-gated on AROS again (see aros-aarch64 UPSTREAM-NOTES
    item 36). Everything else runs clean.
  - **Wheel scrolling WORKS end-to-end** — a real trackpad/mouse scroll over
    the Feraille window scrolls the list: host `NSEvent scrollWheel` → shim
    `CM_EV_WHEEL` → the rebuilt `cocoa.hidd` → NewMouse rawkeys 0x7A/0x7B →
    Intuition `IDCMP_RAWKEY` → `gpui_aros` `ScrollWheel`. Confirms the rebuilt
    `cocoa.hidd` is live in the booted kickstart. (The *injected* automation
    path — `aros-ctl wheel`, the shim's `W` control-FIFO synth — did not move
    the list in testing; that is a harness gap, separate from the real path,
    and likely needs the pointer over the window.)
  - **GPU video path measured** — `hosted/gpufx-bench` (`C:GpuFxBench`) shows the
    3D shim's YUV420→RGBA is 5-7× faster than the software kernel; relevant to a
    future `feraille-video` / preview path on AROS.
- **`cargo check -p feraille-gpui --target aarch64-unknown-aros` is GREEN**
  — the whole app type-checks for AROS: gpui via the `gpui_aros` CPU
  backend, gpui-component (full tree-sitter grammar set), bundled sqlite,
  notify, and every feraille crate.
- **GPUI RUNS INTERACTIVELY ON BOOTED AROS** (2026-07-04). The smoke app
  (`zed-aros/crates/gpui_aros_smoke`, linked via collect-aros into
  `C:GpuiSmoke`) opens a real Intuition window, renders gradients / glyphs /
  shadowed cards / quads through the tiny-skia CPU renderer, and echoes
  live keyboard input end-to-end (cocoametal → keyboard HIDD → Intuition
  RAWKEY → keymap.library → gpui `Keystroke` → re-render). Proof:
  `screenshots/aros-gpui-window.png`, `aros-gpui-bigstack-key.png`,
  `aros-gpui-final.png`.
- **Stack is the sharp edge**: AROS shells launch commands with tens-of-KB
  stacks; gpui's dispatch/layout recursion overflows that, and in AROS's
  single address space the overflow corrupts *neighboring* tasks
  (emul-handler / graphics.library crash with wild NULL-offset faults —
  nothing points back at the app). `Stack 16000000` before launching (see
  `gpui_aros_smoke/gpui-smoke.startup`) fixes it. Feraille's eventual
  launcher must set this (startup script or icon stack tooltype).
- **FERAILLE ITSELF RUNS ON AROS** (same day): `crates/feraille-aros-app`
  (staticlib + C harness + `link-aros.sh`, mirroring the smoke) boots the
  full app through the shared `feraille_gpui::boot::run_gui` — complete
  chrome, dark theme, and a real `SYS:` listing (30 items: boot, C,
  Classes, Developer, Devs, Fonts, Libs, Prefs, Storage…). Proof:
  `screenshots/aros-feraille-clean.png`. Launch recipe:
  `feraille.startup` (`Stack 16000000` + `SetEnv HOME "SYS:"` +
  `C:Feraille --theme dark`).
- Landed on the way: `feraille_gpui::boot` (the GUI boot moved from
  `main.rs` into the library so the desktop binary and the AROS wrapper
  share one path), and a rust-aros std fix (`cstr()` translates unix
  path joins — `SYS:/C` → `SYS:C` — at the syscall boundary; commit
  a089e3d there).
- Known AROS-side frontier: under Feraille's concurrent metadata walkers
  the OS's own **emul-handler** can bus-fault in `DoExamineNext`
  (graft/UPSTREAM-NOTES item 35 territory — its stack). The
  crash-containment work catches it (Suspend keeps the session alive).
  Consider gating the folder-size walker / thumbnail warmers on AROS
  until the handler is hardened. Also: `--screenshot` (headless) is
  macOS-only; proof shots go through `graft/aros-ctl shot`.

## The pieces and where they live

| Piece | Where | Branch |
| --- | --- | --- |
| gpui AROS backend (`gpui_aros`: Intuition/CyberGraphics C glue, tiny-skia CPU renderer, std-thread dispatcher, keyboard/wheel input, clipboard.device) | `~/Source/zed-aros` | `aros-platform` |
| Custom Rust std for AROS (posixc-backed fs/thread/net/random) | `~/Source/rust-aros` (symlinked into the nightly's `rust-src`) | — |
| Target spec JSON + C compat shims (`endian.h`, `sys/ioctl.h`) + std C glue | `~/Source/aros-aarch64/hosted/rust/` | — |
| gpui-component with `smol` → `async-channel` (keeps the async-io/rustix reactor out) | `~/Source/gpui-component-aros` (worktree @ pinned rev c112e7b) | `aros-port` |
| AROS OS source (incl. `exec/types.h` storage-class-macro guards) | `~/Source/aros-upstream` | `crash-containment` |
| AROS build tree / SDK / boot image | `~/aros-build` (env `AROS_BUILD`) | — |
| Patched crosstools (AROS clang/lld, `aarch64elf_aros` emulation, compiler-rt builtins) | `~/aros-crosstools` (env `AROS_CROSSTOOLS`) | — |
| Hosted-AROS run/automation harness | `~/Source/aros-aarch64/graft/` (`aros-ctl`, `run-window.sh`, smokes) | — |

## The check command

```sh
cargo +nightly-2026-06-27 check -p feraille-gpui \
  --target /Users/jkn/Source/aros-aarch64/hosted/rust/aarch64-unknown-aros.json \
  -Zjson-target-spec -Zbuild-std=std,panic_abort
```

(or `scripts/check-aros.sh`). Notes:

- The pinned nightly matters: the custom std rides the toolchain's
  `rust-src` symlink (`$(rustc --print sysroot)/lib/rustlib/src/rust` →
  `~/Source/rust-aros`).
- `-Zjson-target-spec` became required for JSON targets on this nightly.
- `.cargo/config.toml` (force-added on this branch; normally gitignored)
  carries the target rustflags (`getrandom_backend="custom"`) and the
  AROS C recipe for cc-rs (`CC/AR/CFLAGS_aarch64_unknown_aros`).

## C-of-the-graph lessons (all encoded in `.cargo/config.toml`)

- **`-D_GNU_SOURCE`** — AROS posixc guards core-POSIX `fdopen`/`popen`
  behind `_GNU_SOURCE`/`_XOPEN_SOURCE`; tree-sitter et al. expect them
  visible.
- **`-DHAVE_ENDIAN_H` + compat include** — AROS has no `<endian.h>`;
  tree-sitter's `portable/endian.h` honors `HAVE_ENDIAN_H`. Shim at
  `aros-aarch64/hosted/rust/compat/include/endian.h`; the same dir shims
  `sys/ioctl.h` (sqlite includes it unconditionally, calls it only behind
  `__linux__`).
- **sqlite**: `-DSQLITE_OMIT_WAL=1 -DSQLITE_MAX_MMAP_SIZE=0
  -DSQLITE_OMIT_LOAD_EXTENSION=1` — AROS has no mmap/shm-WAL/dlopen; the
  classic rollback-journal configuration is semantically required, not just
  a compile fix.
- **`exec/types.h`** unconditionally `#define GLOBAL extern` and clobbered
  sqlite's function-like `GLOBAL(t,v)` mid-amalgamation (reached via
  `<pthread.h>`); fixed at the source (guarded per-macro, AmigaOS-NDK
  style) in aros-upstream commit `5c1666af`.

## Vendored / patched crates (`[patch]` in `Cargo.toml`)

- `stacker` (zed-aros/vendor-aros) — no-op stack growth on AROS. The lock
  must resolve to the vendored 0.1.23: `cargo update -p stacker --precise
  0.1.23` if a bump reintroduces 0.1.24.
- `filetime` (zed-aros/vendor-aros) — AROS arm reading via std `Metadata`;
  set-times returns `Unsupported` (notify's poll watcher only reads).
- `gpui-component` — the `aros-port` worktree swaps `smol::channel` for
  `async-channel` (identical re-export) so smol's async-io → rustix →
  errno reactor never enters the graph.
- gpui's `test-support` gates `proptest` off AROS (rusty-fork/wait-timeout
  have no arm); `render_to_image` and the rest of test-support survive.

## platform_shell

`feraille-shell-aros` v1 re-exports the `feraille-shell-linux` scaffold
(its `cfg(not(linux))` no-op arms + the pure-Rust `compress_paths`). Real
AROS integrations to grow there: workbench.library reveal, icon.library
icons, clipboard.device file URLs, DefIcons-style thumbnails.

## Feature completeness & roadmap (takeover)

Feraille **runs** on AROS as a browsable, themed file manager, but is **not at
parity** with the Mac build. `feraille-gpui` is barely modified (two
`cfg(target_os="aros")` sites), so most gaps are in the platform layers
(`gpui_aros`, `feraille-shell-aros`) or are macOS-only features needing an
AROS-native equivalent.

Legend: ✅ works · 🟡 partial/unverified · ❌ absent.

| Feature | AROS | Notes / where the work is |
|---|---|---|
| Boot, window, dark theme, navigation, file list | ✅ | verified live (`SYS:` listing) |
| Keyboard / mouse / wheel | ✅ | `gpui_aros` input |
| Clipboard (text) | ✅ | `clipboard.device` via `gpui_aros` |
| Never-block UI | ✅ | GPUI + std-thread dispatcher |
| Magic/content detection | ✅ | logic proven (Track-A probe) |
| Disk-usage treemap + HTML export | ✅ | pure logic, proven |
| Duplicate finder | ✅ | pure logic (blake3/xxh3), proven |
| Bulk rename (regex) | ✅ | pure logic, proven |
| Ant Trail, favorites, undo, SQLite metadata | ✅ | SQLite proven; UI paths unverified |
| Command palette (Cmd+K) | 🟡 | GPUI-drawn (works); no native menu bar |
| Code/syntax preview (tree-sitter) | 🟡 | grammars build; render path unverified |
| Icon grid / native file icons | ❌ | needs `icon.library` (shell stub) |
| Thumbnails | ❌ | needs shell + image decode + walker (gated) |
| Reveal in Workbench | ❌ | needs `workbench.library` (shell stub) |
| Native menu bar | ❌ | `gpui_aros` `set_menus` stub → Intuition menus |
| Native file requesters | ❌ | `gpui_aros` → `asl.library` |
| Media viewer (mpv) | ❌ | `feraille-video-mpv` is a macOS backend |
| Quick Look previews | ❌ | macOS-only; AROS-way = `datatypes.library` |
| Spotlight / indexed search | ❌ | macOS-only; no AROS equivalent yet |
| Tags, quarantine "where from" | ❌ | macOS-only; AROS-way = **filenote** comment |
| Drag & drop | ❌ | needs Intuition/Workbench drag |
| Multi-window / tabs | 🟡 | single window verified; multi untested |

### Chrome / bundled SVG icons don't draw — RESOLVED (2026-07-14)

Fixed live on booted AROS. Every icon now draws (toolbar, sidebar nav, window
buttons, sort chevrons, per-row folder icons); the `could not find asset` error
count went from 2041/frame to 0.

**Root cause: `rust-embed` was in debug filesystem-passthrough mode, so the SVG
bytes were never in the AROS binary.** Both icon bundles are `#[derive(RustEmbed)]`:
feraille's `LocalAssets` (`crates/feraille-gpui/src/assets.rs`, `#[folder = "resources"]`)
and the upstream `gpui_component_assets::Assets` (`#[folder = "assets"]`). Neither
enabled rust-embed's `debug-embed` feature. Without it a **debug** build does not
embed the files; it reads them from `$CARGO_MANIFEST_DIR/{resources,assets}/...`
on the host filesystem at runtime. That path exists on macOS (so `assets::tests`
and every desktop build pass) but not inside booted AROS, so `FeraAssets::load`
returned `Ok(None)` for every icon and gpui's `svg()` element drew nothing. The
`C:Feraille` in the image is a debug build (`link-aros.sh` `PROFILE=debug`), which
is exactly the profile rust-embed skips embedding in; a release build would have
masked the bug. The alpha-mask the ranked causes below argued about was never
produced, because the SVG source bytes were never found.

**Fix (one line):** add `debug-embed` to feraille-gpui's `rust-embed` feature list
(`crates/feraille-gpui/Cargo.toml`). rust-embed is feature-unified across the
graph, so the single flag also makes `gpui_component_assets` embed.

**How it was found — the AROS diagnostic channel had to be built first.** The
obvious step ("boot AROS and grep the run log") could not work: on AROS no app
output reaches the aros-ctl log at all. gpui's `.log_err()` routes through the
`log` crate facade, which is a no-op until a logger is installed, and nothing
installs one on AROS (the desktop binary does; the `feraille_aros_main` entry does
not). Feraille's own `eprintln!` does not reach the host log either, because AROS
routes a shell command's stderr to the AROS console, not the host fd 2 that
aros-ctl captures. So `obs::init()` now (AROS-only) installs a `log::Log` bridge
and mirrors every diagnostic line into `MacRW:feraille-log.txt` (host-shared,
readable as `~/AROS/Shared/feraille-log.txt`) — the same host-durable trick the
panic hook uses for `feraille-panic.txt`. With it, the `could not find asset`
lines were visible on the first frame and named the cause. Any future AROS-side
gpui diagnosis reads that file.

> The investigation below is kept for the record. It chased the render path
> (scale_factor / atlas / tint), but the actual cause was upstream: the assets
> were never embedded, so nothing downstream of asset load was ever reached.

Symptom (original): **most icons in the main window don't draw on AROS** — the
toolbar / sidebar / command glyphs *and* the Lucide file-type fallback glyphs (the
`739c9b2` fallback that kicks in because `fetch_icon_rgba` is a stub on AROS).
This is **separate from** the "native file icons" row above (that's the
`icon.library` shell stub); it's the bundled-SVG render path.

Traced from the Windows box against the pinned gpui (the AROS fork isn't
checked out there, so this is a read of the *shared* code + feraille-level
tests, not an AROS repro):

- **Not a bad asset.** A feraille test rasterizes every icon the app draws
  (`assets::tests` in `feraille-gpui`) through the same `usvg`/`resvg` path gpui
  uses — all pass, non-empty masks. So every glyph is a valid SVG.
- **Not the rasterizer.** `SvgRenderer::render_alpha_mask` →
  `render_pixmap` is pure Rust (`usvg` + `resvg`/`tiny_skia`) and produces an
  **A8 alpha mask**, exactly like text glyphs. The AROS smoke test already
  renders glyphs/gradients/quads through tiny-skia, so the mask is produced
  fine on AROS too.
- **So the failure is in `gpui_aros`'s paint path**, downstream of the mask:
  `elements/svg.rs` calls `window.paint_svg(bounds, …, color, …).log_err()`,
  which builds a `MonochromeSprite` (A8 atlas sample tinted by the element's
  text color) — the *same* primitive kind as text glyphs. The error is
  **swallowed into a log line**, so:

  **First step on the Mac: grep the AROS run log for the `paint_svg` failure.**
  `render_alpha_mask` bails with *"can't render at a zero size"* when
  `params.size` (bounds × `window.scale_factor()`) collapses — the leading
  hypothesis, since a bad AROS `scale_factor()` would zero the icon size while
  text (font-point sized) still renders. If the log is silent, the mask is
  reaching the atlas and the bug is in how `gpui_aros` uploads/draws
  `PrimitiveBatch::MonochromeSprites` for SVG-sourced masks vs. glyph-sourced
  ones (atlas texture kind, sampling, or the tint color resolving to the
  background). Compare the two in the `gpui_aros` renderer.

  Ranked causes: (1) `window.scale_factor()` / bounds → zero device size for
  icons; (2) monochrome-atlas allocation failing for icon-sized (24 px+) masks
  where glyph-sized ones fit; (3) the `MonochromeSprite` tint color resolving
  wrong on AROS. All three live in `gpui_aros`, not in feraille.

#### Action plan on the Mac (do these in order)

The whole loop runs on the Mac against the `zed-aros` checkout — nothing here is
reproducible from the Windows/Linux boxes (their renderers work). Boot Feraille
on AROS (`graft/aros-ctl run`, see [aros-building.md](aros-building.md)); the
window paints (title bar, rows, text) but the glyphs are missing.

1. **Read the log first — it almost certainly names the cause.**
   `elements/svg.rs` swallows every SVG paint failure with `.log_err()`, so the
   reason is already being printed each frame. Look for `can't render at a zero
   size` (from `SvgRenderer::render_alpha_mask`) or any atlas error. Feraille's
   panic/obs log lands in `MacRW:feraille-panic.txt` on hard failures, but these
   are *logged, not panicked* — capture stderr from `aros-ctl run` instead.
   - **Log is loud (`zero size`) → cause #1.** Confirm by logging
     `window.scale_factor()` and the SVG `bounds` once in `elements/svg.rs`
     before `paint_svg`. If `scale_factor()` is `0.0`/NaN or `bounds` is empty,
     fix it in `gpui_aros`'s window: return a sane device-pixel ratio (`1.0` if
     AROS has no HiDPI notion) from the `scale_factor()`/`PlatformWindow` impl.
     Text survives a zero scale factor because glyph rasterization floors the
     font size; SVG multiplies straight through to a 0×0 pixmap.
   - **Log is silent → cause #2 or #3.** The mask reached the atlas; the draw is
     wrong. Go to step 2.

2. **Minimal repro to iterate fast** (outside Feraille, in a `gpui_aros`
   example): one window whose root is `svg().path("icons/nav/home.svg").size_6()
   .text_color(white)` next to a `div().child("A")`. If the letter draws and the
   house doesn't, you've isolated the SVG-sprite path from text with a 20-line
   repro — rebuild-run that, not the whole app.

3. **Compare the two `MonochromeSprite` sources in the renderer.** Text glyphs
   and SVG icons emit the *same* `Primitive::MonochromeSprite`
   (`scene.rs`), batched as `PrimitiveBatch::MonochromeSprites`. In
   `gpui_aros`'s batch handler, check: (a) the atlas tile is allocated in the
   **monochrome/A8** texture (not the polychrome one) for both; (b) the sample
   uses the tile's real size — an icon tile (24 px+) is bigger than a glyph
   tile, so a row-packing or max-tile-size bug shows up only for icons;
   (c) the tint is `sprite.color` (the element's `text_color`), not the
   background — a swapped/zeroed color paints the glyph invisibly.

4. **Regression guard back in feraille:** once it draws, the existing
   `feraille-gpui` `assets::tests` (every icon rasterizes non-empty) already
   guards the *asset* side cross-platform; the *AROS render* side wants a
   `gpui_aros` conformance test (its `PORTING.md` bug→test ledger) that renders
   one SVG sprite and asserts non-zero coverage, mirroring the glyph test.

> Status-table cross-ref: the **Icon grid / native file icons ❌** row is the
> *separate* `icon.library` shell stub (real per-file OS icons). This section is
> the bundled-SVG chrome + Lucide-fallback path — fixing it makes the toolbar,
> sidebar, and type-icon glyphs appear even while native file icons stay stubbed.

#### Static analysis done on the Mac (2026-07-14) — the three ranked causes are ruled out

Followed the action plan on the Mac against the `zed-aros` checkout, but by code
read + host tests rather than a boot. **All three ranked causes above are
eliminated — the bug is not statically findable, so the run log is genuinely the
next step, not more code reading.**

- **Cause #1 (`scale_factor()` → 0) is impossible.** `gpui_aros`'s
  `window.rs::scale_factor()` returns `render_scale`, seeded by `dynres_scale()`,
  which `.clamp(0.25, 1.0)`s and defaults to `1.0` — it can never be `0.0`/NaN.
  And `Window::paint_svg` (shared gpui `window.rs`) sizes the raster as
  `bounds.size * SMOOTH_SVG_SCALE_FACTOR` (the constant `2`), **not**
  `* scale_factor()` — so the icon raster can't collapse to zero at scale 1.0.
  The "text survives a zero scale factor, SVG doesn't" theory doesn't apply here.
- **Causes #2 (atlas) and #3 (tint) are already pinned green on the host.**
  `cargo test -p gpui_aros` passes 16/16, including `svg_sprite_downsamples_into_bounds`,
  `glyph_sprite_blits_one_to_one`, and `monochrome_tint_is_premultiplied` — the
  exact monochrome-sprite atlas + downsample + premultiplied-tint path. The
  renderer draws SVG-sourced masks correctly on the host (a prior 2×-size icon
  bug, 2026-07-04, was already fixed and pinned there). The conformance suite
  the plan's step 4 asks for **already exists**.

So the fault is **runtime-specific to booted AROS**, downstream of the
host-tested Rust: either the SVG mask never reaches the atlas (real
`render_alpha_mask` bailing / `paint_svg` returning `Ok(None)` on the device,
which the `.log_err()` swallows), or the AROS-side scene upload/draw treats the
SVG `MonochromeSprites` batch differently from the glyph one at runtime. **Do
step 1 first: capture `graft/aros-ctl run` stderr and grep for the `paint_svg` /
`can't render at a zero size` / atlas line — it names which.** Static analysis
can't narrow it further from here.

### Roadmap (rough order)

1. **Make `gpui_aros` native-shell-complete** — menus, `asl.library` file
   requesters, cursor shapes. (See `gpui_aros/HANDOFF.md` §What's missing.)
2. **Implement `feraille-shell-aros` for real** (it's a Linux-stub re-export
   today): `icon.library` icons, Workbench reveal, `clipboard.device` file
   URLs, `datatypes.library` thumbnails.
3. **Harden the OS frontier** — the `emul-handler` bus-fault under concurrent
   metadata walkers (folder-size walker is gated off on AROS today; see Status).
4. **Native-the-Amiga-way features** — quarantine→filenote, previews via
   `datatypes.library`, an AROS media path.

### The AROS-specific crates (the owner flagged this — do it)

Both `gpui_aros` and `feraille-shell-aros` need AROS platform APIs, and both
currently reach them through ad-hoc C glue. Consolidate into a shared binding
layer instead:

- **`aros-sys`** — raw `extern "C"` + C shim for exec/dos/intuition/graphics/
  cybergraphics/keymap/asl/workbench/icon/datatypes/clipboard.device.
- **`aros`** — safe wrappers (`Window`, `MenuStrip`, `FileRequester`, `Icon`,
  `DataType`, `Clipboard`, …), RAII, `-ffixed-x18`-safe.

Then `feraille-shell-aros` builds reveal/icons/thumbnails on `aros`, and
`gpui_aros` moves its window/menu/dialog/clipboard glue onto the same crate.
Full rationale + surface in
[`gpui_aros/HANDOFF.md`](../../../zed-aros/crates/gpui_aros/HANDOFF.md#proposed-next-architecture-an-aros-platform-crate-family).

## Running on hosted AROS

```sh
# build + link + deploy the GPUI smoke:
cd ~/Source/zed-aros
cargo +nightly-2026-06-27 build -p gpui_aros_smoke --target ... (as above)
crates/gpui_aros_smoke/link-aros.sh          # -> C:GpuiSmoke in the image

# boot the desktop with the smoke as payload, screenshot:
cd ~/Source/aros-aarch64
AROS_CTL_DESKTOP_EXTRA='Run <NIL: >NIL: QUIET C:GpuiSmoke' \
  AROS_CTL_STARTUP_MODE=desktop graft/aros-ctl run
graft/aros-ctl shot proof.png
```

## Build tree location (stable) + recovery

The canonical AROS build tree and toolchain live **outside `/tmp`** (moved
2026-07-05 after the macOS `/tmp` cleaner repeatedly gutted them):

- **`~/aros-build`** — the AROS SDK / build tree / boot image.
- **`~/aros-crosstools`** — the preserved patched clang 20.1.0 + `ld.lld` +
  compiler-rt (the expensive part; never rebuild it).

`check-aros.sh`, `link-aros.sh`, and the `.cargo/config.toml` CFLAGS all point
here; `gpui_aros/build.rs` probes `$AROS_BUILD` (default `~/aros-build`) for the
SDK headers. **To rebuild/recover the whole boot + desktop set** in one command
(reusing the toolchain): `aros-aarch64/graft/rebuild-aros.sh` (see
[build/README.md](../../../aros-aarch64/docs/features/build/README.md)). Then
rebuild + relink Feraille: `cargo … build -p feraille-aros-app …` then
`crates/feraille-aros-app/link-aros.sh`.

### Legacy: the `/tmp` GC failure modes (why the move happened)

macOS periodically GCs `/tmp` **by file atime**, so `/tmp/arosbuild` decayed
piecemeal — headers, kickstart modules, C: commands and crosstools binaries
vanished independently while directories survived. Encountered:

- SDK headers (`gen/include`) — regenerate with the scratch recipe:
  configure a fresh `/tmp/arosbuild2` (thin xtools per
  `graft/build-darwin-aarch64.sh` step 2, retargeted at
  `/tmp/aros-crosstools`'s patched clang) and `make includes`, then rsync
  over the canonical tree.
- `Devs/FileSystem.resource` (kickstart module) — `make kernel-filesystem`
  in the scratch tree, copy over. Needs the crosstools clang (spec flags
  like `-noposixc`), Homebrew clang builtin headers copied into its GC'd
  resource dir, and `lib/generic/libclang_rt.builtins-aarch64.a` symlinked
  into the xtools.
- **`AROS.boot`** — the sneakiest one. dos's `__dos_IsBootable` requires a
  signature file `:AROS.boot` at the boot volume root containing the CPU
  string (`aarch64`). With it GC'd, every mount succeeds but the boot
  volume is deemed "not bootable" (Res2=212) and dos.library's init
  returns NULL — surfacing as the wildly misleading `Could not open
  version 36 or higher of library "dos.library"` alert from dosboot.
  Restore with `echo aarch64 > <bootdir>/AROS.boot`. (Diagnosed by
  transplanting a `-DDEBUG=1` dos.library built via
  `make kernel-dos` in the scratch tree — the `Dos/CliInit:` trace names
  the failing step directly. Keep the debug build out of normal boots:
  it logs every DOS packet and makes big LoadSegs crawl.)
- Single-file transplants from the scratch tree into the old image work
  fine (FileSystem.resource, dos.library round-tripped cleanly);
  `make <metatarget>` in the scratch tree is the recovery tool of choice.
  Avoid plain `make` — the default target tries to fetch and build LLVM.

The durable fix would be moving the canonical build out of `/tmp` (the
tooling honors `AROS_BUILD`/`BUILD`/`AROS_CTL_BOOTD` env for that).
