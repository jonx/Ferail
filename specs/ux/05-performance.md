# Performance Targets

These are the spec, not the aspiration. Each number is enforced by a regression test in CI (target tier; in v1, by a manual benchmark harness).

## The promise

> A folder with 1,000,000 files opens, scrolls, sorts, and searches without a perceptible hitch.

Everything below is a measurable consequence of that promise.

## Frame budget

| Mode | Target FPS | Frame budget |
|---|---|---|
| Idle (cursor not moving) | 0 (no paint) | — |
| Scrolling, hovering, typing | 144 | 6.94 ms |
| Drag in progress | 60 (system limit) | 16.6 ms |
| Animation (state transitions) | 144 | 6.94 ms |

CPU paint cost target: **≤ 60% of frame budget** (≈ 4 ms at 144 Hz). The remaining 40% is for input handling, layout, and OS overhead.

## Input latency

Input event → reflected in next presented frame:

| Event | Target | Hard cap |
|---|---|---|
| Keystroke (type-ahead, text input) | 8 ms | 16 ms |
| Mouse click | 8 ms | 16 ms |
| Scroll (wheel or trackpad) | 8 ms | 16 ms |
| Drag-modifier-key change (cursor update) | 8 ms | 16 ms |

These are end-to-end including OS message-pump latency. The app's contribution must be ≤ 4 ms.

## Folder open / navigation

| Folder size | First paint | Full enumeration |
|---|---|---|
| 0 – 1,000 files | ≤ 16 ms | ≤ 50 ms |
| 1,000 – 10,000 | ≤ 16 ms (partial) | ≤ 200 ms |
| 10,000 – 100,000 | ≤ 16 ms (partial, ≥ 200 rows visible) | ≤ 1 s |
| 100,000 – 1,000,000 | ≤ 16 ms (partial, ≥ 200 rows visible) | ≤ 8 s |
| > 1,000,000 | ≤ 16 ms (partial) | streamed; UI never blocks |

**Streaming rule:** the first paint *never* waits for full enumeration. The list paints whatever the FS layer has emitted at frame N, and grows as more arrives. Sorting during streaming is *only* applied at the visible window unless the user explicitly sorts.

## Scrolling

| Folder size | Scroll FPS | Notes |
|---|---|---|
| ≤ 100,000 | 144 | unconditional |
| 100,000 – 1,000,000 | 144 | with lazy thumbnails (placeholders shown, thumbnails fill in) |
| > 1,000,000 | 144 | lazy thumbnails + lazy display-string formatting |

Scrolling **never** allocates. Display strings (size, mtime) are pre-formatted at item construction; thumbnails are off-the-hot-path.

## Memory budget

| Items | Target memory (excl. thumbnails) | Hard cap |
|---|---|---|
| 1,000 | < 1 MB | 4 MB |
| 100,000 | < 10 MB | 50 MB |
| 1,000,000 | < 100 MB | 250 MB |

Per item: target ≤ 96 bytes including cached display strings. (Reference: a `FileEntry` is `name: SmallStr<24>` + `metadata: u64×4` + `display_size: SmallStr<8>` + `display_mtime: SmallStr<10>`.)

Thumbnails are LRU-evicted; max thumbnail memory 200 MB regardless of folder size.

## Cold start

Cold start = no warm caches (first launch after reboot).

| Phase | Target |
|---|---|
| Process start → window visible | ≤ 100 ms |
| Window visible → first paint of file list | ≤ 200 ms |
| First paint → enumeration done (10k folder) | ≤ 200 ms |
| **Total: launch → usable** | **≤ 500 ms** |

This is the headline number. Explorer takes ~1.5 s on the same hardware. Beating that meaningfully is the proof point.

## Hot navigation

Navigating to a previously-visited folder (cache hit):

| Phase | Target |
|---|---|
| Click → first paint | ≤ 16 ms (one frame) |
| First paint → revalidation (timestamps re-checked) | ≤ 50 ms |

## Search

In-folder search (Ctrl+F):

| Folder size | First result | All results |
|---|---|---|
| 1,000 | ≤ 8 ms | ≤ 16 ms |
| 100,000 | ≤ 16 ms | ≤ 200 ms |
| 1,000,000 | ≤ 50 ms | ≤ 2 s |

Search is **incremental**: each keystroke filters the existing result set without re-enumerating.

Recursive / full-system search is out of scope for v1. (We'd integrate Windows Search Service for that.)

## Operation throughput

Copy / move / delete operations are bounded by I/O, not by us. But we must:

- Show first progress paint within 200 ms of operation start.
- Update progress at ≤ 60 Hz (don't waste frames on a 5%-progress repaint).
- Never block the UI thread on shell operations.

## What "perceptible hitch" means

A hitch is a frame that takes longer than its budget. Tracked metrics:

- **P50, P95, P99 frame time** during representative scenarios (scroll, type-ahead, drag).
- **Long frames** (> 16 ms during 144 Hz mode): logged, count must be 0 in steady state, ≤ 2 during folder-load streaming.
- **GC-style pauses:** Rust has none, but allocation spikes can mimic them. Allocation rate during scroll must be ≤ 0 bytes per frame after warmup.

The allocation budget *is* zero on the hot path. It is enforced by `#[deny(clippy::useless_vec)]`-style lints and an allocator hook in debug builds that panics on allocation during paint.

## Where these numbers come from

- 144 Hz: target hardware is high-refresh gaming/creator displays. Falling back to 60 Hz on lower-refresh monitors is automatic; numbers above scale.
- 100 ms cold-start: human attention threshold ≈ 100 ms (Doherty); we want no detectable delay.
- 8 ms input latency: roughly half a frame at 60 Hz, perceived as "immediate" by 95% of users (Microsoft input-latency papers).
- 96 bytes/item: 1 MB / ~10k items, comfortable cache locality for L1.

## Anti-features (because they would cost the budget)

- No virtualized DOM-style diffing (we render commands directly).
- No per-item React-like component instances.
- No GC'd language anywhere on the hot path.
- No SQLite-on-the-render-thread (DB queries off-thread, results pushed in).
- No synchronous shell COM calls during paint (all marshalled to a worker — the [shell_pump](../../Ferail/crates/ferail-win32/src/shell_pump.rs) approach from Ferail carries forward).
