# Dependency changelog

Tracks every external-dep rev bump in the GPUI migration. Update on a
schedule (monthly is reasonable), not on impulse. When something breaks
after an update, having this file makes the bisect trivial.

The actual reproducibility comes from `Cargo.lock` (committed). This
file is the human-readable narrative: *why* we changed each pin and
what broke when we did.

## 2026-05-13 — Initial pins (Phase 1)

First migration commits.

- `gpui` (git+https://github.com/zed-industries/zed) — unpinned at the
  `[workspace.dependencies]` level. See the Cargo.toml comment for why:
  pinning a rev here makes the source URL diverge from gpui-component's
  unpinned `gpui` dep, producing two copies in the graph.
  `Cargo.lock` locks the actual commit to `481854f7541636c63f79abec9785697ed74f4005`.
- `gpui_platform` — same as gpui; locked to `481854f7…`.
- `gpui-component` — pinned to `ba44512e0cfe633869714054e8ea1afd133d4a28` (2026-05-13).
- `gpui-component-assets` — pinned to `ba44512e0cfe633869714054e8ea1afd133d4a28`.

System prerequisite (one-time, not in Cargo.toml):

- macOS Metal Toolchain (`xcodebuild -downloadComponent MetalToolchain`).
  Required to compile gpui's Metal shaders. ~500 MB.
