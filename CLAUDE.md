# Claude Notes for Feraille

This file is the operating manual when an LLM (Claude or otherwise) edits
this project. The specs in `specs/` are the *what*; this file is the *how*.

## Read these first, every session

1. [specs/controls/00-overview.md](specs/controls/00-overview.md) — control
   inventory and hard rules.
2. [specs/ux/00-overview.md](specs/ux/00-overview.md) — what the app is and
   isn't.
3. The crate-level docstring in any file you're about to touch.

If a request contradicts the spec, **flag it before coding**. The right
move is usually "edit the spec, then edit the code."

## Hard rules (carried from the spec)

1. **No raw color literals or pixel values outside `feraille-design`.**
   If a control needs a color it doesn't have, the *palette* is wrong.
2. **No allocation on the hot path.** `VirtualizedList::paint` and the
   renderer's `fill_rect` / `draw_text` must not allocate. The current
   slice violates this in `fake_item` (returns owned strings); fixing
   that is iteration 2's first task.
3. **No platform code in `feraille-core`, `feraille-design`,
   `feraille-controls`, or `feraille-render` (soft backend).** Use
   `cfg(windows)` only inside `feraille-shell-win32` and the production
   D2D renderer when it lands.
4. **Layout is in DIPs; surfaces are in physical pixels.** The renderer
   trait owns the conversion — controls never see `scale_factor`.
5. **Paint is read-only.** No I/O, no `ensure_*` calls, no path
   resolution during paint. Inherited from Ferail (the predecessor) and
   non-negotiable.

## Architecture invariants

- **Controls speak `NodeId`, not `PathBuf`.** The mapping
  `NodeId -> path/PIDL` lives in the FS layer.
- **`feraille-controls` does not depend on `feraille-fs-*` or
  `feraille-shell-*`.** Direction of dependency is one-way.
- **Tokens are immutable for the process lifetime.** Theme switching is
  a v2 concern; in v1 we set `Tokens::for_theme(Theme::detect())` once
  in `App::new`.

## Renderer abstraction

`Renderer` (in [crates/feraille-render/src/lib.rs](crates/feraille-render/src/lib.rs))
is the seam between controls and graphics. Two implementations are
planned:

- `SoftRenderer` (current): CPU rasterization into an `[u32]` buffer
  presented via `softbuffer`. Cross-platform; used on macOS dev mode and
  for unit tests.
- `D2DRenderer` (iteration 3, Windows-only): Direct2D + DirectWrite. Same
  trait surface; controls do not change.

Anti-pattern to avoid: cfg-gating control code on the rendering backend.
If a control needs to know which backend it has, the trait is wrong.

## Coordinate convention

- All `Renderer` methods take coordinates in **DIPs**.
- The soft renderer's internal `pixels` buffer is in physical pixels.
- Conversion happens *inside* the renderer impl (`scale_factor`).
- DPI scale changes route through `WindowEvent::ScaleFactorChanged` →
  `SoftRenderer::set_scale_factor`.

This is a deliberate inversion of the Ferail (predecessor) pattern, where
controls had to call `gdi::physical_to_dips`. Mistakes there caused the
DPI bugs noted in the predecessor's `CLAUDE.md`. Don't repeat them.

## Where to put things

- New design token? `feraille-design/src/lib.rs`. Update the spec table
  in `specs/controls/01-design-tokens.md` in the same change.
- New primitive? `feraille-controls/src/primitives/<name>.rs`, plus a
  spec entry in `specs/controls/02-primitives.md`.
- New explorer-specific control? `feraille-controls/src/<name>.rs` plus
  `specs/controls/03-explorer-controls.md`.
- New renderer capability? Add to the `Renderer` trait *and* implement
  in `SoftRenderer` in the same change. No optional methods.

## Windows-specific work

When porting shell features from the predecessor Ferail
(`/Users/jkn/Source/Ferail/crates/ferail-win32`):

- `shell.rs`, `shell_pump.rs`, `enumerate.rs`, `wsl.rs` →
  `feraille-shell-win32::shell` (rename to clearer module names).
- `popup_menu.rs`, `menu_*.rs` → `feraille-shell-win32::context_menu`.
- `drag_drop.rs` → `feraille-shell-win32::drag_drop`.
- `d2d.rs`, `gdi.rs` → `feraille-render-d2d` (a new crate; do **not**
  put rendering inside the shell crate).

The decoupling line is: **shell crate is HWND-aware but UI-unaware**.
It registers `IDropTarget` on an HWND owned by the app; it doesn't draw
the drag preview (that's in `feraille-controls`).

## Testing

- State-machine tests live with the control they test (e.g.,
  `virtualized_list::tests`). They don't render; they assert state
  transitions on synthetic events.
- Renderer correctness tests use a small fixed-buffer `SoftRenderer`,
  paint a known scene, and assert pixel-exact bytes. Cheap to run.
- Performance tests live in `crates/feraille-controls/benches/` (not yet
  set up).

## Iteration roadmap

| # | Adds | Removes |
|---|---|---|
| 1 | workspace, specs, soft renderer, virtualized list of fake rows | — |
| 2 | Zed-aligned tokens, glyph cache, real $HOME FS, Selection, Scrollbar, Splitter, FocusRing overlay, Sidebar (transient), then BreadcrumbBar (read-only), TabStrip + per-tab state, minimal FileTree, App v2 | per-frame alloc, fake_item, Sidebar (replaced by FileTree) |
| 2.5 | TextInput, Breadcrumb edit mode (Cmd+L) | — |
| 2.6 | Headless screenshot CLI (`--screenshot`), scrollbar contrast fix, chevron glyph fix | — |
| 2.7 | Standalone header removed, tree reveal-on-navigate + auto-scroll, fold/unfold redraw + perf bug fixed | redundant header strip |
| 3 (current) | Open-file (Enter), F5 refresh, Ctrl+H / Cmd+Shift+. hidden toggle, Delete to ~/.Trash, file-type icon hue, OS window-title sync | — |
| 3.5+ | List row hover feedback, in-memory ant trail (folder visits), F2 inline rename, Cmd+F search/filter, clipboard ops, tabs persistence | — |
| 4 (current) | macOS shell crate: 4.1 native chrome (transparent titlebar + traffic-light inset). 4.2+: NSPasteboard drag/drop, NSMenu context menu, NSVisualEffectView vibrancy, NSWorkspace recycle | rename-to-Trash fallback (4.2+) |
| 5 | wgpu/Vello GPU renderer; threaded + change-watching FS via FSEvents | sync FS enumeration |
| 6 | Persisted ant trail (SQLite), magic file detection (port Ferail's type DB) | in-memory trail |
| 7 | Disk-usage treemap (port from Ferail) | — |
| 8 | Windows shell crate: IContextMenu, IDataObject DnD, IFileOperation | — |

Don't skip iterations to chase a feature. Each iteration removes one
"slice violation" from the previous so the codebase tightens around the
spec instead of accruing exceptions.

## When you're stuck

- **Compile errors involving `Renderer`?** You probably leaked a
  platform type into the trait. The trait must be platform-agnostic.
- **A control needs OS state (HWND, accent color)?** Pass it via the
  `Tokens` (for design state) or via a side-channel `&dyn Host` (for
  HWND-equivalents). Never reach back through the renderer.
- **Spec and code disagree?** Trust the spec. Edit the code, or in rare
  cases, edit the spec — but never silently let the code be the
  authority.

## What this file is *not*

- It is not a substitute for reading the specs.
- It is not the place to record one-off bug fixes (use git history).
- It is not a roadmap for product decisions (use `specs/ux/`).
