# UI Nonblocking Contract

Feraille's defining requirement is that the interface remains alive under
pressure. The app can be waiting on a slow disk, a network volume, a cloud file,
Finder metadata, thumbnail generation, or a stuck file read, and the user must
still be able to scroll, type, change selection, switch tabs, close a dialog,
or navigate elsewhere.

## The Rule

No I/O on the UI hot path.

The hot path includes:

- Paint and renderer calls.
- Row/cell drawing closures.
- Hit testing.
- Hover updates.
- Selection updates.
- Scroll handling.
- Keyboard and text input.
- Window resize and scale changes.
- Modal overlay drawing.

## Forbidden On The Hot Path

- `std::fs::read`, `File::open`, `metadata`, `read_dir`, `canonicalize`.
- Magic sniffing.
- Thumbnail or preview generation.
- Icon extraction.
- Database queries.
- Network, cloud, mounted-volume, or symlink resolution.
- Finder/AppKit shell queries that can touch disk or block.
- Context menu construction that asks the OS or filesystem for details.

## Allowed On The Hot Path

- Reading data already present in app state.
- Looking up a cached icon/bitmap/string.
- Drawing placeholders for missing data.
- Updating small in-memory interaction state.
- Queueing a job if the queue operation itself is constant-time and nonblocking.

## Worker Pattern

Every expensive feature follows the same shape:

1. A semantic event schedules work: navigation committed, row entered viewport,
   selection changed, user opened preview, user right-clicked, idle prefetch.
2. Work receives a generation id or request token.
3. Work runs outside the UI thread with bounded concurrency.
4. It returns a compact result through an app event/channel.
5. The UI applies the result only if the token still matches current state.
6. Paint reads the cached result on the next frame.

## Current Examples

Done:

- Magic detection runs on a worker and returns through winit user events.
- Search/filter uses the current in-memory listing and does not re-enumerate.
- The preview pane reads already-enumerated metadata only.

Needs work:

- Directory enumeration should stream from workers with cancellation.
- Icon fetching should move behind a worker/cache boundary.
- Real previews must be cancellable worker jobs.
- Context menu warming should precompute only pure app state on hover; native
  menu construction must happen asynchronously or at invocation with visible
  progress and no paint blocking.

## Failure Policy

If an async job fails, times out, or returns after the user moved on:

- Keep the UI usable.
- Preserve the current selection and scroll if possible.
- Show stale or missing metadata rather than block.
- Surface failure through status/toast/error state, not by freezing.

## Testing Targets

Every feature that performs I/O should eventually have:

- A worker test for cancellation/stale-result dropping.
- A screenshot/CLI state that proves the UI still renders without results.
- A manual slow-path test using a slow folder, network mount, or injected sleep.
- Frame-time logging for scroll and navigation.
