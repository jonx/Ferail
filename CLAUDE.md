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

`crates/feraille-app`, `crates/feraille-controls`, and
`crates/feraille-render` are the old soft-rendered stack. Treat them as
reference/fallback code unless the user explicitly asks to work there.

The Windows predecessor lives at `/Users/jkn/Source/Ferail`. When the user
says a feature was better in the Windows version, inspect that repo before
redesigning from scratch. Copy intent and lessons, not Win32-specific shape.

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
- Do not run whole-repo formatters casually; this repo may have local dirty
  work.
