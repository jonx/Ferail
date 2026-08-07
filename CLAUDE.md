# Claude Notes for Ferail

This is the operating manual for AI or human edits in this repo.

Read first:

1. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
2. [TODO.md](TODO.md)
3. The crate-level docs and nearby code for the area you are changing.

## Active Target

`crates/ferail-gpui` is the active app. Run it with:

```sh
cargo run --bin ferail-gpui
```

Ferail has a Windows predecessor, `Ferail-Win32` — a separate, older codebase
in its own GitHub repo, `jonx/Ferail-win32` (local checkout: `../Ferail-win32`,
branch `master`). If you have it, inspect it before redesigning a feature the
user says worked better in the Windows version. Copy intent and lessons, not
Win32-specific shape.

**Name history:** this app was called *Feraille* until 2026-07-30, when it took
over the predecessor's name and the predecessor became *Ferail-Win32*. Anything
older than that commit — git history, branch names, external links — says
Feraille and means this app.

The GitHub repos were renamed the same day: this app is now `jonx/Ferail` (was
`jonx/Feraille`, which still redirects), and the predecessor is
`jonx/Ferail-win32` (was `jonx/Ferail`). So a pre-rename link to
`github.com/jonx/Ferail` meant the *Windows* app and now means this one. Never
create a new repo named `Feraille` — that silently kills the redirect. The
local directory moved `~/Source/Feraille` → `~/Source/Ferail`, and old Claude
Code transcripts were rewritten to match, so sessions predating the rename
still show the new path.

## Prime Directive

The UI must never stop. **This is non-negotiable** — it outranks feature
completeness, code brevity, and every convenience. Full doctrine, the
compliant pattern, and the enforcement machinery:
[Architecture § Prime Directive](docs/ARCHITECTURE.md#prime-directive).

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

The same applies to **action/click handlers and subscriptions** — they run
on the UI thread too. Any call that can touch a disk or the shell blocks
for *seconds* on a spun-down external drive or network mount, even ones
that look free on a local SSD: `Path::exists`, `metadata`, `canonicalize`,
`read_dir`, `notify`'s `Watcher::watch()` (canonicalizes internally),
NSWorkspace/LaunchServices lookups, xattr reads.

Expensive or possibly-blocking work is scheduled from semantic events, runs
on `cx.background_executor()` (or a worker thread), and reports back through
GPUI entity/update boundaries, guarded by a generation counter and a cancel
flag. If a result arrives after the user moved on, drop it.
`Shell::load_path_for_tab` is the canonical example — copy its shape.

Debug builds enforce this at runtime (`ferail_core::path_guard`): path
resolution during render panics, and known-blocking `ferail-fs-native`
entry points panic when called on the UI thread. **Never fix a guard panic
by removing the guard** — move the work off-thread. When you add a new
blocking entry point, add `assert_off_ui_thread` to it.

## Architecture Invariants

- `ferail-core` owns platform-neutral domain types and command identity.
- `ferail-fs-native` owns native filesystem work.
- `ferail-shell-mac` owns AppKit/Cocoa integrations and does not paint UI.
- `ferail-disk-usage` is pure model/layout logic.
- `ferail-gpui` owns GPUI views, actions, task scheduling, and shell state.
- UI rendering reads cached state; it does not resolve paths or touch I/O.

## Porting Rule From Ferail-Win32

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
- The scales live in `ferail_design` (`TextTokens::BASE`, `IconTokens`).
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
- What changed, for users: [CHANGELOG.md](CHANGELOG.md) (see below)

Do not add new root ledgers under `docs/`. If it is current structure, put it
in Architecture. If it is not done, put it in TODO.

## Changelog

[CHANGELOG.md](CHANGELOG.md) answers "what would I notice as a user?" — git
history carries the full detail, so the changelog is not a commit log.

**When to add an entry.** Any change a user could notice: a new or changed
feature, a new setting or command, a fixed bug they could have hit, a
behavioural change, a performance change big enough to feel, a packaging or
platform-support change. Add it **in the same change that ships the code**, not
at release time — a reconstructed changelog is always thinner and less honest
than one written while the work is fresh.

**When to skip it.** Refactors, internal architecture, test-only or docs-only
changes, and anything invisible from outside the app. Dependency-pin bumps go
in [CHANGELOG-DEPS.md](CHANGELOG-DEPS.md) instead. If nothing user-visible
changed, say so in the release section rather than inventing filler ("No
user-visible macOS changes." is a real 0.2.2 line).

**How to write it.**

- Add to the `## Unreleased` section at the top; a release moves that block
  under a new `## <version> — <date>` heading. Newest first, always.
- One bullet per change, leading with a **bold sentence in plain language**
  that names the user-facing outcome — not the module, function, or type you
  touched.
- Write for someone who does not know the codebase: "folders showed a file
  format such as *ZIP archive*", not "`display_magic` leaked onto `Directory`
  rows". Internal names belong in the code and in `docs/features/`.
- For a fix, say what went wrong from the user's side, and when it is worth
  knowing, why — the existing entries explain the *cause* in one clause because
  that is what makes a fix trustworthy.
- Be honest about what is still broken or unsigned; 0.2.2's SmartScreen and
  "nothing builds Windows yet" notes are the standard to match.

## Verification

Before finishing code changes:

- Run `cargo check` for the touched binary or workspace as appropriate.
- Run `cargo clippy -p ferail-gpui` when the change touches that crate —
  the `disallowed_methods` deny is part of Prime Directive enforcement; a
  hit means move the call off-thread, not silence the lint.
- Run `cargo test` unless the change is docs-only.
- For UI changes, render at least one screenshot with
  `cargo run --bin ferail-gpui -- --screenshot ...` and inspect it.
- Write screenshots to `screenshots/<feature>.png`, not `/tmp`.
  `screenshots/` is gitignored scratch — any image a committed document
  references (README, docs/) must live in `docs/images/` instead, or it
  will be broken on GitHub.
- Do not run whole-repo formatters casually; this repo may have local dirty
  work.
- If the change touches icons, update
  [docs/features/ICONS.md](docs/features/ICONS.md) (see [Icons](#icons)).
- If a user could notice the change, add a [CHANGELOG.md](CHANGELOG.md) entry
  under `## Unreleased` in the same commit (see [Changelog](#changelog)).
