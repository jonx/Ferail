//! macOS/Windows/Linux libmpv binding. Hand-written FFI (the surface is ~14
//! stable functions; the Phase 0 spike confirmed raw FFI is enough — no
//! `libmpv`/`mpv` crate dep, no extra deps at all).
//!
//! libmpv is **pull**-based: we create a software render context once and call
//! `mpv_render_context_render` to fill a BGRA buffer whenever
//! `mpv_render_context_update` reports a new frame. Colour grade, enhancement
//! filters, and the chroma key are all one live `vf` filtergraph
//! (`rebuild_vf`) — no per-change stream re-open.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use feraille_core::video::{ChromaKey, VideoAdjust, VideoBackend, VideoEnhance, VideoStream};

// ---- libmpv constants ------------------------------------------------------

// mpv_format
const MPV_FORMAT_INT64: c_int = 4;
const MPV_FORMAT_DOUBLE: c_int = 5;
// mpv_render_param_type
const PARAM_API_TYPE: c_int = 1;
const PARAM_SW_SIZE: c_int = 17;
const PARAM_SW_FORMAT: c_int = 18;
const PARAM_SW_STRIDE: c_int = 19;
const PARAM_SW_POINTER: c_int = 20;
// mpv_event_id
const EVENT_NONE: c_int = 0;
const EVENT_LOG_MESSAGE: c_int = 2;
const EVENT_END_FILE: c_int = 7;
// mpv_end_file_reason
const END_FILE_REASON_EOF: c_int = 0;
// mpv_render_context_update() flags
const RENDER_UPDATE_FRAME: u64 = 1;

#[repr(C)]
struct RenderParam {
    typ: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEventEndFile {
    reason: c_int,
    error: c_int,
}

#[repr(C)]
#[allow(dead_code)] // `level`/`log_level` exist for C layout fidelity; we read prefix+text.
struct MpvEventLogMessage {
    prefix: *const c_char,
    level: *const c_char,
    text: *const c_char,
    log_level: c_int,
}

// ---- cross-platform dynamic loader -----------------------------------------
//
// libmpv is loaded at runtime (no build-time link) so a stock build needs no
// mpv. mpv is self-contained (ffmpeg built in), so there is
// no plugin directory to point an env var at. Unix uses `dlopen`/`dlsym`;
// Windows uses `LoadLibraryW`/`GetProcAddress`.
mod dynload {
    use std::ffi::c_void;
    use std::path::Path;

    #[cfg(unix)]
    pub unsafe fn open(path: &Path) -> *mut c_void {
        use std::ffi::{c_char, c_int, CString};
        extern "C" {
            fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        }
        const RTLD_NOW: c_int = 2;
        const RTLD_GLOBAL: c_int = 8;
        match CString::new(path.to_string_lossy().as_bytes().to_vec()) {
            Ok(c) => dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL),
            Err(_) => std::ptr::null_mut(),
        }
    }

    #[cfg(unix)]
    pub unsafe fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
        use std::ffi::{c_char, CString};
        extern "C" {
            fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        }
        match CString::new(name) {
            Ok(c) => dlsym(handle, c.as_ptr()),
            Err(_) => std::ptr::null_mut(),
        }
    }

    #[cfg(windows)]
    pub unsafe fn open(path: &Path) -> *mut c_void {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn LoadLibraryW(name: *const u16) -> *mut c_void;
        }
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        LoadLibraryW(wide.as_ptr())
    }

    #[cfg(windows)]
    pub unsafe fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
        use std::ffi::{c_char, CString};
        #[link(name = "kernel32")]
        extern "system" {
            fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        }
        match CString::new(name) {
            Ok(c) => GetProcAddress(handle, c.as_ptr()),
            Err(_) => std::ptr::null_mut(),
        }
    }
}

// ---- resolved libmpv entry points -----------------------------------------

struct LibMpv {
    // Kept so the loader doesn't unload the image while we hold fn pointers.
    _lib: *mut c_void,
    create: extern "C" fn() -> *mut c_void,
    initialize: extern "C" fn(*mut c_void) -> c_int,
    terminate_destroy: extern "C" fn(*mut c_void),
    set_option_string: extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    set_property_string: extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    request_log_messages: extern "C" fn(*mut c_void, *const c_char) -> c_int,
    get_property: extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int,
    command: extern "C" fn(*mut c_void, *const *const c_char) -> c_int,
    wait_event: extern "C" fn(*mut c_void, f64) -> *mut MpvEvent,
    rc_create: extern "C" fn(*mut *mut c_void, *mut c_void, *mut RenderParam) -> c_int,
    rc_render: extern "C" fn(*mut c_void, *mut RenderParam) -> c_int,
    rc_update: extern "C" fn(*mut c_void) -> u64,
    rc_free: extern "C" fn(*mut c_void),
}

/// `dlsym` a symbol and transmute it to the named function-pointer type.
macro_rules! sym {
    ($h:expr, $name:literal, $ty:ty) => {{
        let p = dynload::sym($h, $name);
        if p.is_null() {
            return Err(format!("libmpv: missing symbol {}", $name));
        }
        std::mem::transmute::<*mut std::ffi::c_void, $ty>(p)
    }};
}

/// Resolve the libmpv shared library from the user-pointed location, falling
/// back to the platform's usual install paths. The user may point at the dylib
/// itself, a directory containing it, or (macOS) `mpv.app`.
fn resolve_lib(hint: &Path) -> Result<PathBuf, String> {
    // An explicit file wins.
    if hint.is_file() {
        return Ok(hint.to_path_buf());
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if hint.is_dir() {
        for rel in LIB_RELATIVE {
            candidates.push(hint.join(rel));
        }
    }
    candidates.extend(LIB_DEFAULTS.iter().map(PathBuf::from));
    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    // Last resort: a bare soname the loader searches for.
    Ok(PathBuf::from(LIB_SONAME))
}

#[cfg(target_os = "macos")]
const LIB_RELATIVE: &[&str] = &[
    "libmpv.dylib",
    "libmpv.2.dylib",
    "lib/libmpv.dylib",
    "lib/libmpv.2.dylib",
    "Contents/Frameworks/libmpv.2.dylib",
];
#[cfg(target_os = "macos")]
const LIB_DEFAULTS: &[&str] = &[
    "/opt/homebrew/opt/mpv/lib/libmpv.dylib",
    "/opt/homebrew/lib/libmpv.dylib",
    "/usr/local/opt/mpv/lib/libmpv.dylib",
    "/usr/local/lib/libmpv.dylib",
];
#[cfg(target_os = "macos")]
const LIB_SONAME: &str = "libmpv.2.dylib";

#[cfg(windows)]
const LIB_RELATIVE: &[&str] = &["libmpv-2.dll", "mpv-2.dll", "libmpv.dll"];
#[cfg(windows)]
const LIB_DEFAULTS: &[&str] = &[];
#[cfg(windows)]
const LIB_SONAME: &str = "libmpv-2.dll";

#[cfg(target_os = "linux")]
const LIB_RELATIVE: &[&str] = &["libmpv.so", "libmpv.so.2", "lib/libmpv.so.2"];
#[cfg(target_os = "linux")]
const LIB_DEFAULTS: &[&str] = &["/usr/lib/libmpv.so.2", "/usr/local/lib/libmpv.so.2"];
#[cfg(target_os = "linux")]
const LIB_SONAME: &str = "libmpv.so.2";

impl LibMpv {
    unsafe fn load(hint: &Path) -> Result<LibMpv, String> {
        let path = resolve_lib(hint)?;
        let lib = dynload::open(&path);
        if lib.is_null() {
            return Err(format!("loading {} failed", path.display()));
        }
        Ok(LibMpv {
            _lib: lib,
            create: sym!(lib, "mpv_create", extern "C" fn() -> *mut c_void),
            initialize: sym!(lib, "mpv_initialize", extern "C" fn(*mut c_void) -> c_int),
            terminate_destroy: sym!(lib, "mpv_terminate_destroy", extern "C" fn(*mut c_void)),
            set_option_string: sym!(
                lib,
                "mpv_set_option_string",
                extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int
            ),
            set_property_string: sym!(
                lib,
                "mpv_set_property_string",
                extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int
            ),
            request_log_messages: sym!(
                lib,
                "mpv_request_log_messages",
                extern "C" fn(*mut c_void, *const c_char) -> c_int
            ),
            get_property: sym!(
                lib,
                "mpv_get_property",
                extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int
            ),
            command: sym!(
                lib,
                "mpv_command",
                extern "C" fn(*mut c_void, *const *const c_char) -> c_int
            ),
            wait_event: sym!(
                lib,
                "mpv_wait_event",
                extern "C" fn(*mut c_void, f64) -> *mut MpvEvent
            ),
            rc_create: sym!(
                lib,
                "mpv_render_context_create",
                extern "C" fn(*mut *mut c_void, *mut c_void, *mut RenderParam) -> c_int
            ),
            rc_render: sym!(
                lib,
                "mpv_render_context_render",
                extern "C" fn(*mut c_void, *mut RenderParam) -> c_int
            ),
            rc_update: sym!(lib, "mpv_render_context_update", extern "C" fn(*mut c_void) -> u64),
            rc_free: sym!(lib, "mpv_render_context_free", extern "C" fn(*mut c_void)),
        })
    }
}

// ---- libmpv loader (the dylib is loaded once; handles are per-stream) -------

thread_local! {
    // (mpv hint path, resolved libmpv). First path wins for the session.
    static LIB: RefCell<Option<(PathBuf, Rc<LibMpv>)>> = const { RefCell::new(None) };
}

fn load_lib(hint: &Path) -> Option<Rc<LibMpv>> {
    LIB.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            match unsafe { LibMpv::load(hint) } {
                Ok(l) => *slot = Some((hint.to_path_buf(), Rc::new(l))),
                Err(e) => {
                    eprintln!("[feraille] mpv backend unavailable: {e}");
                    return None;
                }
            }
        }
        Some(slot.as_ref().unwrap().1.clone())
    })
}

/// Build an mpv [`VideoBackend`], loading libmpv from `hint` (a dylib path, a
/// directory, or `mpv.app`). Returns `None` if libmpv can't be loaded.
pub fn backend(hint: &Path) -> Option<Box<dyn VideoBackend>> {
    let lib = load_lib(hint)?;
    Some(Box::new(MpvBackend { lib }))
}

struct MpvBackend {
    lib: Rc<LibMpv>,
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

/// Rotate a tightly-packed BGRA buffer (`w`×`h`, stride `w*4`) clockwise by
/// `deg` ∈ {90, 180, 270}, returning the new `(width, height, buffer)`. 90/270
/// swap the dimensions. Any other `deg` returns a straight copy. Per-frame and
/// bounds-checked, but a portrait preview is small enough that this is cheap;
/// optimise (SIMD / tiled transpose) only if a hot path needs it.
fn rotate_bgra(src: &[u8], w: u32, h: u32, deg: u32) -> (u32, u32, Vec<u8>) {
    let (w, h) = (w as usize, h as usize);
    // For 90/270 the output is h×w; for 180 it stays w×h.
    let (nw, nh) = if deg == 180 { (w, h) } else { (h, w) };
    let mut dst = vec![0u8; nw * nh * 4];
    for sy in 0..h {
        for sx in 0..w {
            // Destination pixel for a clockwise rotation.
            let (dx, dy) = match deg {
                90 => (h - 1 - sy, sx),
                270 => (sy, w - 1 - sx),
                _ => (w - 1 - sx, h - 1 - sy), // 180
            };
            let si = (sy * w + sx) * 4;
            let di = (dy * nw + dx) * 4;
            dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    (nw as u32, nh as u32, dst)
}

impl VideoBackend for MpvBackend {
    fn open(
        &self,
        path: &Path,
        on_ended: Box<dyn Fn() + Send + 'static>,
        enhance: VideoEnhance,
    ) -> Option<Box<dyn VideoStream>> {
        let lib = self.lib.clone();
        let h = (lib.create)();
        if h.is_null() {
            return None;
        }
        // Options before initialize. No user config/OSC/OSD; hardware decode
        // with copy-back so the SW render path gets frames in system memory;
        // keep-open so a seek-after-end (the viewer's loop) still works; quiet.
        for (k, v) in [
            // CRITICAL: route video output through the render API, not a
            // native window. Without this mpv creates a macOS `gpu` (Cocoa)
            // vo with a CVDisplayLink that `dispatch_sync`s to the main thread
            // — and since we tear the stream down (and pull frames) from the
            // main thread, that deadlocks (vo_thread waits on main, main waits
            // joining vo_thread). `vo=libmpv` makes the SW render context the
            // only output: no NSWindow, no display link, no main-thread hop.
            ("vo", "libmpv"),
            ("config", "no"),
            ("osc", "no"),
            ("osd-level", "0"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            ("ytdl", "no"),
            ("hwdec", "auto-copy"),
            ("keep-open", "yes"),
            // The libmpv *software* render path asserts (mp_image_crop) on
            // rotated video: it computes the crop in rotated space and applies
            // it to the un-rotated source. So we tell mpv never to rotate, read
            // the intended rotation off `video-params/rotate`, and rotate the
            // BGRA buffer ourselves in `copy_frame`.
            ("video-rotate", "no"),
            ("msg-level", "all=error"),
        ] {
            (lib.set_option_string)(h, cstr(k).as_ptr(), cstr(v).as_ptr());
        }
        if (lib.initialize)(h) != 0 {
            (lib.terminate_destroy)(h);
            return None;
        }

        // Route mpv's own log stream to stderr so a crash (e.g. an assert deep
        // in libmpv's render path) leaves a breadcrumb — the lines just before
        // the abort name the file, decoder, hwdec, and frame geometry. Quiet by
        // default (`error`); set `FERAILLE_MPV_LOG=v` (or `debug`) to see the
        // decode/VO setup. Requested before `loadfile` so setup logs are caught.
        let log_level = std::env::var("FERAILLE_MPV_LOG").unwrap_or_else(|_| "error".into());
        (lib.request_log_messages)(h, cstr(&log_level).as_ptr());

        // Software render context.
        let api = cstr("sw");
        let mut cparams = [
            RenderParam { typ: PARAM_API_TYPE, data: api.as_ptr() as *mut c_void },
            RenderParam { typ: 0, data: std::ptr::null_mut() },
        ];
        let mut rctx: *mut c_void = std::ptr::null_mut();
        if (lib.rc_create)(&mut rctx, h, cparams.as_mut_ptr()) != 0 {
            (lib.terminate_destroy)(h);
            return None;
        }

        // Strip the Windows `\\?\` extended-length prefix the file list uses.
        let path_str = path.to_string_lossy();
        let path_str = path_str.strip_prefix(r"\\?\").unwrap_or(&path_str);
        let load = [cstr("loadfile"), cstr(path_str)];
        let argv = [load[0].as_ptr(), load[1].as_ptr(), std::ptr::null()];
        if (lib.command)(h, argv.as_ptr()) != 0 {
            (lib.rc_free)(rctx);
            (lib.terminate_destroy)(h);
            return None;
        }

        let stream = MpvStream {
            lib,
            h,
            rctx,
            on_ended,
            ended_fired: false,
            adjust: VideoAdjust::default(),
            enhance,
            key: None,
            buf: Vec::new(),
            dims: (0, 0),
        };
        // Apply the baked enhancement immediately (live — no re-open).
        stream.rebuild_vf();
        Some(Box::new(stream))
    }
}

struct MpvStream {
    lib: Rc<LibMpv>,
    h: *mut c_void,
    rctx: *mut c_void,
    on_ended: Box<dyn Fn() + Send + 'static>,
    ended_fired: bool,
    adjust: VideoAdjust,
    enhance: VideoEnhance,
    key: Option<ChromaKey>,
    buf: Vec<u8>,
    dims: (u32, u32),
}

impl MpvStream {
    /// Drain mpv's event queue (non-blocking). Fires `on_ended` once on a
    /// natural end-of-file. Cheap to call every poll tick.
    fn pump_events(&mut self) {
        loop {
            let ev = (self.lib.wait_event)(self.h, 0.0);
            if ev.is_null() {
                break;
            }
            let e = unsafe { &*ev };
            if e.event_id == EVENT_NONE {
                break;
            }
            if e.event_id == EVENT_LOG_MESSAGE && !e.data.is_null() {
                let m = unsafe { &*(e.data as *const MpvEventLogMessage) };
                // `text` already carries its trailing newline; `prefix` is the
                // emitting module (e.g. `vd`, `vo/libmpv`, `ffmpeg`).
                let prefix = unsafe { CStr::from_ptr(m.prefix) }.to_string_lossy();
                let text = unsafe { CStr::from_ptr(m.text) }.to_string_lossy();
                eprint!("[mpv/{prefix}] {text}");
                continue;
            }
            if e.event_id == EVENT_END_FILE && !e.data.is_null() {
                let ef = unsafe { &*(e.data as *const MpvEventEndFile) };
                if ef.reason == END_FILE_REASON_EOF && !self.ended_fired {
                    self.ended_fired = true;
                    (self.on_ended)();
                }
            }
        }
    }

    /// Read an int64 property (display width/height); 0 if unavailable yet.
    fn prop_i64(&self, name: &str) -> u32 {
        let mut v: i64 = 0;
        let r = (self.lib.get_property)(
            self.h,
            cstr(name).as_ptr(),
            MPV_FORMAT_INT64,
            &mut v as *mut i64 as *mut c_void,
        );
        if r == 0 && v > 0 {
            v as u32
        } else {
            0
        }
    }

    fn prop_f64(&self, name: &str) -> f64 {
        let mut v: f64 = 0.0;
        let r = (self.lib.get_property)(
            self.h,
            cstr(name).as_ptr(),
            MPV_FORMAT_DOUBLE,
            &mut v as *mut f64 as *mut c_void,
        );
        if r == 0 {
            v.max(0.0)
        } else {
            0.0
        }
    }

    /// Send one libmpv command from a slice of string arguments.
    fn command(&self, args: &[&str]) {
        let cargs: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut argv: Vec<*const c_char> = cargs.iter().map(|c| c.as_ptr()).collect();
        argv.push(std::ptr::null());
        (self.lib.command)(self.h, argv.as_ptr());
    }

    /// Compose the single live `vf` filtergraph from grade + enhancement +
    /// key. Empty when fully neutral (which clears mpv's chain). The chroma
    /// key ends the chain in `format=rgba` so SW render emits real alpha (the
    /// Phase 0 finding). Order: grade → denoise → deband → sharpen → grain →
    /// key (clean the source before sharpening).
    fn rebuild_vf(&self) {
        let mut f: Vec<String> = Vec::new();
        let a = self.adjust;
        if !a.is_neutral() {
            // ffmpeg `eq`: brightness [-1,1] (0 neutral); contrast/saturation
            // 1.0 neutral; gamma 1.0 neutral mapped 2^v. Hue is a separate
            // filter in degrees.
            f.push(format!(
                "eq=brightness={:.3}:contrast={:.3}:saturation={:.3}:gamma={:.3}",
                a.brightness,
                1.0 + a.contrast,
                1.0 + a.saturation,
                2.0_f32.powf(a.gamma),
            ));
            if a.hue != 0.0 {
                f.push(format!("hue=h={:.1}", a.hue * 180.0));
            }
        }
        let e = self.enhance;
        if e.denoise > 0.0 {
            f.push(format!(
                "hqdn3d={:.1}:{:.1}:{:.1}:{:.1}",
                e.denoise * 8.0,
                e.denoise * 6.0,
                e.denoise * 8.0,
                e.denoise * 6.0,
            ));
        }
        if e.banding > 0.0 {
            // gradfun strength 0.51..64, radius 8..32.
            f.push(format!(
                "gradfun={:.2}:{}",
                0.51 + e.banding * 1.5,
                (8.0 + e.banding * 8.0) as u32
            ));
        }
        if e.sharpen > 0.0 {
            // unsharp luma 3x3, gentle amount; denoise-first keeps it on edges.
            f.push(format!("unsharp=3:3:{:.3}:3:3:0", e.sharpen));
        }
        if e.grain > 0.0 {
            f.push(format!("noise=alls={:.0}:allf=t+u", e.grain * 20.0));
        }
        if let Some(k) = self.key {
            f.push(format!(
                "colorkey=color=0x{:02X}{:02X}{:02X}:similarity={:.3}:blend={:.3}",
                k.color[0], k.color[1], k.color[2], k.similarity, k.blend
            ));
            f.push("format=rgba".to_string());
        }
        let vf = if f.is_empty() {
            String::new()
        } else {
            format!("lavfi=[{}]", f.join(","))
        };
        (self.lib.set_property_string)(self.h, cstr("vf").as_ptr(), cstr(&vf).as_ptr());
    }
}

impl VideoStream for MpvStream {
    fn copy_frame(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        self.pump_events();
        // Only render when mpv reports a fresh frame — so a poll between the
        // video's own frames is a cheap no-op.
        if (self.lib.rc_update)(self.rctx) & RENDER_UPDATE_FRAME == 0 {
            return None;
        }
        let (w, h) = (self.prop_i64("dwidth"), self.prop_i64("dheight"));
        if w == 0 || h == 0 {
            return None;
        }
        let len = w as usize * h as usize * 4;
        if self.buf.len() != len {
            self.buf = vec![0u8; len];
        }
        let mut size = [w as c_int, h as c_int];
        let fmt = cstr("bgra");
        let mut stride: usize = w as usize * 4;
        let mut params = [
            RenderParam { typ: PARAM_SW_SIZE, data: size.as_mut_ptr() as *mut c_void },
            RenderParam { typ: PARAM_SW_FORMAT, data: fmt.as_ptr() as *mut c_void },
            RenderParam { typ: PARAM_SW_STRIDE, data: &mut stride as *mut usize as *mut c_void },
            RenderParam { typ: PARAM_SW_POINTER, data: self.buf.as_mut_ptr() as *mut c_void },
            RenderParam { typ: 0, data: std::ptr::null_mut() },
        ];
        if (self.lib.rc_render)(self.rctx, params.as_mut_ptr()) != 0 {
            return None;
        }
        // mpv rendered the native-orientation frame (we set `video-rotate=no`).
        // Apply the file's intended rotation ourselves. Read it from
        // `video-dec-params/rotate` (the decoder-level params), NOT
        // `video-params/rotate`: the latter is the *post-rotation* value and
        // `video-rotate=no` zeroes it, whereas the decoder params keep the
        // intended clockwise rotation in degrees regardless.
        let rot = self.prop_i64("video-dec-params/rotate");
        if matches!(rot, 90 | 180 | 270) {
            let (rw, rh, rbuf) = rotate_bgra(&self.buf, w, h, rot);
            self.dims = (rw, rh);
            Some((rw, rh, rbuf))
        } else {
            self.dims = (w, h);
            Some((w, h, self.buf.clone()))
        }
    }

    fn set_paused(&mut self, paused: bool) {
        let v = if paused { "yes" } else { "no" };
        (self.lib.set_property_string)(self.h, cstr("pause").as_ptr(), cstr(v).as_ptr());
    }

    fn seek(&mut self, seconds: f64) {
        self.ended_fired = false; // moving off the end re-arms the end event
        self.command(&["seek", &format!("{seconds:.3}"), "absolute"]);
    }

    fn step(&mut self, frames: i64) {
        self.ended_fired = false;
        if frames > 0 {
            for _ in 0..frames {
                self.command(&["frame-step"]); // implies pause
            }
        } else if frames < 0 {
            for _ in 0..frames.unsigned_abs() {
                self.command(&["frame-back-step"]); // real reverse step
            }
        }
    }

    fn time(&self) -> (f64, f64) {
        (self.prop_f64("time-pos"), self.prop_f64("duration"))
    }

    fn natural_size(&self) -> (f64, f64) {
        if self.dims.0 > 0 && self.dims.1 > 0 {
            (self.dims.0 as f64, self.dims.1 as f64)
        } else {
            (self.prop_i64("dwidth") as f64, self.prop_i64("dheight") as f64)
        }
    }

    fn set_adjust(&mut self, adjust: VideoAdjust) -> bool {
        self.adjust = adjust;
        self.rebuild_vf();
        true
    }

    fn set_enhance(&mut self, enhance: VideoEnhance) -> bool {
        self.enhance = enhance;
        self.rebuild_vf();
        true
    }

    fn set_chroma_key(&mut self, key: Option<ChromaKey>) -> bool {
        self.key = key;
        self.rebuild_vf();
        true
    }

    fn set_muted(&mut self, muted: bool) {
        let v = if muted { "yes" } else { "no" };
        (self.lib.set_property_string)(self.h, cstr("mute").as_ptr(), cstr(v).as_ptr());
    }
}

impl Drop for MpvStream {
    fn drop(&mut self) {
        // Free the render context before destroying the core.
        (self.lib.rc_free)(self.rctx);
        (self.lib.terminate_destroy)(self.h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // Pixel n is the BGRA quad [n, n, n, n]; a buffer is built row-major.
    fn px(n: u8) -> [u8; 4] {
        [n, n, n, n]
    }

    #[test]
    fn rotate_bgra_clockwise_geometry() {
        // 2×2: P0 P1 / P2 P3 (row-major).
        let src: Vec<u8> = [0u8, 1, 2, 3].iter().flat_map(|&n| px(n)).collect();
        let grid = |bytes: &[u8]| bytes.iter().step_by(4).copied().collect::<Vec<u8>>();

        // 90° CW: bottom-left rotates up to top-left → P2 P0 / P3 P1.
        let (w, h, out) = rotate_bgra(&src, 2, 2, 90);
        assert_eq!((w, h), (2, 2));
        assert_eq!(grid(&out), vec![2, 0, 3, 1]);

        // 180°: P3 P2 / P1 P0.
        let (_, _, out) = rotate_bgra(&src, 2, 2, 180);
        assert_eq!(grid(&out), vec![3, 2, 1, 0]);

        // 270° CW: P1 P3 / P0 P2.
        let (_, _, out) = rotate_bgra(&src, 2, 2, 270);
        assert_eq!(grid(&out), vec![1, 3, 0, 2]);
    }

    #[test]
    fn rotate_bgra_swaps_non_square_dims() {
        // 3 wide × 1 tall → 90/270 give 1 wide × 3 tall.
        let src: Vec<u8> = [10u8, 20, 30].iter().flat_map(|&n| px(n)).collect();
        let (w, h, out) = rotate_bgra(&src, 3, 1, 90);
        assert_eq!((w, h), (1, 3));
        // Row [10 20 30] stood up clockwise → column 10/20/30 top-to-bottom.
        assert_eq!(out.iter().step_by(4).copied().collect::<Vec<u8>>(), vec![10, 20, 30]);
    }

    /// End-to-end against the real crate code (load libmpv, SW render pull,
    /// live grade via lavfi). Skips when libmpv or the probe clip aren't
    /// present. Generate the clip with:
    ///   ffmpeg -f lavfi -i testsrc=duration=3:size=320x240:rate=10 \
    ///          -pix_fmt yuv420p /tmp/mpv_probe.mp4
    #[test]
    fn opens_pulls_a_frame_and_adjusts() {
        let clip = Path::new("/tmp/mpv_probe.mp4");
        if !clip.exists() {
            eprintln!("skip: /tmp/mpv_probe.mp4 missing");
            return;
        }
        let hint = Path::new("/opt/homebrew/opt/mpv/lib/libmpv.dylib");
        let Some(backend) = backend(hint) else {
            eprintln!("skip: libmpv could not be loaded");
            return;
        };
        let mut stream = backend
            .open(
                clip,
                Box::new(|| {}),
                VideoEnhance { denoise: 0.4, sharpen: 0.4, ..Default::default() },
            )
            .expect("open clip");

        let start = Instant::now();
        let mut frame = None;
        while start.elapsed() < Duration::from_secs(5) {
            if let Some(f) = stream.copy_frame() {
                frame = Some(f);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let (w, h, bytes) = frame.expect("a decoded frame within 5s");
        assert!(w > 0 && h > 0, "native size resolved");
        assert_eq!(bytes.len(), (w * h * 4) as usize, "BGRA buffer matches dims");

        assert!(stream.set_adjust(VideoAdjust { brightness: 0.4, ..Default::default() }));
        assert!(stream.set_chroma_key(Some(ChromaKey {
            color: [0, 255, 0],
            similarity: 0.3,
            blend: 0.05,
        })));
        let (_, dur) = stream.time();
        assert!(dur > 0.0, "duration reads back");
    }
}
