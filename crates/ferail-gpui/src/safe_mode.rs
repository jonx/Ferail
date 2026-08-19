//! Safe mode — launch with every optional background subsystem off.
//!
//! `--safe-mode` (or `FERAIL_SAFE_MODE=1`) is the freeze-bisection switch
//! (docs/features/FREEZE_DIAGNOSTICS.md): when a user reports the app
//! hanging, one relaunch in safe mode answers "is it the background work?"
//! in a single step. It disables, for the session:
//!
//! - the filesystem watcher (notify backend + poll fan-out),
//! - the recursive folder-size walker,
//! - thumbnails and the per-row file-detail scan (magic sniff, Finder tags),
//! - the metadata SQLite database (so favorites / Ant Trail / recents stay
//!   cold — expected, not a bug),
//! - per-navigation volume-info (statfs / NSURL) refreshes,
//! - the volume-mount, power, and system-stats watchers,
//! - the startup scratch sweep.
//!
//! The freeze watchdog itself stays **on** — safe mode exists to diagnose
//! freezes, so the diagnostic layer is exactly what must keep running.
//!
//! Process-global `AtomicBool` (mirroring `crate::redact`) so worker
//! spawn sites can consult it without a GPUI context. Set once at boot
//! from the CLI/env; the Settings toggles can still re-enable individual
//! features mid-session, which is fine — safe mode only forces the
//! *launch* state.

use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether this session was launched in safe mode.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Set the session's safe-mode state. Called once from `boot::run_gui`
/// before any background subsystem starts.
pub fn set(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// `FERAIL_SAFE_MODE` set, non-empty, and not `"0"`. The env spelling
/// exists for launches that never see a command line (Finder, a `.desktop`
/// entry, a crashing session a user can't reach flags from).
pub fn from_env() -> bool {
    std::env::var_os("FERAIL_SAFE_MODE")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off() {
        // Other tests must not flip the global; safe mode is boot-only.
        assert!(!enabled());
    }
}
