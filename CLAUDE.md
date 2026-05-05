# Claude Notes for Feraille

This is the operating manual for AI or human edits in this repo. Feraille is
the macOS port and UI rewrite of `../Ferail`. The old docs are source material,
not law. The cleaned port map is in [docs/porting/FERAIL_DOCS_MAP.md](docs/porting/FERAIL_DOCS_MAP.md).

## Read First

1. [docs/UI_NONBLOCKING.md](docs/UI_NONBLOCKING.md)
2. [docs/FEATURE_LEDGER.md](docs/FEATURE_LEDGER.md)
3. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
4. [specs/ux/05-performance.md](specs/ux/05-performance.md)
5. The crate-level docs and local code around the file you are touching.

## Prime Directive

The UI must never stop.

Paint, hover, hit-test, scroll, resize, keyboard input, text input, selection,
and modal drawing are read-only and nonblocking. They must not:

- Read files or directories.
- Query Finder/AppKit/NSWorkspace for data.
- Query SQLite or other persistent stores.
- Generate previews or thumbnails.
- Sniff magic bytes.
- Resolve symlinks, aliases, cloud placeholders, or network locations.
- Build context menus by touching filesystem or shell state.
- Allocate in row-by-row hot paint paths.

All expensive work is scheduled from semantic events, runs off the UI thread,
and reports back through a small event/message boundary. If a result arrives
after the user moved on, drop it.

## Current Nonblocking Lessons

- Tree unfold froze once because navigation called synchronous magic sniffing,
  which blocked in `read()`. Magic detection now runs on a worker and returns
  through winit user events. Do not reintroduce synchronous I/O on navigation.
- Icon prefetch (iter-5.6) is now chunked across event-loop ticks via
  `IconChunkTick`. NSWorkspace.iconForFile: is main-thread only, so the work
  itself can't move to a worker; the chunking yields between batches. Don't
  put it back into a synchronous loop.
- The filesystem trait still returns an eager batch in places. Future work is
  streaming enumeration with cancellation.

## Cross-Reference: Ferail (the Windows predecessor)

The original Windows project lives at `/Users/jkn/Source/Ferail`. Most
features in Feraille started life there and were simplified during the
port — sometimes deliberately, sometimes because the macOS analogue
hadn't been figured out yet. **When the user says a feature "was more
advanced in Ferail" or seems regressed, go read the corresponding code
and docs there before redesigning from scratch.** Useful entry points:

- `/Users/jkn/Source/Ferail/CLAUDE.md` — the operating manual for that repo.
- `/Users/jkn/Source/Ferail/docs/` — design, notes, done-list, testing overlays.
- `/Users/jkn/Source/Ferail/crates/` — the source the port came from.

Treat Ferail as a reference implementation, not a blueprint: copy the
intent and the lessons, not the Win32-specific shape. The mapping from
Ferail docs to Feraille docs is in
[docs/porting/FERAIL_DOCS_MAP.md](docs/porting/FERAIL_DOCS_MAP.md).

## Architecture Invariants

- `feraille-controls` knows controls, not paths.
- `feraille-render` knows pixels, not files or platform shell APIs.
- `feraille-design` is the only source of visual tokens.
- `feraille-core` owns shared model types and must stay platform-neutral.
- `feraille-fs-native` can perform filesystem work, but callers decide whether
  that work is safe for the current thread.
- `feraille-shell-mac` owns AppKit/Cocoa integrations.
- Shell crates are window-aware but UI-unaware: they do not paint controls.

## Paint Contract

Paint may:

- Read already-cached strings, metrics, flags, icons, and bitmaps.
- Draw placeholders when data is missing.
- Clip, fill, stroke, draw text, draw bitmap.

Paint may not:

- Format paths or metadata in loops.
- Call any `ensure_*`, `fetch_*`, `detect_*`, `enumerate`, `stat`, `read`,
  `canonicalize`, `metadata`, or shell query function.
- Spawn tasks directly.
- Mutate app model state except renderer-internal caches that are explicitly
  designed for paint, such as glyph caches.

## Porting Rule From Ferail

When bringing a Ferail feature over, translate by intent:

- Win32 `IContextMenu` becomes a macOS `NSMenu`/Finder-action boundary.
- Win32 drag/drop COM becomes `NSPasteboard` and AppKit dragging.
- WSL features become "not applicable to Mac v1" unless they map to remote
  mounts, SSHFS, or network volumes.
- Direct2D/GDI details become renderer/control guidance only.
- SQLite metadata, Ant Trail persistence, disk usage, previews, and duplicate
  finding remain valuable, but must use Mac-safe paths and workers.

Never copy a Windows implementation shape just because the old doc said it.

## Where To Document Work

- Product status: [docs/FEATURE_LEDGER.md](docs/FEATURE_LEDGER.md)
- Architecture decisions: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- Hard responsiveness rules: [docs/UI_NONBLOCKING.md](docs/UI_NONBLOCKING.md)
- Feature design notes: [docs/features](docs/features)
- UX behavior specs: [specs/ux](specs/ux)
- Control/render specs: [specs/controls](specs/controls)
- Chronological shipped notes: [NOTES.md](NOTES.md)
- Free-form open risks and "look into later" items: [todo.md](todo.md)

## Logging

Stderr logging lives in [crates/feraille-app/src/obs.rs](crates/feraille-app/src/obs.rs).
Each `log_info!` / `log_warn!` / `log_error!` call carries an integer ID
(by convention, the iteration number — iter-5.6 calls use `56`). Lines
with `id < obs::LOG_THRESHOLD` are silently dropped.

When starting a new iteration, **bump `LOG_THRESHOLD`** to suppress old
diagnostic noise without deleting the call sites, and tag new log calls
with the new iter number. Crash diagnostics (startup banner, panic hook,
worker-panic line) bypass the macros and always print.

## Verification

Before finishing code changes:

- Run `cargo check`.
- Run `cargo test` unless the change is docs-only.
- For UI changes, render at least one screenshot with `cargo run -- --screenshot ...`
  and inspect it. (The package has a single binary, so `--bin` is unnecessary.)
- Do not run whole-repo formatters casually; this repo may have local dirty work.

## Open backlog

Free-form risks, gaps, and "look into later" items live in
[todo.md](todo.md). Don't crowd this file with them — keep CLAUDE.md
focused on operating rules. When something on that list ships, delete
it; the commit and NOTES.md entry are the record.
