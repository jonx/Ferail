# GPUI Migration

Feraille is migrating from its custom soft renderer (`feraille-render` +
`feraille-controls`) to **GPUI** (Zed's GPU-accelerated UI framework) +
**gpui-component** (Longbridge's component library on top of GPUI).

Started 2026-05-13. Estimated six to eight weeks of part-time work.

## Why

The soft renderer composes curves from `fill_rect` calls. Rectangle
edges round to integer pixels with no fractional-coverage blending, so
every rounded surface and circle visibly stair-steps (most evident in
the Phase 0 / iter-5.11 settings preview tiles). The fix list grew as
polish progressed; a one-shot rendering rewrite was the structural
answer.

GPUI also brings, for free: native window chrome, animation pipeline,
virtualization, focus tree, action/keymap system, drag-and-drop, and a
real text/font stack.

## Strategy: parallel build, then harvest

A new `crates/feraille-gpui` crate sits next to `crates/feraille-app`.
Both binaries (`Feraille` and `feraille-gpui`) compile and run every
day of the migration. The cutover deletes `feraille-app`,
`feraille-render`, and the relevant `feraille-controls` modules in a
single PR — but only after every feature is ported.

**Direction: harvest out of the old app into the new one.** The old
binary is the *source* we're reading from; the new binary is the
destination. Importing new-app pieces back into the old codebase
creates a hybrid that has to satisfy both worlds and never reaches a
"done" state. The migration plan only works if one side actively
shrinks. The old app is the shrinking side.

Concretely, once the harvest started (Phase 5.6+):

- The old `feraille-app` is **strictly frozen** — only deletions and
  bug fixes for shipped users. Bug-fix-while-porting goes into the
  new app with a note in [PORTING.md](PORTING.md).
- The single source of truth for migration progress is
  [docs/PORTING.md](PORTING.md). One section per feature, with
  status (Not started / In progress / Ported / N/A). Updated at every
  commit boundary; answers "what's left?" without anyone needing to
  remember.
- Pure-logic modules in the old app that don't depend on
  `feraille-render` / `feraille-controls` move into `feraille-gpui` —
  or into an already-existing shared crate when one fits (e.g.
  `disk_usage_state.rs` → `feraille-disk-usage`). UI-coupled code is
  rewritten using gpui-component primitives; the old code is the
  spec, not the implementation.

## Crates

| Crate | Status during migration | Status after cutover |
|---|---|---|
| `feraille-core` | Shared (domain) — stays UI-agnostic, used by both. | Kept. |
| `feraille-design` | Shared (tokens) — gpui-component has its own theme system; the design brief tokens *map onto* `ThemeColor` rather than being replaced. | Mostly kept; the rect-renderer-specific dims may go. |
| `feraille-render` | Frozen. Only used by `feraille-app`. | **Deleted.** |
| `feraille-controls` | Frozen. Only used by `feraille-app`. | **Deleted.** |
| `feraille-fs-native` | Shared (domain). | Kept. |
| `feraille-meta` | Shared (domain). | Kept. |
| `feraille-disk-usage` | Shared (domain). | Kept. |
| `feraille-shell-mac` | Shared (platform). Some bits (`NSMenu` context-menu builder) are obviated by `gpui_component::menu`; audited in Phase 5. | Slimmer. |
| `feraille-shell-win32` | Frozen — Windows isn't a v1 target on the new stack. | Kept or deleted depending on Phase 5 audit. |
| `feraille-app` | Frozen. | **Deleted.** |
| `feraille-gpui` | **Active.** New work lands here. | Becomes the only app. |

## Phases

- **Phase 0 — Sandbox.** Throwaway crate at `~/Source/feraille-gpui-sandbox/`
  outside the workspace. Answered three paradigm questions: chained
  styling API, entity model, visual default. Verdict: **commit**.
  ([sandbox NOTES.md](../../feraille-gpui-sandbox/NOTES.md) in that crate.)
- **Phase 1 — Foundation.** New `feraille-gpui` crate in the workspace
  with pinned git deps and an empty shell (sidebar + main pane, no
  domain logic). Both binaries build.
- **Phase 2 — Domain isolation.** Audit every non-UI crate
  (`feraille-core`, `feraille-fs-native`, `feraille-meta`,
  `feraille-disk-usage`, `feraille-shell-mac`) to confirm it compiles
  with zero dependency on `feraille-render` / `feraille-controls`.
- **Phase 3 — Settings panel.** Port the Settings panel first; it's
  small, self-contained, and exercises every component category we'll
  need elsewhere. Last cheap off-ramp at the end of this phase.
- **Phase 4 — Main surfaces.** Header → sidebar → virtualized file
  list → preview pane → toolbar → empty states. Use gpui-component's
  virtualized `Table` for the file list, not a hand-rolled list.
- **Phase 5 — Native integrations.** Action/keymap system, file-watcher
  events on the foreground executor, drag-and-drop, context menus,
  window state persistence.
- **Phase 6 — Cutover.** Performance audit, parity walkthrough, one PR
  that flips the default binary and deletes the old crates.

## Dependency discipline

- All git deps follow the pin strategy documented in
  [CHANGELOG-DEPS.md](../CHANGELOG-DEPS.md).
- `gpui` is **unpinned** at `[workspace.dependencies]` (URL-matched
  with gpui-component's unpinned dep so cargo unifies them); the
  actual commit is locked in `Cargo.lock`.
- `gpui-component` itself is rev-pinned because it's our higher-level
  coupling.
- Bus factor: `gpui-component` is one person's part-time project.
  Mitigation: no wrappers around it, only public API. Track which
  components we use in [gpui-component-usage.md](features/gpui-component-usage.md)
  (created in Phase 3) so a fork stays tractable.

## Running

```sh
# Old app (soft renderer):
cargo run --bin Feraille

# New shell (GPUI):
cargo run --bin feraille-gpui
```

Both work every day. If only one does, the migration is broken.

## Where today's work goes

Today's `feraille-controls::primitives::{draw, settings_widgets}` and
`feraille-app/src/main.rs` Settings paint code is the **specification**
for the Phase 3 Settings port, not code to translate. The design
tokens, the IA (sidebar nav with Appearance / Files / Layout / About),
preview tiles, segmented Narrow/Medium/Wide width stops, and the
"changes save instantly" footer all carry forward as user-facing
behaviour. The Rust implementing them is on the chopping block at
cutover.
