# Power: sleep / wake handling

Ferail reacts to the machine (and its displays) going to sleep and
waking. Two distinct mechanisms, because video and file-ops want
opposite treatment:

- **Video + slideshow → *react*.** Pause them when sleep begins.
- **File copy/move → *prevent*.** Hold off idle sleep for the duration
  of a transfer so it never starts mid-stream.

This satisfies the Prime Directive: detection is a notification
callback that only pokes a channel; all real work runs in a foreground
drain, off the notification thread.

## Vocabulary

`ferail-core::power::PowerEvent` is the platform-neutral enum both
shells speak:

| Variant           | Meaning                                  |
|-------------------|------------------------------------------|
| `WillSleep`       | System about to sleep (lid, idle, menu). |
| `DidWake`         | System woke from sleep.                  |
| `ScreensDidSleep` | Displays dimmed; system still awake.     |
| `ScreensDidWake`  | Displays woke.                           |

`is_sleep()` covers `WillSleep | ScreensDidSleep`; `is_system_wake()` is
`DidWake` only (a mere display wake doesn't warrant a volume re-list).

## Detection

### macOS

`ferail-shell-mac::start_power_observer` subscribes an Objective-C
target to **NSWorkspace's own** notification center (not the default or
distributed one) for the four `NSWorkspace…Sleep/Wake` notifications.
Same lifecycle as the theme / volume observers: main-thread-only,
idempotent, observer retained in a thread-local. The callback fires on
the main thread. `WillSleep` is delivered synchronously before the
machine sleeps, so the callback must be cheap: it only sends on a
channel.

### Windows (planned / scaffolded)

`ferail-shell-win32::start_power_observer` spawns a worker thread
owning a **hidden top-level** window (power broadcasts skip
`HWND_MESSAGE`-only windows, like device-change does) and maps
`WM_POWERBROADCAST`:

| `wParam`                 | PowerEvent  |
|--------------------------|-------------|
| `PBT_APMSUSPEND`         | `WillSleep` |
| `PBT_APMRESUMESUSPEND`   | `DidWake`   |
| `PBT_APMRESUMEAUTOMATIC` | `DidWake`   |

The callback fires on the **worker thread** (hence the `Send` bound),
unlike macOS, so the gpui bridge writes only to a channel, never a
gpui entity, satisfying the weaker contract.

**Not yet covered on Windows:** display on/off
(`ScreensDidSleep/Wake`). That arrives as `PBT_POWERSETTINGCHANGE` for
`GUID_CONSOLE_DISPLAY_STATE`, which must first be armed with
`RegisterPowerSettingNotification`; the broadcast then carries a
`POWERBROADCAST_SETTING` whose `Data[0]` is 0=off / 1=on / 2=dimmed.
Left as a follow-up: system suspend/resume is the higher-value signal
and macOS supplies screen events today.

### Linux (note only - no code yet)

The modern, desktop-agnostic source is **systemd-logind** over D-Bus
(`org.freedesktop.login1`, object `/org/freedesktop/login1`). Subscribe
to its `PrepareForSleep(bool)` signal: `true` is delivered *before*
suspend (our `WillSleep`), `false` *after* resume (our `DidWake`).
That's the only signal that maps cleanly; like the Windows worker it
arrives on whatever thread runs the D-Bus loop, so the same `Send`
channel-poke contract applies. Display dim/undim
(`ScreensDidSleep/Wake`) has no single portable source: it lives in the
compositor (`org.gnome.Mutter.IdleMonitor`, KDE's PowerManagement, or
`org.freedesktop.ScreenSaver` ActiveChanged) and is best left unmapped,
mirroring the Windows gap. A `zbus` connection on a dedicated thread is
the natural implementation when a Linux shell crate lands.

## Reaction (gpui)

`process_state::start_power_watch` registers the observer and drains the
channel on the foreground executor:

- **On `is_sleep()`**: pause **every** open viewer via
  `ViewerWindow::suspend_for_power`: pause a playing video
  (`VideoStream::set_paused(true)`) and stop the slideshow timer
  (`set_playing(false)`). We **do not auto-resume on wake**: a clip
  springing back to life as the screen relights is jarring; the user
  hits space. Viewers are tracked in `ProcessState::viewers` (a pruned
  weak-handle registry mirroring `shells`); `live_viewers()` snapshots
  the live ones for the fan-out.
- **On `is_system_wake()`**: re-list volumes off-thread (a drive may
  have been unplugged while asleep), re-probe Favorites mount states,
  and `Shell::reload_dir_tabs` on every live window (contents may have
  drifted past the watcher; results views are skipped so a reload
  doesn't clobber them).

## Prevention (idle-sleep block during transfers)

`platform_shell::prevent_idle_sleep(reason) -> Option<SleepBlocker>`
returns an RAII guard; dropping it re-allows sleep.
`shell::file_ops::spawn_transfer_op` takes it around the byte-moving
engine run and drops it the instant the engine returns (the completion
tail is cheap UI). We prevent rather than react because the engine
can't checkpoint-and-resume a half-written file, and `WillSleep` gives
only a few seconds.

- **macOS**: IOKit `IOPMAssertionCreateWithName` with
  `kIOPMAssertPreventUserIdleSystemSleep` (allows display sleep, still
  honours a deliberate Apple-menu → Sleep). Process-wide, thread-safe;
  `reason` shows in `pmset -g assertions`.
- **Windows**: `SetThreadExecutionState(ES_SYSTEM_REQUIRED |
  ES_CONTINUOUS)`, cleared with a plain `ES_CONTINUOUS` on drop. That
  flag is **per-thread and sticky**; the host holds the guard inside one
  foreground task, so set and clear land on the same thread, which is
  the constraint. If a future caller needs to assert from a thread-pool
  worker that may not release on the same thread, switch to the Power
  Request API (`PowerCreateRequest` / `PowerSetRequest` /
  `PowerClearRequest` + `CloseHandle`), which is process-wide.
- **Linux** (note only): the portable equivalent is a **logind
  inhibitor lock**: `org.freedesktop.login1.Manager.Inhibit(what="idle",
  who, why, mode="block")` returns a file descriptor; idle sleep is
  blocked for as long as that fd stays open, so the `SleepBlocker` RAII
  guard simply owns the fd and closes it on drop: the same
  hold-a-handle shape as the other two platforms. Use `what="sleep"`
  instead to also delay (briefly) a deliberate suspend; `what="idle"` is
  the closer analogue to the macOS `PreventUserIdleSystemSleep` scope we
  want for transfers. The `who`/`why` strings surface in
  `systemd-inhibit --list`, the Linux counterpart to `pmset -g
  assertions`.

## Files

- `crates/ferail-core/src/power.rs`: `PowerEvent`.
- `crates/ferail-shell-mac/src/power_observer.rs`: NSWorkspace hooks.
- `crates/ferail-shell-mac/src/power_assert.rs`: IOKit assertion.
- `crates/ferail-shell-win32/src/lib.rs`: `start_power_observer`,
  `prevent_idle_sleep`, `SleepBlocker` (cfg(windows) + stubs).
- `crates/ferail-gpui/src/process_state.rs`: `start_power_watch`.
- `crates/ferail-gpui/src/viewer/window.rs`: `suspend_for_power`.
- `crates/ferail-gpui/src/shell/file_ops.rs`: transfer-time guard.
