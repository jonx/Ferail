//! Platform icon fetching. macOS goes through `NSWorkspace.iconForFile:`
//! and rasterizes the result into an RGBA8 buffer. Non-macOS platforms
//! return `None` for now (Windows shell extract lands with the Win32
//! shell crate).
//!
//! Returns straight (non-premultiplied) RGBA so the compositor's blit
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

/// Windows arm: `IShellItemImageFactory::GetImage` with
/// `SIIGBF_ICONONLY` so we get the system icon for the file's type
/// without the shell trying to render a preview / thumbnail. Result
/// is read via a DIB section (same pattern as
/// `feraille-shell-win32::fetch_quick_look_thumbnail`) so transparent
/// icon backgrounds are preserved.
///
/// Returns straight (non-premultiplied) RGBA — the shell's icon
/// pipeline gives back premultiplied BGRA, we swap channels here.
/// (For most file icons the premultiplied vs straight distinction
/// is invisible because the alpha is 0 or 255 everywhere; soft-edge
/// glyphs are the cases where it'd matter.)
#[cfg(windows)]
pub fn fetch_icon_rgba(path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{DeleteObject, GetObjectW, DIBSECTION, HBITMAP};
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY, SIIGBF_RESIZETOFIT,
    };

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);

    unsafe {
        let co_hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let we_initialized = co_hr.is_ok();

        let result = (|| -> Option<(Vec<u8>, u32, u32)> {
            let factory: IShellItemImageFactory =
                SHCreateItemFromParsingName(PCWSTR::from_raw(wide.as_ptr()), None).ok()?;
            let size = SIZE {
                cx: size_px as i32,
                cy: size_px as i32,
            };
            let hbitmap: HBITMAP = factory
                .GetImage(size, SIIGBF_ICONONLY | SIIGBF_RESIZETOFIT)
                .ok()?;

            let mut ds = DIBSECTION::default();
            let nb = GetObjectW(
                hbitmap,
                std::mem::size_of::<DIBSECTION>() as i32,
                Some(&mut ds as *mut _ as *mut _),
            );
            if nb == 0 || ds.dsBm.bmBits.is_null() || ds.dsBm.bmBitsPixel != 32 {
                let _ = DeleteObject(hbitmap);
                return None;
            }
            let w = ds.dsBm.bmWidth as u32;
            let h = ds.dsBmih.biHeight.unsigned_abs();
            let stride = ds.dsBm.bmWidthBytes as usize;
            let src = ds.dsBm.bmBits as *const u8;
            let row_bytes = (w as usize) * 4;
            // SIIGBF_ICONONLY returns bitmaps from the system icon
            // resources, which are bottom-up by Win32 ICON convention
            // (the THUMBNAILONLY pathway is the exception that gave
            // back top-down). Walk the source from last row to first
            // so the output is top-down.
            let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
            for y in 0..(h as usize) {
                let src_row = (h as usize) - 1 - y;
                std::ptr::copy_nonoverlapping(
                    src.add(src_row * stride),
                    pixels.as_mut_ptr().add(y * row_bytes),
                    row_bytes,
                );
            }
            let _ = DeleteObject(hbitmap);

            // BGRA → straight RGBA. Force alpha=255 only when alpha
            // is zero everywhere (some legacy file types' icons
            // arrive with the alpha channel zeroed).
            let all_alpha_zero = pixels.chunks_exact(4).all(|px| px[3] == 0);
            for px in pixels.chunks_exact_mut(4) {
                px.swap(0, 2);
                if all_alpha_zero {
                    px[3] = 0xFF;
                }
            }
            Some((pixels, w, h))
        })();

        if we_initialized {
            CoUninitialize();
        }
        result
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn fetch_icon_rgba(_path: &Path, _size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    None
}
