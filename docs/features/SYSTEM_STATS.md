# System Stats

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

The status bar's app-footprint segment:

```
up 3d 4h · CPU 6.8% · MEM 184.0 MB · 58 rps
```

## What is built

Implemented:

- `crates/ferail-gpui/src/system_stats.rs`: sampler, snapshot type,
  formatting.
- `ProcessState::system_stats`: the cached snapshot render reads.
- Muted segment in `status_bar.rs`, between the free-space label and the
  hidden-content summary. Each figure sits in a fixed-min-width,
  right-aligned, rem-based box so live updates never change the segment's
  layout: the separators stay put instead of jittering on every tick.
- `--screenshot … --simulate-stats` renders the segment with fixed reference
  values for deterministic captures.

## What it measures

Everything is **app-centric**: what Ferail itself costs, not the machine:

| Figure | Source | Notes |
|---|---|---|
| `up` | `sysinfo` process `run_time()` | Time since this process started. Two coarsest units (`45s`, `12m`, `4h 12m`, `3d 4h`). |
| `CPU` | `sysinfo` process `cpu_usage()` | Activity Monitor convention: % of one core, so >100 is possible on multi-core work. One decimal below 10%, whole numbers above. |
| `MEM` | `sysinfo` process `memory()` | Resident set size, formatted by the status bar's shared `humanize_bytes`. |
| `rps` | gpui's frame-timing profiler | **Redraws per second**: count of `Window::draw`s ÷ the sample window's duration, floored. Per-window: each window's status bar shows its own figure. |

A machine-wide variant (system CPU/GPU/memory) was considered and rejected
for v1: system GPU utilization has no portable API (IOKit / D3DKMT / sysfs
per platform), and the app-centric framing answers a sharper question:
"what is this file manager costing me?"

## Why `rps`, not "fps"

gpui only draws invalidated frames, so a file manager's frame history is
bursty: a `frames ÷ window` average labelled "fps" misreads a brief scroll
inside a mostly-idle window as low smoothness ("12 fps" for a perfectly
smooth flick). This shipped first and caused exactly that confusion. A
burst-windowed fps was tried next, but that bends the *measurement* to fit
the *label*. The resolution is the reverse: keep the honest, simple
measurement, a plain redraw count over the window, and **name it what it
is**: `rps`, redraws per second.

Read that way every value is simply true: idle reads `0 rps`; a brief
scroll reads a middling number ("the window redrew 12×/s on average");
sustained scrolling approaches the display rate; and a nonzero value while
the app should be idle is the repaint-leak tripwire: the number is the
leak's actual loop rate. The display floors (rather than rounds) so the
sampler's own 0.5 Hz notify ticks (~0.5 rps) read 0 instead of flickering
to 1.

Frame data comes from `gpui::profiler` (`set_frame_trace_enabled` +
`FrameTimingCollector`), which records every `Window::draw` into a global
ring. Each sample tick drains the ring and then flips tracing off/on: off
clears and shrinks the ring, so it holds at most one tick's worth of frames
instead of creeping toward its 16 MiB cap over a long session. This measures
CPU-side draw cost/count only; gpui exposes no GPU-side execution timing.

## Prime Directive compliance

The sysinfo refresh is a syscall, so the sampler follows the canonical
shape (see [Architecture § Prime Directive](../ARCHITECTURE.md#prime-directive)):

- A foreground loop (`system_stats::start_sampler`, spawned once at boot)
  wakes every 2 s and ships the long-lived `sysinfo::System` to
  `cx.background_executor()` for the actual refresh. The `System` must live
  across ticks because process CPU% is a delta between two refreshes; the
  first refresh only primes the baseline and is never published.
- The finished `StatsSnapshot` is stored in `ProcessState::system_stats`
  on the UI thread and each live Shell is notified.
- Render formats the cached snapshot; it never samples. In screenshot mode
  the sampler is never started (screenshots go through `screenshot::run`,
  not `boot::run_gui`), and `--simulate-stats` pins fixed values instead.

The refresh asks for exactly `ProcessesToUpdate::Some(&[our pid])` with
CPU + memory only, never `refresh_all()`, which would enumerate every
process on the box.

## Known gaps / follow-ups

- The 0.5 Hz sampler notify redraws each window's status bar even when
  nothing else changed; the draw is cheap but it is why an "idle" Ferail
  still draws ~1 frame every 2 s (~0.5 rps, which the floor keeps reading
  as 0).
- No settings toggle yet: the segment is always on. If status-bar space
  gets tight on narrow windows, a Feature Settings switch is the natural
  next step.
- Machine-wide GPU % (IOKit `IOAccelerator` on macOS first) remains a
  possible later addition; see git history of this file's feature
  discussion for the per-platform routes.
