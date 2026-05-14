//! ProgressStrip — debounced footer progress indicator.
//!
//! Lives at the boundary between the file pane and the status bar. When
//! a long-running task is in flight, the strip shows either a sliding
//! "comet" (indeterminate) or a fill bar (determinate). When idle, it
//! paints nothing — the existing 1-DIP separator remains untouched.
//!
//! The strip debounces on start: tasks that complete within
//! `DEBOUNCE` never become visible, avoiding a flicker on fast paths.
//! On completion it fades out smoothly so the user notices "done" but
//! isn't distracted by a hard pop.
//!
//! Ported from Ferail's `d2d_statusbar.rs` ProgressTask machine; this
//! version is renderer-agnostic and tokenized.
//!
//! Multiple concurrent tasks: the strip tracks a single active task at
//! a time. Starting a second task while the first is active replaces
//! the visual; the first task's `complete()` is then a no-op (its id
//! is stale). A future iteration could stack tasks; iter-5.5 keeps it
//! single-slot to match Ferail's behavior.
//!
//! Animation is self-driven: `next_wakeup(now)` returns the time the
//! host should request the next redraw. Hosts typically chain
//! `request_redraw` while `next_wakeup` is `Some`.

use std::time::{Duration, Instant};

use feraille_design::Tokens;
use feraille_render::{Rect, Renderer};

/// Tasks completing inside this window are never visible.
const DEBOUNCE: Duration = Duration::from_millis(50);
/// Sliding-comet period for the indeterminate animation.
const PULSE_PERIOD: Duration = Duration::from_millis(1500);
/// Comet head width as a fraction of the strip width.
const COMET_FRACTION: f32 = 0.32;
/// Time over which the strip fades out after `complete`.
const FADE_OUT: Duration = Duration::from_millis(500);
/// Strip thickness in DIPs.
const STRIP_HEIGHT: f32 = 2.0;
/// Animation tick — the host should redraw at least this often while
/// the strip is visible. 60Hz is plenty for a 2-DIP comet.
const ANIM_TICK: Duration = Duration::from_millis(16);

/// Identifier for a single progress task. Returned by `start_*`,
/// passed to `set_progress` / `complete` / `cancel` so callers can
/// only affect the task they own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProgressTaskId(u64);

#[derive(Clone, Copy, Debug)]
enum ProgressMode {
    Indeterminate,
    Determinate(f32),
}

#[derive(Clone, Copy, Debug)]
enum ProgressState {
    Idle,
    /// Task started; will become Active after `DEBOUNCE` elapses.
    Pending {
        id: ProgressTaskId,
        mode: ProgressMode,
        started: Instant,
    },
    /// Visible. `shown_at` (debounce-completion time) is recorded for
    /// future "minimum-visible-duration" tweaks; unused today.
    Active {
        id: ProgressTaskId,
        mode: ProgressMode,
        #[allow(dead_code)]
        shown_at: Instant,
    },
    /// Completed; fading toward Idle.
    FadingOut {
        completed: Instant,
        /// The mode at the moment of completion, used to draw the
        /// freezing-frame visual during the fade.
        mode: ProgressMode,
    },
}

pub struct ProgressStrip {
    state: ProgressState,
    next_id: u64,
}

impl Default for ProgressStrip {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressStrip {
    pub fn new() -> Self {
        Self { state: ProgressState::Idle, next_id: 1 }
    }

    fn alloc_id(&mut self) -> ProgressTaskId {
        let id = ProgressTaskId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    /// Start an indeterminate task. The strip will become visible after
    /// `DEBOUNCE` if the task has not completed by then.
    pub fn start_indeterminate(&mut self) -> ProgressTaskId {
        let id = self.alloc_id();
        self.state = ProgressState::Pending {
            id,
            mode: ProgressMode::Indeterminate,
            started: Instant::now(),
        };
        id
    }

    /// Start a determinate task with an initial progress in 0.0..=1.0.
    pub fn start_determinate(&mut self, progress: f32) -> ProgressTaskId {
        let id = self.alloc_id();
        self.state = ProgressState::Pending {
            id,
            mode: ProgressMode::Determinate(progress.clamp(0.0, 1.0)),
            started: Instant::now(),
        };
        id
    }

    /// Update determinate progress for `id`. No-op for stale ids or when
    /// the active task is indeterminate.
    pub fn set_progress(&mut self, id: ProgressTaskId, progress: f32) {
        let p = progress.clamp(0.0, 1.0);
        match &mut self.state {
            ProgressState::Pending { id: cur, mode, .. }
            | ProgressState::Active { id: cur, mode, .. }
                if *cur == id =>
            {
                if let ProgressMode::Determinate(_) = mode {
                    *mode = ProgressMode::Determinate(p);
                }
            }
            _ => {}
        }
    }

    /// Mark `id` complete. If still in debounce, jump straight to Idle
    /// (no flicker). Otherwise start fading.
    pub fn complete(&mut self, id: ProgressTaskId) {
        match self.state {
            ProgressState::Pending { id: cur, .. } if cur == id => {
                self.state = ProgressState::Idle;
            }
            ProgressState::Active { id: cur, mode, .. } if cur == id => {
                self.state = ProgressState::FadingOut { completed: Instant::now(), mode };
            }
            _ => {}
        }
    }

    /// Cancel `id` immediately — no fade, no flicker.
    pub fn cancel(&mut self, id: ProgressTaskId) {
        match self.state {
            ProgressState::Pending { id: cur, .. } | ProgressState::Active { id: cur, .. }
                if cur == id =>
            {
                self.state = ProgressState::Idle;
            }
            _ => {}
        }
    }

    /// Tick the state machine: promotes Pending → Active when the
    /// debounce elapses, FadingOut → Idle when the fade finishes.
    /// Idempotent; safe to call from `paint` without affecting layout.
    fn tick(&mut self, now: Instant) {
        match self.state {
            ProgressState::Pending { id, mode, started } if now - started >= DEBOUNCE => {
                self.state = ProgressState::Active { id, mode, shown_at: started + DEBOUNCE };
            }
            ProgressState::FadingOut { completed, .. } if now - completed >= FADE_OUT => {
                self.state = ProgressState::Idle;
            }
            _ => {}
        }
    }

    /// Whether the strip is currently drawing anything (pending tasks
    /// still in debounce report `false`).
    pub fn is_visible(&self, now: Instant) -> bool {
        match self.state {
            ProgressState::Idle => false,
            ProgressState::Pending { started, .. } => now - started >= DEBOUNCE,
            ProgressState::Active { .. } | ProgressState::FadingOut { .. } => true,
        }
    }

    /// Time at which the host should next redraw to keep the animation
    /// smooth. Returns `None` when the strip is idle.
    pub fn next_wakeup(&self, now: Instant) -> Option<Instant> {
        match self.state {
            ProgressState::Idle => None,
            ProgressState::Pending { started, .. } => Some(started + DEBOUNCE),
            ProgressState::Active { .. } => Some(now + ANIM_TICK),
            ProgressState::FadingOut { completed, .. } => {
                let end = completed + FADE_OUT;
                if now >= end {
                    Some(now)
                } else {
                    Some((now + ANIM_TICK).min(end))
                }
            }
        }
    }

    pub fn paint(&mut self, rect: Rect, now: Instant, tokens: &Tokens, painter: &mut dyn Renderer) {
        self.tick(now);
        if !self.is_visible(now) {
            return;
        }
        if rect.size.width <= 0.0 || rect.size.height < STRIP_HEIGHT - 0.5 {
            return;
        }
        // Place the strip at the top of `rect`, occupying STRIP_HEIGHT.
        let band = Rect::new(rect.left(), rect.top(), rect.size.width, STRIP_HEIGHT);
        // Background under the strip — same as separator.
        painter.fill_rect(band, tokens.border.subtle);

        let (mode, fade_alpha) = match self.state {
            ProgressState::Active { mode, .. } => (mode, 1.0_f32),
            ProgressState::FadingOut { completed, mode } => {
                let t = (now - completed).as_secs_f32() / FADE_OUT.as_secs_f32();
                (mode, (1.0 - t).clamp(0.0, 1.0))
            }
            _ => return,
        };

        let accent = tokens.accent.fill;
        let alpha = (accent.a as f32 * fade_alpha) as u8;
        let color = feraille_design::Color { r: accent.r, g: accent.g, b: accent.b, a: alpha };

        match mode {
            ProgressMode::Determinate(p) => {
                let w = band.size.width * p.clamp(0.0, 1.0);
                if w > 0.0 {
                    painter.fill_rect(Rect::new(band.left(), band.top(), w, band.size.height), color);
                }
            }
            ProgressMode::Indeterminate => {
                // Phase 0..1 across PULSE_PERIOD; comet sweeps from
                // -COMET_FRACTION..1.0 so it enters and exits the band.
                let phase = ((now - epoch()).as_millis() as f32
                    % PULSE_PERIOD.as_millis() as f32)
                    / PULSE_PERIOD.as_millis() as f32;
                let comet_w = band.size.width * COMET_FRACTION;
                let span = band.size.width + comet_w;
                let x = band.left() - comet_w + phase * span;
                let visible_left = x.max(band.left());
                let visible_right = (x + comet_w).min(band.right());
                let w = (visible_right - visible_left).max(0.0);
                if w > 0.0 {
                    painter.fill_rect(
                        Rect::new(visible_left, band.top(), w, band.size.height),
                        color,
                    );
                }
            }
        }
    }
}

/// A monotonic anchor for the animation phase. Using a shared `LazyLock`
/// means all paints across the app phase against the same baseline,
/// which matters if multiple ProgressStrip instances ever coexist.
fn epoch() -> Instant {
    use std::sync::OnceLock;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_swallows_fast_tasks() {
        let mut s = ProgressStrip::new();
        let id = s.start_indeterminate();
        let now = Instant::now();
        // Immediately after start, the strip is invisible.
        assert!(!s.is_visible(now));
        // Complete inside debounce — never becomes visible.
        s.complete(id);
        assert!(matches!(s.state, ProgressState::Idle));
    }

    #[test]
    fn slow_task_becomes_visible_then_fades() {
        let mut s = ProgressStrip::new();
        let id = s.start_indeterminate();
        let mut now = Instant::now();
        // Past debounce: tick promotes Pending → Active.
        now += DEBOUNCE + Duration::from_millis(10);
        s.tick(now);
        assert!(matches!(s.state, ProgressState::Active { .. }));
        assert!(s.is_visible(now));
        // Complete starts FadingOut.
        s.complete(id);
        assert!(matches!(s.state, ProgressState::FadingOut { .. }));
        // After FADE_OUT, returns to Idle.
        now += FADE_OUT + Duration::from_millis(10);
        s.tick(now);
        assert!(matches!(s.state, ProgressState::Idle));
    }

    #[test]
    fn stale_complete_is_noop() {
        let mut s = ProgressStrip::new();
        let id1 = s.start_indeterminate();
        let _id2 = s.start_indeterminate(); // replaces id1
        s.complete(id1); // stale
        assert!(matches!(s.state, ProgressState::Pending { .. }));
    }

    #[test]
    fn determinate_progress_clamps() {
        let mut s = ProgressStrip::new();
        let id = s.start_determinate(0.5);
        s.set_progress(id, 2.0);
        match s.state {
            ProgressState::Pending { mode: ProgressMode::Determinate(p), .. } => {
                assert!((p - 1.0).abs() < f32::EPSILON);
            }
            _ => panic!("expected Pending Determinate"),
        }
    }
}
