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
  Open / Show in Folder once done. On Windows, an installed copy instead gets
  Install and Restart for the downloaded Inno package. When something newer exists, a
  **"What's new"** box renders the release notes — the markdown body of
  the GitHub release — so the decision to update is made with the
  changes in front of you. If the user skipped versions, every release
  through the newest compatible version appears, newest first, each under
  its own `### <title> · <date>` heading (capped at `NOTES_MAX`; the rest
  collapse to a count); a release with no notes says so rather than
  vanishing. A link below still opens the tag's GitHub page (assets,
  checksums). The box is a bounded `overflow_y_scroll` region with a
  `TextView::markdown` keyed on the version, so a different release
  gets a fresh parse/selection state.
- **Staggered platform releases** — the primary result is always the newest
  release with an asset for the running OS and architecture. A newer release
  that only ships for another platform is shown as a separate linked note;
  it never hides an older compatible update or replaces its Download button.
  Automatic notifications remain limited to updates this machine can install.
- **Failures** — the check has a 20-second deadline. Connection/DNS
  failures, timeouts, missing endpoints (404), rate limits, server errors,
  malformed responses, and an empty release feed become concise localized
  messages with a Retry button; transport details stay in the log. A repeated
  menu command raises the window that owns the existing dialog, and a stale
  host-window guard recovers onto another window instead of silently no-oping.

## What "update" means

Download the platform's asset into `~/Downloads` (`.part` file, then
rename; names never overwrite — "x (2).dmg"), and hand off to the user.
Any failed/truncated write removes its `.part` file. Then:
Open mounts the DMG or opens the portable package; installing is the user's
step on macOS/Linux. On Windows, CI publishes both
`Ferail-<v>-win-x64.zip` and `Ferail-<v>-win-x64-setup.exe`. Ferail consults
Inno's stable uninstall registration and verifies that its `InstallLocation`
is the executable actually running. Only then does it prefer setup, launch it
with `/SILENT /CLOSEAPPLICATIONS /RESTARTAPPLICATIONS`. Inno/Restart Manager
then closes and relaunches Ferail. A ZIP copy always remains portable and continues to receive
the ZIP, including when an unrelated installed copy exists. Releases without
a setup asset fall back to ZIP. Inno owns locked-file handling, restart and
rollback; Ferail never hand-replaces its executable. No downloaded asset is
trusted beyond HTTPS today. Linux uses `ferail_<v>-1_{amd64,arm64}.deb` for
the running architecture.

## Wiring

- **HTTP**: gpui ships a `NullHttpClient`; boot installs zed's
  `ReqwestClient` (same zed source as gpui, one locked rev — see the
  workspace Cargo.toml note) behind `cx.http_client()`. The check makes
  one `GET /repos/jonx/Ferail/releases?per_page=30` (same shape as
  zed's `http_client::github` helper — redirects followed, optional
  `GITHUB_TOKEN` bearer — but parsed into our own `GhRelease` because
  zed's struct drops the release `body`/`name`). `summarize()` (pure,
  unit-tested) keeps published, non-prerelease releases *with assets*,
  sorts by version, and tracks both the globally newest release and the
  newest release compatible with the running target. Against
  `CARGO_PKG_VERSION` it yields either UpToDate or Available{compatible
  asset + notes through that compatible version}, plus separate metadata
  when the global release is newer but only available elsewhere.
  Version compare is strict `major.minor.patch` — a malformed remote
  tag is skipped, *never* "newer".
- **Prime Directive**: the API call, timeout, download, and every filesystem
  touch (including failed `.part` cleanup) run on the background executor;
  results cross back over an `async_channel` into the global on the foreground
  executor. The foreground side only mutates state, raises/opens the dialog,
  sends notifications, and requests repaints.
- **Privacy**: no telemetry; the only requests are the release-list
  call and a user-initiated asset download. The automatic path is
  opt-in precisely because even a version check tells GitHub an app
  instance exists.

## Testing

- Pure logic (version parse/compare, target-parameterized asset picks for
  macOS, Windows, and both Linux architectures; staggered-release folding;
  skipped-version notes and no-asset releases; notes-markdown assembly;
  name uniquifying) is unit-tested on every host.
- `cargo test -p ferail-gpui update_check -- --ignored` runs the real
  download-path test (network; fetches a release asset, verifies size,
  cleans up).
- `--screenshot x.png --update-dialog <state>` renders the dialog with
  a seeded state, no network: `checking`, `uptodate`, `available`,
  `elsewhere`, `noasset`, `downloading`, `done`, `failed` — plus `live`,
  which runs the real check (docs/features/SCREENSHOTS.md).

## Known gaps

- No in-place install on macOS/Linux and no skip-this-version memory; the
  automatic check's daily timer resets on relaunch. The Windows setup path
  remains unsigned, so SmartScreen may still require explicit confirmation.
- The status bar's task registry doesn't show the download — progress
  lives in the dialog (and completion posts a toast).
