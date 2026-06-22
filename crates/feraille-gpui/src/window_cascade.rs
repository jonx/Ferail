//! Spiral window cascade: where to place the Nth instance of a window so a
//! batch (or a long session) of same-kind windows spreads out instead of
//! stacking on the exact same centred spot.
//!
//! A linear cascade (each window down-right of the last) marches off-screen
//! once you open a lot; this winds them in an Archimedean spiral around the
//! display centre and clamps each to stay fully on-screen, so even a big
//! fan-out (e.g. Get Info on a large selection) stays usable. The caller
//! owns the slot counter for its window kind (see `entry_info`); this
//! module is the stateless geometry.

use gpui::{App, Bounds, Pixels, Size, WindowBounds, point, px};

/// Radius growth per spiral step (px). With `r = RADIUS * sqrt(slot)` the
/// covered area grows linearly with the slot count, so windows stay evenly
/// spread rather than bunching or racing outward.
const RADIUS: f32 = 34.0;

/// Angular winding per spiral step (radians). Paired with the sqrt radius
/// below, `theta = ANGLE * sqrt(slot)` puts windows at roughly equal
/// arc-length along a single spiral arm — not a tight ring (would overlap)
/// and not a straight diagonal (would walk off-screen).
const ANGLE: f32 = 1.9;

/// Offset in px from the centred position for the `slot`-th window (slot 0
/// → no offset → centred). Equal-arc-length Archimedean spiral: both radius
/// and angle grow with `sqrt(slot)`, so `r ∝ theta` — a clean single arm.
fn spiral_offset(slot: usize) -> (f32, f32) {
    if slot == 0 {
        return (0.0, 0.0);
    }
    let t = (slot as f32).sqrt();
    let theta = ANGLE * t;
    let r = RADIUS * t;
    (r * theta.cos(), r * theta.sin())
}

/// Windowed bounds for the `slot`-th instance of a `size`-sized window: the
/// display-centred position shifted along the spiral, then clamped so the
/// whole window stays within the primary display (windows past the spiral's
/// on-screen reach pile at the edge rather than disappearing).
pub fn cascaded_bounds(slot: usize, size: Size<Pixels>, cx: &App) -> WindowBounds {
    let frame = cx.primary_display().map(|d| d.bounds());
    let (dx, dy) = spiral_offset(slot);
    let w = f32::from(size.width);
    let h = f32::from(size.height);

    let (center_x, center_y) = match frame {
        Some(f) => (f32::from(f.center().x), f32::from(f.center().y)),
        // No display info (e.g. headless) — fall back to a fixed origin.
        None => (w / 2.0, h / 2.0),
    };
    let mut ox = center_x - w / 2.0 + dx;
    let mut oy = center_y - h / 2.0 + dy;

    if let Some(f) = frame {
        let fx = f32::from(f.origin.x);
        let fy = f32::from(f.origin.y);
        let fw = f32::from(f.size.width);
        let fh = f32::from(f.size.height);
        // Keep the whole window on-screen. `max(fx)`/`max(fy)` guard the
        // case where the window is larger than the display.
        ox = ox.clamp(fx, (fx + fw - w).max(fx));
        oy = oy.clamp(fy, (fy + fh - h).max(fy));
    }

    WindowBounds::Windowed(Bounds {
        origin: point(px(ox), px(oy)),
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::{ANGLE, RADIUS, spiral_offset};

    fn mag(p: (f32, f32)) -> f32 {
        (p.0 * p.0 + p.1 * p.1).sqrt()
    }

    /// Slot 0 is the centred position — no offset.
    #[test]
    fn slot_zero_is_centred() {
        assert_eq!(spiral_offset(0), (0.0, 0.0));
    }

    /// The radius is exactly `RADIUS * sqrt(slot)`, so it grows without
    /// bound but slowly (area ∝ slot) — the property that keeps a large
    /// batch evenly spread instead of racing off-screen.
    #[test]
    fn radius_follows_sqrt_law() {
        for slot in 1..50 {
            let expected = RADIUS * (slot as f32).sqrt();
            assert!((mag(spiral_offset(slot)) - expected).abs() < 0.01);
        }
    }

    /// Radius is monotonic in the slot — later windows never move closer to
    /// the centre than earlier ones.
    #[test]
    fn radius_is_monotonic() {
        for slot in 1..50 {
            assert!(mag(spiral_offset(slot + 1)) >= mag(spiral_offset(slot)));
        }
    }

    /// Consecutive windows are spread apart (not stacked) — the whole point
    /// of cascading. Each step is at least a grabbable title-bar's worth.
    #[test]
    fn consecutive_windows_are_spread() {
        for slot in 1..50 {
            let a = spiral_offset(slot);
            let b = spiral_offset(slot + 1);
            let d = ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
            assert!(d > 20.0, "slot {slot} step too small: {d}");
        }
    }

    /// It winds — the angle advances every step rather than marching in one
    /// fixed direction (the difference from a linear cascade).
    #[test]
    fn angle_advances_each_step() {
        let theta = |slot: usize| ANGLE * (slot as f32).sqrt();
        assert!(theta(2) > theta(1));
        assert!(theta(10) > theta(9));
    }
}
