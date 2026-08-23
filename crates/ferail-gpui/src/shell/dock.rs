//! Window-docking geometry (docs/features/DOCK.md).
//!
//! Pure model: no GPUI, no AppKit, no wall-clock. The shell drives an
//! auto-hiding drawer — dock the whole window to the **left or right** screen
//! edge, where it floats over everything and slides off-screen leaving only a
//! thin handle; pushing the cursor into that screen edge slides it back in.
//! Everything position-related bottoms out in the functions here so it can be
//! reasoned about and unit-tested without a running window.
//!
//! Docking is left/right only by design (top/bottom were dropped — the top
//! edge fights the menu bar and the horizontal drawer is the useful one).
//!
//! All coordinates are macOS **global screen space**: origin at the
//! bottom-left of the main display, `y` growing upward. That is the one space
//! `NSEvent.mouseLocation`, `NSScreen.visibleFrame`, and `NSWindow.frame`
//! already share, so the host hands these functions raw values and applies the
//! results verbatim — no flipping. (Overlay *rendering* uses GPUI's top-left
//! window space instead and is handled in `render.rs`.)

/// Visible thickness of the handle strip left on-screen when a docked drawer
/// is hidden. Small enough to stay out of the way, fat enough to slam into.
pub const STRIP_PX: f64 = 6.0;

/// How close to the docked screen edge the cursor must come to trigger a
/// reveal. The trigger is the whole edge ("edge-slam"), not just the handle,
/// so it is easy to hit; the handle is the visual hint.
pub const REVEAL_PX: f64 = 4.0;

// (No size floor/clamp: docking NEVER resizes the window. gpui's
// drawable does not follow an out-of-band AppKit `setFrame:` resize —
// forcing the drawer to full screen height left the extra area black —
// so the drawer is the window at its own size, purely translated.)

/// Fraction of the hidden→revealed travel the slide advances per poll tick.
/// At the ~16 ms animating poll interval this is a ~5-frame, ~80 ms slide.
pub const SLIDE_STEP: f32 = 0.22;

/// A rectangle in global screen space. `(x, y)` is the bottom-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenFrame {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl ScreenFrame {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// Inclusive point-in-rect test.
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
}

/// Which screen edge the window is docked to. Left/right only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockEdge {
    Left,
    Right,
}

impl DockEdge {
    pub const ALL: [DockEdge; 2] = [DockEdge::Left, DockEdge::Right];

    /// Stable token for app_state persistence.
    pub fn as_str(self) -> &'static str {
        match self {
            DockEdge::Left => "left",
            DockEdge::Right => "right",
        }
    }

    pub fn from_token(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "left" => Some(DockEdge::Left),
            "right" => Some(DockEdge::Right),
            _ => None,
        }
    }
}

/// The window frame when the drawer is fully revealed: flush against the dock
/// edge at the window's OWN size — docking never resizes (see module docs).
/// `win` supplies the size and the preferred `y`; the `y` is clamped so the
/// window stays on the screen vertically (a window taller than the screen
/// pins to the screen's bottom edge).
pub fn revealed_frame(edge: DockEdge, s: ScreenFrame, win: ScreenFrame) -> ScreenFrame {
    let y = win.y.clamp(s.y, (s.y + s.h - win.h).max(s.y));
    match edge {
        DockEdge::Left => ScreenFrame::new(s.x, y, win.w, win.h),
        DockEdge::Right => ScreenFrame::new(s.x + s.w - win.w, y, win.w, win.h),
    }
}

/// The window frame when the drawer is fully hidden: same size as revealed,
/// shoved out toward the dock edge until only `strip` px remain on-screen.
pub fn hidden_frame(edge: DockEdge, s: ScreenFrame, win: ScreenFrame, strip: f64) -> ScreenFrame {
    let r = revealed_frame(edge, s, win);
    let off = (win.w - strip).max(0.0);
    match edge {
        DockEdge::Left => ScreenFrame { x: r.x - off, ..r },
        DockEdge::Right => ScreenFrame { x: r.x + off, ..r },
    }
}

/// Is the cursor in the reveal trigger zone — within `REVEAL_PX` of the docked
/// screen edge, and within the screen's height (so an adjacent display's edge
/// can't trigger it)?
pub fn cursor_in_trigger_zone(edge: DockEdge, s: ScreenFrame, mouse: (f64, f64)) -> bool {
    let (mx, my) = mouse;
    let in_y = my >= s.y && my <= s.y + s.h;
    match edge {
        DockEdge::Left => mx <= s.x + REVEAL_PX && in_y,
        DockEdge::Right => mx >= s.x + s.w - REVEAL_PX && in_y,
    }
}

/// Ease-out so the slide decelerates into its resting position.
pub fn ease_out_cubic(t: f64) -> f64 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

/// Linear blend between two frames. Only the origin actually moves during a
/// slide (size is constant), so this stays a pure translation.
pub fn lerp_frame(a: ScreenFrame, b: ScreenFrame, t: f64) -> ScreenFrame {
    ScreenFrame {
        x: a.x + (b.x - a.x) * t,
        y: a.y + (b.y - a.y) * t,
        w: a.w + (b.w - a.w) * t,
        h: a.h + (b.h - a.h) * t,
    }
}

/// Live docking state the shell holds while an edge is active.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DockState {
    pub edge: DockEdge,
    /// `visibleFrame` of the docked display, captured when docking.
    pub screen: ScreenFrame,
    /// The drawer's frame source: the window's own size + preferred `y`.
    /// Docking never resizes, so this is the live window frame, refreshed
    /// by the host on reveal transitions (the user may have moved/resized
    /// the revealed drawer).
    pub win: ScreenFrame,
    /// Window frame before docking, restored on undock.
    pub restore: ScreenFrame,
    /// Whether the drawer is sliding toward / resting revealed.
    pub revealed: bool,
    /// 0.0 = fully hidden (handle only), 1.0 = fully revealed. The poll loop
    /// steps this toward its target each tick; `current_frame` eases over it.
    pub progress: f32,
}

impl DockState {
    pub fn new(
        edge: DockEdge,
        screen: ScreenFrame,
        win: ScreenFrame,
        restore: ScreenFrame,
    ) -> Self {
        Self {
            edge,
            screen,
            win,
            restore,
            revealed: false,
            progress: 0.0,
        }
    }

    /// The window frame to apply this tick, eased between hidden and revealed.
    pub fn current_frame(&self) -> ScreenFrame {
        let hidden = hidden_frame(self.edge, self.screen, self.win, STRIP_PX);
        let shown = revealed_frame(self.edge, self.screen, self.win);
        lerp_frame(hidden, shown, ease_out_cubic(self.progress as f64))
    }

    /// Decide whether the drawer wants to be revealed for the given cursor:
    /// in the edge trigger zone, or (once revealed) still over the drawer, so
    /// it stays put until the pointer actually leaves.
    pub fn wants_reveal(&self, mouse: (f64, f64)) -> bool {
        if cursor_in_trigger_zone(self.edge, self.screen, mouse) {
            return true;
        }
        if self.revealed {
            let shown = revealed_frame(self.edge, self.screen, self.win);
            if shown.contains(mouse.0, mouse.1) {
                return true;
            }
        }
        false
    }

    /// Advance `progress` one tick toward the current target (revealed→1,
    /// hidden→0). Returns whether it moved (false once settled).
    pub fn step(&mut self) -> bool {
        let target: f32 = if self.revealed { 1.0 } else { 0.0 };
        if (self.progress - target).abs() < 1e-4 {
            self.progress = target;
            return false;
        }
        let next = if target > self.progress {
            (self.progress + SLIDE_STEP).min(target)
        } else {
            (self.progress - SLIDE_STEP).max(target)
        };
        self.progress = next.clamp(0.0, 1.0);
        true
    }

    /// Whether the slide is mid-flight (poll faster) vs. settled (poll lazily).
    pub fn is_animating(&self) -> bool {
        let target: f32 = if self.revealed { 1.0 } else { 0.0 };
        (self.progress - target).abs() >= 1e-4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 1440x875 visible frame at the global origin.
    fn screen() -> ScreenFrame {
        ScreenFrame::new(0.0, 0.0, 1440.0, 875.0)
    }

    #[test]
    fn edge_str_roundtrips() {
        for e in DockEdge::ALL {
            assert_eq!(DockEdge::from_token(e.as_str()), Some(e));
        }
        assert_eq!(DockEdge::from_token("LEFT"), Some(DockEdge::Left));
        assert_eq!(DockEdge::from_token("top"), None); // dropped
        assert_eq!(DockEdge::from_token("nonsense"), None);
    }

    // A 400x600 window sitting somewhere mid-screen.
    fn window() -> ScreenFrame {
        ScreenFrame::new(500.0, 120.0, 400.0, 600.0)
    }

    #[test]
    fn revealed_left_is_flush_and_keeps_window_size_and_y() {
        let s = screen();
        let w = window();
        let r = revealed_frame(DockEdge::Left, s, w);
        // Docking is a pure translation: size and y untouched.
        assert_eq!(r, ScreenFrame::new(0.0, 120.0, 400.0, 600.0));
    }

    #[test]
    fn revealed_right_hugs_right_edge() {
        let s = screen();
        let w = window();
        let r = revealed_frame(DockEdge::Right, s, w);
        assert_eq!(r.x + r.w, s.x + s.w); // right edge flush
        assert_eq!(r, ScreenFrame::new(1040.0, 120.0, 400.0, 600.0));
    }

    #[test]
    fn revealed_clamps_y_onto_the_screen_but_never_resizes() {
        let s = screen();
        // Window hanging off the top of the screen → pinned to the top.
        let high = ScreenFrame::new(500.0, 800.0, 400.0, 600.0);
        let r = revealed_frame(DockEdge::Left, s, high);
        assert_eq!((r.y, r.w, r.h), (275.0, 400.0, 600.0)); // 875 - 600
        // Window below the screen → pinned to the bottom.
        let low = ScreenFrame::new(500.0, -300.0, 400.0, 600.0);
        let r = revealed_frame(DockEdge::Left, s, low);
        assert_eq!((r.y, r.w, r.h), (0.0, 400.0, 600.0));
        // Taller than the screen: pinned to the bottom, STILL not resized.
        let tall = ScreenFrame::new(500.0, 100.0, 400.0, 1200.0);
        let r = revealed_frame(DockEdge::Left, s, tall);
        assert_eq!((r.y, r.w, r.h), (0.0, 400.0, 1200.0));
    }

    #[test]
    fn hidden_leaves_exactly_a_strip_on_screen() {
        let s = screen();
        let w = window();
        let strip = STRIP_PX;
        // Left: window's right edge sits `strip` px past the screen's left.
        let h = hidden_frame(DockEdge::Left, s, w, strip);
        assert!((h.x + h.w - (s.x + strip)).abs() < 1e-9);
        // Right: window's left edge sits `strip` px short of the screen's right.
        let h = hidden_frame(DockEdge::Right, s, w, strip);
        assert!((h.x - (s.x + s.w - strip)).abs() < 1e-9);
    }

    #[test]
    fn hidden_and_revealed_keep_the_same_size() {
        let s = screen();
        let w = window();
        for e in DockEdge::ALL {
            let r = revealed_frame(e, s, w);
            let h = hidden_frame(e, s, w, STRIP_PX);
            assert!((r.w - h.w).abs() < 1e-9 && (r.h - h.h).abs() < 1e-9);
            assert!((r.w - w.w).abs() < 1e-9 && (r.h - w.h).abs() < 1e-9);
        }
    }

    #[test]
    fn trigger_zone_fires_at_the_edge_only() {
        let s = screen();
        // Just inside the left edge → fires.
        assert!(cursor_in_trigger_zone(DockEdge::Left, s, (1.0, 400.0)));
        // Well inside the screen → does not.
        assert!(!cursor_in_trigger_zone(DockEdge::Left, s, (200.0, 400.0)));
        // At the edge but above the screen's visible height → does not.
        assert!(!cursor_in_trigger_zone(DockEdge::Left, s, (1.0, 1000.0)));
        // Right edge.
        assert!(cursor_in_trigger_zone(DockEdge::Right, s, (1439.0, 400.0)));
        assert!(!cursor_in_trigger_zone(DockEdge::Right, s, (1200.0, 400.0)));
    }

    #[test]
    fn revealed_drawer_stays_open_while_pointer_is_over_it() {
        let s = screen();
        let win = ScreenFrame::new(0.0, 0.0, 400.0, 700.0);
        let mut st = DockState::new(DockEdge::Left, s, win, win);
        st.revealed = true;
        // Pointer deep inside the drawer (x=200), away from the edge zone.
        assert!(st.wants_reveal((200.0, 400.0)));
        // Pointer outside the drawer entirely → wants to hide.
        assert!(!st.wants_reveal((900.0, 400.0)));
    }

    #[test]
    fn step_converges_to_target_and_then_settles() {
        let s = screen();
        let win = ScreenFrame::new(0.0, 0.0, 400.0, 700.0);
        let mut st = DockState::new(DockEdge::Left, s, win, win);
        st.revealed = true;
        let mut ticks = 0;
        while st.step() {
            ticks += 1;
            assert!(ticks < 100, "slide should converge quickly");
        }
        assert_eq!(st.progress, 1.0);
        assert!(!st.is_animating());
        st.revealed = false;
        while st.step() {}
        assert_eq!(st.progress, 0.0);
    }

    #[test]
    fn current_frame_endpoints_match_hidden_and_revealed() {
        let s = screen();
        let win = ScreenFrame::new(0.0, 0.0, 300.0, 875.0);
        let mut st = DockState::new(DockEdge::Right, s, win, win);
        // progress 0 → hidden
        assert_eq!(
            st.current_frame(),
            hidden_frame(DockEdge::Right, s, win, STRIP_PX)
        );
        // progress 1 → revealed
        st.progress = 1.0;
        assert_eq!(st.current_frame(), revealed_frame(DockEdge::Right, s, win));
    }
}
