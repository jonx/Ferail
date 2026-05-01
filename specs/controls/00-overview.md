# Feraille Controls — Overview

## Purpose

This is **not** a generic UI framework. It is a *small, opinionated control set* sized exactly to what a fast Windows file explorer needs: ~12 atomic primitives + ~7 explorer-specific controls. Done well, on a fast custom rendering pipeline.

The reason every previous attempt at "make my Direct2D app look polished" failed is not the rendering tech — it is the absence of a coherent **design system**. Tokens before pixels, primitives before features, states before variants. This spec exists so that visual decisions are made *once*, in tokens, not 50 times in scattered drawing code.

## Hard rules

1. **No control draws a raw color literal.** Every paint call resolves to a token (see [01-design-tokens.md](01-design-tokens.md)). If a control needs a color that isn't in the palette, the palette is wrong, not the control.
2. **No control hard-codes a pixel value.** All sizes, radii, paddings come from the spacing/radius scale.
3. **Every interactive control implements the same state machine** (idle → hover → pressed → focused → disabled → busy). See [04-state-machine.md](04-state-machine.md).
4. **Layout is in DIPs, surfaces are in physical pixels.** The renderer abstraction owns the conversion. Controls never see the scale factor.
5. **Paint is read-only.** No allocation, no I/O, no resolve-on-paint. Inherited from the predecessor project (Ferail) — it is non-negotiable for the perf targets in [`specs/ux/05-performance.md`](../ux/05-performance.md).
6. **Every control is keyboard-reachable and screen-reader announceable.** Mouse-only is a bug.

## Inventory at a glance

### Atomic primitives — [02-primitives.md](02-primitives.md)
| # | Control | Why it exists |
|---|---|---|
| 1 | Label | Display-only text with token-driven style |
| 2 | Icon | Vector or SVG glyph, tinted by token |
| 3 | Button | Primary action surface |
| 4 | TextInput | Single-line editable text |
| 5 | Checkbox | Boolean toggle, also used for multi-select state |
| 6 | Scrollbar | Vertical/horizontal, auto-hide |
| 7 | Splitter | Drag handle between resizable panels |
| 8 | Panel | Bounded surface with optional title/padding |
| 9 | Divider | 1-DIP rule, horizontal or vertical |
| 10 | Spacer | Invisible flex element |
| 11 | FocusRing | Keyboard focus indicator overlay |
| 12 | Tooltip | Delayed hover/focus disclosure |

### Less-visible controls — also [02-primitives.md](02-primitives.md)
- ResizeHandle (window edges, distinct from Splitter)
- LoadingSpinner (indeterminate progress)
- ProgressBar (linear, determinate)
- Toast (transient notification stack)
- Overlay (modal scrim host)
- EmptyState (no-content placeholder)
- ErrorState (failure placeholder with action)

### Explorer-specific controls — [03-explorer-controls.md](03-explorer-controls.md)
| # | Control | Replaces |
|---|---|---|
| 1 | VirtualizedList | The file pane |
| 2 | FileTree | Left navigation pane |
| 3 | BreadcrumbBar | The address bar |
| 4 | TabStrip | Window tabs |
| 5 | ContextMenuHost | Right-click menu (wraps Win32 IContextMenu on Windows) |
| 6 | StatusBar | Bottom status |
| 7 | SearchBox | Find-in-folder input with chips |

## What is *not* in scope

- Generic data grid, generic tree-grid, generic tab control, generic menubar.
- Theming UI (preferences pane). Tokens are code-defined; user theming is a v2 concern.
- Animations beyond the four motion tokens (`fast`, `default`, `slow`, `entrance`). No spring physics, no path animations.
- Form controls beyond TextInput + Checkbox. No dropdowns, sliders, date pickers — the explorer doesn't need them.

## Layering

```
feraille-app          (binary; owns window + state)
   │
   ├─► feraille-controls  (this spec)
   │       │
   │       ├─► feraille-design   (tokens; this spec, file 01)
   │       └─► feraille-render   (Renderer trait; backends behind it)
   │
   └─► feraille-core      (FileEntry, FsTrait — UI knows nothing about Win32)
```

The control layer never imports from `feraille-shell-win32` or `feraille-fs-*`. Controls receive `&[ListItem]`-like data and emit `Event`s; they don't know what a path is.
