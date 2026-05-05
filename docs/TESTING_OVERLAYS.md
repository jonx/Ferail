# Testing And Debug Overlays

Feraille has a screenshot CLI today and should grow a small set of debug
overlays that make performance and async behavior visible.

## Current Tool

Use the binary's screenshot mode:

```sh
cargo run --bin feraille -- \
  --screenshot /tmp/feraille.png \
  --navigate ~/Source/Feraille \
  --width 1400 --height 900
```

Useful flags include:

- `--theme dark`
- `--expand <path>`
- `--select-name <name>`
- `--splitter <x>`
- `--scroll <y>`
- `--sort <column[-desc]>`
- `--filter <text>`
- `--search`
- `--preview`
- `--properties`
- `--rename`
- `--new-folder`
- `--mac-chrome`

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
