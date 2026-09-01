# Freeze Diagnostics

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

What happens when the Prime Directive fails in the field: the app stops
responding on a user's machine, far from a debugger, and how Ferail turns
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

<!-- toc depth=2 -->

- [The hang report](#the-hang-report)
- [Trigger 1: the watchdog (automatic - all platforms)](#trigger-1-the-watchdog-automatic---all-platforms)
- [Trigger 2: kill interception (terminal launches)](#trigger-2-kill-interception-terminal-launches)
- [Thread-stack tiers](#thread-stack-tiers)
- [Safe mode: `--safe-mode` / `FERAIL_SAFE_MODE=1`](#safe-mode---safe-mode--ferailsafemode1)
- [Live performance HUD](#live-performance-hud)
- [The shutdown report](#the-shutdown-report)
- [Testing the machinery](#testing-the-machinery)
- [What to ask a user reporting a freeze](#what-to-ask-a-user-reporting-a-freeze)

<!-- /toc -->

## The hang report

A plain-text file written to the same folder as the issue bundle:
`<config>/reports/ferail-hang-<pid>-<seq>.txt` (AROS: also appended to
`MacRW:ferail-hang.txt`). It contains:

- header: version, OS/arch, pid, uptime, what triggered the report,
  whether the session ran in safe mode;
- **background tasks**: the last per-second snapshot of the task registry
  before the stall (kind, age, scrubbed label). A freeze on a network
  mount usually names its culprit right here;
- **breadcrumbs**: the `obs` ring;
- **activity trail**: the last ~40 user actions, path-redacted under the
  same privacy toggle as the issue bundle;
- **thread stacks or dump location**: where a platform capture tier exists
  (tier list below). Windows writes a sibling `.dmp` rather than embedding
  binary stack memory in the text file.

The in-process half is persisted *before* the stack capture is attempted,
so a misbehaving capture tool can never lose the report.

### What the terminal shows

The report itself is *not* echoed to stderr: a full one is thousands of
lines (`sample` alone lists every loaded dyld image), which scrolls the
one line that matters, where the file landed, past the top of the window.
A terminal launch gets a digest instead: one line as soon as the freeze is
noticed, and after the stack capture the few facts that identify it:
build/pid/uptime, the innermost UI-thread frames (parsed out of the
capture, tree glyphs, sample counts and symbol hashes stripped), the
longest-running background task, the last activity-trail entry, and the
report path.

```
─── Ferail hang: UI thread unresponsive for ~10s (heartbeat stalled) ───
  where   : 0.6.0 macos/aarch64 · pid 70089 · up 15.1s
  ui stack: __semwait_signal  (in libsystem_kernel.dylib) + 8
          ← nanosleep  (in libsystem_c.dylib) + 220
          ← std::thread::sleep  (in ferail-gpui) + 64
  tasks   : FolderSize · running 2.0s · Sizing 36 folders…
  last    : navigate → …
  report  : …/reports/ferail-hang-70089-0.txt: attach it when reporting the freeze
```

`FERAIL_FULL_HANG_REPORT=1` restores the old behaviour and echoes the
whole report, stacks included, to stderr as well, for a session where
piping stderr is easier than fetching the file.

## Trigger 1: the watchdog (automatic - all platforms)

A foreground-executor loop bumps an atomic heartbeat once a second; the
continuation only runs while the UI thread pumps its event loop, so a
stopped counter *is* a wedged UI thread. A plain `std::thread` watchdog
(not the GPUI pool, which the bug under diagnosis could starve) checks the
counter and writes a hang report after ~10 s of silence, no user action
needed, which covers Finder/desktop launches where nobody can press a key
in a terminal. It re-arms if the UI recovers, logs the recovery, and
debounces system sleep (a check gap far past the interval re-baselines
instead of alarming). Report assembly never waits for a mutex the frozen UI
thread may own; an unavailable section says so and the rest of the report is
still written.

## Trigger 2: kill interception (terminal launches)

- **macOS / Linux**: `Ctrl+\` (SIGQUIT) always writes a report and exits
  `128+3`. `Ctrl+C` (SIGINT) and `kill` (SIGTERM) write one only if the UI
  thread was already stalled for ~3 s, then exit as usual: a healthy quit
  stays quiet. The signal handler itself only `write(2)`s a byte to a
  self-pipe (the only async-signal-safe part); a dedicated `signal-dump`
  thread, schedulable even while the UI thread is frozen, does the rest.
- **Windows**: a console-control handler gives `Ctrl+Break` the
  always-dump role and `Ctrl+C` the dump-if-stalled role, then lets the
  default handler terminate. Console launches only; GUI launches rely on
  the automatic watchdog.
- **AROS**: no signal layer; the watchdog covers it.

## Thread-stack tiers

| Platform | Tool | Notes |
| --- | --- | --- |
| macOS | `/usr/bin/sample <pid> 1 -mayDie` | Symbolized stacks of every thread, no root needed for our own pid. Always available. |
| Linux | `eu-stack -p`, else `gdb --batch -p … thread apply all bt` | Optional installs (`elfutils` / `gdb`). Yama `ptrace_scope=1` blocks child→parent attach, so the capture opens a `PR_SET_PTRACER_ANY` window for its duration and closes it after. |
| Windows | out-of-process `MiniDumpWriteDump` broker | Automatic sibling `ferail-hang-<pid>-<seq>.dmp` with all thread contexts/stacks, loaded/unloaded modules and handle/thread metadata. The disposable broker starts before GPUI initialization, needs no administrator rights to dump its same-user parent, times out after 20 seconds and commits via `.part` → `.dmp`. Use the exact matching release PDB bundle in WinDbg. |
| AROS | none | In-process sections only. |

When no tool produces stacks/dump, the report says so and names the manual
alternative: users on macOS can always run `sample Ferail 3` (or Activity
Monitor → Sample Process) against a frozen app themselves, even on builds
predating this feature.

## Safe mode: `--safe-mode` / `FERAIL_SAFE_MODE=1`

The bisection switch for "does it still freeze without the background
work?". One relaunch halves the search space. For the session it disables:
the filesystem watcher (no notify backend, no fs-watcher thread), the
folder-size walker, thumbnails, the per-row file-detail scan (magic sniff
+ Finder tags), the metadata SQLite DB (favorites / Ant Trail / recents
stay cold: expected), per-navigation volume-info refreshes (the free-
space segment stays empty), the volume/power/system-stats watchers, and
the startup scratch sweep.

Persisted settings are untouched; a normal relaunch restores everything.
The env spelling exists for launches that never see a command line. The
watchdog stays **on** in safe mode: it is the diagnostic layer safe mode
exists to serve. The AROS port's `FERAIL_FOLDER_SIZES` gate predates this
and remains the finer-grained switch for that one walker.

## Live performance HUD

For a slowdown that still leaves the interface responsive, launch with
`--performance-hud` or `FERAIL_PERFORMANCE_HUD=1`. The same per-window overlay
can be toggled from the command palette with **Toggle Performance HUD**. It
shows GPUI frame time, tail latency, dropped-frame ratio and process resource
samples. It is session-only, disabled by default, and configured in
non-continuous mode: it observes Ferail's actual redraws instead of creating a
busy render loop of its own.

## The shutdown report

A frozen window is one failure; a process that outlives its own window is the
other. The reported symptom: the user closes Ferail to install an update, the
taskbar icon disappears, and `ferail.exe` is still running, holding its own
executable, with nothing on screen to close. Only Task Manager ends it.

The mechanism is not exotic. Outside macOS, Ferail quits when gpui reports no
windows left (`boot.rs`, `on_window_closed`); anything that keeps one window
registered, an orphaned sub-window or a close callback that never fired, keeps
the process alive with no surface to interact with, and nothing forces the exit
afterwards. macOS deliberately stays resident with no windows (the Finder
model), so only an explicit Quit arms this there.

`shutdown.rs` makes that observable and then survivable:

- the watchdog heartbeat publishes, once a second from the UI thread, how many
  windows gpui still tracks and the scrubbed labels of the live auxiliary
  windows (`publish_window_snapshot`), so the reporting thread never has to
  touch `App`;
- every window close leaves a breadcrumb with the remaining count: a run of
  closes that never reaches zero is the whole diagnosis;
- quitting arms a watchdog thread. Reaching its first deadline at all means
  the process outlived its own quit, since a clean exit would have taken the
  thread with it. It writes `reports/ferail-shutdown-<pid>-<seq>.txt`: the same
  body as a hang report (task snapshot, breadcrumbs, activity trail) with the
  window facts lifted above them, and no whole-process stack capture, because
  the UI thread is not wedged here and a page of addresses would add nothing;
- at the second deadline it exits the process anyway. A process the user has
  to hunt down in Task Manager is worse than one that stopped a few seconds
  late. `FERAIL_NO_SHUTDOWN_EXIT=1` suppresses that when the point is to attach
  a debugger to the stuck process instead.

**The report needs no terminal; the knobs do.** The `.txt` is written to the
config folder however Ferail was started, including from Explorer, the Dock, or
a desktop shortcut: that is the whole point, since the user hitting this bug is
not running the app from a shell. What *does* require a command line is
everything that has to be set before launch: the environment variables below,
the probe flag, and the log line naming the report path, which goes to stderr
and is simply lost when there is no console attached. Tell users to look for
the file, not for the message.

Launching with the knobs set, per shell:

```sh
# macOS / Linux
FERAIL_SHUTDOWN_GRACE_MS=500 ./ferail-gpui --stuck-shutdown
FERAIL_NO_SHUTDOWN_EXIT=1 ./ferail-gpui
```

```powershell
# Windows PowerShell (the assignment is a separate statement)
$env:FERAIL_SHUTDOWN_GRACE_MS = "500"; .\ferail.exe --stuck-shutdown
$env:FERAIL_NO_SHUTDOWN_EXIT = "1"; .\ferail.exe
```

```bat
rem Windows cmd.exe
set FERAIL_SHUTDOWN_GRACE_MS=500 && ferail.exe --stuck-shutdown
```

`--stuck-shutdown` arms the watchdog and then deliberately outlives it, which
is the only way to see a real report land without a hanging quit to reproduce.
`FERAIL_SHUTDOWN_GRACE_MS` collapses both deadlines to one short interval. Dev
builds only: the packaged build drops the harness feature, so the flag is
inert (and unknown flags are ignored) in a released binary.

## Testing the machinery

`FERAIL_DEBUG_FREEZE=<secs>` deliberately wedges the UI thread for that
many seconds, three seconds after boot: the one sanctioned Prime
Directive violation, kept so the whole pipeline can be verified
end-to-end: `FERAIL_DEBUG_FREEZE=20 ferail-gpui` produces the automatic
watchdog report at ~10 s, logs the recovery at 20 s, and the report's
main-thread stack shows the synthetic sleep. On Windows, the same run must
produce both a `.txt` and a non-empty same-stem `.dmp`; open it in WinDbg with
the PDB bundle for that exact commit and verify that the UI thread resolves
through the synthetic sleep. This is the release qualification for the broker,
because macOS cross-compilation cannot exercise DbgHelp or the packaged path.

Unit tests also drive the watchdog state machine through suspect, one-shot
report, recovery/re-arm, and system-sleep sequences. Report rendering is tested
separately so the independent thread's decision logic does not depend on GPUI.

## What to ask a user reporting a freeze

1. Reproduce with the app started normally; when it freezes, wait ~15 s
   (the watchdog report), or press `Ctrl+\` in the launching terminal
   (macOS/Linux) / `Ctrl+Break` (Windows console).
2. Attach `reports/ferail-hang-*.txt` from the config folder (Settings →
   Diagnostics shows the path; the issue bundle lives in the same place). On
   Windows, attach the same-stem `.dmp` too; it contains the useful stacks.
3. Relaunch with `--safe-mode` and say whether the freeze survives.

For a process that survives its own close, ask for `reports/ferail-shutdown-*.txt`
instead: its `windows` line says how many gpui still had, and `aux` names the
sub-windows among them. No terminal is needed to produce it, and Settings →
Diagnostics prints the config folder it sits in. Only ask someone to relaunch
from a command line when you want a shorter grace period or the forced exit
suppressed, and give them the exact line for their shell: an environment
variable typed after the executable name silently does nothing.
