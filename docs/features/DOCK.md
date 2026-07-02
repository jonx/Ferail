# Window Docking

← [Feature index](README.md) · [Architecture](../ARCHITECTURE.md)

Dock the **whole Feraille window** to the **left or right** screen edge as an
auto-hiding, always-on-top **drawer** — a Quake/iTerm-hotkey-style panel. When
docked it floats above every other app and slides off-screen, leaving only a
thin grab **handle** on that edge. Slamming the cursor into the docked screen
edge slides the window back in; moving the pointer away tucks it out again. The
point is to reach the file manager from inside any other app without cluttering
the desktop.

Left/right only by design — the top edge fights the menu bar and the horizontal
drawer is the useful shape.

macOS-only in practice: it bottoms out in AppKit `NSWindow` calls. The other
platforms' shell stubs no-op, so the toolbar menu silently does nothing there.

## Using it

- Toolbar → the **dock** glyph (top-right cluster, after Refresh). Its dropdown
  offers **Dock Left**, **Dock Right**, and **Undock**, each with an icon. The
  button shows a pressed state while docked.
- Actions: `DockLeft`, `DockRight`, `Undock` (`shell/actions.rs`) — also
  reachable from the command surfaces.

## Behaviour

- **Snap, then tuck.** Docking snaps the window flush to the edge fully shown,
  then (unless the cursor is already at that edge) it slides out to the handle —
  so the transition reads as "snap to edge → tuck away".
- **Reveal = edge-slam.** The whole docked screen edge is the trigger, not just
  the handle, so it is easy to hit; the handle is the visual hint. Once shown,
  the drawer stays open while the pointer is over it and tucks away when the
  pointer leaves both the drawer and the edge (hysteresis).
- **Floats, doesn't steal focus.** Revealing slides the window in over your
  current app without activating Feraille (`NSFloatingWindowLevel`); a click
  activates it. It also joins all Spaces and floats over full-screen apps
  (`NSWindowCollectionBehaviorCanJoinAllSpaces | FullScreenAuxiliary`), so it's
  reachable from any Space.
- **Drawer size.** The drawer keeps the window's pre-dock width (clamped to
  `[MIN_EXTENT, screen]`) and fills the screen height. Undocking restores the
  exact pre-dock frame.

## Architecture

Three layers, matching the repo's boundaries:

- **`shell/dock.rs` — pure geometry.** No GPUI, no AppKit, no wall-clock:
  `DockEdge`, `DockState`, and the frame math (`revealed_frame`,
  `hidden_frame`, `cursor_in_trigger_zone`, `current_frame`, `step`,
  `wants_reveal`). All in macOS **global screen space** (origin bottom-left,
  y-up) so values from `NSEvent`/`NSScreen`/`NSWindow` flow through unflipped.
  Fully unit-tested.
- **`shell.rs` — GPUI glue.** `Shell::set_dock` captures the window/screen
  frames once, sets the float + all-Spaces behaviours, and starts the reveal
  poll. The poll is a **self-re-arming one-shot** (`schedule_dock_poll` →
  `dock_poll_tick`, like the viewer's slideshow timer, epoch-guarded): each tick
  reads `NSEvent.mouseLocation`, flips the drawer's revealed target, steps the
  slide, and moves the window **only when the slide actually advanced** — a
  settled drawer touches AppKit zero times per tick. It polls ~16 ms mid-slide,
  ~33 ms once settled, and runs **only while docked**. The window's `NSView` is
  captured as a `usize` (raw pointers aren't `Send`) so the async loop can move
  the window without a `Window` handle.
- **`feraille-shell-mac` — the AppKit primitives.** `current_mouse_location`,
  `screen_visible_frame_for_window`, `window_frame`, `set_window_frame`
  (deliberately *not* animated — the host drives the slide so nothing spins the
  run loop, per the Prime Directive; size stays fixed so gpui never resizes its
  drawable), and `set_window_all_spaces`. Reuses the existing
  `set_window_floating`.

The **handle** is a Root-level overlay (`Shell::dock_handle`) anchored to the
window edge opposite the dock edge (the side left on-screen), drawn only while
docked and not fully revealed.

## Prime Directive

Nothing here runs on the paint path. The reveal poll is a scheduled task that
exists only while docked; rendering just reads `DockState` and draws the handle.

## Deferred / known limitations

- **No persistence / auto-restore.** Docking is a session action (like the
  viewer's "Stay on top"), intentionally not persisted: auto-restoring on launch
  would start the app already hidden as a handle, which is surprising and only
  recoverable through the handle itself. A future opt-in setting could restore
  the docked state, ideally starting revealed.
- **Native titlebar in drawer mode.** The traffic-light titlebar still shows
  when docked. A borderless drawer chrome (and hiding the traffic lights) is a
  follow-up.
- **No per-item active-edge checkmark.** The menu items carry directional
  icons instead of a checkmark; the active edge is shown by the toolbar
  button's pressed state and by the drawer itself being visibly on a side.
- **Multi-display:** docks to the display the window currently occupies; the
  captured screen frame goes stale if the display layout changes while docked.
- **Full-screen coverage:** join-all-Spaces + `FullScreenAuxiliary` lets the
  drawer appear over most full-screen apps, but a true-full-screen app on its
  own Space can still occlude it in some cases.
