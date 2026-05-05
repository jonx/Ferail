# Status Progress

Ferail's status-bar progress idea ports cleanly to Feraille, but not as a
Win32 custom control. In Feraille it is a renderer/control-native task
presentation model.

## Status

Todo.

Feraille has a status text row. It does not yet have task aggregation,
determinate progress, indeterminate pulse, cancellation affordances, or task
details.

## Target

The status bar should show:

- Current selection/folder state.
- Active background tasks.
- Determinate progress for copy/move/hash/scan.
- Indeterminate pulse for enumeration, preview, metadata, icon, and magic jobs.
- A compact "N tasks running" summary when multiple workers are active.

## Rules

- Progress updates should be throttled; do not repaint at unbounded worker speed.
- Starting a task must not block the UI.
- Completing a task should fade or clear without modal interruption.
- Errors become toast/status details, not freezes.

## Task Types

- Enumeration.
- Magic sniffing.
- Icon/metadata warming.
- Preview generation.
- Copy/move/delete.
- Disk usage scanning.
- Duplicate hashing.
- Ant Trail persistence.

## Todo

- Add a task registry in app state.
- Add status progress rendering.
- Add determinate and indeterminate states.
- Add CLI screenshot states for progress.
- Add worker integration as async enumeration lands.
