# Freeze Diagnostics

What happens when the Prime Directive fails in the field — the app stops
responding on a user's machine, far from a debugger — and how Ferail turns
that into a report worth attaching to an issue. Code: `ferail-gpui/src/
watchdog.rs` (watchdog + hang reports), `ferail-gpui/src/safe_mode.rs`
(safe mode). Sibling surfaces: `obs.rs` (breadcrumbs, panic hook),
`trail.rs` (activity trail), `report.rs` (issue bundle).

Everything here is cross-platform at the core (std-only): macOS, Windows,
Linux, and the AROS port all get the watchdog, the heartbeat, and the
in-process report. Whole-process stack capture is a per-platform bonus
tier on top.

Crash coverage is a separate sibling path: `obs::init()` installs the panic
hook before the GUI and its workers start, so Rust panics persist their location,
breadcrumbs, and backtrace as `reports/ferail-crash-<pid>.txt` as well as
printing it (and GPUI can recover from an unwind where it has a panic boundary).
The essential panic facts are written before backtrace capture, and diagnostic
mutexes use non-blocking snapshots so a panic or freeze while holding one cannot
deadlock its own reporter. The watchdog is deliberately an independent OS
thread, but it is still inside the Ferail process: it detects a live process
with a frozen UI; it cannot survive `SIGKILL`, a hard abort, or process death to
report that death. Those remain the OS crash reporter's job; detecting them in
Ferail would require a separate helper *process*, not another in-process thread.

## The hang report

A plain-text file written to the same folder as the issue bundle:
`<config>/reports/ferail-hang-<pid>-<seq>.txt`, echoed to stderr for
terminal launches (AROS: also appended to `MacRW:ferail-hang.txt`). It
contains:

- header — version, OS/arch, pid, uptime, what triggered the report,
  whether the session ran in safe mode;
- **background tasks** — the last per-second snapshot of the task registry
  before the stall (kind, age, scrubbed label). A freeze on a network
  mount usually names its culprit right here;
- **breadcrumbs** — the `obs` ring;
- **activity trail** — the last ~40 user actions, path-redacted under the
  same privacy toggle as the issue bundle;
- **thread stacks** — where a platform tool exists (tier list below).

The in-process half is persisted *before* the stack capture is attempted,
so a misbehaving capture tool can never lose the report.

## Trigger 1: the watchdog (automatic — all platforms)

A foreground-executor loop bumps an atomic heartbeat once a second; the
continuation only runs while the UI thread pumps its event loop, so a
stopped counter *is* a wedged UI thread. A plain `std::thread` watchdog
(not the GPUI pool, which the bug under diagnosis could starve) checks the
counter and writes a hang report after ~10 s of silence — no user action
needed, which covers Finder/desktop launches where nobody can press a key
in a terminal. It re-arms if the UI recovers, logs the recovery, and
debounces system sleep (a check gap far past the interval re-baselines
instead of alarming). Report assembly never waits for a mutex the frozen UI
thread may own; an unavailable section says so and the rest of the report is
still written.

## Trigger 2: kill interception (terminal launches)

- **macOS / Linux** — `Ctrl+\` (SIGQUIT) always writes a report and exits
  `128+3`. `Ctrl+C` (SIGINT) and `kill` (SIGTERM) write one only if the UI
  thread was already stalled for ~3 s, then exit as usual — a healthy quit
  stays quiet. The signal handler itself only `write(2)`s a byte to a
  self-pipe (the only async-signal-safe part); a dedicated `signal-dump`
  thread — schedulable even while the UI thread is frozen — does the rest.
- **Windows** — a console-control handler gives `Ctrl+Break` the
  always-dump role and `Ctrl+C` the dump-if-stalled role, then lets the
  default handler terminate. Console launches only; GUI launches rely on
  the automatic watchdog.
- **AROS** — no signal layer; the watchdog covers it.

## Thread-stack tiers

| Platform | Tool | Notes |
| --- | --- | --- |
| macOS | `/usr/bin/sample <pid> 1 -mayDie` | Symbolized stacks of every thread, no root needed for our own pid. Always available. |
| Linux | `eu-stack -p`, else `gdb --batch -p … thread apply all bt` | Optional installs (`elfutils` / `gdb`). Yama `ptrace_scope=1` blocks child→parent attach, so the capture opens a `PR_SET_PTRACER_ANY` window for its duration and closes it after. |
| Windows | none in-process yet | Report tells the user: Task Manager → Details → right-click → Create dump file. A future tier could walk suspended threads via DbgHelp. |
| AROS | none | In-process sections only. |

When no tool produces stacks, the report says so and names the manual
alternative — users on macOS can always run `sample Ferail 3` (or Activity
Monitor → Sample Process) against a frozen app themselves, even on builds
predating this feature.

## Safe mode: `--safe-mode` / `FERAIL_SAFE_MODE=1`

The bisection switch for "does it still freeze without the background
work?". One relaunch halves the search space. For the session it disables:
the filesystem watcher (no notify backend, no fs-watcher thread), the
folder-size walker, thumbnails, the per-row file-detail scan (magic sniff
+ Finder tags), the metadata SQLite DB (favorites / Ant Trail / recents
stay cold — expected), per-navigation volume-info refreshes (the free-
space segment stays empty), the volume/power/system-stats watchers, and
the startup scratch sweep.

Persisted settings are untouched; a normal relaunch restores everything.
The env spelling exists for launches that never see a command line. The
watchdog stays **on** in safe mode — it is the diagnostic layer safe mode
exists to serve. The AROS port's `FERAIL_FOLDER_SIZES` gate predates this
and remains the finer-grained switch for that one walker.

## Testing the machinery

`FERAIL_DEBUG_FREEZE=<secs>` deliberately wedges the UI thread for that
many seconds, three seconds after boot — the one sanctioned Prime
Directive violation, kept so the whole pipeline can be verified
end-to-end: `FERAIL_DEBUG_FREEZE=20 ferail-gpui` produces the automatic
watchdog report at ~10 s, logs the recovery at 20 s, and the report's
main-thread stack shows the synthetic sleep.

Unit tests also drive the watchdog state machine through suspect, one-shot
report, recovery/re-arm, and system-sleep sequences. Report rendering is tested
separately so the independent thread's decision logic does not depend on GPUI.

## What to ask a user reporting a freeze

1. Reproduce with the app started normally; when it freezes, wait ~15 s
   (the watchdog report), or press `Ctrl+\` in the launching terminal
   (macOS/Linux) / `Ctrl+Break` (Windows console).
2. Attach `reports/ferail-hang-*.txt` from the config folder (Settings →
   Diagnostics shows the path; the issue bundle lives in the same place).
3. Relaunch with `--safe-mode` and say whether the freeze survives.
