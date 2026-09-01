# Windows Headless Screenshots - No-Flash Plan

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

Ferail's screenshot CLI should be able to render Windows UI states to PNGs
without ever showing a real window. The desired behavior is the same as macOS:
`ferail-gpui --screenshot ...` opens a GPUI window with `show: false` and
`focus: false`, waits for the requested state to settle, captures the rendered
framebuffer, writes the PNG, and exits. There should be no taskbar button, no
Alt-Tab entry, and no visible window flash.

## Current shape

The screenshot harness is in
[`screenshot.rs`](../../crates/ferail-gpui/src/screenshot.rs). Its primary
capture path calls `Window::render_to_image()` on an invisible GPUI window.
That is the right architecture for no-flash screenshots because it samples the
renderer output instead of asking Windows to capture an onscreen HWND.

Windows support is upstream as of Zed PR
[#63012](https://github.com/zed-industries/zed/pull/63012). Ferail pins Zed
`f66ed399`, which contains that merge. The in-repo `gpui_windows` fork is based
on the same revision and differs only for outbound Shell/OLE file dragging; it
does not duplicate the DirectX readback implementation. The historical patch
under `patches/` is review provenance and is not wired into Cargo.

## Work to finish

1. Keep `gpui_platform`'s `test-support` feature connected to
   `gpui_windows/test-support`. The trait method is available only behind the
   same support feature used by the macOS path.

2. Keep the screenshot window hidden in
   [`screenshot.rs`](../../crates/ferail-gpui/src/screenshot.rs):
   `WindowOptions { show: false, focus: false, ... }`. Do not show, move, or
   minimize the window in the primary capture path.

3. Keep `Window::render_to_image()` as the only normal Windows screenshot
   backend. It should render into GPUI's DirectX target, copy to a staging
   texture, map CPU-readable pixels, convert BGRA to RGBA, and return an
   `image::RgbaImage`.

4. Treat [`capture_window_rgba`](../../crates/ferail-shell-win32/src/capture.rs)
   and `PrintWindow` as emergency fallback/debugging code only. `PrintWindow`
   captures HWND contents and is tied to compositor/window visibility behavior;
   it can require showing or moving a window and is exactly the path that risks
   the visible flash.

5. Re-run the native Windows acceptance checks whenever the Zed snapshot or
   the vendored Windows backend moves. A macOS cross-check cannot execute the
   Windows resource compiler and is not a substitute for that run.

## Acceptance checks

Run these on a real Windows machine after the dependency wiring is portable:

- Start `ferail-gpui --screenshot screenshots\win-baseline.png` from a clean
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
