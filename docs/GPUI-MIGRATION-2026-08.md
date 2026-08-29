# GPUI dependency migration — 2026-08-28

This is the checkpoint for Ferail's move to the current coherent
gpui-component/Zed pair. It records what is complete, which local deltas remain
justified, and what must be finished on platform-native machines before a
release.

## Pinned pair

- Rust: `1.97.1` (workspace MSRV and `rust-toolchain.toml`).
- gpui-component / assets: `e8f54ebf0af0b0d5773b38111bb5b5f308c57781`.
- Zed / GPUI: `f66ed399cdde86092af8af3dc7b418abf45f37f8`.

`e8f54eb` leaves its Zed dependencies unpinned; its own lockfile exercises
`f66ed399`. Ferail deliberately updates the one shared bare-git source in
`Cargo.lock` to that commit. Do not add `?rev=` to only one declaration: Cargo
then creates a second source identity and the GPUI types stop interoperating.

## Adaptations made in Ferail

- Frame statistics use the new profiler trace API and count only
  `FrameEvent::Draw` events, preserving the status bar's redraws-per-second
  meaning.
- The completion-capable filter, Cmd+L breadcrumb, and Go to Folder field use
  the new `EditorState` language-feature boundary. They are explicitly
  single-line-submit, non-wrapping, with line numbers and indent guides off;
  Ferail keeps its proportional UI font and inline clear button.
- The local virtual-list handle implements the new viewport-bounds contract.
- Development screenshot teardown passes the app context to GPUI's new focus
  API.
- `vendor/gpui_windows` was rebased from Zed `38ca9106` to `f66ed399`. It now
  receives the merged Windows `render_to_image` implementation from upstream;
  its only functional delta remains outbound Shell/OLE file drag and drag-over
  throttling/effect negotiation.
- The standard GPUI graph no longer contains `stacker`, so the unused global
  patch was removed. The older AROS fork still receives its AROS stacker shim
  from the AROS-only Cargo config until that fork is rebased.

## Validation completed on macOS

- `cargo check --workspace --all-targets`: clean.
- `cargo test --workspace --all-targets --no-fail-fast`: all executed tests
  pass; only the existing network and Spotlight tests remain ignored.
- `LEAK_BACKTRACE=1 --screenshot --properties`: exits cleanly and writes a
  nonblank PNG.
- Breadcrumb completion screenshot driven by real Down/Enter actions: exits
  cleanly.
- Unsafe local filter screenshot: visually checked after removing the editor
  line-number gutter; the field and clear affordance match the title bar.
- Dependency audit: one Zed source at `f66ed399`, one gpui-component source at
  `e8f54eb`; local `ztracing` is the only tracing shim, with no `zlog` or
  standard-build `stacker` in the graph.

The macOS host cannot finish a Windows-target GPUI check without the Windows
resource compiler and SDK. The isolated build reaches the updated
`gpui_windows` crate, then GPUI's build script stops at missing `llvm-rc`; this
is an environment boundary, not a successful Windows validation.

## Local deltas still justified after the bump

| Delta | Post-bump finding | Decision |
| --- | --- | --- |
| `multi_table/` modifier-aware selection | Upstream row selection events still omit click modifiers. | Keep; issue #2795 remains the right upstream discussion. |
| Context-menu wrapper | `PopupMenu::rebuild` now solves async submenu content; root open/close remains private and Shift+right-click needs a Windows guard. | Removed revision polling/root rebuild; keep only the narrow platform wrapper. Issues #2796/#2797 describe the remaining general gaps. |
| `vendor/gpui_windows` external drag | Upstream Windows still leaves external-drag promotion disabled. | Keep the narrow OLE delta; native Windows regression test required. |
| `vendor/ztracing` | Zed still links its tracing crate through GPUI/sum-tree. | Keep the permissive no-op source patch and graph assertions. |
| GPUI dev/test teardown | The old per-paint strong callback is gone in e8f54eb, and the deterministic leak repro passes. | Keep containment until native Windows close/overlay tests pass; do not propose a PR without a new minimal repro. |

No new upstream PR is authorized by this migration. Before proposing one:
reproduce on current upstream without Ferail, reduce to a minimal tested patch,
read that repository's current contribution rules, check the user's existing
open-PR count, and present the candidate for approval. Three concurrent PRs is
a hard ceiling, not a target.

## Platform handover still required

### Windows

On a native Windows checkout, run the normal clean build/test workflow, then
exercise:

1. hidden `--screenshot` capture (nonblank PNG, no flash/taskbar/Alt-Tab);
2. drag files and folders out to Explorer, back into Ferail, and across Ferail
   windows with Ctrl/Shift/Alt effect negotiation;
3. filter token completion, Cmd+L path completion, Go to Folder, and the clear
   button;
4. ordinary close and Quit with a focused input, Get Info window, and context
   menu open under the dev leak detector;
5. the existing P0 Windows plan in `docs/testing/WINDOWS_HANDOVER.md`.

### AROS

The checked-out `../zed-aros` branch is still based on the pre-migration GPUI,
and `../gpui-component-aros` is not present on this Mac. Before claiming AROS
compatibility:

1. rebase the GPUI AROS fork onto `f66ed399`, preserving `gpui_aros` and its
   target-gated deltas;
2. rebuild the gpui-component AROS worktree at `e8f54eb`, adapting its narrow
   smol-channel substitution to the new base/UI split;
3. verify the AROS Cargo graph resolves both source patches, AROS libc
   `0.2.186`, filetime `0.2.26`, and only the necessary stacker shim;
4. run the host `gpui_aros` conformance tests, `scripts/check-aros.sh`, link,
   and the on-device input/render smoke list.

Until those steps pass, macOS/Windows/Linux migration is validated but AROS is
explicitly pending rather than silently assumed compatible.
