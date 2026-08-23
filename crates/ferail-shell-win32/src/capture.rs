//! Capture a window's rendered contents to an RGBA buffer via Win32
//! [`PrintWindow`] with `PW_RENDERFULLCONTENT` (0x2). Used by the
//! `--screenshot` headless harness as the Windows analog of macOS's
//! `MetalRenderer::render_to_image` — gpui_windows has no equivalent
//! method, and forking gpui would be heavy for a tool.
//!
//! `PW_RENDERFULLCONTENT` was introduced in Windows 8.1 specifically to
//! capture DirectComposition / DirectX-rendered windows that ordinary
//! `WM_PRINT` misses. gpui's Windows path uses DirectX, so plain
//! `BitBlt` from the window DC won't see anything but background.
//!
//! Caller passes the raw HWND as an `isize` (the same opaque value
//! `raw_window_handle::Win32WindowHandle::hwnd` exposes), keeping
//! `windows`-crate types out of the public surface.

#![cfg(windows)]

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

/// `PW_RENDERFULLCONTENT` — capture DirectComposition / DirectX layered
/// content as if presented. Not exported as a named constant by the
/// 0.58 `windows` crate; documented value is 0x2.
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(0x2);

/// Capture the window identified by `hwnd_raw` into an RGBA8 buffer.
/// Returns `(width, height, rgba)` on success.
///
/// `hwnd_raw` is the raw `isize` HWND value (what
/// `raw_window_handle::Win32WindowHandle::hwnd` carries). Zero is
/// rejected with `Err`.
///
/// The window must already be in the rendered state caller wants —
/// this helper does not show / activate / wait for paint. Headless
/// callers typically need to make the window visible first so its
/// DirectX swap chain actually presents at least one frame before
/// this is invoked.
pub fn capture_window_rgba(hwnd_raw: isize) -> Result<(u32, u32, Vec<u8>), String> {
    if hwnd_raw == 0 {
        return Err("capture_window_rgba: null HWND".into());
    }
    let hwnd = HWND(hwnd_raw as *mut _);

    let mut rect = windows::Win32::Foundation::RECT::default();
    unsafe {
        GetClientRect(hwnd, &mut rect).map_err(|e| format!("GetClientRect failed: {e}"))?;
    }
    let width = (rect.right - rect.left).max(1);
    let height = (rect.bottom - rect.top).max(1);
    if width <= 0 || height <= 0 {
        return Err(format!("zero-sized client rect: {width}x{height}"));
    }

    // Compatible DC + top-down 32-bit BGRA DIB section. Negative
    // height gives us row-zero at the top, matching the layout an
    // image::RgbaImage expects.
    let screen_dc = unsafe { GetDC(HWND::default()) };
    if screen_dc.is_invalid() {
        return Err("GetDC(NULL) failed".into());
    }
    let mem_dc = unsafe { CreateCompatibleDC(screen_dc) };
    if mem_dc.is_invalid() {
        unsafe {
            ReleaseDC(HWND::default(), screen_dc);
        }
        return Err("CreateCompatibleDC failed".into());
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        ..Default::default()
    };

    let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let dib: HBITMAP = unsafe {
        match CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0) {
            Ok(h) => h,
            Err(e) => {
                let _ = DeleteDC(mem_dc);
                ReleaseDC(HWND::default(), screen_dc);
                return Err(format!("CreateDIBSection failed: {e}"));
            }
        }
    };
    if bits_ptr.is_null() {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(dib.0));
            let _ = DeleteDC(mem_dc);
            ReleaseDC(HWND::default(), screen_dc);
        }
        return Err("CreateDIBSection returned null bits pointer".into());
    }

    let prev = unsafe { SelectObject(mem_dc, HGDIOBJ(dib.0)) };

    let ok = unsafe { PrintWindow(hwnd, mem_dc, PW_RENDERFULLCONTENT) };
    let captured_ok = ok.as_bool();

    // Copy DIB bytes out before tearing down the GDI objects.
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    if captured_ok {
        let stride = (width * 4) as usize;
        let buf_len = stride * height as usize;
        // Safety: CreateDIBSection populated bits_ptr with a buffer of
        // `width * |height| * 4` bytes; we read exactly that.
        unsafe {
            let slice = std::slice::from_raw_parts(bits_ptr as *const u8, buf_len);
            rgba.extend_from_slice(slice);
        }
        // BGRA -> RGBA in place.
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }

    // Tear-down (errors during cleanup just leak GDI handles; not fatal).
    unsafe {
        SelectObject(mem_dc, prev);
        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(mem_dc);
        ReleaseDC(HWND::default(), screen_dc);
    }

    if !captured_ok {
        return Err("PrintWindow returned FALSE".into());
    }
    Ok((width as u32, height as u32, rgba))
}

/// Move a window far off-screen, strip it from the taskbar / Alt-Tab, and show
/// it **without activating**, so its DirectComposition swap chain presents at
/// least one frame for [`capture_window_rgba`] to read back.
///
/// This is the fallback the headless `--screenshot` harness needs on a stock
/// `gpui_windows`, which has no `render_to_image` (docs/GPUI-UPSTREAM.md item
/// 7): the harness creates the window with `show: false`, and a never-shown
/// window's swap chain never presents, so PrintWindow would capture nothing.
/// The window IS technically shown here — but at (-32000, -32000), with no
/// taskbar button and no focus steal, so nothing is visible to the user.
///
/// Best-effort — failures are silent, and the caller reports the eventual
/// capture error instead.
pub fn present_offscreen_for_capture(hwnd_raw: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, ShowWindow, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOWNOACTIVATE,
    };
    if hwnd_raw == 0 {
        return;
    }
    let hwnd = HWND(hwnd_raw as *mut _);
    // Order matters: ex-style first (so the taskbar never sees it), then the
    // off-screen move, then the show.
    hide_window_for_capture(hwnd_raw);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            OFFSCREEN_ORIGIN,
            OFFSCREEN_ORIGIN,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
}

/// Far enough off every plausible virtual-desktop arrangement that the window
/// cannot appear on any monitor, while staying inside the 16-bit coordinate
/// range legacy `SetWindowPos` paths still assume.
const OFFSCREEN_ORIGIN: i32 = -32000;

/// Make a window invisible to the user while keeping it *shown* (so its
/// DirectComposition swap chain still presents a frame for
/// [`capture_window_rgba`]). Removes its taskbar / Alt-Tab button
/// (`WS_EX_TOOLWINDOW`, clearing `WS_EX_APPWINDOW`) and marks it no-activate so
/// it can't steal focus. Positioning is the caller's job — see
/// [`present_offscreen_for_capture`] for the full sequence.
/// Best-effort — failures are silent.
pub fn hide_window_for_capture(hwnd_raw: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW,
    };
    if hwnd_raw == 0 {
        return;
    }
    let hwnd = HWND(hwnd_raw as *mut _);
    unsafe {
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let toolwindow = WS_EX_TOOLWINDOW.0 as isize;
        let noactivate = WS_EX_NOACTIVATE.0 as isize;
        let appwindow = WS_EX_APPWINDOW.0 as isize;
        let updated = (current | toolwindow | noactivate) & !appwindow;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, updated);
        // The taskbar only re-reads the ex-style on a frame change.
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}
