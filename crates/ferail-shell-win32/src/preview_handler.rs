//! IPreviewHandler-based file preview rendering, brokered off-process.
//!
//! `IShellItemImageFactory` is great for files with a registered
//! thumbnail provider (PNG, PPTX with Office installed, MP4, etc.),
//! but lots of common types (docx, xls, rtf, …) ship only a preview
//! handler: Word, Excel, etc. install `IPreviewHandler` COM servers
//! that render the document's content into a host window. This
//! module wraps the dance so callers get an RGBA buffer back.
//!
//! **Preview pane only.** A preview handler is a live, interactive
//! viewer: what it paints includes its own chrome: Word's scrollbar,
//! Excel's grid, a toolbar, and a static capture of that reads as a
//! screenshot of an application, not a thumbnail of a document.
//! Explorer never uses `IPreviewHandler` for thumbnails, and neither
//! does Ferail: only the preview pane's fetch (`fetch_preview_image`)
//! reaches this module, and only after the shell thumbnail and the
//! native PDF renderer (`pdf_render`) both came up empty. The proper
//! long-term shape for the pane is Explorer's: the handler hosted
//! *live* in a child window over the pane, which is tracked in
//! `TODO.md`; this capture is the interim.
//!
//! **Containment (WIN-002).** Third-party preview handlers are
//! arbitrary native code; the 0.6.5 tester crash was a `c0000005`
//! inside `pdfprevhndlr.dll` hosted *in* Ferail. A thread cannot
//! contain that: it provides scheduling isolation, not memory-safety
//! or termination isolation. So the parent never activates a handler
//! in-process: [`try_capture`] resolves the provider CLSID, then
//! re-launches the Ferail binary as a disposable `--preview-broker`
//! child which does the COM hosting and writes one validated frame to
//! stdout ([`crate::broker_proto`]). A crash or hang kills only the
//! child: the deadline here terminates the process (never leaving a
//! detached thread behind), and a CLSID that repeatedly crashes or
//! times out is session-quarantined so the caller degrades to
//! `IShellItemImageFactory` / icon fallback instead of a crash loop.
//!
//! Inside the broker the handler is activated in-process first. That is
//! deliberate: the helper is already the disposable crash boundary, and
//! owning the provider in that process means its six-second deadline can
//! actually terminate the faulty code. Local-server activation is retained
//! only as a compatibility fallback for providers that expose no in-proc
//! class. Initialization tries
//! `IInitializeWithFile`, then `IInitializeWithStream`, then
//! `IInitializeWithItem` (the only one `.msg` and some others accept).
//!
//! Known limitations (broker side):
//! - Background captured at a fixed white fill: preview handlers
//!   that paint partially-transparent content end up with white
//!   showing through.
//! - Message-pump budget of 3.5 s for `DoPreview` to render, probed
//!   every ~250 ms: the pump exits early once the capture shows
//!   non-background pixels, so fast handlers don't pay the whole
//!   budget; handlers still blank at the deadline get cut. There is no
//!   reliable "finished rendering" signal from a preview handler, so
//!   a slow one can be captured mid-paint.
//!
//! Caller must run this off the UI thread (process spawn + up to six
//! seconds of waiting).

#![cfg(windows)]

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::broker_proto::{self, Quarantine};

use windows::core::{Interface, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject, FillRect, GetDC, GetObjectW,
    ReleaseDC, SelectObject, DIBSECTION, HBRUSH,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::{
    CoCreateInstance, IStream, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, STGM_READ,
    STGM_SHARE_DENY_WRITE,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IInitializeWithFile, IInitializeWithStream};
use windows::Win32::UI::Shell::{
    AssocQueryStringW, IInitializeWithItem, IPreviewHandler, IShellItem,
    SHCreateItemFromParsingName, SHCreateStreamOnFileEx, ASSOCF_INIT_DEFAULTTOSTAR,
    ASSOCSTR_SHELLEXTENSION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW,
    RegisterClassExW, TranslateMessage, CS_HREDRAW, CS_VREDRAW, HMENU, MSG, PM_REMOVE,
    PW_RENDERFULLCONTENT, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};

const HOST_CLASS_NAME: &str = "FerailShellWin32PreviewHost\0";

fn debug() -> bool {
    std::env::var("FERAIL_THUMB_DEBUG").is_ok()
}

/// Session quarantine for provider CLSIDs, shared by every preview
/// worker in this process.
fn quarantine() -> &'static std::sync::Mutex<Quarantine> {
    static Q: std::sync::OnceLock<std::sync::Mutex<Quarantine>> = std::sync::OnceLock::new();
    Q.get_or_init(|| std::sync::Mutex::new(Quarantine::default()))
}

/// Record a broker failure for `clsid`; logs the quarantine transition
/// exactly once per session per provider (redaction-safe: a CLSID and a
/// failure kind, never a path).
fn strike(clsid: &str, why: &str) {
    let newly = quarantine().lock().unwrap().note_failure(clsid);
    if newly {
        eprintln!(
            "preview-broker: quarantining preview handler {{{clsid}}} for this session ({why})"
        );
    } else if debug() {
        eprintln!("preview-broker: {{{clsid}}} {why}");
    }
}

/// Render the file's preview through a disposable `--preview-broker`
/// child process and return the captured RGBA frame. Returns `None`
/// if no preview handler is registered for the extension, the provider
/// is quarantined, or the broker failed/crashed/timed out.
///
/// The child owns COM activation, the STA message pump, and the
/// capture. The 6 s deadline here: the broker's 3.5 s pump budget
/// plus headroom for process and handler startup: is enforced by
/// terminating the child, so a hung provider never leaves detached
/// work behind in this process.
pub(crate) fn try_capture(
    path: &Path,
    size_px: u32,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Option<(Vec<u8>, u32, u32)> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};

    ferail_core::path_guard::assert_off_ui_thread("preview_handler::try_capture");

    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let clsid = lookup_handler_clsid(&ext)?;
    if debug() {
        eprintln!("preview_handler: CLSID for .{} = {{{clsid}}}", ext);
    }
    if quarantine().lock().unwrap().is_quarantined(&clsid) {
        if debug() {
            eprintln!("preview_handler: {{{clsid}}} is quarantined: icon fallback");
        }
        return None;
    }

    let exe = std::env::current_exe().ok()?;
    let mut cmd = Command::new(exe);
    cmd.arg("--preview-broker")
        .arg(path)
        .arg(size_px.to_string())
        .arg(&clsid)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(if debug() {
            Stdio::inherit()
        } else {
            Stdio::null()
        });
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().ok()?;

    // Drain stdout concurrently: the frame can be several MB, far
    // beyond the pipe buffer, so reading after exit would deadlock a
    // healthy child. The reader always terminates: child exit or our
    // kill() closes the pipe. `take` caps a misbehaving child at just
    // over the largest legal frame instead of ballooning parent memory.
    let mut stdout = child.stdout.take()?;
    let requested_frame_cap = 16u64
        .checked_add(
            u64::from(size_px)
                .checked_mul(u64::from(size_px))?
                .checked_mul(4)?,
        )?
        .checked_add(1)?;
    let reader = std::thread::Builder::new()
        .name("ferail-preview-broker-read".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout
                .by_ref()
                .take(requested_frame_cap)
                .read_to_end(&mut buf);
            buf
        });
    let reader = match reader {
        Ok(r) => r,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };

    const DEADLINE: Duration = Duration::from_secs(6);
    let started = Instant::now();
    let mut canceled = false;
    let status = loop {
        if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
            canceled = true;
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < DEADLINE => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => break None,
        }
    };
    let timed_out = status.is_none() && !canceled;
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let output = reader.join().unwrap_or_default();

    match status.and_then(|s| s.code()) {
        Some(broker_proto::EXIT_OK) => {
            // Validate before trusting anything from a process that
            // just hosted arbitrary native code: exact frame shape,
            // exact requested dimensions.
            match broker_proto::parse_frame(&output) {
                Some((rgba, w, h)) if w == size_px && h == size_px => {
                    quarantine().lock().unwrap().note_success(&clsid);
                    Some((rgba, w, h))
                }
                _ => {
                    strike(&clsid, "malformed frame");
                    None
                }
            }
        }
        // Clean "no preview available", not the provider's fault.
        Some(broker_proto::EXIT_NO_PREVIEW) => None,
        // Argument-contract bug on our side; don't punish the provider.
        Some(broker_proto::EXIT_USAGE) => None,
        // Timeout, crash, or a Windows exception code (0xC0000005 &
        // friends): the containment case.
        _ if canceled => None,
        _ => {
            strike(&clsid, if timed_out { "timeout" } else { "crash" });
            None
        }
    }
}

/// Entry point of the `--preview-broker` worker mode: render one
/// preview in this (disposable) process and write the frame to stdout.
/// Argument contract: `<path> <size_px> <clsid-without-braces>`.
///
/// `FERAIL_PREVIEW_BROKER_TEST=crash|av|hang` forces the containment
/// failure modes so the acceptance matrix (WTEST-046/047) can verify
/// that a broken provider terminates only this helper: `crash` aborts,
/// `av` raises a genuine access violation (exercising the minidump
/// filter), `hang` never returns.
pub fn preview_broker_main(args: &[String]) -> i32 {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

    // The broker's stdout is an inherited pipe back to the parent. Clear its
    // inherit bit before loading arbitrary in-proc provider code: otherwise a
    // provider-spawned descendant could keep the write end alive after this
    // broker is killed and strand the parent's reader join forever.
    make_stdout_non_inheritable();

    match std::env::var("FERAIL_PREVIEW_BROKER_TEST").as_deref() {
        Ok("crash") => std::process::abort(),
        Ok("av") => {
            // black_box keeps the optimizer from proving the pointer null
            // and folding the store into a trap instruction; we want the
            // real 0xC0000005 the tester saw.
            let p = std::hint::black_box(0usize) as *mut u32;
            unsafe { p.write_volatile(1) };
        }
        Ok("hang") => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
        _ => {}
    }

    let [path, size, clsid] = args else {
        eprintln!("usage: --preview-broker <path> <size_px> <clsid>");
        return broker_proto::EXIT_USAGE;
    };
    let Ok(size_px) = size.parse::<u32>() else {
        return broker_proto::EXIT_USAGE;
    };
    if size_px == 0 || size_px > broker_proto::MAX_DIM {
        return broker_proto::EXIT_USAGE;
    }
    let Some(clsid) = parse_clsid(clsid) else {
        return broker_proto::EXIT_USAGE;
    };
    let path = std::path::PathBuf::from(path);

    let result = unsafe {
        // Fresh process → COM is uninitialized → STA init succeeds,
        // and the preview handler's posted completion messages reach
        // the queue `pump_messages_until` drains.
        let co_hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let we_initialized = co_hr.is_ok();
        let result = try_capture_inner(&clsid, &path, size_px);
        if we_initialized {
            CoUninitialize();
        }
        result
    };

    match result {
        Some((rgba, w, h)) => {
            use std::io::Write as _;
            let frame = broker_proto::encode_frame(w, h, &rgba);
            let mut stdout = std::io::stdout().lock();
            if stdout
                .write_all(&frame)
                .and_then(|()| stdout.flush())
                .is_err()
            {
                // Parent went away or the pipe broke: report a clean
                // miss so a partial frame is never mistaken for output.
                return broker_proto::EXIT_NO_PREVIEW;
            }
            broker_proto::EXIT_OK
        }
        None => broker_proto::EXIT_NO_PREVIEW,
    }
}

fn make_stdout_non_inheritable() {
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::{
        SetHandleInformation, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT,
    };

    let stdout = std::io::stdout();
    let handle = HANDLE(stdout.as_raw_handle());
    // Best effort: failure only loses the extra inheritance hardening. The
    // parent still owns the six-second process deadline.
    let _ = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) };
}

unsafe fn try_capture_inner(
    clsid: &GUID,
    path: &Path,
    size_px: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    // In-proc first: this process is the disposable containment boundary.
    // Loading the provider here gives the parent deterministic ownership,
    // killing the broker also kills the hung/crashed DLL. Asking COM for a
    // local server first can move the fault into SCM-owned prevhost.exe, whose
    // lifetime the parent cannot bound. Retain LOCAL_SERVER only for unusual
    // providers that expose no in-proc class.
    let handler: IPreviewHandler = match CoCreateInstance::<_, IPreviewHandler>(
        clsid,
        None,
        CLSCTX_INPROC_SERVER,
    ) {
        Ok(h) => {
            if debug() {
                eprintln!("preview_handler: activated in disposable broker");
            }
            h
        }
        Err(inproc_err) => {
            if debug() {
                eprintln!(
                    "preview_handler: in-proc activation failed: {inproc_err:?}, trying local server"
                );
            }
            match CoCreateInstance::<_, IPreviewHandler>(clsid, None, CLSCTX_LOCAL_SERVER) {
                Ok(h) => h,
                Err(e) => {
                    if debug() {
                        eprintln!("CoCreateInstance failed: {e:?}");
                    }
                    return None;
                }
            }
        }
    };

    if let Err(e) = init_handler(&handler, path) {
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
    // handlers (PDF, Excel) post async work and need extra time:
    // large PDFs can take a second or two on cold starts. Cap at 3.5s
    // so a broken handler doesn't hang the worker (the outer
    // try_capture timeout is 6s), but don't burn the whole budget
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
            // Budget exhausted with no visible content: return the
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
/// text or chrome. Alpha is ignored: `capture_window` already
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

/// Resolve the preview-handler CLSID registered for an extension.
/// Returns the brace-less string form: the parent keys the quarantine
/// on it and passes it to the broker verbatim, so both sides agree on
/// the identity byte-for-byte.
fn lookup_handler_clsid(ext_lower: &str) -> Option<String> {
    // IPreviewHandler IID: the shell exposes it as a verb-like
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
    // mixed case): `GUID::try_from` in windows-0.58 wants a bare
    // hex format without braces.
    let raw = String::from_utf16_lossy(&buf[..(len as usize - 1)]);
    let cleaned = raw.trim_matches(|c| c == '{' || c == '}').to_string();
    if debug() {
        eprintln!("preview_handler: clsid = {} (raw={:?})", cleaned, raw);
    }
    // Validate up front so a bad registry value is dropped here rather
    // than surfacing as a broker usage error.
    parse_clsid(&cleaned)?;
    Some(cleaned)
}

/// Parse a brace-less CLSID string. windows-0.58 only offers the
/// *infallible* `GUID: From<&str>`, which panics on a malformed string
/// (the `TryFrom` that used to be called here was just the blanket impl
/// over it, with `Error = Infallible`, so it could never actually
/// report the failure it appeared to). Catching the panic is therefore
/// the only way a bad value doesn't kill the process.
fn parse_clsid(s: &str) -> Option<GUID> {
    std::panic::catch_unwind(|| GUID::from(s)).ok()
}

/// Hand the file to the handler through whichever initialization
/// interface it implements: `IInitializeWithFile` (the common one, and
/// the one this pipeline has always shipped with), then
/// `IInitializeWithStream` (Microsoft's recommended one; a few handlers
/// accept nothing else), then `IInitializeWithItem` (`.msg` and other
/// item-bound previewers). Each stage is only attempted when the
/// handler exposes the interface; the last error wins.
unsafe fn init_handler(handler: &IPreviewHandler, path: &Path) -> windows::core::Result<()> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let pcw = PCWSTR::from_raw(wide.as_ptr());

    let mut last_err = windows::core::Error::from(windows::Win32::Foundation::E_NOINTERFACE);
    if let Ok(init) = handler.cast::<IInitializeWithFile>() {
        match init.Initialize(pcw, STGM_READ.0) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if debug() {
                    eprintln!("preview_handler: IInitializeWithFile failed: {e:?}");
                }
                last_err = e;
            }
        }
    }
    if let Ok(init) = handler.cast::<IInitializeWithStream>() {
        match SHCreateStreamOnFileEx(pcw, (STGM_READ | STGM_SHARE_DENY_WRITE).0, 0, false, None)
            .and_then(|stream: IStream| init.Initialize(&stream, STGM_READ.0))
        {
            Ok(()) => return Ok(()),
            Err(e) => {
                if debug() {
                    eprintln!("preview_handler: IInitializeWithStream failed: {e:?}");
                }
                last_err = e;
            }
        }
    }
    if let Ok(init) = handler.cast::<IInitializeWithItem>() {
        match SHCreateItemFromParsingName::<_, _, IShellItem>(pcw, None)
            .and_then(|item| init.Initialize(&item, STGM_READ.0))
        {
            Ok(()) => return Ok(()),
            Err(e) => {
                if debug() {
                    eprintln!("preview_handler: IInitializeWithItem failed: {e:?}");
                }
                last_err = e;
            }
        }
    }
    Err(last_err)
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
            // No messages: short sleep to yield to other threads
            // (in particular, the preview handler's worker threads
            // posting completion).
            std::thread::sleep(Duration::from_millis(8));
        }
    }
}

unsafe fn capture_window(hwnd: HWND, w: u32, h: u32) -> Option<(Vec<u8>, u32, u32)> {
    // Skip the compatible-bitmap intermediate: create a top-down
    // DIB section straight away, select it into the memory DC, and
    // let PrintWindow paint directly into it. The intermediate
    // CompatibleBitmap path was double-transferring and somewhere in
    // the GDI driver chain the result came out flipped 180° for
    // out-of-proc preview handlers (PowerPoint, Excel). Going
    // straight to a DIB section avoids the question.
    // Check both DCs, under GDI handle pressure either call can
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
        // IShellItemImageFactory path: biHeight is unreliable here.)
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
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}
