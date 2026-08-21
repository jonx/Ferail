# Update Check

"Is there a newer Ferail on GitHub Releases, and how do I get it?" —
`ferail-gpui/src/update_check.rs`. Three surfaces over one state machine
(check status × download status), all reading a process-wide global that
the dialog re-renders from every frame.

## Surfaces

- **Ferail ▸ Check for Updates…** (`app.check_updates`) — opens the
  Software Update dialog and starts a check. Always available; the
  setting below gates only the *automatic* path. A check or download
  already in flight is left alone (reopening must not clobber a 90%-done
  download).
- **Settings ▸ About ▸ Updates ▸ "Check for updates automatically"** —
  **off by default**. On: a daily background check; a newer release
  posts one notification per version per session (with a View… button
  into the dialog); up-to-date and failed checks stay silent. The loop
  re-reads the setting each wake, so toggling works without a relaunch;
  toggling *on* checks immediately. Skipped entirely in safe mode.
- **The dialog** — Installed version, check outcome, then per state:
  Download `<asset>` / Later; a live percent while downloading;
  Open / Show in Folder once done. When something newer exists, a
  **"What's new"** box renders the release notes — the markdown body of
  the GitHub release — so the decision to update is made with the
  changes in front of you. If the user skipped versions, *every* newer
  release's notes appear, newest first, each under its own
  `### <title> · <date>` heading (capped at `NOTES_MAX`; the rest
  collapse to a count); a release with no notes says so rather than
  vanishing. A link below still opens the tag's GitHub page (assets,
  checksums). The box is a bounded `overflow_y_scroll` region with a
  `TextView::markdown` keyed on the version, so a different release
  gets a fresh parse/selection state.

## What "update" means (v1)

Download the platform's asset into `~/Downloads` (`.part` file, then
rename; names never overwrite — "x (2).dmg"), and hand off to the user:
Open mounts the DMG / opens the installer; installing is their step.
Ferail does not replace its own binary — no Sparkle-style in-place
swap, no signature verification of its own beyond HTTPS. Asset per
platform, matching what CI publishes: macOS `Ferail-<v>.dmg`, Windows
`Ferail-<v>-win-x64.zip`, Linux `ferail_<v>-1_{amd64,arm64}.deb` for
the running arch. A release without a matching asset falls back to
opening the release page.

## Wiring

- **HTTP**: gpui ships a `NullHttpClient`; boot installs zed's
  `ReqwestClient` (same zed source as gpui, one locked rev — see the
  workspace Cargo.toml note) behind `cx.http_client()`. The check makes
  one `GET /repos/jonx/Ferail/releases?per_page=30` (same shape as
  zed's `http_client::github` helper — redirects followed, optional
  `GITHUB_TOKEN` bearer — but parsed into our own `GhRelease` because
  zed's struct drops the release `body`/`name`). `summarize()` (pure,
  unit-tested) keeps published, non-prerelease releases *with assets*,
  sorts by version, and against `CARGO_PKG_VERSION` yields either
  UpToDate or Available{latest's asset + notes of every newer release}.
  Version compare is strict `major.minor.patch` — a malformed remote
  tag is skipped, *never* "newer".
- **Prime Directive**: the API call, the download, and every filesystem
  touch run on the background executor; results cross back over an
  `async_channel` into the global on the foreground executor.
- **Privacy**: no telemetry; the only requests are the release-list
  call and a user-initiated asset download. The automatic path is
  opt-in precisely because even a version check tells GitHub an app
  instance exists.

## Testing

- Pure logic (version parse/compare, per-platform asset pick, the
  release-list fold incl. skipped-version notes and no-asset releases,
  notes-markdown assembly, name uniquifying) is unit-tested.
- `cargo test -p ferail-gpui update_check -- --ignored` runs the real
  download-path test (network; fetches a release asset, verifies size,
  cleans up).
- `--screenshot x.png --update-dialog <state>` renders the dialog with
  a seeded state, no network: `checking`, `uptodate`, `available`,
  `noasset`, `downloading`, `done`, `failed` — plus `live`, which runs
  the real check (docs/features/SCREENSHOTS.md).

## Known gaps

- No in-place install (by design for now); no skip-this-version
  memory; the automatic check's daily timer resets on relaunch.
- The status bar's task registry doesn't show the download — progress
  lives in the dialog (and completion posts a toast).
