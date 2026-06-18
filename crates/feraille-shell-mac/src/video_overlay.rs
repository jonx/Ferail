//! Native video frame source for the viewer
//! (docs/features/VIEWER.md, "Video playback in the slideshow").
//!
//! gpui has no video element, but it does have an image element backed
//! by `RenderImage`. So rather than float a native `AVPlayerView` NSView
//! over the gpui window (the old design — which forced a clipped wrapper,
//! a Core Animation rotation transform, hidden native controls, and a
//! transparent-layer hack against AVPlayerView's black), we drive a
//! headless `AVPlayer` and pull decoded frames out of an
//! `AVPlayerItemVideoOutput` as BGRA pixel buffers. The gpui host uploads
//! each frame as a `RenderImage` and draws it through the very same
//! stage/zoom/pan/rotate path as still images — so the video rect is a
//! real gpui element: it composites in-tree, the transport buttons and
//! seek bar hit-test correctly, and rotation is the image path.
//!
//! The public functions keep the `video_overlay_*` names from the old
//! overlay design (callers in feraille-gpui), but there is no overlay
//! NSView anymore — `AVPlayer` is windowless and `copy_frame` is the
//! display surface.
//!
//! AVKit/AVFoundation classes are reached through runtime lookup
//! (`AnyClass::get` + `msg_send`) rather than typed bindings: the
//! objc2 0.2-generation framework crates this workspace uses predate
//! the AVFoundation bindings (those start at objc2 0.6). The `#[link]`
//! blocks below make the frameworks load so the lookups resolve and the
//! CoreVideo `CVPixelBuffer*` C functions link.
//!
//! Threading: every function here is main-thread-only (the registry is
//! a `thread_local`), like the rest of this crate's window-touching
//! surface. Off-thread calls are no-ops. The end-of-playback callback
//! fires on the main thread via `NSNotificationCenter`; it MUST NOT call
//! back into this module's API synchronously (the registry borrow is
//! held) — the gpui host forwards it through a channel, which is the
//! supported shape.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{ClassType, DeclaredClass, declare_class, msg_send, msg_send_id, mutability, sel};
use objc2_foundation::{
    MainThreadMarker, NSDictionary, NSNotification, NSNotificationCenter, NSNumber, NSObject,
    NSObjectProtocol, NSSize, NSString, NSURL,
};

#[link(name = "AVFoundation", kind = "framework")]
extern "C" {}
#[link(name = "CoreVideo", kind = "framework")]
extern "C" {}

// CoreVideo pixel-buffer access (C API). A `CVPixelBufferRef` is an
// opaque pointer; `copyPixelBufferForItemTime:` hands back a +1 retained
// one that we release with `CFRelease` after copying its bytes out.
extern "C" {
    fn CVPixelBufferLockBaseAddress(pb: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pb: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pb: *mut c_void) -> *mut c_void;
    fn CVPixelBufferGetWidth(pb: *mut c_void) -> usize;
    fn CVPixelBufferGetHeight(pb: *mut c_void) -> usize;
    fn CVPixelBufferGetBytesPerRow(pb: *mut c_void) -> usize;
    fn CFRelease(cf: *const c_void);
    /// `CFStringRef` attribute key, toll-free bridged to `NSString*`.
    static kCVPixelBufferPixelFormatTypeKey: *const c_void;
}

/// Opaque `CVPixelBufferRef` (a `CVBufferRef`), only so the return of
/// `copyPixelBufferForItemTime:` types as `^{__CVBuffer=}` for objc2's
/// encoding check — a bare `*mut c_void` (`^v`) is rejected. Same dance
/// as `CGColor` in the old overlay (docs/GPUI-UPSTREAM.md #5b).
#[repr(C)]
struct CVBuffer {
    _private: [u8; 0],
}

// SAFETY: a pointer to CVBuffer encodes as `^{__CVBuffer=}`, the type the
// runtime reports for a CVPixelBufferRef return.
unsafe impl objc2::RefEncode for CVBuffer {
    const ENCODING_REF: objc2::Encoding =
        objc2::Encoding::Pointer(&objc2::Encoding::Struct("__CVBuffer", &[]));
}

/// `kCVPixelFormatType_32BGRA` — four bytes per pixel, B,G,R,A order,
/// which is exactly what gpui's `RenderImage` wants (no channel swap).
const PIXEL_FORMAT_32BGRA: u32 = 0x42475241; // 'BGRA'
/// `kCVPixelBufferLock_ReadOnly`.
const LOCK_READ_ONLY: u64 = 1;

/// Minimal `CMTime` mirror so we can build a zero time (loop), seek, and
/// pass the player's current time to the video output without pulling in
/// CoreMedia. Layout and ObjC type-encoding (`{CMTime=qiIq}`) match the
/// system struct.
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

// SAFETY: a pointer to CMTime (the `itemTimeForDisplay:` out-param) encodes
// as a pointer to the same anonymous struct the Encode impl names.
unsafe impl objc2::RefEncode for CMTime {
    const ENCODING_REF: objc2::Encoding = objc2::Encoding::Pointer(&<CMTime as objc2::Encode>::ENCODING);
}

/// `kCMTimeFlags_Valid`.
const CM_TIME_FLAGS_VALID: u32 = 1;

/// Host callback invoked when the video plays to its end.
type EndedCallback = Box<dyn Fn() + 'static>;

struct OverlayEntry {
    /// The windowless `AVPlayer` (typed as NSObject — runtime class).
    player: Retained<NSObject>,
    /// The `AVPlayerItemVideoOutput` attached to the player's item; held
    /// so it stays alive for the lifetime of frame pulls. `copy_frame`
    /// pulls BGRA pixel buffers from it.
    output: Retained<NSObject>,
    observer: Retained<VideoEndObserver>,
    on_ended: EndedCallback,
}

thread_local! {
    /// Live players by handle. Entries own every Retained object a
    /// player needs; `remove` tears the whole bundle down.
    static OVERLAYS: RefCell<HashMap<u64, OverlayEntry>> = RefCell::new(HashMap::new());
    /// Monotonic handle mint. 0 is reserved as the "failed" sentinel.
    static NEXT_OVERLAY_ID: Cell<u64> = const { Cell::new(1) };
}

declare_class!(
    /// Objective-C target for `AVPlayerItemDidPlayToEndTimeNotification`,
    /// registered with `object:` = this player's item so each observer
    /// only hears its own video end. Carries the handle in its ivars to
    /// find the host callback.
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

/// Build the `AVPlayerItemVideoOutput` pixel-buffer attributes dict
/// pinning the format to 32-bit BGRA (`{kCVPixelBufferPixelFormatTypeKey:
/// 0x42475241}`), so every frame we pull is gpui-ready with no swap.
fn bgra_attributes() -> Retained<NSDictionary<NSString, NSNumber>> {
    let format = NSNumber::numberWithUnsignedInt(PIXEL_FORMAT_32BGRA);
    // kCVPixelBufferPixelFormatTypeKey is a CFStringRef, toll-free
    // bridged to NSString*.
    let key: &NSString = unsafe { &*(kCVPixelBufferPixelFormatTypeKey as *const NSString) };
    NSDictionary::from_slice(&[key], &[&*format])
}

/// Create a windowless `AVPlayer` for `path`, attach a BGRA video
/// output, and start playback. Returns the handle, or 0 on failure /
/// off-main-thread.
pub fn show(path: &Path, on_ended: EndedCallback) -> u64 {
    let Some(mtm) = MainThreadMarker::new() else {
        return 0;
    };
    let (Some(player_cls), Some(output_cls)) = (
        AnyClass::get("AVPlayer"),
        AnyClass::get("AVPlayerItemVideoOutput"),
    ) else {
        return 0;
    };

    let url = unsafe { NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy())) };
    let player: Retained<NSObject> = unsafe { msg_send_id![player_cls, playerWithURL: &*url] };

    // Attach a BGRA video output to the player's item. Without an item
    // (e.g. a URL that failed to open) there's nothing to display.
    let item: Option<Retained<NSObject>> = unsafe { msg_send_id![&*player, currentItem] };
    let Some(item) = item else {
        return 0;
    };
    let attrs = bgra_attributes();
    let output: Retained<NSObject> = unsafe {
        let alloc: Allocated<NSObject> = msg_send_id![output_cls, alloc];
        msg_send_id![alloc, initWithPixelBufferAttributes: &*attrs]
    };
    unsafe {
        let output_ref: &AnyObject = &output;
        let _: () = msg_send![&*item, addOutput: output_ref];
    }

    let id = NEXT_OVERLAY_ID.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    let observer = VideoEndObserver::new(mtm, id);
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
        let _: () = msg_send![&*player, play];
    }

    OVERLAYS.with(|m| {
        m.borrow_mut().insert(
            id,
            OverlayEntry {
                player,
                output,
                observer,
                on_ended,
            },
        )
    });
    id
}

/// Pull the latest decoded frame as tightly-packed BGRA bytes plus its
/// `(width, height)` in pixels, or `None` when there is no new frame
/// since the last pull (the caller keeps showing the previous frame /
/// poster — which is what masks the decode latency on a video switch).
/// Main-thread only; stale ids give `None`.
pub fn copy_frame(id: u64) -> Option<(u32, u32, Vec<u8>)> {
    MainThreadMarker::new()?;
    OVERLAYS.with(|m| {
        let map = m.borrow();
        let entry = map.get(&id)?;
        unsafe {
            // Pull at the player's current presentation time.
            let item_time: CMTime = msg_send![&*entry.player, currentTime];
            let has_new: bool =
                msg_send![&*entry.output, hasNewPixelBufferForItemTime: item_time];
            if !has_new {
                return None;
            }
            let pb: *mut CVBuffer = msg_send![
                &*entry.output,
                copyPixelBufferForItemTime: item_time,
                itemTimeForDisplay: std::ptr::null_mut::<CMTime>()
            ];
            if pb.is_null() {
                return None;
            }
            let pb = pb as *mut c_void;
            let frame = copy_pixel_buffer_bgra(pb);
            CFRelease(pb);
            frame
        }
    })
}

/// Copy a locked `CVPixelBuffer`'s BGRA bytes into a tightly-packed
/// `Vec` (dropping any row padding), returning `(w, h, bytes)`. Assumes
/// the buffer is 32BGRA (we pinned the output's format). The caller owns
/// `pb` and releases it.
///
/// SAFETY: `pb` must be a valid, non-null `CVPixelBufferRef`.
unsafe fn copy_pixel_buffer_bgra(pb: *mut c_void) -> Option<(u32, u32, Vec<u8>)> {
    let w = CVPixelBufferGetWidth(pb);
    let h = CVPixelBufferGetHeight(pb);
    if w == 0 || h == 0 {
        return None;
    }
    if CVPixelBufferLockBaseAddress(pb, LOCK_READ_ONLY) != 0 {
        return None;
    }
    let base = CVPixelBufferGetBaseAddress(pb) as *const u8;
    let bytes_per_row = CVPixelBufferGetBytesPerRow(pb);
    let row_bytes = w * 4;
    let mut out = Vec::with_capacity(row_bytes * h);
    if !base.is_null() {
        for row in 0..h {
            let src = base.add(row * bytes_per_row);
            out.extend_from_slice(std::slice::from_raw_parts(src, row_bytes));
        }
    }
    let _ = CVPixelBufferUnlockBaseAddress(pb, LOCK_READ_ONLY);
    if out.len() != row_bytes * h {
        return None;
    }
    Some((w as u32, h as u32, out))
}

/// Pause or resume a live player's playback. Main-thread only; stale
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

/// Seek the player back to the start and resume — used to loop the
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

/// `(current, duration)` of the player's video in seconds. Duration is
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

/// Seek the player to `seconds`. Main-thread only; stale ids no-op.
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

/// Step the player's video by `frames` frames (negative = backward).
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

/// Stop playback and tear the player down. Safe to call with a
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
        }
        // Dropping `entry` releases the player, item-output, and observer.
    }
}
