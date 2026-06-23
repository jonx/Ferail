# Windows Headless Screenshots - No-Flash Plan

Feraille's screenshot CLI should be able to render Windows UI states to PNGs
without ever showing a real window. The desired behavior is the same as macOS:
`feraille-gpui --screenshot ...` opens a GPUI window with `show: false` and
`focus: false`, waits for the requested state to settle, captures the rendered
framebuffer, writes the PNG, and exits. There should be no taskbar button, no
Alt-Tab entry, and no visible window flash.

## Current shape

The screenshot harness is in
[`screenshot.rs`](../../crates/feraille-gpui/src/screenshot.rs). Its primary
capture path calls `Window::render_to_image()` on an invisible GPUI window.
That is the right architecture for no-flash screenshots because it samples the
renderer output instead of asking Windows to capture an onscreen HWND.

Windows support depends on a GPUI backend patch:

- [`patches/gpui-windows-render-to-image.patch`](../../patches/gpui-windows-render-to-image.patch)
  adds `gpui_windows` D3D11 staging-texture readback.
- Root [`Cargo.toml`](../../Cargo.toml) currently wires this through a local
  `[patch."https://github.com/zed-industries/zed"]` path to
  `../zed-feraille-patch`.
- [`docs/GPUI-UPSTREAM.md`](../GPUI-UPSTREAM.md) tracks the upstream issue and
  patch details.

That local path is fine for proving the feature on one workstation, but it is
not a portable final state for the branch.

## Work to finish

1. Move the GPUI Windows `render_to_image` patch out of a local sibling path.
   Prefer an upstream Zed PR. If that is not ready, use a Feraille-owned fork
   pinned by `git` + `rev` so a fresh checkout resolves without local files.

2. Keep `gpui_platform`'s `test-support` feature connected to
   `gpui_windows/test-support`. The trait method is available only behind the
   same support feature used by the macOS path.

3. Keep the screenshot window hidden in
   [`screenshot.rs`](../../crates/feraille-gpui/src/screenshot.rs):
   `WindowOptions { show: false, focus: false, ... }`. Do not show, move, or
   minimize the window in the primary capture path.

4. Keep `Window::render_to_image()` as the only normal Windows screenshot
   backend. It should render into GPUI's DirectX target, copy to a staging
   texture, map CPU-readable pixels, convert BGRA to RGBA, and return an
   `image::RgbaImage`.

5. Treat [`capture_window_rgba`](../../crates/feraille-shell-win32/src/capture.rs)
   and `PrintWindow` as emergency fallback/debugging code only. `PrintWindow`
   captures HWND contents and is tied to compositor/window visibility behavior;
   it can require showing or moving a window and is exactly the path that risks
   the visible flash.

6. Once the upstream/fork dependency is in place, remove or update comments that
   describe the local `../zed-feraille-patch` setup as required. The branch
   should document one reproducible dependency path.

## Acceptance checks

Run these on a real Windows machine after the dependency wiring is portable:

- Start `feraille-gpui --screenshot screenshots\win-baseline.png` from a clean
  checkout with no local sibling GPUI clone.
- Confirm no window appears, no taskbar button is created, and focus stays with
  the launching terminal/editor.
- Confirm the output PNG exists and is nonblank.
- Repeat with representative surfaces: shell, settings, disk usage, viewer, and
  at least one overlay-heavy state.
- Search the screenshot harness and verify the primary path still uses
  `Window::render_to_image()` rather than `capture_window_rgba()`.

If all of those pass, Windows screenshots are on par with macOS for the
no-visible-window development loop.