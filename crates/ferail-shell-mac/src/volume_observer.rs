//! Live volume mount/unmount observer.
//!
//! Subscribes to `NSWorkspace`'s notification center for
//! `NSWorkspaceDidMountNotification`, `…DidUnmountNotification`, and
//! `…DidRenameVolumeNotification`. The host registers one callback at
//! startup; we invoke it (no payload: the host re-lists volumes,
//! which is O(mounted volumes) of cached NSURL keys) on every change.
//!
//! Same shape and lifecycle rules as [`crate::theme_observer`]: the
//! callback runs on the main thread (NSWorkspace's center delivers on
//! the registering run loop), registration is main-thread-only and
//! idempotent.

use std::cell::RefCell;

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

/// Host-supplied volumes-changed callback. Carries no payload: mount
/// metadata is cheap to re-list wholesale and a diff would just be
/// re-derived state.
type VolumeCallback = Box<dyn Fn() + 'static>;

thread_local! {
    /// Owned reference to the notification target (the center keeps
    /// only an unsafe pointer; see THEME_OBSERVER).
    static VOLUME_OBSERVER: RefCell<Option<Retained<VolumeObserver>>> =
        const { RefCell::new(None) };

    static VOLUME_CALLBACK: RefCell<Option<VolumeCallback>> =
        const { RefCell::new(None) };
}

declare_class!(
    /// Objective-C target holding the single `volumesChanged:`
    /// selector all three notifications route to.
    pub struct VolumeObserver;

    unsafe impl ClassType for VolumeObserver {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "FeraVolumeObserver";
    }

    impl DeclaredClass for VolumeObserver {
        type Ivars = ();
    }

    unsafe impl VolumeObserver {
        #[method(volumesChanged:)]
        fn volumes_changed(&self, _note: &NSNotification) {
            VOLUME_CALLBACK.with(|cell| {
                if let Some(cb) = cell.borrow().as_ref() {
                    cb();
                }
            });
        }
    }
);

unsafe impl NSObjectProtocol for VolumeObserver {}

impl VolumeObserver {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let alloc = mtm.alloc::<Self>().set_ivars(());
        unsafe { msg_send_id![super(alloc), init] }
    }
}

/// Begin observing volume mount/unmount/rename. Replaces any prior
/// callback. Must run on the main thread; off-thread calls are a
/// no-op. Idempotent: re-registering refreshes the callback without
/// stacking observers.
pub fn start(callback: Box<dyn Fn() + 'static>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    VOLUME_CALLBACK.with(|cell| *cell.borrow_mut() = Some(callback));

    let already_installed = VOLUME_OBSERVER.with(|cell| cell.borrow().is_some());
    if already_installed {
        return;
    }

    let observer = VolumeObserver::new(mtm);
    VOLUME_OBSERVER.with(|cell| *cell.borrow_mut() = Some(observer.clone()));

    unsafe {
        // Mount notifications post to NSWorkspace's own center, not
        // the default or distributed one.
        let workspace = NSWorkspace::sharedWorkspace();
        let center: Retained<NSNotificationCenter> = msg_send_id![&*workspace, notificationCenter];
        let observer_ref: &objc2::runtime::AnyObject = &observer;
        for name in [
            "NSWorkspaceDidMountNotification",
            "NSWorkspaceDidUnmountNotification",
            "NSWorkspaceDidRenameVolumeNotification",
        ] {
            let name = NSString::from_str(name);
            center.addObserver_selector_name_object(
                observer_ref,
                sel!(volumesChanged:),
                Some(&name),
                None,
            );
        }
    }
}
