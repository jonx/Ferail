//! Slideshow playback state for the viewer window.
//!
//! Pure state machine — the timer itself is a `cx.spawn` loop in
//! `window.rs`. Staleness uses the same epoch idiom as enumeration
//! cancel flags: every play/pause/manual-nav/interval change bumps
//! `epoch`, and a timer that wakes up with an older epoch simply
//! drops its tick instead of advancing under the user.

/// Default auto-advance interval.
pub const DEFAULT_INTERVAL_SECS: u64 = 3;

/// The interval steps the toolbar button cycles through.
pub const INTERVALS: &[u64] = &[2, 3, 5, 10];

pub struct Playback {
    pub playing: bool,
    pub interval_secs: u64,
    /// Monotonic staleness counter for in-flight timers.
    pub epoch: u64,
}

impl Playback {
    pub fn new(interval_secs: u64) -> Self {
        Self {
            playing: false,
            interval_secs: interval_secs.clamp(1, 60),
            epoch: 0,
        }
    }

    /// Invalidate any pending timer tick; returns the new epoch for
    /// the next timer to carry.
    pub fn bump(&mut self) -> u64 {
        self.epoch += 1;
        self.epoch
    }

    /// Next interval in the cycle (2 → 3 → 5 → 10 → 2). Unknown
    /// values (e.g. hand-edited state file) reset to the first step.
    pub fn next_interval(secs: u64) -> u64 {
        match INTERVALS.iter().position(|&s| s == secs) {
            Some(i) => INTERVALS[(i + 1) % INTERVALS.len()],
            None => INTERVALS[0],
        }
    }

    pub fn interval_label(secs: u64) -> String {
        tr!("{secs} s", secs = secs).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_cycles_and_wraps() {
        assert_eq!(Playback::next_interval(2), 3);
        assert_eq!(Playback::next_interval(3), 5);
        assert_eq!(Playback::next_interval(5), 10);
        assert_eq!(Playback::next_interval(10), 2);
    }

    #[test]
    fn unknown_interval_resets() {
        assert_eq!(Playback::next_interval(7), 2);
        assert_eq!(Playback::next_interval(0), 2);
    }

    #[test]
    fn bump_invalidates_prior_epoch() {
        let mut p = Playback::new(DEFAULT_INTERVAL_SECS);
        let first = p.bump();
        let second = p.bump();
        assert!(second > first);
    }

    #[test]
    fn new_clamps_interval() {
        assert_eq!(Playback::new(0).interval_secs, 1);
        assert_eq!(Playback::new(600).interval_secs, 60);
    }
}
