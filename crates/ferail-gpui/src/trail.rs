//! Activity trail — a typed, timestamped flight recorder of what the user did:
//! folder navigations (new / back / forward) and the key commands they ran.
//!
//! This answers "where are we, where have we been, and what got us here" for
//! the Diagnostics page and the issue reporter — a bug report carries the last
//! ~256 actions so a problem can be reproduced from context.
//!
//! It is an in-memory ring buffer with no I/O, so recording is cheap and safe
//! from the UI thread (it does not violate the prime directive). It mirrors the
//! `obs` breadcrumb buffer but stores **typed** events rather than strings, so
//! the report and the in-app view can format them however they like. `obs`
//! stays as the raw stderr log; this is the structured user-action history.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Most recent actions kept. A few hundred is plenty of reproduction context
/// without unbounded growth in a long-running session.
const TRAIL_CAP: usize = 256;

/// How a navigation was initiated, so the trail reads like a browser history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKind {
    /// A fresh navigation: a click, Enter, breadcrumb, typed path, favorite.
    Go,
    Back,
    Forward,
}

impl NavKind {
    fn label(self) -> &'static str {
        match self {
            NavKind::Go => "go",
            NavKind::Back => "back",
            NavKind::Forward => "forward",
        }
    }
}

/// One recorded action.
#[derive(Clone, Debug)]
pub enum TrailEvent {
    Navigate { kind: NavKind, path: PathBuf },
    /// A command identified by its human label (e.g. "Move to Trash").
    Command { label: &'static str },
    /// A free-form note — e.g. an error surfaced to the user.
    Note(String),
}

struct Entry {
    /// Seconds since the first recorded event (≈ app start).
    at: f64,
    event: TrailEvent,
}

static TRAIL: OnceLock<Mutex<VecDeque<Entry>>> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

fn trail() -> &'static Mutex<VecDeque<Entry>> {
    TRAIL.get_or_init(|| Mutex::new(VecDeque::with_capacity(TRAIL_CAP)))
}

fn elapsed_secs() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn push(event: TrailEvent) {
    let entry = Entry {
        at: elapsed_secs(),
        event,
    };
    let Ok(mut guard) = trail().lock() else {
        return; // a poisoned trail must never take down the UI
    };
    if guard.len() == TRAIL_CAP {
        guard.pop_front();
    }
    guard.push_back(entry);
}

/// Record a navigation.
pub fn navigate(kind: NavKind, path: &Path) {
    push(TrailEvent::Navigate {
        kind,
        path: path.to_path_buf(),
    });
}

/// Record a command execution by its human label. Pass a stable `&'static str`
/// (the command's menu label) so trails read consistently.
pub fn command(label: &'static str) {
    push(TrailEvent::Command { label });
}

/// Record a free-form note (e.g. an error message shown to the user).
pub fn note(msg: impl Into<String>) {
    push(TrailEvent::Note(msg.into()));
}

/// Render the trail oldest → newest as plain-text lines, with raw paths. For
/// internal use; every *user-facing* surface (the diagnostics page, "Copy
/// report", the issue bundle) must use [`render_lines_sanitized`] so the
/// privacy toggle is honored.
pub fn render_lines() -> Vec<String> {
    render(false)
}

/// Like [`render_lines`], but redacts every navigated path to a name-free shape
/// when [`crate::redact::enabled`] is set (the default). This is what gets
/// shown, copied, and bundled — so a shared report leaks no file or folder
/// names.
pub fn render_lines_sanitized() -> Vec<String> {
    render(crate::redact::enabled())
}

/// Non-blocking sanitized snapshot for the freeze watchdog. Returning `None`
/// is preferable to waiting forever when the UI thread froze while recording
/// an action and still owns this mutex.
pub fn try_render_lines_sanitized() -> Option<Vec<String>> {
    use std::sync::TryLockError;
    let guard = match trail().try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => return None,
    };
    Some(render_entries(&guard, crate::redact::enabled()))
}

fn render(redact_paths: bool) -> Vec<String> {
    let Ok(guard) = trail().lock() else {
        return Vec::new();
    };
    render_entries(&guard, redact_paths)
}

fn render_entries(guard: &VecDeque<Entry>, redact_paths: bool) -> Vec<String> {
    guard
        .iter()
        .map(|e| {
            let body = match &e.event {
                TrailEvent::Navigate { kind, path } => {
                    let shown = if redact_paths {
                        crate::redact::redact_path(path)
                    } else {
                        path.display().to_string()
                    };
                    format!("nav/{}: {}", kind.label(), shown)
                }
                TrailEvent::Command { label } => format!("cmd: {label}"),
                TrailEvent::Note(m) => {
                    let shown = if redact_paths {
                        crate::redact::scrub_text(m)
                    } else {
                        m.clone()
                    };
                    format!("note: {shown}")
                }
            };
            format!("[+{:8.3}s] {body}", e.at)
        })
        .collect()
}

/// Number of events currently held (for the diagnostics summary line).
pub fn len() -> usize {
    trail().lock().map(|g| g.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_renders_in_order() {
        // Note: the ring buffer is process-global; this test owns the first
        // events in a fresh test binary. Use distinct paths/labels.
        navigate(NavKind::Go, Path::new("/trail-test/a"));
        command("Trail Test Command");
        navigate(NavKind::Back, Path::new("/trail-test/a"));

        let lines = render_lines();
        // Find our markers (other tests in the same binary may also record).
        let joined = lines.join("\n");
        assert!(joined.contains("nav/go: /trail-test/a"), "{joined}");
        assert!(joined.contains("cmd: Trail Test Command"), "{joined}");
        assert!(joined.contains("nav/back: /trail-test/a"), "{joined}");
        assert!(len() >= 3);
    }
}
