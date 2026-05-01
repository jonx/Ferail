# Atomic + Less-Visible Primitives

For each control: **purpose**, **API surface**, **states it implements**, **interaction**, **accessibility**, **rendering notes**. Where rendering is non-obvious, the inspiration source from the GPUI study is named.

All primitives live in `feraille-controls/src/primitives/`.

---

## 1. Label

**Purpose:** display-only text that resolves a token and obeys layout rules. Not a `Text` component — it's intentionally narrower (no rich runs, no inline children).

**API:**
```rust
pub struct Label<'a> {
    text: &'a str,
    style: LabelStyle,           // size + weight + color tokens
    align: TextAlign,            // Start | Center | End
    overflow: TextOverflow,      // Clip | Ellipsis | Wrap (defaults to Ellipsis)
    max_lines: Option<u16>,      // None = single line
}
```

**States:** none (non-interactive).

**Rendering:** measured once, painted from the cached glyph layout. Truncation computes the ellipsis width once per (text, font, size).

**Accessibility:** announced as static text. If the text is identical to a sibling button's accessible name, set `aria_hidden = true`.

---

## 2. Icon

**Purpose:** vector glyph from the bundled SVG sprite, tinted to a foreground token.

**API:**
```rust
pub struct Icon {
    name: IconName,              // enum, exhaustive — no string lookup
    size: IconSize,              // Sm | Md | Lg
    color: ColorToken,           // resolved at paint
}
```

**States:** none.

**Rendering:** SVG path is parsed once at startup into a flat list of fill/stroke commands; per-frame, only the tint color changes. The path cache is in `feraille-design::icons`.

For platform shell icons (file thumbnails, drive glyphs), the control delegates to the host — see `IconName::ShellHandle(u32)`. The handle is opaque; on Windows it's an `IShellItemImageFactory` token, on dev mode it's a placeholder rectangle.

---

## 3. Button

**Purpose:** the only general-purpose action surface. Three variants, no more.

**Variants:** `Primary` (accent fill), `Secondary` (outline), `Ghost` (no border, hover-only background).

**API:**
```rust
pub struct Button<'a> {
    label: &'a str,
    icon: Option<IconName>,      // optional leading icon
    variant: ButtonVariant,
    state: ButtonState,          // owner-controlled: Enabled | Disabled | Busy
    on_press: Callback,
    keyboard_hint: Option<&'a str>, // e.g. "Ctrl+N"
}
```

**States:** idle, hover, pressed, focused, disabled, busy. Implements the full state machine ([04-state-machine.md](04-state-machine.md)).

**Sizing:** height = `hit.button`. Horizontal padding = `space.md` if labelled, else `space.sm`. Icon-only square = `hit.button × hit.button`.

**Interaction:**
- Pointer down → `Pressed` state (visual only).
- Pointer up *inside* bounds → fire `on_press`.
- Pointer up *outside* bounds → cancel.
- Keyboard: `Space` press-and-release fires; `Enter` fires immediately.
- `Busy` shows a spinner overlay and ignores input.

**Accessibility:** `role=button`, accessible name = `label` (or `aria_label` if icon-only), keyboard hint announced via `aria-keyshortcuts`.

---

## 4. TextInput

**Purpose:** single-line editable text. Used by SearchBox, BreadcrumbBar (edit mode), rename overlay.

**API:**
```rust
pub struct TextInput<'a> {
    value: &'a str,
    placeholder: &'a str,
    on_change: Callback<String>,
    on_submit: Option<Callback<String>>,
    on_cancel: Option<Callback>,
    select_all_on_focus: bool,
    ime_enabled: bool,
}
```

**States:** idle, hover, focused (caret visible), disabled, invalid (border switches to `status.danger`).

**Interaction:**
- Caret blink: 530ms half-period, paused while typing.
- Selection: shift+arrow, double-click word, triple-click line.
- IME: composition rectangle drawn beneath caret (Windows: WM_IME_COMPOSITION; macOS dev: `NSTextInputClient`).
- Clipboard: Ctrl+C/X/V (Cmd on macOS).
- Esc fires `on_cancel`; Enter fires `on_submit`.

**Inspiration:** GPUI's text-input handling lives in `crates/gpui/src/input.rs` — particularly the IME composition state machine. Worth studying, not copying.

---

## 5. Checkbox

**Purpose:** boolean toggle. Also used inside list rows for explicit multi-select mode.

**API:**
```rust
pub struct Checkbox {
    state: CheckState,           // Off | On | Indeterminate
    on_toggle: Callback<CheckState>,
    label: Option<String>,       // optional inline label
}
```

**States:** idle, hover, pressed, focused, disabled. The `Indeterminate` value is for "some children selected" cases (folder tree multi-select).

**Sizing:** 16 × 16 box, 4 DIP gap to label, total hit = `hit.min`.

---

## 6. Scrollbar

**Purpose:** vertical or horizontal viewport indicator + drag handle.

**API:**
```rust
pub struct Scrollbar {
    axis: Axis,                  // Vertical | Horizontal
    content_size: f32,           // total scrollable extent (DIPs)
    viewport_size: f32,
    offset: f32,                 // current scroll position
    on_scroll: Callback<f32>,
}
```

**States:**
- `Hidden` (no overflow → not painted at all)
- `Idle` (auto-hidden after 1s of no interaction; only a 2-DIP rail is visible)
- `Hover` (full thumb width, expanded track)
- `Dragging` (thumb pinned to pointer)

**Interaction:**
- Drag thumb: relative offset.
- Click track above/below thumb: page-up / page-down (`viewport_size * 0.9`).
- Mousewheel anywhere over the scrollable area: scroll by 3 lines × `text.md.line_height` (≈ 56 DIPs at default).
- Shift+wheel: horizontal.

**Rendering:** thumb is a `radius.full` capsule, color `fg.tertiary` at 60% opacity, hover at 90%. Track only visible on hover. Auto-hide animates over `motion.fast`.

**Inspiration:** GPUI's scrollbar implementation is small and worth reading — `crates/gpui/src/elements/list.rs` (search "Scrollbar"). It correctly handles the case where content size changes while dragging; we will too.

---

## 7. Splitter

**Purpose:** horizontal or vertical drag handle resizing two adjacent panels.

**API:**
```rust
pub struct Splitter {
    axis: Axis,                  // Vertical splitter divides horizontally
    position: f32,               // distance from start (DIPs)
    bounds: SplitterBounds,      // min, max, optional snap points
    on_drag: Callback<f32>,
}
```

**States:** idle, hover (1-DIP highlight), dragging.

**Sizing:** the visible rule is **1 DIP**. The hit area is **6 DIPs** centered on the rule. This 6:1 ratio is critical — users cannot grab a 1-DIP target reliably, and a 6-DIP visible bar looks crude. Inspired by Files App and most pro IDEs.

**Cursor:** `ew-resize` for vertical splitter, `ns-resize` for horizontal. Set on hover, released on pointer-leave.

**Snap:** if a snap point is within ±4 DIPs of the dragged position, snap with no animation. Used for "default sidebar widths."

---

## 8. Panel

**Purpose:** a bounded region with optional title and optional padding. The structural unit of layout — sidebars, the main file pane, the preview pane are all Panels.

**API:**
```rust
pub struct Panel<Body> {
    title: Option<String>,
    title_actions: Vec<Button>,  // right-aligned in title bar
    padding: Spacing,            // any of space.* tokens
    collapsible: bool,
    body: Body,
}
```

**States:** expanded, collapsed (collapsible only), focused (subtle 1-DIP `border.focus` ring at content radius).

**Visual:** background `bg.layer1`, border `border.subtle`, radius `radius.md` only on outer corners (inner edges are flush with parent).

---

## 9. Divider

1-DIP rule. Color `border.subtle`. Horizontal or vertical. Margin defaults to `space.sm` perpendicular to its axis.

---

## 10. Spacer

Invisible flex element. Used inside row/column layouts to push siblings to opposite ends. No DIP value — it claims remaining space along the parent's main axis.

---

## 11. FocusRing

**Purpose:** the keyboard-focus visual — drawn as an *overlay*, not as part of the focused control. This avoids size shift when a control gains focus (a common anti-pattern).

**API:**
```rust
pub fn focus_ring(target_bounds: Rect, radius: f32, painter: &mut dyn Renderer);
```

**Rendering:** stroke = `focus.ring-width`, offset = `focus.ring-offset` outside the target, color = `border.focus`, radius = target radius + offset.

Rendered in a *late paint pass* (after all sibling content), so it overlaps neighbors cleanly.

---

## 12. Tooltip

**Purpose:** delayed disclosure of a control's full label or hint.

**API:**
```rust
pub struct Tooltip<'a> {
    text: &'a str,
    placement: Placement,        // Above | Below | Start | End | Auto
    delay_ms: u32,               // default 500
}
```

**States:** hidden, opening (fade in over `motion.default`), visible, closing.

**Behavior:**
- Open after pointer rests on host for `delay_ms`.
- Close immediately on pointer-leave or any key press.
- A second tooltip within 250ms of dismissal opens with no delay (matches Win11 behavior).
- Auto-flip placement to stay on-screen.

**Rendering:** background `bg.layer3`, border `border.subtle`, radius `radius.sm`, padding `4 8`, elevation `elev.2`, max-width 240 DIPs (wraps).

---

# Less-visible primitives

These have no obvious chrome but the system breaks without them.

## ResizeHandle

A 4-DIP-wide invisible region around the *window* edges (and corners) that cursor-hints and hands the drag to the OS via `WM_NCHITTEST`. Distinct from Splitter (which moves layout) — ResizeHandle moves the OS window. On macOS dev mode, it's free (system handles).

## LoadingSpinner

Indeterminate. 16-DIP default size. Single arc rotating at 1.2s per revolution, easing `linear`. Stroke `accent.fill`, track stroke `border.subtle`. Pause when off-screen (don't waste paint).

## ProgressBar

Linear, determinate. Height 2 DIPs. Track `border.subtle`, fill `accent.fill`. Indeterminate variant: bar of 30% width sweeps left→right over 1.5s, ease `ease.standard`.

## Toast

Transient notification — stack of up to 3, anchored bottom-right with `space.lg` margin. Each toast: icon + label + optional action + close. Dismiss after 4s (or 8s if it has an action). Slide+fade entrance over `motion.entrance`. Stack reflows as items leave.

## Overlay (Scrim)

Modal background. `rgba(0,0,0,0.32)` light, `rgba(0,0,0,0.50)` dark. Fade in over `motion.default`. Click outside the modal target → fire `on_dismiss`. Esc → same.

## EmptyState

Three-line placeholder for an empty pane: icon (lg), title (`text.xl`, `weight.semibold`), description (`text.md`, `fg.secondary`), optional action button. Vertically centered, max-width 320 DIPs.

## ErrorState

Same layout as EmptyState but with `status.danger` icon and a "Retry" or contextual action button. Used when shell enumeration fails, drive disconnects, permission denied.

---

## What's deliberately missing

- **Dropdown / Combobox** — replaced by ContextMenuHost where needed (e.g. breadcrumb segment dropdown).
- **Date/Time picker** — explorer doesn't compose dates; it displays them.
- **Slider** — not a primary action surface in this app.
- **Accordion** — Panel handles collapse for the cases we have.
- **Tabs** (general) — TabStrip is its own thing in [03-explorer-controls.md](03-explorer-controls.md), not a generic primitive.

If you reach for one of these later, the question is "are we becoming a generic UI framework?" If yes, stop.
