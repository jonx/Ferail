//! Idle-sleep suppression: for long-running file transfers, and for the
//! screen while the viewer plays a video or a slideshow.
//!
//! A copy/move shouldn't be interrupted by the machine idle-sleeping
//! mid-stream. Rather than *react* to `WillSleep` (we'd get only a few
//! seconds, and the engine isn't built to checkpoint a half-written
//! file), we *prevent* idle sleep for the duration of the transfer with
//! an IOKit power assertion.
//!
//! [`prevent_idle_sleep`] takes the assertion and returns an RAII
//! [`SleepBlocker`]; dropping it releases the assertion. We assert
//! `kIOPMAssertPreventUserIdleSystemSleep`, which holds off *idle*
//! system sleep but still allows the display to sleep and still honours
//! a deliberate Apple-menu → Sleep, exactly the scope a background
//! copy wants.
//!
//! [`prevent_display_sleep`] is the viewer's variant: it asserts
//! `kIOPMAssertPreventUserIdleDisplaySleep`, which also keeps the
//! display lit, the scope a video or a slideshow wants (nobody is
//! touching the keyboard, and the screen must stay on). Both guards
//! share the [`SleepBlocker`] type.
//!
//! The assertions are process-wide and thread-safe (no main-thread
//! requirement), so the host can take them from any task.

use std::ffi::c_void;

use objc2_foundation::NSString;

#[link(name = "IOKit", kind = "framework")]
extern "C" {}

// IOKit power-assertion C API. `IOReturn` is a kern_return_t (i32),
// `kIOReturnSuccess` is 0. `IOPMAssertionID` / `IOPMAssertionLevel`
// are uint32. A `CFStringRef` is toll-free bridged with `NSString*`,
// so we pass `NSString` pointers straight through.
extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: *const c_void,
        assertion_level: u32,
        assertion_name: *const c_void,
        assertion_id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(assertion_id: u32) -> i32;
}

/// `kIOPMAssertionLevelOn`.
const ASSERTION_LEVEL_ON: u32 = 255;

/// RAII guard that keeps the system awake while held. Dropping it
/// releases the underlying IOKit assertion. Created by
/// [`prevent_idle_sleep`]; on non-macOS builds it's an inert
/// zero-sized stand-in so cross-platform callers can hold the same
/// type unconditionally.
#[must_use = "dropping the blocker immediately re-allows idle sleep"]
pub struct SleepBlocker {
    id: u32,
}

impl Drop for SleepBlocker {
    fn drop(&mut self) {
        // Safe: `id` is a live assertion this guard owns; release is
        // idempotent against a valid id and we never copy the guard.
        unsafe {
            let _ = IOPMAssertionRelease(self.id);
        }
    }
}

/// Assert `PreventUserIdleSystemSleep` under the given human-readable
/// reason (shown in `pmset -g assertions`). Returns `None` if IOKit
/// declines: the caller proceeds without the guard; the worst case is
/// the OS may idle-sleep mid-copy, which is survivable, just not ideal.
pub fn prevent_idle_sleep(reason: &str) -> Option<SleepBlocker> {
    assert_power("PreventUserIdleSystemSleep", reason)
}

/// Assert `PreventUserIdleDisplaySleep` under the given reason: the
/// display stays lit and the system stays awake while the guard is
/// held. Same failure semantics as [`prevent_idle_sleep`].
pub fn prevent_display_sleep(reason: &str) -> Option<SleepBlocker> {
    assert_power("PreventUserIdleDisplaySleep", reason)
}

fn assert_power(assertion_type: &str, reason: &str) -> Option<SleepBlocker> {
    let assertion_type = NSString::from_str(assertion_type);
    let assertion_name = NSString::from_str(reason);
    let mut id: u32 = 0;
    // Safe: both strings outlive the call; IOKit copies what it needs.
    let rc = unsafe {
        IOPMAssertionCreateWithName(
            assertion_type.as_ref() as *const NSString as *const c_void,
            ASSERTION_LEVEL_ON,
            assertion_name.as_ref() as *const NSString as *const c_void,
            &mut id,
        )
    };
    if rc == 0 {
        Some(SleepBlocker { id })
    } else {
        None
    }
}
