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

`crates/ferail-gpui/src/trail.rs` — a typed, timestamped ring buffer
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

`crates/ferail-gpui/src/diagnostics.rs` — `run_checks()` produces a
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
2. **`ferail-gpui --doctor`** — same `run_checks()` headless, prints the report
   to stdout, exits. Works even when the GUI won't start; no duplicated logic.

Needs `app_state::config_dir()` made `pub` (or a diagnostics accessor).

**Reveal buttons (implemented).** A `Check` additionally carries
`path: Option<PathBuf>` — structured, never re-parsed from the prose
`detail` (whose shape varies per status and gets username-scrubbed in
shared output). Rows with a path — the App group's **Executable** row
(`platform_shell::app_bundle_path()`, i.e. the running `.app` bundle or
binary), config dir, settings file, metadata DB, mpv install — render a
"Reveal" button that jumps to the location *in Ferail*:
`shell::reveal_path_in_app` opens a new tab in the first live Shell
window (or a fresh window if none) at the parent folder with the entry
queued for selection via `pending_select_names`, then raises the window.
`render_text` ignores the field, so `--doctor` / Copy report / bundle
output are unchanged, and the jump is a local UI action — it coexists
with the redaction toggle. A not-yet-created target ("written on first
change" rows) still reveals the parent; the unresolved name is dropped
when the load completes.

## Phase 3 — Issue reporter  (TODO)

`crates/ferail-gpui/src/report.rs` + a redaction modal.
- Capture: `window.render_to_image()` → `image::RgbaImage` (same path as the
  `--screenshot` harness; available at runtime via the `test-support` feature).
- Redact: a modal where the user drags black rectangles over sensitive areas,
  composited onto the image on Done.
- Bundle: a `.zip` (existing `zip` dep) with redacted PNG + diagnostics report +
  activity trail + user note. Username redacted out of report paths
  (`C:\Users\<you>` → `%USERPROFILE%`).
- Deliver: save the zip + reveal it; optionally open a prefilled mail/issue
  draft.

## Phase 4 — Privacy redaction  ✅ implemented

`crates/ferail-gpui/src/redact.rs` — so a user can share a report and we
**learn nothing about their files**. Two layers compose:

- **Account scrub** ([`report::redact_username`]) is *always* applied: the home
  prefix becomes `~` / `%USERPROFILE%`. This now also covers "Copy report",
  which previously pasted the raw account name (fixed).
- **Path redaction** (a user toggle, **default on**) reduces every filesystem
  path to its *shape*: the root anchor, one `…` per segment, and the final file
  extension. `/Users/ada/Taxes/2025/return.pdf` → `/…/…/…/…/….pdf`. Enough to
  reproduce a bug ("five deep, opening a PDF"), nothing that identifies the user.

Where it applies:

- The **activity trail** is the real leaker (it records the folders you browse),
  so it is redacted *structurally* at the source — `trail::render_lines_sanitized`
  reshapes each `Navigate` path via `redact::redact_path`, so no guessing.
- The **issue bundle** and **"Copy report"** both render the sanitized trail and
  scrub the account name; the bundle README states whether redaction was on.
- The **Settings → Diagnostics page** shows the *redacted* trail live, so the
  user literally sees what a shared report contains ("what you see is what we
  get"), with a caption confirming the state.
- The **diagnostics report itself** carries only app-owned paths (config dir, DB)
  and gets the account scrub; those paths stay readable because they're useful
  for storage bugs and reveal nothing about user content.

The toggle is a process-global `AtomicBool` (mirroring the `obs` log threshold),
seeded at startup from the persisted `redact_diagnostics` preference (`app_state`)
and flipped live by the **Settings → Diagnostics → Privacy** switch. It starts on
so a fresh install never emits a file name in a report until the user opts out.
