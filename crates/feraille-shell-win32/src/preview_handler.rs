//! IPreviewHandler-based file preview rendering.
//!
//! `IShellItemImageFactory` is great for files with a registered
//! thumbnail provider (PNG, PPTX with Office installed, MP4, etc.),
//! but lots of common types (docx, xls, rtf, …) ship only a preview
//! handler — Word, Excel, etc. install `IPreviewHandler` COM servers
//! that render the document's content into a host window. This
//! module wraps the dance so callers get an RGBA buffer back.
//!
//! v1 limitations:
//! - Tries only `CLSCTX_INPROC_SERVER`; cross-process preview handlers
//!   (which need OLE marshalling and are typically much slower) are
//!   skipped.
//! - Only `IInitializeWithFile` is attempted today; future revisions
//!   should also try `IInitializeWithStream` and `IInitializeWithItem`
//!   for handlers that refuse the file-path init.
//! - Background captured at a fixed white fill — preview handlers
//!   that paint partially-transparent content end up with white
//!   showing through.
//! - Message-pump budget of 3.5 s for `DoPreview` to render, probed
//!   every ~250 ms: the pump exits early once the capture shows
//!   non-background pixels, so fast handlers don't pay the whole
//!   budget; handlers still blank at the deadline get cut.
//!
//! Caller must run this off the UI thread (preview handlers may post
//! messages and call back into shell extensions that block).

#![cfg(windows)]

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

use windows::core::{Interface, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject,
    FillRect, GetDC, GetObjectW, ReleaseDC, SelectObject, DIBSECTION, HBRUSH,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::Shell::{
    AssocQueryStringW, IPreviewHandler, ASSOCF_INIT_DEFAULTTOSTAR, ASSOCSTR_SHELLEXTENSION,
};
use windows::Win32::UI::Shell::PropertiesSystem::IInitializeWithFile;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW,
    RegisterClassExW, TranslateMessage, CS_HREDRAW, CS_VREDRAW, HMENU, MSG, PM_REMOVE,
    PW_RENDERFULLCONTENT, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};

const HOST_CLASS_NAME: &str = "FerailleShellWin32PreviewHost\0";

fn debug() -> bool {
    std::env::var("FERAILLE_THUMB_DEBUG").is_ok()
}

/// Render the file's preview handler into an off-screen HWND and
/// capture the result as RGBA bytes. Returns `None` if no preview
/// handler is registered for the extension or the capture failed.
///
/// Preview handlers are STA-affine — they expect to be created on a
/// thread that's COM-initialized as `COINIT_APARTMENTTHREADED` and
/// that pumps its own message queue. The gpui background executor
/// is MTA, so we spawn a fresh thread per call: the new thread can
/// freely set itself STA without `RPC_E_CHANGED_MODE`, and the
/// preview handler's posted completion messages reach the same
/// queue our `pump_messages` loop is draining.
///
/// One thread per preview is wasteful but previews are spread out
/// in time (selection changes), so the overhead is negligible
/// relative to the ~hundreds-of-ms cost of the preview handler
/// itself. A future STA-thread-pool with semaphore-capped
/// concurrency (ShellBat pattern) is the proper next step.
pub(crate) fn try_capture(path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    let path = path.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel::<Option<(Vec<u8>, u32, u32)>>();
    let join = std::thread::Builder::new()
        .name("feraille-preview-sta".into())
        .spawn(move || {
            let result = try_capture_sta(&path, size_px);
            let _ = tx.send(result);
        })
        .ok()?;

    // Allow up to 6s overall — the 3.5s message-pump budget plus headroom
    // for handler startup (prevhost.exe cold-launch can take a moment).
    let result = rx.recv_timeout(std::time::Duration::from_secs(6)).ok().flatten();
    // Let the worker finish its cleanup; if it's hung past our
    // timeout it'll exit when the process does.
    drop(join);
    result
}

fn try_capture_sta(path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let clsid = lookup_handler_clsid(&ext)?;

    if debug() {
        eprintln!("preview_handler: CLSID for .{} = {:?}", ext, clsid);
    }

    unsafe {
        // Fresh thread → COM is uninitialized → STA init succeeds.
        let co_hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let we_initialized = co_hr.is_ok();
        let result = try_capture_inner(&clsid, path, size_px);
        if we_initialized {
            CoUninitialize();
        }
        result
    }
}

unsafe fn try_capture_inner(
    clsid: &GUID,
    path: &Path,
    size_px: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    // Word/Excel/PowerPoint preview handlers run out-of-proc in
    // prevhost.exe; PDF previewers vary. Try in-proc first (fast),
    // then local server.
    let handler: IPreviewHandler = match CoCreateInstance::<_, IPreviewHandler>(
        clsid,
        None,
        CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER,
    ) {
            Ok(h) => h,
            Err(e) => {
                if debug() {
                    eprintln!("CoCreateInstance failed: {e:?}");
                }
                return None;
            }
        };

        if let Err(e) = init_with_file(&handler, path) {
            if debug() {
                eprintln!("preview_handler: init failed: {e:?}");
            }
            return None;
        }

        let hwnd = create_host_window(size_px)?;
        let rect = RECT {
            left: 0,
            top: 0,
            right: size_px as i32,
            bottom: size_px as i32,
        };

        // Fill the host's client area with white so preview handlers
        // that don't paint a background don't leave transparent /
        // garbage pixels.
        let white_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00FFFFFF));
        let dc = GetDC(hwnd);
        let _ = FillRect(dc, &rect, white_brush);
        ReleaseDC(hwnd, dc);
        let _ = DeleteObject(white_brush);

        if let Err(e) = handler.SetWindow(hwnd, &rect) {
            if debug() {
                eprintln!("SetWindow failed: {e:?}");
            }
            let _ = handler.Unload();
            let _ = DestroyWindow(hwnd);
            return None;
        }
        let _ = handler.SetRect(&rect);

        if let Err(e) = handler.DoPreview() {
            if debug() {
                eprintln!("DoPreview failed: {e:?}");
            }
            let _ = handler.Unload();
            let _ = DestroyWindow(hwnd);
            return None;
        }

    // Pump messages so async-rendering handlers can complete. Some
    // handlers (PDF, Excel) post async work and need extra time —
    // large PDFs can take a second or two on cold starts. Cap at 3.5s
    // so a broken handler doesn't hang the worker (the outer
    // try_capture timeout is 6s) — but don't burn the whole budget
    // blindly: every ~250ms of pumping, capture the host window and
    // stop as soon as non-background pixels appear. When content
    // first shows up, pump one extra slice and re-capture so
    // progressive renderers (Office via prevhost paints chrome before
    // body content) get a settle pass before we take the final frame.
    const PUMP_BUDGET: Duration = Duration::from_millis(3500);
    const PROBE_EVERY: Duration = Duration::from_millis(250);
    let started = Instant::now();
    let deadline = started + PUMP_BUDGET;
    let rgba = loop {
        let slice_end = (Instant::now() + PROBE_EVERY).min(deadline);
        pump_messages_until(slice_end);
        let shot = capture_window(hwnd, size_px, size_px);
        let painted = shot
            .as_ref()
            .is_some_and(|(px, _, _)| has_non_background_pixels(px));
        if painted {
            if debug() {
                eprintln!(
                    "preview_handler: content after {:?} (budget {:?})",
                    started.elapsed(),
                    PUMP_BUDGET
                );
            }
            // Settle pass, then the final frame; keep the probe shot
            // if the re-capture fails under GDI pressure.
            pump_messages_until((Instant::now() + PROBE_EVERY).min(deadline));
            break capture_window(hwnd, size_px, size_px).or(shot);
        }
        if Instant::now() >= deadline {
            // Budget exhausted with no visible content — return the
            // last capture anyway (a handler may legitimately render
            // an all-white page).
            if debug() {
                eprintln!("preview_handler: no content within {:?}", PUMP_BUDGET);
            }
            break shot;
        }
    };
    let _ = handler.Unload();
    let _ = DestroyWindow(hwnd);
    rgba
}

/// True once a captured frame holds "real" content: more than a
/// handful of pixels differing from the white background the host
/// pre-fills (see the FillRect calls in [`try_capture_inner`] and
/// [`capture_window`]). The small threshold ignores stray one-pixel
/// artifacts while still triggering on the first line of rendered
/// text or chrome. Alpha is ignored — `capture_window` already
/// normalizes the all-alpha-zero case to opaque.
fn has_non_background_pixels(rgba: &[u8]) -> bool {
    const THRESHOLD: usize = 32;
    let mut n = 0usize;
    for px in rgba.chunks_exact(4) {
        if px[0] != 0xFF || px[1] != 0xFF || px[2] != 0xFF {
            n += 1;
            if n >= THRESHOLD {
                return true;
            }
        }
    }
    false
}

fn lookup_handler_clsid(ext_lower: &str) -> Option<GUID> {
    // IPreviewHandler IID — the shell exposes it as a verb-like
    // string under the file's ProgID via AssocQueryString.
    const IPREVIEW_HANDLER_IID_STR: &str = "{8895b1c6-b41f-4c1c-a562-0d564250836f}";

    let ext_with_dot = format!(".{}", ext_lower);
    let ext_w: Vec<u16> = ext_with_dot
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let iid_w: Vec<u16> = IPREVIEW_HANDLER_IID_STR
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut buf = [0u16; 64];
    let mut len = buf.len() as u32;

    unsafe {
        let hr = AssocQueryStringW(
            ASSOCF_INIT_DEFAULTTOSTAR,
            ASSOCSTR_SHELLEXTENSION,
            PCWSTR::from_raw(ext_w.as_ptr()),
            PCWSTR::from_raw(iid_w.as_ptr()),
            PWSTR::from_raw(buf.as_mut_ptr()),
            &mut len,
        );
        if hr.is_err() || len <= 1 {
            return None;
        }
    }

    // `len` includes the null terminator; trim it. AssocQueryString
    // returns the CLSID with surrounding `{}` braces (and possibly
    // mixed case) — `GUID::try_from` in windows-0.58 wants a bare
    // hex format without braces.
    let raw = String::from_utf16_lossy(&buf[..(len as usize - 1)]);
    let cleaned = raw.trim_matches(|c| c == '{' || c == '}');
    if debug() {
        eprintln!("preview_handler: clsid = {} (raw={:?})", cleaned, raw);
    }
    // GUID::try_from panics on bad input rather than returning Err in
    // windows-0.58. Catch the panic so a malformed registry value
    // doesn't kill the worker thread.
    std::panic::catch_unwind(|| GUID::try_from(cleaned).ok())
        .ok()
        .flatten()
}

unsafe fn init_with_file(handler: &IPreviewHandler, path: &Path) -> windows::core::Result<()> {
    let init: IInitializeWithFile = handler.cast()?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    init.Initialize(PCWSTR::from_raw(wide.as_ptr()), 0)
}

unsafe fn create_host_window(size_px: u32) -> Option<HWND> {
    use std::sync::OnceLock;
    use windows::Win32::Foundation::HINSTANCE;

    // Register the window class exactly once per process. Win32
    // forbids re-registering a class with the same name+HINSTANCE,
    // and the previous attempt-then-fallback logic was returning
    // None on the second preview-handler invocation because
    // `GetClassInfoExW` was unreliable across STA threads.
    static CLASS_REGISTERED: OnceLock<()> = OnceLock::new();
    static CLASS_NAME: OnceLock<Vec<u16>> = OnceLock::new();
    let class_name_w = CLASS_NAME.get_or_init(|| HOST_CLASS_NAME.encode_utf16().collect());

    CLASS_REGISTERED.get_or_init(|| {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(host_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: HINSTANCE::default(),
            hIcon: Default::default(),
            hCursor: Default::default(),
            hbrBackground: HBRUSH::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: PCWSTR::from_raw(class_name_w.as_ptr()),
            hIconSm: Default::default(),
        };
        let atom = RegisterClassExW(&wc);
        if debug() && atom == 0 {
            eprintln!(
                "RegisterClassExW returned 0 (likely already registered, atom={})",
                atom
            );
        }
    });

    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        PCWSTR::from_raw(class_name_w.as_ptr()),
        PCWSTR::null(),
        WS_POPUP | WS_VISIBLE,
        // Position well off-screen so the host window isn't visible
        // while it hosts the preview handler.
        -32000,
        -32000,
        size_px as i32,
        size_px as i32,
        HWND::default(),
        HMENU::default(),
        HINSTANCE::default(),
        None,
    )
    .ok()?;

    Some(hwnd)
}

unsafe extern "system" fn host_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe fn pump_messages_until(deadline: Instant) {
    let mut msg = MSG::default();
    while Instant::now() < deadline {
        // Non-blocking peek + dispatch
        if PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        } else {
            // No messages — short sleep to yield to other threads
            // (in particular, the preview handler's worker threads
            // posting completion).
            std::thread::sleep(Duration::from_millis(8));
        }
    }
}

unsafe fn capture_window(hwnd: HWND, w: u32, h: u32) -> Option<(Vec<u8>, u32, u32)> {
    // Skip the compatible-bitmap intermediate — create a top-down
    // DIB section straight away, select it into the memory DC, and
    // let PrintWindow paint directly into it. The intermediate
    // CompatibleBitmap path was double-transferring and somewhere in
    // the GDI driver chain the result came out flipped 180° for
    // out-of-proc preview handlers (PowerPoint, Excel). Going
    // straight to a DIB section avoids the question.
    // Check both DCs — under GDI handle pressure either call can
    // fail (null DC), and every downstream call would then fail
    // silently with no way to diagnose. capture.rs already does
    // this; mirror it.
    let screen_dc = GetDC(HWND::default());
    if screen_dc.is_invalid() {
        if debug() {
            eprintln!("capture_window: GetDC failed");
        }
        return None;
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    if mem_dc.is_invalid() {
        if debug() {
            eprintln!("capture_window: CreateCompatibleDC failed");
        }
        ReleaseDC(HWND::default(), screen_dc);
        return None;
    }

    let dib_info = make_top_down_dib(w, h);
    let mut dib_bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let dib = match windows::Win32::Graphics::Gdi::CreateDIBSection(
        screen_dc,
        &dib_info,
        windows::Win32::Graphics::Gdi::DIB_RGB_COLORS,
        &mut dib_bits,
        windows::Win32::Foundation::HANDLE::default(),
        0,
    ) {
        Ok(d) => d,
        Err(_) => {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(HWND::default(), screen_dc);
            return None;
        }
    };
    let old_obj = SelectObject(mem_dc, dib);

    // Fill the DIB with white before the handler paints, so handlers
    // that under-paint leave clean white instead of garbage.
    let white_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00FFFFFF));
    let rect = windows::Win32::Foundation::RECT {
        left: 0,
        top: 0,
        right: w as i32,
        bottom: h as i32,
    };
    let _ = FillRect(mem_dc, &rect, white_brush);
    let _ = DeleteObject(white_brush);

    // PW_RENDERFULLCONTENT pulls modern (DirectComposition-backed)
    // surfaces; required for Word, Excel, PowerPoint, Edge PDF.
    let pw_ok = PrintWindow(hwnd, mem_dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool();
    if debug() {
        eprintln!("PrintWindow ok={}", pw_ok);
    }

    // Read the DIB bits.
    let mut ds = DIBSECTION::default();
    let nb = GetObjectW(
        dib,
        std::mem::size_of::<DIBSECTION>() as i32,
        Some(&mut ds as *mut _ as *mut _),
    );
    let rgba = if nb != 0 && !ds.dsBm.bmBits.is_null() {
        let stride = ds.dsBm.bmWidthBytes as usize;
        let row_bytes = (w as usize) * 4;
        let src = ds.dsBm.bmBits as *const u8;
        // Empirically: GDI / preview-handler `PrintWindow` output is
        // top-down regardless of the biHeight sign reported on the
        // DIB. Walk in source order. (Same fix as `lib.rs`'s
        // IShellItemImageFactory path — biHeight is unreliable here.)
        let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
        for y in 0..(h as usize) {
            std::ptr::copy_nonoverlapping(
                src.add(y * stride),
                pixels.as_mut_ptr().add(y * row_bytes),
                row_bytes,
            );
        }
        // BGRA → RGBA (preview.rs swaps back to BGRA for gpui).
        // PrintWindow's output may have alpha=0; force opaque if so.
        let all_alpha_zero = pixels.chunks_exact(4).all(|px| px[3] == 0);
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
            if all_alpha_zero {
                px[3] = 0xFF;
            }
        }
        Some((pixels, w, h))
    } else {
        None
    };

    SelectObject(mem_dc, old_obj);
    let _ = DeleteObject(dib);
    let _ = DeleteDC(mem_dc);
    ReleaseDC(HWND::default(), screen_dc);

    rgba
}

fn make_top_down_dib(w: u32, h: u32) -> windows::Win32::Graphics::Gdi::BITMAPINFO {
    use windows::Win32::Graphics::Gdi::{BITMAPINFO, BITMAPINFOHEADER, BI_RGB};
    let mut bi = BITMAPINFO::default();
    bi.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w as i32,
        biHeight: -(h as i32), // top-down
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0 as u32,
        ..Default::default()
    };
    bi
}
