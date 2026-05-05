# Performance Targets

The performance spec is a UX contract. See also
[../../docs/UI_NONBLOCKING.md](../../docs/UI_NONBLOCKING.md).

## Prime Target

The UI thread never waits on file, shell, database, preview, thumbnail, magic,
network, cloud, or volume I/O.

If data is not ready, draw the best cached state or a placeholder.

## Frame Budget

| Mode | Target | App CPU budget |
|---|---:|---:|
| Idle | no repaint | 0 ms |
| Scroll/hover/type | 144 Hz | <= 4 ms |
| Drag | 60 Hz or better | <= 8 ms |
| Animation | 144 Hz | <= 4 ms |

Any steady-state frame over 16 ms is a bug. Long frames during streamed folder
load should be rare and logged.

## Input Latency

| Event | Target | Hard cap |
|---|---:|---:|
| Key press | <= 8 ms | 16 ms |
| Text input | <= 8 ms | 16 ms |
| Mouse click | <= 8 ms | 16 ms |
| Scroll/trackpad | <= 8 ms | 16 ms |
| Drag modifier change | <= 8 ms | 16 ms |

## Folder Navigation

| Folder size | First paint | Full data |
|---|---:|---:|
| 0-1k | <= 16 ms | <= 50 ms |
| 1k-10k | <= 16 ms partial | <= 200 ms |
| 10k-100k | <= 16 ms partial | <= 1 s |
| 100k-1M | <= 16 ms partial | streamed |
| >1M | <= 16 ms partial | streamed |

Final architecture requirement: first paint never waits for full enumeration.

## Search/Filter

Current-folder filtering must be incremental over the current in-memory listing.
Recursive search is a separate worker-backed feature.

| Folder size | First visible response |
|---|---:|
| 1k | <= 8 ms |
| 100k | <= 16 ms |
| 1M | <= 50 ms with incremental strategy |

## Memory

The list must be virtualized. Per-row model data should stay compact and should
cache display strings needed by paint. Thumbnails, previews, magic results, and
icons use bounded caches.

## Anti-Features

- No I/O in paint.
- No DB queries in paint.
- No OS shell calls in paint.
- No per-row component instances.
- No unbounded task-result repaint loops.
- No synchronous preview/icon/magic work on navigation.
- No animations that delay navigation or selection feedback.

## Required Instrumentation

- Frame time logging.
- Long-frame counter.
- Paint allocation guard in debug builds.
- Worker queue depth.
- Stale result drop count.
- Screenshot CLI states for loading, error, empty, progress, and overlays.
