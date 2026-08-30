//! Live system sleep / wake observer.
//!
//! Subscribes to `NSWorkspace`'s notification center for the four
//! power transitions we care about and forwards each to the host as a
//! [`PowerEvent`]:
//!
//! | Notification                          | PowerEvent        |
//! |---------------------------------------|-------------------|
//! | `NSWorkspaceWillSleepNotification`    | `WillSleep`       |
//! | `NSWorkspaceDidWakeNotification`      | `DidWake`         |
//! | `NSWorkspaceScreensDidSleepNotification` | `ScreensDidSleep` |
//! | `NSWorkspaceScreensDidWakeNotification`  | `ScreensDidWake`  |
//!
//! Same shape and lifecycle rules as [`crate::volume_observer`]: these
//! post to NSWorkspace's *own* center (not the default or distributed
//! one), the callback runs on the main thread, and registration is
//! main-thread-only and idempotent.
//!
//! `WillSleep` is delivered synchronously before the machine sleeps:
//! the callback must be cheap and non-blocking (the host just pokes a
//! channel). Don't do real work here; the system is waiting on us.

use std::cell::RefCell;

use ferail_core::power::PowerEvent;
use objc2::declare_class;
use objc2::msg_send_id;
use objc2::mutability;
use objc2::rc::Retained;
use objc2::sel;
use objc2::{ClassType, DeclaredClass};
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSNotificationCenter, NSObject, NSObjectProtocol, NSString,
};

/// Host-supplied power-transition callback.
type PowerCallback = Box<dyn Fn(PowerEvent) + 'static>;

thread_local! {
    /// Owned reference to the notification target (the center keeps
    /// only an unsafe pointer; see VOLUME_OBSERVER).
    static POWER_OBSERVER: RefCell<Option<Retained<PowerObserver>>> =
        const { RefCell::new(None) };

    static POWER_CALLBACK: RefCell<Option<PowerCallback>> =
        const { RefCell::new(None) };
}

declare_class!(
    /// Objective-C target. Each of the four selectors maps one
    /// notification to its `PowerEvent`, so the dispatch is fixed at
    /// registration and the handler needs no name lookup.
    pub struct PowerObserver;

    unsafe impl ClassType for PowerObserver {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "FeraPowerObserver";
    }

    impl DeclaredClass for PowerObserver {
        type Ivars = ();
    }

    unsafe impl PowerObserver {
        #[method(willSleep:)]
        fn will_sleep(&self, _note: &NSNotification) {
            fire(PowerEvent::WillSleep);
        }
        #[method(didWake:)]
        fn did_wake(&self, _note: &NSNotification) {
            fire(PowerEvent::DidWake);
        }
        #[method(screensDidSleep:)]
        fn screens_did_sleep(&self, _note: &NSNotification) {
            fire(PowerEvent::ScreensDidSleep);
        }
        #[method(screensDidWake:)]
        fn screens_did_wake(&self, _note: &NSNotification) {
            fire(PowerEvent::ScreensDidWake);
        }
    }
);

unsafe impl NSObjectProtocol for PowerObserver {}

fn fire(event: PowerEvent) {
    POWER_CALLBACK.with(|cell| {
        if let Some(cb) = cell.borrow().as_ref() {
            cb(event);
        }
    });
}

impl PowerObserver {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let alloc = mtm.alloc::<Self>().set_ivars(());
        unsafe { msg_send_id![super(alloc), init] }
    }
}

/// Begin observing system sleep/wake and display sleep/wake. Replaces
/// any prior callback. Must run on the main thread; off-thread calls
/// are a no-op. Idempotent: re-registering refreshes the callback
/// without stacking observers.
pub fn start(callback: Box<dyn Fn(PowerEvent) + 'static>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    POWER_CALLBACK.with(|cell| *cell.borrow_mut() = Some(callback));

    let already_installed = POWER_OBSERVER.with(|cell| cell.borrow().is_some());
    if already_installed {
        return;
    }

    let observer = PowerObserver::new(mtm);
    POWER_OBSERVER.with(|cell| *cell.borrow_mut() = Some(observer.clone()));

    unsafe {
        // Sleep/wake notifications post to NSWorkspace's own center,
        // not the default or distributed one.
        let workspace = NSWorkspace::sharedWorkspace();
        let center: Retained<NSNotificationCenter> = msg_send_id![&*workspace, notificationCenter];
        let observer_ref: &objc2::runtime::AnyObject = &observer;
        for (name, selector) in [
            ("NSWorkspaceWillSleepNotification", sel!(willSleep:)),
            ("NSWorkspaceDidWakeNotification", sel!(didWake:)),
            (
                "NSWorkspaceScreensDidSleepNotification",
                sel!(screensDidSleep:),
            ),
            (
                "NSWorkspaceScreensDidWakeNotification",
                sel!(screensDidWake:),
            ),
        ] {
            let name = NSString::from_str(name);
            center.addObserver_selector_name_object(observer_ref, selector, Some(&name), None);
        }
    }
}
