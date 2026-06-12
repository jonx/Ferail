//! Live system-theme observer.
//!
//! Subscribes to the global `AppleInterfaceThemeChangedNotification`
//! delivered by `NSDistributedNotificationCenter` whenever the user
//! flips macOS Appearance (System Settings → Appearance, or the
//! "Auto" mode crossing the day/night boundary). The host registers
//! a callback once at startup; on every flip we re-query
//! [`crate::system_is_dark`] and hand the new value to the callback.
//!
//! The callback runs on the main thread (the run loop the observer
//! was registered on, which is the AppKit main loop). Hosts typically
//! close over an `EventLoopProxy` and dispatch a Feraille
//! `AppEvent::SystemThemeChanged` so the rest of the work happens
//! in the normal event-handling path.

use std::cell::RefCell;

use objc2::declare_class;
use objc2::msg_send_id;
use objc2::mutability;
use objc2::rc::Retained;
use objc2::sel;
use objc2::{ClassType, DeclaredClass};
use objc2_foundation::{
    MainThreadMarker, NSDistributedNotificationCenter, NSNotification, NSObject, NSObjectProtocol,
    NSString,
};

/// Host-supplied theme-change callback; `true` = Dark mode active.
type ThemeCallback = Box<dyn Fn(bool) + 'static>;

thread_local! {
    /// Owned reference to our notification target. Like `AppMenuTarget`,
    /// the notification center keeps observers as unsafe pointers, so
    /// we have to stash a Retained handle ourselves to keep the
    /// instance alive for the life of the app.
    static THEME_OBSERVER: RefCell<Option<Retained<ThemeObserver>>> =
        const { RefCell::new(None) };

    /// Host-supplied callback. `bool` argument is `true` when the
    /// system is now in Dark mode.
    static THEME_CALLBACK: RefCell<Option<ThemeCallback>> =
        const { RefCell::new(None) };
}

declare_class!(
    /// Tiny Objective-C target that holds a single
    /// `themeChanged:` selector. Owned reference is kept in
    /// [`THEME_OBSERVER`] so AppKit's weak reference doesn't dangle.
    pub struct ThemeObserver;

    unsafe impl ClassType for ThemeObserver {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "FeraThemeObserver";
    }

    impl DeclaredClass for ThemeObserver {
        type Ivars = ();
    }

    unsafe impl ThemeObserver {
        #[method(themeChanged:)]
        fn theme_changed(&self, _note: &NSNotification) {
            // `system_is_dark` reads NSApp.effectiveAppearance which
            // is updated *before* this notification fires, so the
            // re-query reflects the new state.
            let dark = crate::system_is_dark();
            THEME_CALLBACK.with(|cell| {
                if let Some(cb) = cell.borrow().as_ref() {
                    cb(dark);
                }
            });
        }
    }
);

unsafe impl NSObjectProtocol for ThemeObserver {}

impl ThemeObserver {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let alloc = mtm.alloc::<Self>().set_ivars(());
        unsafe { msg_send_id![super(alloc), init] }
    }
}

/// Begin observing macOS Appearance changes. Replaces any prior
/// callback. Must run on the main thread; off-thread calls are a
/// no-op. Idempotent: re-registering refreshes the callback without
/// stacking duplicate observers.
pub fn start(callback: Box<dyn Fn(bool) + 'static>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    THEME_CALLBACK.with(|cell| *cell.borrow_mut() = Some(callback));

    // Install the observer once. Repeat calls just refresh the
    // callback above and short-circuit here.
    let already_installed = THEME_OBSERVER.with(|cell| cell.borrow().is_some());
    if already_installed {
        return;
    }

    let observer = ThemeObserver::new(mtm);
    THEME_OBSERVER.with(|cell| *cell.borrow_mut() = Some(observer.clone()));

    unsafe {
        let center = NSDistributedNotificationCenter::defaultCenter();
        let name = NSString::from_str("AppleInterfaceThemeChangedNotification");
        // NSNotificationName is a typealias for NSString.
        let observer_ref: &objc2::runtime::AnyObject = &observer;
        center.addObserver_selector_name_object(
            observer_ref,
            sel!(themeChanged:),
            Some(&name),
            None,
        );
    }
}
