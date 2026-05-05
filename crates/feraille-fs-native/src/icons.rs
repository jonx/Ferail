//! Platform icon fetching. macOS goes through `NSWorkspace.iconForFile:`
//! and rasterizes the result into an RGBA8 buffer. Non-macOS platforms
//! return `None` for now (Windows shell extract lands with the Win32
//! shell crate).
//!
//! Returns straight (non-premultiplied) RGBA so the soft renderer's blit
//! composes correctly.

use std::path::Path;

/// Fetch the system icon for `path` at `size_px`, rasterized to straight
/// (non-premultiplied) RGBA8.
///
/// **macOS: main-thread only.** This calls
/// `NSWorkspace.sharedWorkspace().iconForFile:`, which is not safe to invoke
/// from worker threads. Callers that want to keep the UI thread free should
/// schedule the fetch via the event loop in chunks (see
/// `App::prefetch_icons` / `IconChunkTick`), not spawn a worker.
#[cfg(target_os = "macos")]
pub fn fetch_icon_rgba(path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    use objc2::msg_send;
    use objc2::ClassType;
    use objc2_app_kit::{
        NSBitmapFormat, NSBitmapImageRep, NSCompositingOperation, NSDeviceRGBColorSpace,
        NSGraphicsContext, NSWorkspace,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    let path_str = path.to_str()?;
    let size_f = size_px as f64;

    unsafe {
        let workspace = NSWorkspace::sharedWorkspace();
        let ns_path = NSString::from_str(path_str);
        let image = workspace.iconForFile(&ns_path);
        image.setSize(NSSize::new(size_f, size_f));

        let alloc = NSBitmapImageRep::alloc();
        // NSBitmapFormat::empty() = 0 = "default", which is premultiplied
        // RGBA. NSGraphicsContext refuses non-premultiplied formats for
        // drawing — Apple's "drawing always premultiplies" rule. We undo
        // the premult on the read side below.
        let rep = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            alloc,
            std::ptr::null_mut(),
            size_px as isize,
            size_px as isize,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat::empty(),
            (size_px * 4) as isize,
            32,
        )?;

        let gc = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
        let class = NSGraphicsContext::class();
        let _: () = msg_send![class, saveGraphicsState];
        NSGraphicsContext::setCurrentContext(Some(&gc));
        image.drawInRect_fromRect_operation_fraction(
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(size_f, size_f)),
            NSRect::ZERO,
            NSCompositingOperation::Copy,
            1.0,
        );
        let _: () = msg_send![class, restoreGraphicsState];

        let data_ptr = rep.bitmapData();
        if data_ptr.is_null() {
            return None;
        }
        let buf_size = (size_px * size_px * 4) as usize;
        let mut buf = std::slice::from_raw_parts(data_ptr, buf_size).to_vec();
        // Undo premultiplied alpha so callers see straight RGBA.
        for chunk in buf.chunks_exact_mut(4) {
            let a = chunk[3] as u32;
            if a == 0 || a == 255 {
                continue;
            }
            chunk[0] = ((chunk[0] as u32 * 255 + a / 2) / a).min(255) as u8;
            chunk[1] = ((chunk[1] as u32 * 255 + a / 2) / a).min(255) as u8;
            chunk[2] = ((chunk[2] as u32 * 255 + a / 2) / a).min(255) as u8;
        }
        Some((buf, size_px, size_px))
    }
}

#[cfg(not(target_os = "macos"))]
pub fn fetch_icon_rgba(_path: &Path, _size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    None
}
