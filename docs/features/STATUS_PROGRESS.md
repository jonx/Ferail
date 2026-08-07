# Status Progress

Ferail presents background work through one process-wide task model. The
status bar and the task popover both read the same `TaskRegistry`, so the small
bottom strip and the detailed task list cannot disagree.

## Status

Shipped with follow-ups.

Implemented:

- `TaskRegistry` in `crates/ferail-gpui/src/tasks.rs`.
- Process-wide registry ownership in `ProcessState`.
- Status-bar task text and thin progress strip.
- Determinate and indeterminate progress states.
- Foreground-task priority so copy/search/disk/dupes keep the spotlight over
  ambient prefetch work.
- 150 ms surfacing delay so instant work does not flicker in the UI.
- Task popover with active rows, recent foreground history, progress bars,
  elapsed time, transfer rate/ETA details, and cooperative cancel buttons.
- Screenshot simulation flags for progress and task-panel states.

Remaining work is mostly user feedback polish: completion notifications,
accessibility announcements, and cancellation consistency across every worker.

## Task Model

`TaskKind` separates user-requested foreground work from ambient work.

Foreground:

- File operations.
- Search.
- Disk usage scans.
- Duplicate scans.

Ambient:

- Enumeration.
- Icon prefetch.
- Thumbnail prefetch.
- Magic/quarantine prefetch.
- Folder-size scans.

Foreground tasks are recorded in the recent-history ring when they finish.
Ambient tasks are intentionally omitted from history because they are
housekeeping and would drown out the work the user actually asked for.

## Progress States

`TaskProgress::Indeterminate` is used when a worker can say "running" but not
how far it is. The status bar renders the gpui-component loading animation.

`TaskProgress::Determinate(f32)` is used when a worker can report a fraction.
The value is clamped to `0.0..=1.0`; stale task ids are ignored.

File transfers may also attach `TransferStats`:

- bytes done / total,
- items done / total,
- smoothed bytes per second,
- ETA,
- current file label.

The status bar shows a compact rate/ETA tail for the primary task. The popover
shows the fuller transfer breakdown.

## Surfaces

The status bar shows:

- active folder item count,
- selected count/size when a selection exists,
- total visible size,
- free space when available — swapped for "{volume} is read-only" when the
  tab's volume is mounted read-only (CD/DVD, locked card, `ro` mount):
  "0 B free" on a CD is true but buries the actual story. The macOS boot
  volume is exempt — its sealed snapshot statfs's read-only, but the
  Macintosh HD the user sees is writable via the firmlinked Data volume,
- task label or "N tasks running",
- progress strip,
- a passive "N hidden · X B" chip when *show hidden* is off and the folder
  has hidden entries — count + summed sizes of what the listing skipped, so
  hidden content is discoverable without unhiding it. Fed per-tab by the
  enumeration's `Done` message (`loading::HiddenSummary`; counted before the
  text filter, zeroed at load start; hidden *folders* count at their dirent
  size, like the item total). Suppressed in archive mode. Its sibling
  affordance: with *show hidden* on, hidden rows render dimmed
  (`opacity(0.6)`, list + grid; a cut row's `0.45` wins),
- Show Hidden toggle.

Clicking the task label or progress strip toggles the task popover.

The task popover shows:

- surfaced active tasks,
- determinate/indeterminate progress,
- elapsed time,
- cooperative cancel button when the task has a cancel flag,
- recent foreground tasks with completed/cancelled/failed outcome.

## Integration Points

Current task producers include:

- directory enumeration,
- icon, thumbnail, magic, quarantine, and folder-size prefetch,
- file operations,
- search,
- disk usage,
- duplicate finding,
- screenshot simulation.

Workers begin/end tasks from the shared registry and notify the shell when
foreground state needs to repaint.

## Rules

- Starting a task must not block the UI.
- Progress updates must be throttled or sampled by the worker/poller.
- Instant tasks should not surface visually.
- Success notifications are gated by task visibility: if a task ended before
  it lived long enough to appear in the status bar/panel, the UI change itself
  is the confirmation.
- Errors become toasts, recent-history failures, or status details, never
  modal freezes. Include the raw technical error/enum/code plus actionable next
  steps.
- Cancellation is cooperative: the UI flips a shared flag and the worker exits
  at its next checkpoint.

## Remaining Work

Tracked in [TODO.md](../../TODO.md):

- Completion feedback for secondary-window scans, such as Disk Usage, that do
  not yet have a clean notification handle.
- Accessibility announcements for file operations and long-running tasks.
- More consistent cancellation tokens for workers that currently only drop
  stale results at apply time.
