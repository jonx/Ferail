# Feraille Controls Overview

Feraille is not a generic UI framework. It is a compact control set for a
fast macOS file explorer.

## Hard Rules

1. No control draws raw colors outside design tokens.
2. Layout uses DIPs; renderer backends own scale conversion.
3. Paint is read-only: no I/O, no filesystem, no shell, no DB, no worker waits.
4. Interactive controls follow a shared state model.
5. Controls emit semantic events; the app decides what work to schedule.
6. Controls must be keyboard-reachable.

## Atomic Primitives

- Label
- Icon
- Button
- TextInput
- Checkbox
- Scrollbar
- Splitter
- Panel
- Divider
- Spacer
- FocusRing
- Tooltip
- ProgressBar
- Toast
- Overlay
- EmptyState
- ErrorState

Not every primitive is implemented yet. Do not add one until it pays for itself.

## Explorer Controls

- VirtualizedList
- FileTree
- BreadcrumbBar
- TabStrip
- ContextMenuHost
- StatusBar
- Search/filter input
- Preview pane

## Layering

```text
feraille-app
  |-- feraille-controls
  |     |-- feraille-design
  |     `-- feraille-render
  `-- feraille-core / fs / shell crates
```

Controls do not know paths or shell APIs. They receive display-ready state and
emit events.
