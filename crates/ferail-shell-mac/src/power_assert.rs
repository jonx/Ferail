//! Idle-sleep suppression for long-running file transfers.
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
//! The assertion is process-wide and thread-safe (no main-thread
//! requirement), so the host can take it from the transfer task.

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
    let assertion_type = NSString::from_str("PreventUserIdleSystemSleep");
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
