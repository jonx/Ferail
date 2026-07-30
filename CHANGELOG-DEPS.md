# Dependency changelog (pinned git revs)

`gpui` (zed) and `gpui-component` are git dependencies with no crates.io
releases, so reproducibility comes from the committed `Cargo.lock` plus this
log of the locked revs at each deliberate bump. See the pinning-strategy
comment in the root `Cargo.toml` and the friction log in
[docs/GPUI-UPSTREAM.md](docs/GPUI-UPSTREAM.md).

Pinning rule: `gpui-component` pins `gpui` to an explicit zed rev, and our
`gpui` / `gpui_platform` MUST mirror that exact rev so the graph holds a single
gpui (see GPUI-UPSTREAM.md #1). When bumping `gpui-component`, read its
`Cargo.toml` `gpui = { rev = ... }` and copy it here.

## 2026-06-16 — bump gpui-component (60 commits)

| crate | from | to |
|---|---|---|
| `gpui-component` / `-assets` | `ba44512e0cfe633869714054e8ea1afd133d4a28` (2026-05-13) | `c112e7b482b6b9c53a8c43deaf5847b94a29ad82` (2026-06-16) |
| `gpui` / `gpui_platform` (zed) | `d2953a2b57cd94402dad5fcf20746d8241ada00f` (unpinned, floated) | `1d217ee39d381ac101b7cf49d3d22451ac1093fe` (now pinned to match gpui-component) |

Notable: picks up ~12 TextView/highlighter fixes (code-block perf, scrollbar,
selection, empty-range style leak) and dropdown/ComboBox fixes — all relevant
to the preview pane and settings. Required source changes: 4 `flex_grow()` /
`flex_shrink()` calls in the forked `multi_table` gained a mandatory `f32` arg
(GPUI-UPSTREAM.md #3); `gpui` had to be pinned to mirror gpui-component's rev
(GPUI-UPSTREAM.md #1). Verified: `cargo check -p ferail-gpui` + scoped tests
green; preview/table/markdown screenshots inspected.

## Baseline — initial pins

`gpui-component` `ba44512…` with `gpui` left unpinned (matched gpui-component's
then-unpinned `gpui`). This worked until gpui-component started pinning `gpui`
to an explicit rev, which forced the mirroring rule above.
