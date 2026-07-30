# Streaming Directory Enumeration

Directory loading must be notification-driven, cancellable, and safe to
apply incrementally. Ferail should never synchronously read a whole
directory on the GPUI render/update path.

## Status

Implemented for the active GPUI file list.

- `ferail-fs-native::NativeFs::enumerate_streaming` reads on a worker
  thread, emits bounded batches, and checks an `AtomicBool` cancellation
  flag between entries.
- `ferail-gpui::Shell::load_path` starts one worker per navigation,
  cancels the previous worker, and receives batches through
  `async-channel`.
- The GPUI task awaits the channel directly. There is no timer polling:
  the UI wakes when a worker sends `LoadMsg::Batch` or `LoadMsg::Done`.
- Batches are gated by `load_generation` and the active path, so stale
  worker results are dropped.
- The old rows stay visible until the first visible batch arrives. If a
  directory is empty or all rows are filtered out, the table clears on the
  final `Done` message instead of flashing empty during the read.
- Partial success is first-class: if rows arrived and the worker later
  reports an error, the rows stay visible and the error is logged for
  follow-up UI surfacing.

## Contract

1. Navigation commits immediately: current path, selection, watcher
   target, and task status update before disk enumeration completes.
2. Any previous enumeration is cancelled before the new one starts.
3. Worker batches contain `FileEntry` rows plus the controlled
   `NodeId -> PathBuf` path map needed by the UI layer.
4. The UI applies batches only from the current generation and current
   path.
5. Magic/quarantine prefetch and icon warming start after enumeration
   finishes, using the final visible table snapshot.

## Why Notification Beats Polling

Polling is acceptable for a temporary bridge, but it burns idle wakeups
and makes latency depend on the poll interval. Directory enumeration
already has a natural event boundary: a worker has a batch, or it is
done. A channel expresses that boundary directly and lets GPUI schedule
the update when useful work exists.

Use the same pattern for other long-running flows when the worker owns
the timing: copy/move progress, duplicate finding, preview generation,
and disk usage scanning. Keep polling only for APIs that do not expose a
push or callback shape.

## Non-Goals

- File-system change watching. FSEvents/poll watcher updates are separate
  from the initial listing.
- Worker-side sorting. The table still owns sorting so streamed rows use
  the same visible ordering rules as complete rows.
- Worker-side UI decisions. The worker filters hidden rows and the
  current text filter only to avoid sending rows the current view cannot
  show; it does not know about GPUI state or theme types.
- Stable identity for rename/move/mount events. That remains part of the
  NodeStore work.

## Remaining Work

- Add automated slow-path tests with delayed batches, cancellation, stale
  generation delivery, and partial-error delivery.
- Surface partial enumeration errors in the notification/task UI instead
  of logging only.
- Consider a notification-driven bridge for the disk usage scanner so its
  task updates no longer need queue polling.
- Extend the same cancellation discipline to previews, thumbnails,
  search, copy/move, and duplicate finding.

## Cross-References

- [docs/ARCHITECTURE.md](../ARCHITECTURE.md) — nonblocking rules.
- [LAZY_METADATA.md](LAZY_METADATA.md) — NodeStore and identity model.
- [crates/ferail-fs-native/src/lib.rs](../../crates/ferail-fs-native/src/lib.rs)
  — streaming native enumerator.
- [crates/ferail-gpui/src/shell.rs](../../crates/ferail-gpui/src/shell.rs)
  — GPUI channel bridge and stale-result gate.
