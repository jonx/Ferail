//! Native video playback overlay for the viewer window
//! (docs/features/VIEWER.md, "Video playback in the slideshow").
//!
//! gpui has no video element, so the viewer floats an `AVPlayerView`
//! as an NSView subview of the gpui window's content view, positioned
//! over the stage rect. AVKit gives us aspect-fit, hardware decode,
//! audio, and the native inline controls for free; the cost is that
//! the overlay composites above all gpui content within its frame.
//!
//! AVKit/AVFoundation classes are reached through runtime lookup
//! (`AnyClass::get` + `msg_send`) rather than typed bindings: the
//! objc2 0.2-generation framework crates this workspace uses predate
//! the AVFoundation bindings (those start at objc2 0.6). The two
//! `#[link]` blocks below make the frameworks load so the lookups
//! resolve.
//!
//! Threading: every function here is main-thread-only (AppKit), like
//! the rest of this crate's window-touching surface. Off-thread calls
//! are no-ops. The end-of-playback callback fires on the main thread
//! via `NSNotificationCenter`; it MUST NOT call back into this
//! module's API synchronously (the registry borrow is held) — the
//! gpui host forwards it through a channel, which is the supported
//! shape.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{ClassType, DeclaredClass, declare_class, msg_send, msg_send_id, mutability, sel};
use objc2_app_kit::NSView;
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSNotificationCenter, NSObject, NSObjectProtocol, NSPoint,
    NSRect, NSSize, NSString, NSURL,
};

#[link(name = "AVFoundation", kind = "framework")]
extern "C" {}
#[link(name = "AVKit", kind = "framework")]
extern "C" {}

/// Host callback invoked when the video plays to its end.
type EndedCallback = Box<dyn Fn() + 'static>;

struct OverlayEntry {
    /// The AVPlayerView (typed as NSObject — runtime-looked-up class).
    view: Retained<NSObject>,
    /// Its AVPlayer.
    player: Retained<NSObject>,
    observer: Retained<VideoEndObserver>,
    on_ended: EndedCallback,
}

thread_local! {
    /// Live overlays by handle. Entries own every Retained object an
    /// overlay needs; `remove` tears the whole bundle down.
    static OVERLAYS: RefCell<HashMap<u64, OverlayEntry>> = RefCell::new(HashMap::new());
    /// Monotonic handle mint. 0 is reserved as the "failed" sentinel.
    static NEXT_OVERLAY_ID: Cell<u64> = const { Cell::new(1) };
}

declare_class!(
    /// Objective-C target for `AVPlayerItemDidPlayToEndTimeNotification`,
    /// registered with `object:` = this overlay's player item so each
    /// observer only hears its own video end. Carries the overlay
    /// handle in its ivars to find the host callback.
    pub struct VideoEndObserver;

    unsafe impl ClassType for VideoEndObserver {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "FeraVideoEndObserver";
    }

    impl DeclaredClass for VideoEndObserver {
        type Ivars = Cell<u64>;
    }

    unsafe impl VideoEndObserver {
        #[method(itemEnded:)]
        fn item_ended(&self, _note: &NSNotification) {
            let id = self.ivars().get();
            OVERLAYS.with(|m| {
                // Borrow held across the call — see module docs: the
                // callback must defer (channel), not re-enter.
                if let Some(entry) = m.borrow().get(&id) {
                    (entry.on_ended)();
                }
            });
        }
    }
);

unsafe impl NSObjectProtocol for VideoEndObserver {}

impl VideoEndObserver {
    fn new(mtm: MainThreadMarker, overlay_id: u64) -> Retained<Self> {
        let alloc = mtm.alloc::<Self>().set_ivars(Cell::new(overlay_id));
        unsafe { msg_send_id![super(alloc), init] }
    }
}

/// gpui hands us top-left-origin logical coordinates; AppKit views
/// are bottom-left-origin unless flipped. Convert against the live
/// container bounds.
fn appkit_rect(container: &NSView, frame: (f64, f64, f64, f64)) -> NSRect {
    let (x, y, w, h) = frame;
    let flipped: bool = unsafe { msg_send![container, isFlipped] };
    let bounds: NSRect = unsafe { msg_send![container, bounds] };
    let ny = if flipped {
        y
    } else {
        bounds.size.height - y - h
    };
    NSRect::new(NSPoint::new(x, ny), NSSize::new(w, h))
}

/// Mount an `AVPlayerView` for `path` over `frame` (gpui top-left
/// logical coordinates) inside `container_ns_view` (the gpui window's
/// content NSView from raw-window-handle) and start playback.
/// Returns the overlay handle, or 0 on failure / off-main-thread.
pub fn show(
    container_ns_view: *mut c_void,
    path: &Path,
    frame: (f64, f64, f64, f64),
    on_ended: EndedCallback,
) -> u64 {
    let Some(mtm) = MainThreadMarker::new() else {
        return 0;
    };
    if container_ns_view.is_null() {
        return 0;
    }
    let (Some(player_cls), Some(view_cls)) =
        (AnyClass::get("AVPlayer"), AnyClass::get("AVPlayerView"))
    else {
        return 0;
    };
    let _ = mtm; // AppKit/AVKit class creation below is main-thread work.
    let container: &NSView = unsafe { &*(container_ns_view as *const NSView) };

    let url = unsafe { NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy())) };
    let player: Retained<NSObject> = unsafe { msg_send_id![player_cls, playerWithURL: &*url] };
    let view: Retained<NSObject> = unsafe { msg_send_id![view_cls, new] };
    let rect = appkit_rect(container, frame);
    unsafe {
        let _: () = msg_send![&*view, setFrame: rect];
        let _: () = msg_send![&*view, setPlayer: &*player];
        let view_ref: &AnyObject = &view;
        let _: () = msg_send![container, addSubview: view_ref];
    }

    let id = NEXT_OVERLAY_ID.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    let observer = VideoEndObserver::new(mtm, id);
    let item: Option<Retained<NSObject>> = unsafe { msg_send_id![&*player, currentItem] };
    if let Some(item) = item {
        unsafe {
            let center = NSNotificationCenter::defaultCenter();
            let name = NSString::from_str("AVPlayerItemDidPlayToEndTimeNotification");
            let observer_ref: &AnyObject = &observer;
            let item_ref: &AnyObject = &item;
            center.addObserver_selector_name_object(
                observer_ref,
                sel!(itemEnded:),
                Some(&name),
                Some(item_ref),
            );
        }
    }
    unsafe {
        let _: () = msg_send![&*player, play];
    }

    OVERLAYS.with(|m| {
        m.borrow_mut().insert(
            id,
            OverlayEntry {
                view,
                player,
                observer,
                on_ended,
            },
        )
    });
    id
}

/// Reposition a live overlay (window resize / fullscreen toggle).
pub fn set_frame(id: u64, frame: (f64, f64, f64, f64)) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    OVERLAYS.with(|m| {
        if let Some(entry) = m.borrow().get(&id) {
            let superview: Option<Retained<NSView>> =
                unsafe { msg_send_id![&*entry.view, superview] };
            if let Some(container) = superview {
                let rect = appkit_rect(&container, frame);
                unsafe {
                    let _: () = msg_send![&*entry.view, setFrame: rect];
                }
            }
        }
    });
}

/// Stop playback and tear the overlay down. Safe to call with a
/// stale/unknown id.
pub fn remove(id: u64) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let entry = OVERLAYS.with(|m| m.borrow_mut().remove(&id));
    if let Some(entry) = entry {
        unsafe {
            let _: () = msg_send![&*entry.player, pause];
            let center = NSNotificationCenter::defaultCenter();
            let observer_ref: &AnyObject = &entry.observer;
            center.removeObserver(observer_ref);
            let _: () = msg_send![&*entry.view, removeFromSuperview];
        }
    }
}
