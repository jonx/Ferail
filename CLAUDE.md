# Claude Notes for Feraille

This is the operating manual for AI or human edits in this repo.

Read first:

1. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
2. [TODO.md](TODO.md)
3. The crate-level docs and nearby code for the area you are changing.

## Active Target

`crates/feraille-gpui` is the active app. Run it with:

```sh
cargo run --bin feraille-gpui
```

Feraille has a Windows predecessor, `Ferail` — a separate, older codebase. If
you have a local checkout of it, inspect that before redesigning a feature the
user says worked better in the Windows version. Copy intent and lessons, not
Win32-specific shape.

## Prime Directive

The UI must never stop.

Paint, render, hover, hit-test, scroll, resize, keyboard input, text input,
selection, and modal drawing are read-only and nonblocking. They must not:

- Read files or directories.
- Query Finder/AppKit/NSWorkspace for data.
- Query SQLite or other persistent stores.
- Generate previews or thumbnails.
- Sniff magic bytes.
- Resolve symlinks, aliases, cloud placeholders, or network locations.
- Build context menus by touching filesystem or shell state.
- Allocate heavily in row-by-row hot render paths.

Expensive work is scheduled from semantic events, runs off the UI thread
where possible, and reports back through GPUI entity/update boundaries. If a
result arrives after the user moved on, drop it.

## Architecture Invariants

- `feraille-core` owns platform-neutral domain types and command identity.
- `feraille-fs-native` owns native filesystem work.
- `feraille-shell-mac` owns AppKit/Cocoa integrations and does not paint UI.
- `feraille-disk-usage` is pure model/layout logic.
- `feraille-gpui` owns GPUI views, actions, task scheduling, and shell state.
- UI rendering reads cached state; it does not resolve paths or touch I/O.

## Porting Rule From Ferail

Translate by intent:

- Win32 shell context menus become macOS menus/Finder actions/services.
- Win32 COM drag/drop becomes NSPasteboard and AppKit dragging.
- WSL features are not macOS v1 features unless they map to network or remote
  volumes.
- Direct2D/GDI details are renderer lessons only.
- SQLite metadata, Ant Trail, disk usage, magic, previews, and duplicate
  finding remain valuable but must use Mac-safe workers and identity.

## Icons

Every icon the app draws is cataloged in
[docs/features/ICONS.md](docs/features/ICONS.md) — its source (macOS
NSWorkspace / local Lucide-derived bundle / upstream `gpui-component-assets`),
attribution, and the exact command/surface that uses it.

**When you add, move, or repurpose any icon, update `docs/features/ICONS.md` in
the same change.** Specifically:

- Do not reuse an existing command's glyph for a different command — the
  command→icon mapping is meant to stay ~1:1 so weak/overloaded icons are easy
  to spot. Draw a distinct glyph instead.
- Stay **platform-neutral** when possible: one icon set serves macOS, Windows,
  and Linux, so avoid OS-specific metaphors (⌘/`command`, the Windows logo,
  Finder chrome) for generic commands — use a universal glyph (`keyboard`, not
  ⌘, for shortcuts). Platform-flavored glyphs are only OK on `#[cfg]`-gated
  controls.
- Before drawing or vendoring anything, check the **spare upstream pool** — the
  `gpui-component-assets` bundle already ships ~68 unused Lucide glyphs you can
  reference for free as `icons/<name>.svg`. ICONS.md lists them.
- Keep the house style (24×24, `fill="none"`, `stroke="currentColor"`,
  `stroke-width="1.75"`, round caps/joins). When the pool lacks a glyph, pull it
  from [Lucide](https://lucide.dev) so the visual language stays consistent.
- Add the new asset to the reference's inventory and command tables, note its
  origin/attribution, and record anything that looks weak under "Known gaps".

## Typography

All chrome text is sized through one design-token scale, not gpui's raw
Tailwind helpers. See
[Typography And UI Scale](docs/ARCHITECTURE.md#typography-and-ui-scale) for the
full picture. The rules:

- Size text with `crate::text::TextScale` — `.text_scale_xs()` …
  `.text_scale_xl()` (or `.text_token(TextSize::…)`). **Never** add a raw
  `.text_xs()` / `.text_sm()` or a `px(N)` font size for chrome text.
- Size chrome icons with `crate::text::IconScale` — `svg(…).icon_px(N)` (the
  `N` is the size at `ui_scale == 1`), so they zoom with the text. A
  gpui-component `Icon` with no `with_size` already scales; if it needs an
  explicit `px` size, multiply by `ui_scale`.
- The scales live in `feraille_design` (`TextTokens::BASE`, `IconTokens`).
  Change sizes there, in one place — not at call sites.
- Size gpui-component widgets (Checkbox, Button, Switch, …) with `Sizable`
  (`.xsmall()` inline, `.small()` in dialogs) so their text matches the dense
  scale instead of the `Medium` 16px default.
- Stay on explicit `px` only for: glyph affordances pinned to a fixed-size box
  (disclosure triangles, the favorites `+`, the viewer seek grip), the
  code-block preview font, grid thumbnails + their badges (own size axis), and
  the drag-ghost chip. Everything else is rem-relative so `ui_scale` scales it.

## Where To Document Work

- Current architecture: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- Open work: [TODO.md](TODO.md)
- Feature design notes: [docs/features](docs/features)

Do not add new root ledgers under `docs/`. If it is current structure, put it
in Architecture. If it is not done, put it in TODO.

## Verification

Before finishing code changes:

- Run `cargo check` for the touched binary or workspace as appropriate.
- Run `cargo test` unless the change is docs-only.
- For UI changes, render at least one screenshot with
  `cargo run --bin feraille-gpui -- --screenshot ...` and inspect it.
- Write screenshots to `screenshots/<feature>.png`, not `/tmp`.
  `screenshots/` is gitignored scratch — any image a committed document
  references (README, docs/) must live in `docs/images/` instead, or it
  will be broken on GitHub.
- Do not run whole-repo formatters casually; this repo may have local dirty
  work.
- If the change touches icons, update
  [docs/features/ICONS.md](docs/features/ICONS.md) (see [Icons](#icons)).
