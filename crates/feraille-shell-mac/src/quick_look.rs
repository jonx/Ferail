//! Quick Look integration.
//!
//! Two surfaces:
//!
//! - [`show`] pops a standalone Quick Look window via
//!   `/usr/bin/qlmanage -p` — same renderer as Finder's inline panel,
//!   in its own process so we don't have to thread `QLPreviewPanel`
//!   through our responder chain.
//! - [`fetch_thumbnail`] generates an RGBA thumbnail for the in-app
//!   preview pane, viewer fallback, and the file-list thumbnail cache
//!   via `QLThumbnailGenerator` — the same Quick Look pipeline Finder
//!   uses, backed by the system-wide on-disk thumbnail cache. Because
//!   the framework call is asynchronous (completion block), we bridge
//!   it to a synchronous return so callers keep running it off the UI
//!   thread per the UI-nonblocking contract.

use std::path::Path;
#[cfg(target_os = "macos")]
use std::sync::mpsc;
#[cfg(target_os = "macos")]
use std::time::Duration;

use std::process::{Command, Stdio};

/// Maximum wall-clock time to wait for Quick Look to produce a
/// representation before giving up and returning `None`. Quick Look
/// usually answers in well under a second (most hits come straight
/// from the system thumbnail cache); this backstops the cases where a
/// generator hangs (malformed media, some `.zip` / `.dmg` payloads).
#[cfg(target_os = "macos")]
const QL_TIMEOUT: Duration = Duration::from_secs(8);

// QLThumbnailGenerationRequestRepresentationType bitmask. We ask only
// for the two real-content tiers (high- and low-quality thumbnails) and
// deliberately OMIT the icon tier (`1 << 0`): when Quick Look can't
// render actual content it would otherwise return the file's generic
// type glyph, which `generateBestRepresentationForRequest:` then hands
// back as the "best" representation — indistinguishable from a real
// thumbnail to the caller. Excluding it means a file Quick Look can't
// thumbnail returns `None`, so each caller shows its own, better
// fallback (the list its tinted type icon, the viewer / preview pane a
// "No preview available" message) instead of a misleading grey glyph.
#[cfg(target_os = "macos")]
const QL_REP_LOW_QUALITY: usize = 1 << 1;
#[cfg(target_os = "macos")]
const QL_REP_THUMBNAIL: usize = 1 << 2;

/// Show Quick Look for `paths`. Multiple paths render as a strip
/// the user can step through. No-op if `paths` is empty.
pub fn show(paths: &[&Path]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut cmd = Command::new("/usr/bin/qlmanage");
    cmd.arg("-p");
    for p in paths {
        cmd.arg(p);
    }
    // qlmanage prints chatty status to stderr — mute it so the
    // launching terminal stays clean.
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    cmd.spawn()
        .map_err(|e| format!("failed to spawn qlmanage: {e}"))?;
    Ok(())
}

/// RGBA8888 thumbnail for `path` fitting a `size_px` square (longest
/// edge), aspect ratio preserved. Returns `(rgba, width, height)` on
/// success. Synchronous — the underlying Quick Look call is async, but
/// we block on its completion so callers can keep running this on a
/// worker thread.
///
/// Backed by `QLThumbnailGenerator`, which reads and writes the
/// system-wide Quick Look thumbnail cache: most calls for files the
/// user has already seen in Finder return instantly from disk rather
/// than re-rendering.
#[cfg(target_os = "macos")]
pub fn fetch_thumbnail(path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    use block2::RcBlock;
    use objc2::rc::{Allocated, Id};
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send, msg_send_id};
    use objc2_foundation::{CGSize, NSString, NSURL};

    let path_str = path.to_str()?;
    let size_f = size_px.max(1) as f64;

    // Channel carries the rasterized bytes back out of the completion
    // block. `Vec<u8>` is `Send`; the AppKit objects never cross the
    // boundary.
    let (tx, rx) = mpsc::channel::<Option<(Vec<u8>, u32, u32)>>();

    let result = unsafe {
        let ns_path = NSString::from_str(path_str);
        let url: Id<NSURL> = msg_send_id![class!(NSURL), fileURLWithPath: &*ns_path];

        let types = QL_REP_THUMBNAIL | QL_REP_LOW_QUALITY;
        let size = CGSize::new(size_f, size_f);
        // scale = 1.0: we pass the target edge directly in pixels, so
        // the request's CGSize is already in device pixels.
        let req_alloc: Allocated<AnyObject> =
            msg_send_id![class!(QLThumbnailGenerationRequest), alloc];
        let request: Id<AnyObject> = msg_send_id![
            req_alloc,
            initWithFileAtURL: &*url,
            size: size,
            scale: 1.0_f64,
            representationTypes: types,
        ];

        let generator: Id<AnyObject> =
            msg_send_id![class!(QLThumbnailGenerator), sharedGenerator];

        // Completion block runs on a Quick Look background queue.
        // Rasterize there and hand back straight RGBA.
        let block = RcBlock::new(move |thumb: *mut AnyObject, _err: *mut AnyObject| {
            if thumb.is_null() {
                let _ = tx.send(None);
                return;
            }
            let thumb_ref: &AnyObject = &*thumb;
            // `QLThumbnailRepresentation.CGImage` is the cross-version
            // accessor (older macOS lacks `nsImage`). Rasterize the
            // CGImageRef straight through Core Graphics. The return type
            // is a typed pointer so objc2's encoding check accepts it.
            let cg: CGImageRef = msg_send![thumb_ref, CGImage];
            let _ = tx.send(rasterize_cgimage(cg.0));
        });

        let _: () = msg_send![
            &*generator,
            generateBestRepresentationForRequest: &*request,
            completionHandler: &*block,
        ];

        // Block on the async completion. `request`/`generator`/`block`
        // stay alive on the stack until we return.
        rx.recv_timeout(QL_TIMEOUT).ok().flatten()
    };
    result
}

/// Newtype over a `CGImageRef` whose Objective-C type encoding is
/// `^{CGImage=}`, so `msg_send![rep, CGImage]` passes objc2's runtime
/// return-encoding check (a bare `*mut c_void` encodes as `^v` and is
/// rejected).
#[cfg(target_os = "macos")]
#[repr(transparent)]
struct CGImageRef(*mut std::ffi::c_void);

#[cfg(target_os = "macos")]
unsafe impl objc2::Encode for CGImageRef {
    const ENCODING: objc2::Encoding =
        objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGImage", &[]));
}

// kCGImageAlphaPremultipliedLast: RGBA byte order, alpha last,
// premultiplied — the only alpha layout `CGBitmapContextCreate`
// accepts for RGB. We undo the premultiply on read for straight RGBA.
#[cfg(target_os = "macos")]
const CG_ALPHA_PREMULTIPLIED_LAST: u32 = 1;

/// Draw a `CGImageRef` into an offscreen straight-RGBA8 buffer at its
/// natural pixel size (aspect already baked in by Quick Look). Pure
/// Core Graphics — thread-safe, so it runs fine on the Quick Look
/// completion queue.
#[cfg(target_os = "macos")]
unsafe fn rasterize_cgimage(cg: *mut std::ffi::c_void) -> Option<(Vec<u8>, u32, u32)> {
    use objc2_foundation::{CGPoint, CGRect, CGSize};

    if cg.is_null() {
        return None;
    }
    let w = CGImageGetWidth(cg).min(8192);
    let h = CGImageGetHeight(cg).min(8192);
    if w == 0 || h == 0 {
        return None;
    }
    let bytes_per_row = w * 4;
    let mut buf = vec![0u8; bytes_per_row * h];

    let space = CGColorSpaceCreateDeviceRGB();
    if space.is_null() {
        return None;
    }
    let ctx = CGBitmapContextCreate(
        buf.as_mut_ptr() as *mut std::ffi::c_void,
        w,
        h,
        8,
        bytes_per_row,
        space,
        CG_ALPHA_PREMULTIPLIED_LAST,
    );
    CGColorSpaceRelease(space);
    if ctx.is_null() {
        return None;
    }
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64));
    CGContextDrawImage(ctx, rect, cg);
    CGContextRelease(ctx);

    // Undo premultiplied alpha so callers (gpui compositor, PNG export)
    // see straight RGBA.
    for chunk in buf.chunks_exact_mut(4) {
        let a = chunk[3] as u32;
        if a == 0 || a == 255 {
            continue;
        }
        chunk[0] = ((chunk[0] as u32 * 255 + a / 2) / a).min(255) as u8;
        chunk[1] = ((chunk[1] as u32 * 255 + a / 2) / a).min(255) as u8;
        chunk[2] = ((chunk[2] as u32 * 255 + a / 2) / a).min(255) as u8;
    }
    Some((buf, w as u32, h as u32))
}

// Force-link the Quick Look thumbnailing framework so the
// `QLThumbnailGenerator` / `QLThumbnailGenerationRequest` classes
// resolve at runtime.
#[cfg(target_os = "macos")]
#[link(name = "QuickLookThumbnailing", kind = "framework")]
extern "C" {}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGImageGetWidth(image: *mut std::ffi::c_void) -> usize;
    fn CGImageGetHeight(image: *mut std::ffi::c_void) -> usize;
    fn CGColorSpaceCreateDeviceRGB() -> *mut std::ffi::c_void;
    fn CGColorSpaceRelease(space: *mut std::ffi::c_void);
    fn CGBitmapContextCreate(
        data: *mut std::ffi::c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *mut std::ffi::c_void,
        bitmap_info: u32,
    ) -> *mut std::ffi::c_void;
    fn CGContextDrawImage(c: *mut std::ffi::c_void, rect: objc2_foundation::CGRect, image: *mut std::ffi::c_void);
    fn CGContextRelease(c: *mut std::ffi::c_void);
}
