//! Runtime guards for the Prime Directive ("the UI must never stop",
//! docs/ARCHITECTURE.md#prime-directive). Two independent mechanisms:
//!
//! 1. **Render guard** — UI code is allowed to carry already-snapshotted
//!    display data, but resolving a node into a filesystem path is an
//!    action-boundary operation. Rendering must never do it, because real
//!    implementations may lock, touch shell state, or eventually cross a
//!    process/thread boundary. [`enter_render`] +
//!    [`assert_path_resolution_allowed`] turn a violation into a panic.
//!
//! 2. **UI-thread guard** — blocking filesystem/shell entry points
//!    (directory enumeration, volume stat, magic sniffing, …) must run on
//!    a background thread, *even when called from a semantic event
//!    handler* — a stat against a spun-down external drive or a network
//!    mount blocks for seconds, and on the UI thread that freezes every
//!    open window. The app marks its UI thread once at boot
//!    ([`mark_ui_thread`]); known-blocking functions call
//!    [`assert_off_ui_thread`], which panics in debug builds when invoked
//!    from that thread. Release builds skip the check.
//!
//! These guards exist so the Prime Directive is enforced by the program,
//! not just by prose: a violating change crashes immediately under
//! `cargo run` / `cargo test` instead of shipping a freeze.

use std::cell::Cell;
use std::sync::OnceLock;
use std::thread::ThreadId;

thread_local! {
    static RENDER_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// RAII marker used by render implementations. While this is alive, any call to
/// [`assert_path_resolution_allowed`] panics.
pub struct RenderPathGuard;

/// Mark the current thread as being inside a render pass.
#[must_use]
pub fn enter_render() -> RenderPathGuard {
    RENDER_DEPTH.with(|depth| depth.set(depth.get() + 1));
    RenderPathGuard
}

impl Drop for RenderPathGuard {
    fn drop(&mut self) {
        RENDER_DEPTH.with(|depth| {
            let current = depth.get();
            debug_assert!(current > 0, "path render guard underflow");
            depth.set(current.saturating_sub(1));
        });
    }
}

/// Panic when code resolves a filesystem path during rendering.
pub fn assert_path_resolution_allowed(operation: &str) {
    RENDER_DEPTH.with(|depth| {
        assert!(
            depth.get() == 0,
            "{operation} resolved a filesystem path while rendering; resolve paths from semantic actions/jobs and pass render-ready snapshots instead"
        );
    });
}

pub fn is_rendering() -> bool {
    RENDER_DEPTH.with(|depth| depth.get() > 0)
}

static UI_THREAD: OnceLock<ThreadId> = OnceLock::new();

/// Record the calling thread as the UI thread. Called once from the GUI
/// boot path, on the thread that will run the event loop. Idempotent;
/// never called by CLI utilities or tests, so [`assert_off_ui_thread`]
/// is a no-op there.
pub fn mark_ui_thread() {
    let _ = UI_THREAD.set(std::thread::current().id());
}

/// Whether the calling thread is the marked UI thread. `false` when no
/// thread was ever marked (CLI, tests, workers).
pub fn is_ui_thread() -> bool {
    UI_THREAD.get().copied() == Some(std::thread::current().id())
}

/// Debug-build panic when a known-blocking operation runs on the UI
/// thread. Sprinkled into filesystem/shell entry points that can stall
/// on slow media (spun-down drives, network mounts). The fix is never
/// to remove the assert — schedule the call on the background executor
/// and report back through an entity update.
#[track_caller]
pub fn assert_off_ui_thread(operation: &str) {
    debug_assert!(
        !is_ui_thread(),
        "{operation} ran on the UI thread; it can block on slow media. Schedule it on the \
         background executor and report back through an entity update \
         (Prime Directive, docs/ARCHITECTURE.md#prime-directive)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "resolved a filesystem path while rendering")]
    fn guard_panics_during_render() {
        let _guard = enter_render();
        assert_path_resolution_allowed("test");
    }

    #[test]
    fn guard_allows_outside_render() {
        assert_path_resolution_allowed("test");
    }
}
