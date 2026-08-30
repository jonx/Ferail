//! Platform-neutral power/session transitions.
//!
//! The host subscribes once at startup (via the platform shell's
//! `start_power_observer`) and reacts: pause video playback and the
//! slideshow when the machine or its displays go to sleep, and refresh
//! volume / directory state when it wakes. The actual OS hooks live in
//! the platform crates ([mac] `NSWorkspace` notifications; [win]
//! `WM_POWERBROADCAST`); this enum is the only vocabulary they share.

/// A coarse power/session transition reported by the platform shell.
///
/// Display sleep (`ScreensDidSleep`) fires far more often than true
/// system sleep (`WillSleep`): a user walking away long enough for the
/// display to dim is the common case. Both warrant pausing playback;
/// only a true `DidWake` warrants re-listing volumes (a drive may have
/// been unplugged while the lid was closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    /// The system is about to sleep (lid close, idle sleep, or the
    /// Apple-menu / Start-menu Sleep command).
    WillSleep,
    /// The system has woken from sleep.
    DidWake,
    /// The displays went to sleep (idle dim). The system is still awake.
    ScreensDidSleep,
    /// The displays woke. The system was already awake.
    ScreensDidWake,
}

impl PowerEvent {
    /// True for transitions into a low-power state: the cue to pause
    /// video and the slideshow timer.
    pub fn is_sleep(self) -> bool {
        matches!(self, PowerEvent::WillSleep | PowerEvent::ScreensDidSleep)
    }

    /// True for a return to full system power. Distinct from a mere
    /// display wake (`ScreensDidWake`), which doesn't warrant the
    /// volume / directory refresh that a real `DidWake` does.
    pub fn is_system_wake(self) -> bool {
        matches!(self, PowerEvent::DidWake)
    }
}
