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
    MainThreadMarker, NSNotification, NSNotificationCenter, NSNumber, NSObject, NSObjectProtocol,
    NSPoint, NSRect, NSSize, NSString, NSURL,
};

#[link(name = "AVFoundation", kind = "framework")]
extern "C" {}
#[link(name = "AVKit", kind = "framework")]
extern "C" {}

/// Minimal `CMTime` mirror so we can build a zero time and seek the
/// player back to the start (loop) without pulling in CoreMedia. Layout
/// and ObjC type-encoding (`{CMTime=qiIq}`) match the system struct.
#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

// SAFETY: field layout and encoding match the system CMTime exactly.
// The Objective-C runtime reports CMTime returns with an anonymous
// struct name (`{?=qiIq}`), so the name here must be "?" to match —
// objc2's encoding check compares the name too.
unsafe impl objc2::Encode for CMTime {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct(
        "?",
        &[
            i64::ENCODING,
            i32::ENCODING,
            u32::ENCODING,
            i64::ENCODING,
        ],
    );
}

/// `kCMTimeFlags_Valid`.
const CM_TIME_FLAGS_VALID: u32 = 1;

/// Opaque `CGColor`, only so a null `setBackgroundColor:` argument types
/// as `^{CGColor=}` for objc2's encoding check. A null CGColor renders
/// transparent.
#[repr(C)]
struct CGColor {
    _private: [u8; 0],
}

// SAFETY: a pointer to CGColor encodes as `^{CGColor=}`, matching AppKit.
unsafe impl objc2::RefEncode for CGColor {
    const ENCODING_REF: objc2::Encoding =
        objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGColor", &[]));
}

/// Host callback invoked when the video plays to its end.
type EndedCallback = Box<dyn Fn() + 'static>;

struct OverlayEntry {
    /// Wrapper NSView, sized to exactly the stage and clipped to it. The
    /// rotated AVPlayerView (whose un-rotated frame is larger than the
    /// stage for 90/270) lives inside, so the wrapper's bounds-based
    /// hit-testing keeps it from swallowing clicks over the toolbar.
    wrapper: Retained<NSObject>,
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

/// Lay out the clipped `wrapper` (sized to the stage `viewport`) inside
/// `host` and the rotated `view` at the `content` rect (the zoomed /
/// panned / fit video box, in stage-relative gpui coordinates) inside
/// the wrapper. `quarter_turns` rotates the video clockwise; purely a
/// Core Animation layer transform — no re-encode, no temp file, no I/O.
///
/// The content rect carries the zoom state, so the video tracks the same
/// fit/1:1/zoom/pan as images. For the odd quarter-turns the player view
/// is the content box swapped (H x W) and centred, so a 90/270 turn
/// fills the content rect. The wrapper is clipped to the stage
/// (`masksToBounds`) and only claims the stage rect, so the (possibly
/// oversized) view can't draw into — or swallow clicks over — the
/// toolbar. Re-applied on every reposition so an AppKit relayout can't
/// drop it.
fn apply_geometry(
    wrapper: &AnyObject,
    view: &AnyObject,
    host: &NSView,
    viewport: (f64, f64, f64, f64),
    content: (f64, f64, f64, f64),
    quarter_turns: u8,
) {
    let qt = quarter_turns % 4;
    let wrapper_rect = appkit_rect(host, viewport);
    let (cx_off, cy_off, cw, ch) = content; // content is stage-relative
    // Content centre in wrapper-local (bottom-left) coordinates.
    let centre_x = cx_off + cw / 2.0;
    let centre_y = viewport.3 - cy_off - ch / 2.0;
    // Player view box in wrapper-local coordinates. Odd turns swap to
    // (H x W) so the rotation fills the content rect; even turns use it
    // directly. Centred on the content centre.
    let (bw, bh) = if qt % 2 == 1 { (ch, cw) } else { (cw, ch) };
    let view_rect = NSRect::new(
        NSPoint::new(centre_x - bw / 2.0, centre_y - bh / 2.0),
        NSSize::new(bw, bh),
    );
    unsafe {
        // A null CGColor = transparent, so the gpui poster behind the
        // overlay shows through while a (just-switched) video decodes its
        // first frame, instead of AVPlayerView's opaque black.
        let clear: *const CGColor = std::ptr::null();

        // Wrapper: stage rect, layer-backed, clipped to its bounds.
        let _: () = msg_send![wrapper, setWantsLayer: true];
        let _: () = msg_send![wrapper, setFrame: wrapper_rect];
        let wlayer: Option<Retained<NSObject>> = msg_send_id![wrapper, layer];
        if let Some(wlayer) = wlayer {
            let _: () = msg_send![&*wlayer, setMasksToBounds: true];
            let _: () = msg_send![&*wlayer, setBackgroundColor: clear];
        }

        // Player view: swapped/centred box, rotated about its centre.
        let _: () = msg_send![view, setWantsLayer: true];
        let _: () = msg_send![view, setFrame: view_rect];
        let layer: Option<Retained<NSObject>> = msg_send_id![view, layer];
        let Some(layer) = layer else { return };
        let _: () = msg_send![&*layer, setBackgroundColor: clear];
        let _: () = msg_send![&*layer, setAnchorPoint: NSPoint::new(0.5, 0.5)];
        let _: () = msg_send![&*layer, setPosition: NSPoint::new(centre_x, centre_y)];
        // Core Animation's `transform.rotation.z` is counter-clockwise
        // positive, so clockwise quarter-turns are negative radians.
        let angle: f64 = -std::f64::consts::FRAC_PI_2 * qt as f64;
        let number = NSNumber::numberWithDouble(angle);
        let key = NSString::from_str("transform.rotation.z");
        let _: () = msg_send![&*layer, setValue: &*number, forKeyPath: &*key];

        // Always hide the native AVPlayerView controls (a layer transform
        // rotates pixels but not AppKit hit-testing, so rotated native
        // controls can't be clicked); the viewer draws its own gpui
        // transport. AVPlayerViewControlsStyle::None = 0.
        let _: () = msg_send![view, setControlsStyle: 0isize];
    }
}

/// Mount an `AVPlayerView` for `path` over `frame` (gpui top-left
/// logical coordinates) inside `container_ns_view` (the gpui window's
/// content NSView from raw-window-handle) and start playback.
/// Returns the overlay handle, or 0 on failure / off-main-thread.
pub fn show(
    container_ns_view: *mut c_void,
    path: &Path,
    viewport: (f64, f64, f64, f64),
    content: (f64, f64, f64, f64),
    quarter_turns: u8,
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
    let host: &NSView = unsafe { &*(container_ns_view as *const NSView) };

    let url = unsafe { NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy())) };
    let player: Retained<NSObject> = unsafe { msg_send_id![player_cls, playerWithURL: &*url] };
    let view: Retained<NSObject> = unsafe { msg_send_id![view_cls, new] };
    // Clipped wrapper sized to the stage; the (possibly oversized when
    // rotated) player view nests inside it.
    let wrapper: Retained<NSObject> = unsafe { msg_send_id![AnyClass::get("NSView").unwrap(), new] };
    unsafe {
        let _: () = msg_send![&*view, setPlayer: &*player];
        let view_ref: &AnyObject = &view;
        let wrapper_ref: &AnyObject = &wrapper;
        let _: () = msg_send![&*wrapper, addSubview: view_ref];
        let _: () = msg_send![host, addSubview: wrapper_ref];
    }
    apply_geometry(&wrapper, &view, host, viewport, content, quarter_turns);

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
                wrapper,
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
pub fn set_frame(
    id: u64,
    viewport: (f64, f64, f64, f64),
    content: (f64, f64, f64, f64),
    quarter_turns: u8,
) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    OVERLAYS.with(|m| {
        if let Some(entry) = m.borrow().get(&id) {
            // The wrapper's superview is the gpui window content view (host).
            let host: Option<Retained<NSView>> =
                unsafe { msg_send_id![&*entry.wrapper, superview] };
            if let Some(host) = host {
                apply_geometry(
                    &entry.wrapper,
                    &entry.view,
                    &host,
                    viewport,
                    content,
                    quarter_turns,
                );
            }
        }
    });
}

/// Pause or resume a live overlay's playback. Main-thread only; stale
/// ids no-op.
pub fn set_paused(id: u64, paused: bool) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    OVERLAYS.with(|m| {
        if let Some(entry) = m.borrow().get(&id) {
            unsafe {
                if paused {
                    let _: () = msg_send![&*entry.player, pause];
                } else {
                    let _: () = msg_send![&*entry.player, play];
                }
            }
        }
    });
}

/// Seek the overlay back to the start and resume — used to loop the
/// current video. Main-thread only; stale ids no-op.
pub fn restart(id: u64) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    OVERLAYS.with(|m| {
        if let Some(entry) = m.borrow().get(&id) {
            let zero = CMTime {
                value: 0,
                timescale: 1,
                flags: CM_TIME_FLAGS_VALID,
                epoch: 0,
            };
            unsafe {
                let _: () = msg_send![&*entry.player, seekToTime: zero];
                let _: () = msg_send![&*entry.player, play];
            }
        }
    });
}

/// `CMTime` to seconds (0.0 if invalid / indefinite).
fn cmtime_secs(t: CMTime) -> f64 {
    if t.timescale != 0 && (t.flags & CM_TIME_FLAGS_VALID) != 0 {
        t.value as f64 / t.timescale as f64
    } else {
        0.0
    }
}

/// `(current, duration)` of the overlay's video in seconds. Duration is
/// 0.0 while unknown / indefinite. Main-thread only; stale ids give zeros.
pub fn time(id: u64) -> (f64, f64) {
    if MainThreadMarker::new().is_none() {
        return (0.0, 0.0);
    }
    OVERLAYS.with(|m| {
        if let Some(entry) = m.borrow().get(&id) {
            unsafe {
                let cur: CMTime = msg_send![&*entry.player, currentTime];
                let item: Option<Retained<NSObject>> = msg_send_id![&*entry.player, currentItem];
                let dur = item
                    .map(|it| {
                        let d: CMTime = msg_send![&*it, duration];
                        d
                    })
                    .unwrap_or(CMTime {
                        value: 0,
                        timescale: 0,
                        flags: 0,
                        epoch: 0,
                    });
                (cmtime_secs(cur), cmtime_secs(dur))
            }
        } else {
            (0.0, 0.0)
        }
    })
}

/// The video's intrinsic `(width, height)` in pixels, or `(0, 0)` while
/// unknown (not yet loaded) / stale id. Main-thread only.
pub fn natural_size(id: u64) -> (f64, f64) {
    if MainThreadMarker::new().is_none() {
        return (0.0, 0.0);
    }
    OVERLAYS.with(|m| {
        if let Some(entry) = m.borrow().get(&id) {
            unsafe {
                let item: Option<Retained<NSObject>> = msg_send_id![&*entry.player, currentItem];
                if let Some(item) = item {
                    let sz: NSSize = msg_send![&*item, presentationSize];
                    return (sz.width, sz.height);
                }
            }
        }
        (0.0, 0.0)
    })
}

/// Seek the overlay to `seconds`. Main-thread only; stale ids no-op.
pub fn seek(id: u64, seconds: f64) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    OVERLAYS.with(|m| {
        if let Some(entry) = m.borrow().get(&id) {
            const TS: i32 = 600;
            let t = CMTime {
                value: (seconds.max(0.0) * TS as f64).round() as i64,
                timescale: TS,
                flags: CM_TIME_FLAGS_VALID,
                epoch: 0,
            };
            unsafe {
                let _: () = msg_send![&*entry.player, seekToTime: t];
            }
        }
    });
}

/// Step the overlay's video by `frames` frames (negative = backward).
/// Stepping pauses playback (AVFoundation behaviour). Main-thread only.
pub fn step(id: u64, frames: i64) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    OVERLAYS.with(|m| {
        if let Some(entry) = m.borrow().get(&id) {
            unsafe {
                let item: Option<Retained<NSObject>> = msg_send_id![&*entry.player, currentItem];
                if let Some(item) = item {
                    let _: () = msg_send![&*item, stepByCount: frames as isize];
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
            // Removing the wrapper takes the nested player view with it.
            let _: () = msg_send![&*entry.wrapper, removeFromSuperview];
        }
    }
}
