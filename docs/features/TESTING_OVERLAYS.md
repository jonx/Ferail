# Testing And Debug Overlays

← [Feature notes](README.md) · [Status](../STATUS.md) ·
[Architecture](../ARCHITECTURE.md) · [Open work](../../TODO.md)

Ferail has a screenshot CLI today and should grow a small set of debug
overlays that make performance and async behavior visible.

## What is built

The screenshot CLI exists. The visual debug overlays below are a design;
the work is tracked in [TODO.md](../../TODO.md).

## Current Tool

Use the binary's headless screenshot mode: the full dev loop, flag families,
and worked examples live in [SCREENSHOTS.md](SCREENSHOTS.md):

```sh
cargo run --bin ferail-gpui -- \
  --screenshot screenshots/ferail.png \
  --navigate . \
  --width 1400 --height 900
```

## Overlay Goals

Overlays must help verify the "UI never stops" rule.

Planned overlays:

- Frame time and long-frame counter.
- Paint allocation guard state.
- Visible row range and overscan.
- Tree loaded/unloaded nodes.
- Active worker tasks.
- Magic/icon/preview queue depth.
- Stale worker results dropped.
- Ant Trail heat values.
- Hit-test regions for rows, splitters, scrollbars, tabs, breadcrumb segments.

## CLI States To Add

- Long-running enumeration fake state.
- Permission denied error state.
- Empty folder state.
- Search no-results state.
- Drag-over target state.
- Context menu open state.
- Progress status state.
- Worker queue debug overlay.

## Test Rules

- A screenshot state should not depend on real mouse movement.
- A screenshot state should be deterministic.
- Slow I/O should be injectable so the app can prove it remains responsive.
- Visual debug overlays must be removable from production builds or hidden by
  default.
