# Dependency changelog (pinned git revs)

`gpui` (zed) and `gpui-component` are git dependencies with no crates.io
releases, so reproducibility comes from the committed `Cargo.lock` plus this
log of the locked revs at each deliberate bump. See the pinning-strategy
comment in the root `Cargo.toml` and the friction log in
[docs/GPUI-UPSTREAM.md](docs/GPUI-UPSTREAM.md).

Pinning rule: the graph must hold a single gpui source (see GPUI-UPSTREAM.md
#1), so our `gpui` / `gpui_platform` declaration mirrors *how* gpui-component
declares gpui at the pinned rev: when gpui-component pins an explicit zed rev,
copy that exact rev; when it leaves gpui unpinned (the style since ~2026-07),
leave ours unpinned too and let the committed `Cargo.lock` carry the actual
zed rev — bumped deliberately with `cargo update -p gpui`.

## 2026-08-26 — promote checksum implementations to direct dependencies

Sidecar verification now directly uses `crc32fast` 1.x, `md-5` 0.10 and
`sha1` 0.10 alongside the existing direct `sha2` 0.10 dependency. These pure
Rust implementations were already present transitively in `Cargo.lock`; the
change makes the formats Ferail calls part of its explicit dependency surface.

## 2026-08-20 — add reqwest_client (same zed rev; no bumps)

The update check needs real HTTP behind gpui's `cx.http_client()`, so
`reqwest_client` joins from the **already-locked** zed rev (`38ca910`) —
not a bump; `cargo update -p gpui --precise` was used to keep the whole
zed set on that rev after cargo tried to ride the branch head. Its
`zed-reqwest`/tokio underpinnings were already in the graph via
`gpui-component-assets`; genuinely new transitives are the rustls TLS
stack (`http_client_tls`, `rustls-platform-verifier`, `aws-lc-rs`,
webpki roots) plus `futures-lite` as a direct dep. The vendored
`stacker`/`tar`/`filetime` patches were verified still active after the
re-resolve (a first attempt silently dropped the `stacker` patch — check
`[[patch.unused]]` in Cargo.lock whenever touching the zed source).

## 2026-08-08 — bump gpui-component (~8 weeks), zed to current main

| crate | from | to |
|---|---|---|
| `gpui-component` / `-assets` | `c112e7b482b6b9c53a8c43deaf5847b94a29ad82` (2026-06-16) | `6d7847e33ef3b8e043d1fcea82eb7e1b7f919d8b` (2026-08-08) |
| `gpui` / `gpui_platform` (zed) | `1d217ee39d381ac101b7cf49d3d22451ac1093fe` (2026-06-12, pinned) | `38ca9106c5306ef93e52c35643df015a27f15b72` (2026-08-07, unpinned — locked via Cargo.lock) |

Motive: zed #58161 (2026-07-29) added the `external_drag_payload` API — the
missing piece that makes dragging files out to Finder/other apps a real
native drag session (see GPUI-UPSTREAM.md #9). gpui-component `main` dropped
its gpui rev pin again, so both float and the lockfile is the pin.

Fallout handled in the same change:
- gpui gained a **direct GPL-3.0 `ztracing` dependency** (zed `00cba838a`);
  the `vendor/sum-tree` severance was no longer sufficient and was replaced
  by the clean-room `vendor/ztracing` no-op stub (GPUI-UPSTREAM.md #8), which
  also keeps `zlog` / `ztracing_macro` out and needs no per-bump re-sync.
- One source break: gpui-component's `LanguageConfig.language` became
  `Option<tree_sitter::Language>` (grammarless languages); `syntax_extra.rs`
  now skips registry entries without a grammar.
- wgpu moved from zed's git fork (29.0.3) to crates.io 29.0.4; taffy 0.10 →
  0.12; assorted transitive churn recorded in Cargo.lock.

Verified: `cargo check` / `clippy` (no new warnings from this change) /
workspace tests green; screenshot harness renders; drag-out to Finder needs
an interactive session to confirm (OS drag gestures aren't headlessly
drivable).

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
