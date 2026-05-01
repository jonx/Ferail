# Feraille Design Tokens

Tokens are the *only* source of visual values. A control that needs a color,
size, radius, font, or motion duration **must** resolve it through this
table. New tokens require a spec change; new raw values do not exist.

Implementation lives in `feraille-design`. Tokens are plain Rust constants,
grouped into namespaces. No runtime configuration in v1; theming hook
(light/dark swap) is built in but user themes wait for v2.

## Iteration-2 alignment with Zed

The token surface was tightened in iter-2 to match Zed's visual language:

- **Two foreground tiers**, not three. Zed effectively uses primary + a
  single muted gray; a "tertiary" tier produced washed-out controls that
  felt unbranded.
- **One accent**, not a hover/pressed chain. Zed reserves accent for
  selection/focus only; hover is communicated by `bg.layer3`. The
  `accent.fill_hover` and `accent.fill_pressed` slots are removed.
- **Sharp corners by default**. Zed's `Corners::default()` is zero. The
  radius scale is now `none / popover / full` — three values, not six.
- **Subtle layer deltas**. ~6% Y in dark, ~3% in light. Four bg layers,
  not five (`layer4` for "pressed row" is gone — pressed states now use
  `bg.layer3` like hover, distinguished by FocusRing or selection fill).

## Spacing scale

A 4-DIP base grid. Every margin, padding, gap is one of these.

| Token | DIPs | Use |
|---|---|---|
| `space.xxs` | 2 | Hairline insets, focus-ring offset |
| `space.xs`  | 4 | Tight inline spacing |
| `space.sm`  | 8 | Default inline gap |
| `space.md`  | 12 | Default block padding |
| `space.lg`  | 16 | Section gap |
| `space.xl`  | 24 | Panel padding |
| `space.xxl` | 32 | Large region gap |

## Radius scale

Sharp by default — Zed's choice, ours too. Three values.

| Token | DIPs | Use |
|---|---|---|
| `radius.none`    | 0     | All standard surfaces (rows, buttons, inputs, splitter) |
| `radius.popover` | 6     | Future: context menu, tooltip, dropdown body |
| `radius.full`    | 9999  | Pills, scrollbar thumb, circular icon button |

## Typography

Two font families.

| Token | Family | Notes |
|---|---|---|
| `font.ui`   | system UI default (San Francisco on macOS, Segoe UI on Windows) → "Inter" → sans-serif |
| `font.mono` | "SF Mono" / "Cascadia Code" → "JetBrains Mono" → monospace |

In iter-2, the dev binary loads `/System/Library/Fonts/Supplemental/Arial.ttf`
because system-font enumeration via `font-kit` lands with the macOS shell
crate in iter-3. Visual values below assume that placeholder.

### Sizes (DIPs)

| Token | Size | Use |
|---|---|---|
| `text.xs`   | 11 | Status bar, tooltip body |
| `text.sm`   | 12 | Labels, secondary metadata |
| `text.md`   | 13 | **Default body** — list rows, tree items |
| `text.lg`   | 15 | Panel titles, address-bar text |
| `text.xl`   | 18 | Empty-state heading |

### Weights

| Token | Weight |
|---|---|
| `weight.regular`  | 400 |
| `weight.medium`   | 500 |
| `weight.semibold` | 600 |

## Background layers

Light → dark, in increasing prominence on top of base.

| Token | Light | Dark | Use |
|---|---|---|---|
| `bg.base`   | `#FAFAFA` | `#1B1B1B` | Window background, dead space |
| `bg.layer1` | `#FFFFFF` | `#222222` | File pane, sidebar, panels |
| `bg.layer2` | `#F4F4F4` | `#262626` | Toolbar, tabstrip, status bar |
| `bg.layer3` | `#ECECEC` | `#2D2D2D` | Hover row, pressed surfaces |

## Foreground

| Token | Light | Dark | Use |
|---|---|---|---|
| `fg.primary`   | `#1A1A1A` | `#F5F5F5` | Body text, list row primary |
| `fg.secondary` | `#6F6F6F` | `#999999` | Metadata, second column |
| `fg.disabled`  | `#B0B0B0` | `#5A5A5A` | Disabled text |
| `fg.on_accent` | `#FFFFFF` | `#FFFFFF` | Text on accent fill |

(No `fg.tertiary` — see iteration-2 alignment note above.)

## Accent

One slot for the accent color, plus two derived selection-background
tints.

| Token | Light | Dark | Use |
|---|---|---|---|
| `accent.fill`            | `#2A63D9` | `#2457CA` | Focus ring, primary button fill (when we have one) |
| `accent.subtle`          | rgba(accent, 18%) | rgba(accent, 31%) | Selected row in focused list |
| `accent.subtle_inactive` | rgba(neutral, 11%) | rgba(neutral, 14%) | Selected row in unfocused list — no accent leak |

(No `accent.fill_hover` / `accent.fill_pressed` — hover does not use
accent in this design.)

## Border

| Token | Light | Dark | Use |
|---|---|---|---|
| `border.subtle`  | `#E5E5E5` | `#2D2D2D` | Divider, panel edge |
| `border.default` | `#D1D1D1` | `#3A3A3A` | Input outline (idle) |
| `border.focus`   | `accent.fill` | `accent.fill` | 2-DIP focus outline |

(No `border.strong` — only three border weights are needed in practice.)

## Status

| Token | Light | Dark |
|---|---|---|
| `status.success` | `#107C10` | `#6CCB5F` |
| `status.warning` | `#9D5D00` | `#FCE100` |
| `status.danger`  | `#C42B1C` | `#FF99A4` |

## Hit-target sizes

| Token | DIPs |
|---|---|
| `hit.min`    | 24 (toolbar buttons) |
| `hit.row`    | 28 (list rows) |
| `hit.button` | 32 (default button) |
| `hit.input`  | 32 (text inputs) |

## Focus

The FocusRing primitive paints a 2-DIP inset stroke in `border.focus`,
inside the target bounds (so it doesn't shift layout when focus moves).
It is rendered as the *last* layer of the host control.

| Property | Value |
|---|---|
| ring width  | 2 DIPs |
| ring color  | `border.focus` |
| ring radius | matches host (sharp by default) |

## Motion

Zed bakes no defaults. We follow that: animation duration is not a token.
When a control needs to animate, the duration is named at the call site
in the implementation, not pulled from a global. The handful of animated
moments (drag-preview fade-in, scrollbar auto-hide, etc.) are explicit.

## Iconography

| Token | Value |
|---|---|
| `icon.size_sm` | 14 |
| `icon.size_md` | 16 |
| `icon.size_lg` | 20 |
| `icon.stroke`  | 1.5 DIPs |

Iter-2 ships ~6 icons via `feraille-design::icons` (folder, document,
image, drive, chevron, dot). File/folder/drive thumbnails come from the
platform shell at runtime in iter-4+ — never bundled.

## Theme switching

```rust
pub static TOKENS: OnceLock<Tokens> = OnceLock::new();
TOKENS.set(Tokens::for_theme(Theme::detect_from_os()));
```

Theme is fixed for the process lifetime in v1. Hot-swap is a v2 concern.
