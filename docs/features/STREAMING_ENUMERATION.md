# Streaming Directory Enumeration

The biggest known violation of the UI nonblocking contract today.
[CLAUDE.md](../../CLAUDE.md) calls eager enumeration out as the largest
remaining architectural gap; this spec is the design-only first step
toward closing it. **No code lands with this commit.**

## Status

Todo. Iter-5.7.4 ships the spec; implementation is a multi-iter
project that should not begin until the spec lands and is reviewed.

## Why this is hard now

Today's `FsBackend::enumerate` is a synchronous call that reads the
whole directory before it returns:

```rust
pub trait FsBackend: Send + Sync {
    fn enumerate(&self, node: NodeId) -> EnumerationHandle;
}

pub struct EnumerationHandle {
    pub initial: Vec<FileEntry>,
    pub error: Option<EnumerationError>,
}
```

Symptoms this produces:

- Navigation into a slow folder (network volume, large directory,
  iCloud, fuse mount, recently woken disk) blocks the main thread.
- The user can't cancel a slow listing — they can only kill the app.
- Every navigation re-enumerates from zero. `~/Downloads` with 50k
  items rebuilds 50k `FileEntry` rows on each visit.
- A folder that becomes unreachable mid-listing (network drop, eject,
  permission revoked) returns an error covering the whole listing
  rather than the partial listing we already had.

## Goals

1. The UI thread never blocks on enumeration. Navigation commits
   immediately; rows arrive over time.
2. Cancellation is honest. Navigating away cancels in-flight work,
   stops disk I/O, and drops pending results.
3. Partial results are first-class. A folder that yields 1k rows then
   errors should show those 1k rows with an error toast — not "empty
   folder, see error."
4. Same shape works for cloud volumes, network mounts, and
   `feraille-shell-mac`'s eventual `NSMetadataQuery` results, not
   just `std::fs::read_dir`.

## Non-goals (explicit)

- File-system change watching (FSEvents). Separate spec; this one is
  about the initial listing only.
- Sort and filter on the worker side. Continue sorting/filtering in
  the app from the streamed entries.
- NodeStore identity. See [LAZY_METADATA.md](LAZY_METADATA.md);
  streaming and identity are independent landings.

## Proposed shape

### Trait

```rust
pub trait FsBackend: Send + Sync {
    /// Begin enumeration. Returns a handle the caller can poll for
    /// new batches and abort by dropping. The token is the host's way
    /// to gate stale results — see callsite contract below.
    fn enumerate(&self, node: NodeId, token: EnumerationToken) -> EnumerationHandle;
}

pub struct EnumerationToken(pub u64); // generation, monotonic

pub struct EnumerationHandle {
    /// Channel-receive end the host polls (or drains in the
    /// AppEvent::EnumerationBatch handler).
    pub rx: std::sync::mpsc::Receiver<EnumerationEvent>,
    /// Drop-on-cancel: dropping the handle signals the worker to
    /// stop reading. The worker checks this between batches.
    cancel: Arc<AtomicBool>,
}

pub enum EnumerationEvent {
    /// Initial listing chunk. Workers send these as they read; size
    /// is implementation-defined but bounded (target: 256 entries
    /// per batch).
    Batch { token: EnumerationToken, entries: Vec<FileEntry> },
    /// Final marker. Always sent before the channel closes —
    /// successful (no error) or with an error covering the unread
    /// remainder.
    Done { token: EnumerationToken, error: Option<EnumerationError> },
}
```

The token travels with each event so the host can drop late
deliveries the same way `MagicBatch` and `IconChunkTick` do today.

### Caller contract (`feraille-app`)

1. On navigation commit, bump `App.enumeration_generation`, get a new
   `EnumerationToken`, drop any previous handle (this fires
   cancellation), call `enumerate`, store the new handle on the
   active tab.
2. The implementation of `enumerate` posts each batch via the existing
   `AppEvent` channel using `EnumerationBatch`.
3. `App::user_event` matches on `EnumerationBatch` and `EnumerationDone`,
   gates by token, appends to `tab.all_entries`, runs the existing
   filter+sort, requests redraw.
4. Empty-state stays as today — but with the new "partial + error"
   case: if `Done.error.is_some() && !all_entries.is_empty()`, show
   a non-blocking error toast and keep the rows that arrived.

### Worker shape

`feraille-fs-native` runs a single thread per enumeration via
`obs::spawn_logged`. The thread:

1. Opens `read_dir`.
2. Iterates, building a batch buffer. Flushes when buffer hits
   `BATCH_SIZE` (target 256) or a small time budget elapses (target
   16 ms — one frame).
3. Between batches, checks the `Arc<AtomicBool>` cancel flag. If
   set, breaks out and sends `Done { error: None }` (cancel is not
   an error).
4. On `read_dir` failure mid-stream, sends `Done { error: Some(...) }`
   with whatever's been read so far.

This is the same time-slice idea iter-5.6 used for icon prefetch —
yield often, gate by token, drop on cancel — but on a worker because
`std::fs::read_dir` is thread-safe (unlike NSWorkspace).

## Trade-offs

- **One worker per enumeration vs. shared pool.** Per-enumeration is
  simpler; the bound is "active enumerations across all tabs," which
  is small in practice. Revisit if we ever want background prefetch
  of nearby folders.
- **`mpsc::Receiver` vs. winit user events.** User events are how
  iter-5.5/5.6 results flow today. Reusing them keeps one event
  loop. The `Receiver` in the trait is a worker-side detail; the
  app sees `AppEvent::EnumerationBatch` and `EnumerationDone`.
- **Sync poll vs. async.** Stays sync — winit's event loop is the
  scheduler. No tokio/async-std for this work.
- **Batch size.** 256 is a guess. Verify with a screenshot timing
  pass over `~/Library/Caches` (large, slow on first access) before
  committing.

## Cancellation specifics

- Drop-the-handle is the only cancel API. Explicit `.cancel()` is
  redundant.
- Worker reacts within "a batch's worth of work" — 256 entries or 16
  ms, whichever lands first.
- An in-progress `read_dir` iter doesn't honor mid-system-call
  cancellation; this is a kernel-side reality, not a design choice.
  Worst case is one extra batch read after the cancel. Acceptable.

## Compatibility / migration

- The trait change is breaking; `feraille-fs-native` and the
  Win32 stub crate both need updates.
- Shipped behaviour during migration: implement the new trait in
  `feraille-fs-native` first, ship a *single-batch* worker (sends one
  `Batch` then `Done`) to keep parity with today's eager listing.
  Iterate to true streaming in the next sub-iter. This avoids a big
  bang.
- Empty-state, magic prefetch, and icon prefetch all read from
  `tab.all_entries` after refresh — they're append-friendly already,
  which is why the migration is mostly internal.

## Test plan

- Unit: synthetic FsBackend that yields N batches with controlled
  delays; verify cancellation drops the handle within one batch.
- Integration via screenshot CLI:
  - `--navigate ~/Library/Caches --width 1400 --height 900` — visual
    confirms first-paint with partial rows, then full listing.
  - `--navigate <tmp huge dir> --then-navigate <home>` (new flag) —
    verify the second navigation cancels the first.
- Manual on a network volume: navigate, scroll, navigate away mid-
  listing, confirm no stall.

## Out of scope for the implementation iter

- FSEvents (folder-change watching).
- Cross-volume copy/move workers (separate trait, similar shape).
- Worker-side filter/sort.
- Background prefetch of adjacent folders.

These all reuse the same generation-token + AppEvent plumbing, so
this work unblocks them.

## Cross-references

- [docs/ARCHITECTURE.md](../ARCHITECTURE.md) — the nonblocking principles.
- [docs/features/LAZY_METADATA.md](LAZY_METADATA.md) — NodeStore /
  identity model that should land alongside or after this.
- Iter-5.5 magic-prefetch ([crates/feraille-app/src/main.rs `start_magic_prefetch`](../../crates/feraille-app/src/main.rs))
  — current reference for the "worker + generation + AppEvent" shape.
- Iter-5.6 icon-prefetch chunk ticks — current reference for
  cooperative time-slicing on a constrained API.
