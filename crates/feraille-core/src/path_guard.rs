//! Runtime guard for the NodeId -> path boundary.
//!
//! UI code is allowed to carry already-snapshotted display data, but resolving a
//! node into a filesystem path is an action-boundary operation. Rendering must
//! never do it, because real implementations may lock, touch shell state, or
//! eventually cross a process/thread boundary.

use std::cell::Cell;

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
