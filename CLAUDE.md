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

Ferail has a Windows predecessor, `Ferail-Win32`: a separate, older codebase
in its own GitHub repo, `jonx/Ferail-win32` (local checkout: `../Ferail-win32`,
branch `master`). If you have it, inspect it before redesigning a feature the
user says worked better in the Windows version. Copy intent and lessons, not
Win32-specific shape.

**Name history:** this app was called *Feraille* until 2026-07-30, when it took
over the predecessor's name and the predecessor became *Ferail-Win32*. Anything
older than that commit, git history, branch names, external links, says
Feraille and means this app.

The GitHub repos were renamed the same day: this app is now `jonx/Ferail` (was
`jonx/Feraille`, which still redirects), and the predecessor is
`jonx/Ferail-win32` (was `jonx/Ferail`). So a pre-rename link to
`github.com/jonx/Ferail` meant the *Windows* app and now means this one. Never
create a new repo named `Feraille`: that silently kills the redirect. The
local directory moved `~/Source/Feraille` → `~/Source/Ferail`, and old Claude
Code transcripts were rewritten to match, so sessions predating the rename
still show the new path.

## Prime Directive

The UI must never stop. **This is non-negotiable**: it outranks feature
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

The same applies to **action/click handlers and subscriptions**: they run
on the UI thread too. Any call that can touch a disk or the shell blocks
for *seconds* on a spun-down external drive or network mount, even ones
that look free on a local SSD: `Path::exists`, `metadata`, `canonicalize`,
`read_dir`, `notify`'s `Watcher::watch()` (canonicalizes internally),
NSWorkspace/LaunchServices lookups, xattr reads.

Expensive or possibly-blocking work is scheduled from semantic events, runs
on `cx.background_executor()` (or a worker thread), and reports back through
GPUI entity/update boundaries, guarded by a generation counter and a cancel
flag. If a result arrives after the user moved on, drop it.
`Shell::load_path_for_tab` is the canonical example: copy its shape.

Debug builds enforce this at runtime (`ferail_core::path_guard`): path
resolution during render panics, and known-blocking `ferail-fs-native`
entry points panic when called on the UI thread. **Never fix a guard panic
by removing the guard**: move the work off-thread. When you add a new
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
[docs/features/ICONS.md](docs/features/ICONS.md): its source (macOS
NSWorkspace / local Lucide-derived bundle / upstream `gpui-component-assets`),
attribution, and the exact command/surface that uses it.

**When you add, move, or repurpose any icon, update [docs/features/ICONS.md](docs/features/ICONS.md) in
the same change.** Specifically:

- Do not reuse an existing command's glyph for a different command: the
  command→icon mapping is meant to stay ~1:1 so weak/overloaded icons are easy
  to spot. Draw a distinct glyph instead.
- Stay **platform-neutral** when possible: one icon set serves macOS, Windows,
  and Linux, so avoid OS-specific metaphors (⌘/`command`, the Windows logo,
  Finder chrome) for generic commands: use a universal glyph (`keyboard`, not
  ⌘, for shortcuts). Platform-flavored glyphs are only OK on `#[cfg]`-gated
  controls.
- Before drawing or vendoring anything, check the **spare upstream pool**: the
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

- Size text with `crate::text::TextScale`: `.text_scale_xs()` …
  `.text_scale_xl()` (or `.text_token(TextSize::…)`). **Never** add a raw
  `.text_xs()` / `.text_sm()` or a `px(N)` font size for chrome text.
- Size chrome icons with `crate::text::IconScale`: `svg(…).icon_px(N)` (the
  `N` is the size at `ui_scale == 1`), so they zoom with the text. A
  gpui-component `Icon` with no `with_size` already scales; if it needs an
  explicit `px` size, multiply by `ui_scale`.
- The scales live in `ferail_design` (`TextTokens::BASE`, `IconTokens`).
  Change sizes there, in one place, not at call sites.
- Size gpui-component widgets (Checkbox, Button, Switch, …) with `Sizable`
  (`.xsmall()` inline, `.small()` in dialogs) so their text matches the dense
  scale instead of the `Medium` 16px default.
- Stay on explicit `px` only for: glyph affordances pinned to a fixed-size box
  (disclosure triangles, the favorites `+`, the viewer seek grip), the
  code-block preview font, grid thumbnails + their badges (own size axis), and
  the drag-ghost chip. Everything else is rem-relative so `ui_scale` scales it.

## Counts

Every count the app shows a user: files, folders, items, entries, matches,
duplicate groups, archive members, records: is grouped with `.` every three
digits: **1.104.619**, never `1104619`. A file manager routinely reports
millions, and an ungrouped run of digits is unreadable at a glance.

`ferail_core::counts` owns this, and there are only two things to remember:

- A plural `trn!` needs nothing. Its implicit `{n}` is displayed through
  `counts::format_count` by the macro itself, so
  `trn!("{n} file", "{n} files", n)` is already grouped. Never pass
  `n = …` to override it: `fill` takes the first binding, and yours is
  second.
- A count in a **named** placeholder or a raw `format!` is yours to
  format: `tr!("{files} files", files = counts::format_count(n))`. This
  is the preferred form whenever you have the choice, because it groups
  exactly the number you meant.

`counts::group_digits` is the escape hatch for a finished label assembled
from pieces outside `tr!`: what `status_bar::count_labels` does with its
`"3/12 · 1.0 KB"` compositions. It groups *every* run of four or more
digits in the text, so never hand it a string carrying a path, file name,
hash, year, or version.

Sizes, durations, percentages, coordinates and version numbers are not
counts: leave them to `humanize_bytes` and friends.

## Where To Document Work

The full rule set is [docs/DOCUMENTATION.md](docs/DOCUMENTATION.md), and
[docs/README.md](docs/README.md) is the map of every document. Four rules
matter more than the rest:

1. **One home per fact.** Every other mention is a link.
2. **Status lives only in [docs/STATUS.md](docs/STATUS.md).** Not in a feature
   note, not in a README paragraph, not in a `## Status` section.
3. **Write the finished state.** *Now*, *currently*, *still*, *no longer* and
   *after the fix* belong in the decision log or a memo, not in a design
   note.
4. **Point-in-time documents go to `docs/memos/`** with a date, and are never
   cited as a current statement about the app.

| Kind | Home |
| --- | --- |
| Current architecture | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) |
| How a feature is built | [docs/features/](docs/features/README.md) |
| Where the project is | [docs/STATUS.md](docs/STATUS.md) |
| What is not done | [TODO.md](TODO.md) |
| What changed, for users | [CHANGELOG.md](CHANGELOG.md) (see below) |
| What was decided, and why | [NOTES.md](NOTES.md) |
| A session, handover or checkpoint | `docs/memos/` |

Do not add new root ledgers under `docs/`. Run
`python3 scripts/check-docs.py` after any documentation change; it verifies
links, anchors, tables of contents, `WIN-nnn` identifiers, navigation lines
and index membership.

## Changelog

[CHANGELOG.md](CHANGELOG.md) answers "what would I notice as a user?": git
history carries the full detail, so the changelog is not a commit log.

**When to add an entry.** Any change a user could notice: a new or changed
feature, a new setting or command, a fixed bug they could have hit, a
behavioural change, a performance change big enough to feel, a packaging or
platform-support change. Add it **in the same change that ships the code**, not
at release time: a reconstructed changelog is always thinner and less honest
than one written while the work is fresh.

**When to skip it.** Refactors, internal architecture, test-only or docs-only
changes, and anything invisible from outside the app. Dependency-pin bumps go
in [CHANGELOG-DEPS.md](CHANGELOG-DEPS.md) instead. If nothing user-visible
changed, say so in the release section rather than inventing filler ("No
user-visible macOS changes." is a real 0.2.2 line).

**How to write it.**

- Add to the `## Unreleased` section at the top; a release moves that block
  under a new `## <version> - <date>` heading. Newest first, always.
- One bullet per change, leading with a **bold sentence in plain language**
  that names the user-facing outcome, not the module, function, or type you
  touched.
- Write for someone who does not know the codebase: "folders showed a file
  format such as *ZIP archive*", not "`display_magic` leaked onto `Directory`
  rows". Internal names belong in the code and in `docs/features/`.
- For a fix, say what went wrong from the user's side, and when it is worth
  knowing, why: the existing entries explain the *cause* in one clause because
  that is what makes a fix trustworthy.
- Be honest about what is still broken or unsigned; 0.2.2's SmartScreen and
  "nothing builds Windows yet" notes are the standard to match.

## Localization

English is the source language, but it is not the only shipping language.
Whenever a change adds or rewrites user-visible text, localize that text in
the same change:

1. Wrap the source text with the appropriate `tr!`, `trn!`, `trc!`, or
   `msgid!` form described in
   [Localization](docs/features/LOCALIZATION.md#for-developers-writing-strings).
2. Regenerate `locales/en.json` with the extraction test.
3. Add or update the matching entries in both bundled packs,
   `locales/de.json` and `locales/fr.json`. Preserve every placeholder and
   plural shape; do not ship newly added text as an English fallback.
4. Run the i18n extraction and bundled-pack tests before finishing.

This applies to labels, menus, dialogs, tooltips, notifications, empty states,
settings descriptions, and every other string the user reads. The deliberate
English-only exceptions are listed in the localization feature note.

## Verification

Before finishing code changes:

- Run `cargo check` for the touched binary or workspace as appropriate.
- Run `cargo clippy -p ferail-gpui` when the change touches that crate:
  the `disallowed_methods` deny is part of Prime Directive enforcement; a
  hit means move the call off-thread, not silence the lint.
- Run `cargo test` unless the change is docs-only.
- For UI changes, render at least one screenshot with
  `cargo run --bin ferail-gpui -- --screenshot ...` and inspect it.
- Write screenshots to `screenshots/<feature>.png`, not `/tmp`.
  `screenshots/` is gitignored scratch, any image a committed document
  references (README, docs/) must live in `docs/images/` instead, or it
  will be broken on GitHub.
- Run `python3 scripts/check-docs.py` when the change touches any Markdown
  file; `--write-toc` regenerates stale tables of contents.
- Do not run whole-repo formatters casually; this repo may have local dirty
  work.
- If the change touches icons, update
  [docs/features/ICONS.md](docs/features/ICONS.md) (see [Icons](#icons)).
- If the change adds or rewrites user-visible text, regenerate the English
  catalog, translate the new entries in both bundled language packs, and run
  the i18n tests (see [Localization](#localization)).
- If the change displays a count of anything, group it (see
  [Counts](#counts)).
- If a user could notice the change, add a [CHANGELOG.md](CHANGELOG.md) entry
  under `## Unreleased` in the same commit (see [Changelog](#changelog)).
