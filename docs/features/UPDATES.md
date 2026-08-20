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
  Open / Show in Folder once done. "Release notes" links to the tag's
  GitHub page.

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
  workspace Cargo.toml note) behind `cx.http_client()`. The check
  reuses `http_client::github::latest_github_release` (newest
  non-prerelease with assets). Version compare is strict
  `major.minor.patch` — a malformed remote tag is *never* "newer".
- **Prime Directive**: the API call, the download, and every filesystem
  touch run on the background executor; results cross back over an
  `async_channel` into the global on the foreground executor.
- **Privacy**: no telemetry; the only requests are the release-list
  call and a user-initiated asset download. The automatic path is
  opt-in precisely because even a version check tells GitHub an app
  instance exists.

## Testing

- Pure logic (version parse/compare, per-platform asset pick, name
  uniquifying) is unit-tested.
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
