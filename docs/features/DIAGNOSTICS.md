# Diagnostics, Activity Trail & Issue Reporter

Support & observability for power users and for the maintainer receiving bug
reports. Three pieces that compose:

```
  Activity Trail ─┐
                  ├──> Diagnostics Report ──> Issue Reporter ──> bundle.zip
  Health Checks ──┘        (Settings page)       (redact UI)      (maintainer)
                           + `--doctor` CLI
```

Motivating example: the `config_dir()` Windows bug (settings silently never
persisted because `config_dir()` returned `None` with no `$HOME`). A storage
health check that reports "config dir: writable? no" would have caught it
instantly.

## Decisions (approved 2026-06-23)

- **Power-user visible**: a Diagnostics page in Settings + an in-app trail view +
  a `--doctor` CLI flag. Surfaced but tucked under Settings/Help.
- **Report delivery = save `.zip` + reveal it** (no backend yet). Future: a
  direct-upload endpoint.
- **Trail scope = navigation + key commands** (state-changing/destructive ones),
  not all ~50 handlers.

## Phase 1 — Activity trail  ✅ implemented

`crates/feraille-gpui/src/trail.rs` — a typed, timestamped ring buffer
(`TrailEvent`: `Navigate{kind, path}` / `Command{label}` / `Note`), cap 256,
in-memory (no I/O, UI-thread-safe). Mirrors the `obs` breadcrumb buffer but
stores typed events. `obs` stays as the raw stderr log.

Recording hooks:
- Navigation — `Shell::navigate_with_tracking` (Go), `navigate_back` (Back),
  `navigate_forward` (Forward).
- Key commands — one `trail::command("…")` line at the top of the curated
  handlers (Move to Trash, Empty Trash, Paste, Move Here, New Folder, Rename,
  Find Duplicates, Undo, Toggle Hidden Files). New commands opt in with one line.

Not yet surfaced in the UI — Phase 2's Diagnostics page renders it.

## Phase 2 — Diagnostics / health check  (TODO)

`crates/feraille-gpui/src/diagnostics.rs` — `run_checks()` produces a
`DiagnosticsReport` of `Check { name, status: Ok|Warn|Fail, detail }`, grouped
App / Storage / Dependencies / Environment. Checks:
- App: version, debug/release, features (`mpv`), commit.
- **Storage** (bug-catcher): config dir path/exists/**writable** (temp-file
  probe); settings file exists/parseable; metadata DB path/openable/writable;
  cache/thumbnail dirs.
- Dependencies: mpv path valid + libmpv loadable (when selected).
- Environment: OS/version/arch, env-var **presence** only (APPDATA/HOME/XDG —
  privacy), free space on the config volume, platform capabilities.

Two front-ends over one `run_checks()`:
1. **Settings → Diagnostics page** (new `SettingsCategory::Diagnostics`). Renders
   a *cached* report entity; opening / "Re-run" schedules the checks on a
   **background task** (prime directive — they touch the FS) → entity update →
   re-render. Buttons: Re-run, Copy report, Save report…, Report a problem…
2. **`feraille-gpui --doctor`** — same `run_checks()` headless, prints the report
   to stdout, exits. Works even when the GUI won't start; no duplicated logic.

Needs `app_state::config_dir()` made `pub` (or a diagnostics accessor).

## Phase 3 — Issue reporter  (TODO)

`crates/feraille-gpui/src/report.rs` + a redaction modal.
- Capture: `window.render_to_image()` → `image::RgbaImage` (same path as the
  `--screenshot` harness; available at runtime via the `test-support` feature).
- Redact: a modal where the user drags black rectangles over sensitive areas,
  composited onto the image on Done.
- Bundle: a `.zip` (existing `zip` dep) with redacted PNG + diagnostics report +
  activity trail + user note. Username redacted out of report paths
  (`C:\Users\<you>` → `%USERPROFILE%`).
- Deliver: save the zip + reveal it; optionally open a prefilled mail/issue
  draft.
