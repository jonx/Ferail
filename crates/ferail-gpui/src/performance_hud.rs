//! Explicit opt-in for the per-window GPUI performance HUD.
//!
//! The flag is session-only and disabled by default. `--performance-hud` or
//! `FERAIL_PERFORMANCE_HUD=1` seeds newly opened windows; the command palette
//! can then toggle each window independently.

use std::sync::atomic::{AtomicBool, Ordering};

static START_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_start_enabled(enabled: bool) {
    START_ENABLED.store(enabled, Ordering::Release);
}

pub fn start_enabled() -> bool {
    START_ENABLED.load(Ordering::Acquire)
}

pub fn from_env() -> bool {
    std::env::var_os("FERAIL_PERFORMANCE_HUD")
        .is_some_and(|value| !value.is_empty() && value != "0")
}
