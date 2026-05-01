# Interactive Control State Machine

Every interactive control implements **exactly this** state machine. Variations are forbidden.

```
                ┌────────┐  pointer-leave   ┌──────────┐
        ┌──────►│  Idle  │◄─────────────────│  Hover   │
        │       └───┬────┘                  └─────┬────┘
        │           │                             │
        │ blur      │ pointer-enter               │ pointer-down
        │           ▼                             ▼
        │       ┌────────┐                  ┌──────────┐
        └───────│Focused │                  │ Pressed  │──► fire on pointer-up-inside
                └────────┘                  └─────┬────┘
                                                  │ pointer-up-outside  → cancel
                                                  ▼
                                           (back to Idle)

  Disabled and Busy are orthogonal — settable from any state, suppress all input.
```

## States

| State | Visible style change | Input accepted? |
|---|---|---|
| **Idle** | Default token set | Yes (pointer + keyboard focus) |
| **Hover** | Background → next layer up; transition over `motion.fast` | Yes |
| **Pressed** | Background → 2 layers up; foreground may shift slightly | Yes (pointer up = confirm/cancel) |
| **Focused** | FocusRing overlay drawn (does *not* alter the control's bounds) | Yes (keyboard) |
| **Disabled** | All foreground tokens → `fg.disabled`; pointer events ignored | No |
| **Busy** | Spinner overlay; foreground at 60% opacity; pointer events ignored | No |

## Combinations

States can stack: `Hover + Focused`, `Pressed + Focused`. Render order:
1. Compute base style for the visual state (Idle / Hover / Pressed / Disabled / Busy).
2. If `Focused`, overlay FocusRing on top (after the control paints).

`Disabled` and `Busy` are mutually exclusive with `Pressed` — entering either cancels any in-flight press.

## Transitions

| From | Event | To |
|---|---|---|
| Idle | pointer-enter | Hover |
| Hover | pointer-leave | Idle |
| Hover | pointer-down | Pressed |
| Pressed | pointer-up *inside bounds* | Hover, fires `on_press` |
| Pressed | pointer-up *outside bounds* | Idle, no fire |
| Pressed | pointer-leave (button still down) | Pressed (sticky — matches OS convention) |
| any | gain keyboard focus | adds `Focused` overlay |
| any | lose keyboard focus | removes `Focused` overlay |
| any | `set_disabled(true)` | Disabled, cancels Pressed |
| any | `set_busy(true)` | Busy, cancels Pressed |

## Keyboard activation

- `Space`: press-and-release semantics. Down → Pressed. Up → fire + Hover.
- `Enter`: immediate fire (no Pressed visual). Mirrors OS button behavior.
- For **TextInput** and **Checkbox**, Space is reserved for typing/toggling — Enter is the only activator.

## Animation budget

State transitions animate the **background color only**, over `motion.fast` (75 ms), curve `ease.standard`. No size animation, no opacity animation, no scale. Anything more is delight, not function.

Exception: FocusRing fades in over `motion.default` (150 ms) the first time focus enters a *focus group* (e.g. tabbing into the toolbar from the file pane). Within a group, instant.

## Implementation contract

```rust
pub trait Interactive {
    fn state(&self) -> ControlState;
    fn handle_event(&mut self, ev: &InputEvent) -> EventResponse;
    fn paint(&self, painter: &mut dyn Renderer, bounds: Rect, tokens: &Tokens);
}

pub struct ControlState {
    pub visual: VisualState,    // Idle | Hover | Pressed | Disabled | Busy
    pub focused: bool,
}
```

The state lives **on the control instance**, not in the application's global state. Application owns logical state (e.g. "this button is disabled because no row is selected"); the control owns visual state (hover, pressed). Mixing the two is the most common state-machine bug — keep them separate.

## Tests

Each interactive primitive ships with a deterministic state-machine test (no rendering needed):

```rust
#[test]
fn button_pointer_press_then_leave_cancels() {
    let mut b = Button::new("Click");
    b.handle_event(&Event::PointerEnter);
    assert_eq!(b.state().visual, VisualState::Hover);
    b.handle_event(&Event::PointerDown);
    assert_eq!(b.state().visual, VisualState::Pressed);
    let resp = b.handle_event(&Event::PointerUpOutside);
    assert!(!resp.fired);
    assert_eq!(b.state().visual, VisualState::Idle);
}
```

State-machine bugs that escape rendering are caught here, fast.
