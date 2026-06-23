//! macOS libvlc binding. Hand-written FFI (the surface is ~20 stable
//! functions; the spike confirmed raw FFI is enough — no `vlc-rs` dep).
//!
//! Video filters (denoise/sharpen) only take effect as **instance**
//! arguments to `libvlc_new` — media options (`:video-filter=…`) are
//! silently ignored with the vmem output (verified with `invert`). So a
//! stream owns its own libvlc instance built with its filter args, and a
//! filter change re-opens (a new instance). The colour grade is separate:
//! `libvlc_video_set_adjust_*` changes it live, no re-open.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_uint, c_void, CString};
use std::path::{Path, PathBuf};
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use feraille_core::video::{VideoAdjust, VideoBackend, VideoEnhance, VideoStream};

// ---- libvlc constants ------------------------------------------------------

// libvlc_video_adjust_option_t
const ADJUST_ENABLE: c_uint = 0;
const ADJUST_CONTRAST: c_uint = 1;
const ADJUST_BRIGHTNESS: c_uint = 2;
const ADJUST_HUE: c_uint = 3;
const ADJUST_SATURATION: c_uint = 4;
const ADJUST_GAMMA: c_uint = 5;
// libvlc_event_e: MediaPlayer events start at 0x100; EndReached is +9.
const EVENT_END_REACHED: c_int = 0x100 + 9;

// ---- cross-platform dynamic loader -----------------------------------------
//
// libvlc is loaded at runtime (no build-time link) so a stock build needs no
// VLC. The three desktop OSes differ only in *how* you open a shared library
// and set an env var; symbol resolution and the libvlc ABI below are identical
// everywhere. Unix (macOS + Linux) uses `dlopen`/`dlsym`/`setenv`; Windows uses
// `LoadLibraryW`/`GetProcAddress`/`SetEnvironmentVariableW` (raw kernel32 FFI,
// matching this file's no-extra-deps style).
mod dynload {
    use std::ffi::c_void;
    use std::path::Path;

    /// Open a shared library by path (or, on Unix, by bare soname — the loader
    /// then searches its standard paths). Null on failure.
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

    /// Resolve a symbol; null if absent.
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

    /// Set a process env var (libvlc reads `VLC_PLUGIN_PATH` from the env).
    #[cfg(unix)]
    pub fn set_env(name: &str, value: &Path) {
        use std::ffi::{c_char, c_int, CString};
        extern "C" {
            fn setenv(name: *const c_char, value: *const c_char, overwrite: c_int) -> c_int;
        }
        if let (Ok(n), Ok(v)) = (
            CString::new(name),
            CString::new(value.to_string_lossy().as_bytes().to_vec()),
        ) {
            unsafe { setenv(n.as_ptr(), v.as_ptr(), 1) };
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

    #[cfg(windows)]
    pub fn set_env(name: &str, value: &Path) {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn SetEnvironmentVariableW(name: *const u16, value: *const u16) -> i32;
        }
        let n: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let v: Vec<u16> = value
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe { SetEnvironmentVariableW(n.as_ptr(), v.as_ptr()) };
    }
}

// ---- callback ABI ----------------------------------------------------------

type LockCb = extern "C" fn(*mut c_void, *mut *mut c_void) -> *mut c_void;
type UnlockCb = extern "C" fn(*mut c_void, *mut c_void, *const *mut c_void);
type DisplayCb = extern "C" fn(*mut c_void, *mut c_void);
type FormatCb =
    extern "C" fn(*mut *mut c_void, *mut c_char, *mut c_uint, *mut c_uint, *mut c_uint, *mut c_uint)
        -> c_uint;
type CleanupCb = extern "C" fn(*mut c_void);
type EventCb = extern "C" fn(*const c_void, *mut c_void);

// ---- resolved libvlc entry points -----------------------------------------

struct LibVlc {
    // Kept so dyld doesn't unload the images while we hold function pointers.
    _core: *mut c_void,
    _lib: *mut c_void,
    new: extern "C" fn(c_int, *const *const c_char) -> *mut c_void,
    release: extern "C" fn(*mut c_void),
    media_new_path: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    media_release: extern "C" fn(*mut c_void),
    mp_new_from_media: extern "C" fn(*mut c_void) -> *mut c_void,
    mp_release: extern "C" fn(*mut c_void),
    mp_stop: extern "C" fn(*mut c_void),
    mp_play: extern "C" fn(*mut c_void) -> c_int,
    set_format_callbacks: extern "C" fn(*mut c_void, FormatCb, CleanupCb),
    set_callbacks: extern "C" fn(*mut c_void, LockCb, UnlockCb, DisplayCb, *mut c_void),
    set_pause: extern "C" fn(*mut c_void, c_int),
    set_time: extern "C" fn(*mut c_void, i64),
    get_time: extern "C" fn(*mut c_void) -> i64,
    get_length: extern "C" fn(*mut c_void) -> i64,
    next_frame: extern "C" fn(*mut c_void),
    set_adjust_int: extern "C" fn(*mut c_void, c_uint, c_int),
    set_adjust_float: extern "C" fn(*mut c_void, c_uint, f32),
    event_manager: extern "C" fn(*mut c_void) -> *mut c_void,
    event_attach: extern "C" fn(*mut c_void, c_int, EventCb, *mut c_void) -> c_int,
    event_detach: extern "C" fn(*mut c_void, c_int, EventCb, *mut c_void),
}

/// `dlsym` a symbol and transmute it to the bound function-pointer type.
macro_rules! sym {
    ($h:expr, $name:literal) => {{
        let p = dynload::sym($h, $name);
        if p.is_null() {
            return Err(format!("libvlc: missing symbol {}", $name));
        }
        std::mem::transmute(p)
    }};
}

/// Resolved on-disk locations for one platform's libvlc.
struct VlcPaths {
    /// libvlccore, pre-loaded by full path before libvlc so the loader binds
    /// libvlc's reference to the already-loaded image (macOS `@rpath`, Windows
    /// same-dir import). `None` on Linux, where the dynamic linker resolves the
    /// core as an ordinary `NEEDED` dependency.
    core: Option<PathBuf>,
    /// libvlc itself — a full path, or (Linux fallback) a bare soname the
    /// loader searches for.
    lib: PathBuf,
    /// Plugin directory for `VLC_PLUGIN_PATH`. `None` lets libvlc auto-discover.
    plugins: Option<PathBuf>,
}

/// macOS: the user points at `VLC.app`; everything lives under `Contents/MacOS`.
#[cfg(target_os = "macos")]
fn resolve_paths(vlc: &Path) -> Result<VlcPaths, String> {
    let macos = vlc.join("Contents/MacOS");
    let lib = macos.join("lib/libvlc.dylib");
    if !lib.exists() {
        return Err(format!("no libvlc at {}", lib.display()));
    }
    Ok(VlcPaths {
        core: Some(macos.join("lib/libvlccore.dylib")),
        lib,
        plugins: Some(macos.join("plugins")),
    })
}

/// Windows: the user points at the VLC install dir (e.g.
/// `C:\Program Files\VideoLAN\VLC`), or at `vlc.exe` / `libvlc.dll` inside it.
/// `libvlc.dll`, `libvlccore.dll` and `plugins\` sit side by side there.
#[cfg(windows)]
fn resolve_paths(vlc: &Path) -> Result<VlcPaths, String> {
    let dir = if vlc.is_dir() {
        vlc.to_path_buf()
    } else {
        vlc.parent().unwrap_or(vlc).to_path_buf()
    };
    let lib = dir.join("libvlc.dll");
    if !lib.exists() {
        return Err(format!("no libvlc.dll in {}", dir.display()));
    }
    Ok(VlcPaths {
        core: Some(dir.join("libvlccore.dll")),
        lib,
        plugins: Some(dir.join("plugins")),
    })
}

/// Linux: if pointed at a directory holding `libvlc.so*`, load from there;
/// otherwise fall back to the system library by soname (the loader searches
/// the standard paths) and let libvlc auto-discover its plugins.
#[cfg(target_os = "linux")]
fn resolve_paths(vlc: &Path) -> Result<VlcPaths, String> {
    if vlc.is_dir() {
        for name in ["libvlc.so", "libvlc.so.5"] {
            let p = vlc.join(name);
            if p.exists() {
                let plugins = vlc.join("vlc/plugins");
                return Ok(VlcPaths {
                    core: None,
                    lib: p,
                    plugins: plugins.is_dir().then_some(plugins),
                });
            }
        }
    } else if vlc.is_file() {
        return Ok(VlcPaths {
            core: None,
            lib: vlc.to_path_buf(),
            plugins: None,
        });
    }
    // System install: dlopen by soname searches the loader path.
    Ok(VlcPaths {
        core: None,
        lib: PathBuf::from("libvlc.so.5"),
        plugins: None,
    })
}

impl LibVlc {
    /// Load libvlc from the user-pointed VLC location and resolve our symbols.
    /// Also sets `VLC_PLUGIN_PATH` (libvlc's plugin-discovery mechanism — the
    /// path comes from settings, never a user-set env var).
    unsafe fn load(vlc: &Path) -> Result<LibVlc, String> {
        let paths = resolve_paths(vlc)?;
        if let Some(plugins) = paths.plugins.as_deref() {
            dynload::set_env("VLC_PLUGIN_PATH", plugins);
        }
        // Pre-load libvlccore by full path where we know it, so the loader
        // binds libvlc's core reference to the already-loaded image instead of
        // searching (see `VlcPaths::core`).
        let core = match paths.core.as_deref() {
            Some(cp) => {
                let h = dynload::open(cp);
                if h.is_null() {
                    return Err(format!("loading {} failed", cp.display()));
                }
                h
            }
            None => std::ptr::null_mut(),
        };
        let lib = dynload::open(&paths.lib);
        if lib.is_null() {
            return Err(format!("loading {} failed", paths.lib.display()));
        }
        Ok(LibVlc {
            _core: core,
            _lib: lib,
            new: sym!(lib, "libvlc_new"),
            release: sym!(lib, "libvlc_release"),
            media_new_path: sym!(lib, "libvlc_media_new_path"),
            media_release: sym!(lib, "libvlc_media_release"),
            mp_new_from_media: sym!(lib, "libvlc_media_player_new_from_media"),
            mp_release: sym!(lib, "libvlc_media_player_release"),
            mp_stop: sym!(lib, "libvlc_media_player_stop"),
            mp_play: sym!(lib, "libvlc_media_player_play"),
            set_format_callbacks: sym!(lib, "libvlc_video_set_format_callbacks"),
            set_callbacks: sym!(lib, "libvlc_video_set_callbacks"),
            set_pause: sym!(lib, "libvlc_media_player_set_pause"),
            set_time: sym!(lib, "libvlc_media_player_set_time"),
            get_time: sym!(lib, "libvlc_media_player_get_time"),
            get_length: sym!(lib, "libvlc_media_player_get_length"),
            next_frame: sym!(lib, "libvlc_media_player_next_frame"),
            set_adjust_int: sym!(lib, "libvlc_video_set_adjust_int"),
            set_adjust_float: sym!(lib, "libvlc_video_set_adjust_float"),
            event_manager: sym!(lib, "libvlc_media_player_event_manager"),
            event_attach: sym!(lib, "libvlc_event_attach"),
            event_detach: sym!(lib, "libvlc_event_detach"),
        })
    }
}

// ---- shared decode state (crosses into libvlc's threads) -------------------

struct Ready {
    buf: Vec<u8>,
    w: u32,
    h: u32,
}

/// Shared between the viewer (main thread) and libvlc's vout/event threads.
struct Ctx {
    /// Decode target VLC writes between lock/unlock — touched only on the
    /// vout thread (allocated in `fmt_setup`, freed in `fmt_cleanup`).
    decode: AtomicPtr<u8>,
    decode_len: AtomicUsize,
    /// Newest complete frame, copied in `display` under the lock; read by
    /// `copy_frame` on the main thread.
    ready: Mutex<Ready>,
    /// Bumped per displayed frame so `copy_frame` can skip duplicates.
    seq: AtomicU64,
    /// End-of-clip notification (fires on a libvlc thread, hence `Send`).
    on_ended: Box<dyn Fn() + Send + 'static>,
}

// SAFETY: `decode` is only dereferenced on the vout thread (lock/display/
// cleanup run there, serially); `ready`/`seq` are synchronised; `on_ended`
// is `Send`. libvlc holds a `*mut Ctx` and calls back from its threads.
unsafe impl Send for Ctx {}
unsafe impl Sync for Ctx {}

extern "C" fn fmt_setup(
    opaque: *mut *mut c_void,
    chroma: *mut c_char,
    width: *mut c_uint,
    height: *mut c_uint,
    pitches: *mut c_uint,
    lines: *mut c_uint,
) -> c_uint {
    unsafe {
        let ctx = &*(*opaque as *const Ctx);
        let (w, h) = (*width, *height);
        // Ask for RV32 (BGRA on little-endian) at the source resolution.
        ptr::copy_nonoverlapping(b"RV32".as_ptr() as *const c_char, chroma, 4);
        *pitches = w * 4;
        *lines = h;
        let len = (w as usize) * (h as usize) * 4;

        let mut buf = vec![0u8; len].into_boxed_slice();
        let p = buf.as_mut_ptr();
        std::mem::forget(buf); // ownership tracked via decode/decode_len
        let prev = ctx.decode.swap(p, Ordering::SeqCst);
        let prev_len = ctx.decode_len.swap(len, Ordering::SeqCst);
        if !prev.is_null() {
            drop(Box::from_raw(ptr::slice_from_raw_parts_mut(prev, prev_len)));
        }
        if let Ok(mut r) = ctx.ready.lock() {
            r.buf = vec![0u8; len];
            r.w = w;
            r.h = h;
        }
        1 // one plane
    }
}

extern "C" fn fmt_cleanup(opaque: *mut c_void) {
    unsafe {
        let ctx = &*(opaque as *const Ctx);
        let p = ctx.decode.swap(ptr::null_mut(), Ordering::SeqCst);
        let len = ctx.decode_len.swap(0, Ordering::SeqCst);
        if !p.is_null() {
            drop(Box::from_raw(ptr::slice_from_raw_parts_mut(p, len)));
        }
    }
}

extern "C" fn lock(opaque: *mut c_void, planes: *mut *mut c_void) -> *mut c_void {
    unsafe {
        let ctx = &*(opaque as *const Ctx);
        *planes = ctx.decode.load(Ordering::SeqCst) as *mut c_void;
    }
    ptr::null_mut()
}

extern "C" fn unlock(_opaque: *mut c_void, _picture: *mut c_void, _planes: *const *mut c_void) {}

extern "C" fn display(opaque: *mut c_void, _picture: *mut c_void) {
    unsafe {
        let ctx = &*(opaque as *const Ctx);
        let p = ctx.decode.load(Ordering::SeqCst);
        let len = ctx.decode_len.load(Ordering::SeqCst);
        if p.is_null() || len == 0 {
            return;
        }
        if let Ok(mut r) = ctx.ready.lock() {
            if r.buf.len() == len {
                ptr::copy_nonoverlapping(p, r.buf.as_mut_ptr(), len);
                ctx.seq.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

extern "C" fn on_end(_event: *const c_void, user_data: *mut c_void) {
    unsafe {
        let ctx = &*(user_data as *const Ctx);
        (ctx.on_ended)();
    }
}

/// libvlc_new arguments for the video filter chain (denoise `hqdn3d`,
/// sharpen, debanding `gradfun`, film grain `grain`), from the `0..1`
/// slider values. Empty when neutral. These MUST be instance args — media
/// options are ignored for the vmem output.
fn enhance_args(e: VideoEnhance) -> Vec<String> {
    let mut filters: Vec<&str> = Vec::new();
    let mut args: Vec<String> = Vec::new();
    // Chain ORDER matters — VLC applies `--video-filter` left to right. The
    // sane pipeline is denoise → deband → sharpen → grain: clean the source
    // first so sharpen enhances real edges instead of amplifying grain, then
    // (optionally) re-grain. Sharpening before denoising (the old order) just
    // crisped the noise, which read as "sharpen adds noise".
    if e.denoise > 0.0 {
        filters.push("hqdn3d");
        // Real libvlc option names are `-spat`/`-temp`, not `-spatial`.
        args.push(format!("--hqdn3d-luma-spat={:.1}", e.denoise * 8.0));
        args.push(format!("--hqdn3d-chroma-spat={:.1}", e.denoise * 6.0));
    }
    if e.banding > 0.0 {
        filters.push("gradfun");
        // radius 4..16 px, strength 0.0..2.0 (1.2 is plenty for banding).
        args.push(format!("--gradfun-radius={}", (4.0 + e.banding * 12.0) as u32));
        args.push(format!("--gradfun-strength={:.2}", e.banding * 1.2));
    }
    if e.sharpen > 0.0 {
        filters.push("sharpen");
        // VLC's `sharpen` is a Laplacian high-pass (sigma 0..2, default 0.05).
        // It has no edge threshold, so it amplifies whatever high-frequency
        // content exists — including grain. Map the slider into the gentle
        // end (0..0.5) and rely on denoise-first to keep it edge-focused.
        args.push(format!("--sharpen-sigma={:.3}", e.sharpen * 0.5));
    }
    if e.grain > 0.0 {
        filters.push("grain");
        // variance 0..10; subtle by default so it dithers, not snows.
        args.push(format!("--grain-variance={:.2}", e.grain * 2.0));
    }
    if !filters.is_empty() {
        args.push(format!("--video-filter={}", filters.join(":")));
    }
    args
}

// ---- libvlc loader (the dylib is loaded once; instances are per-stream) ----

thread_local! {
    // (VLC.app path, resolved libvlc). First path wins for the session;
    // changing the VLC.app path in Settings needs a restart.
    static LIB: RefCell<Option<(PathBuf, Rc<LibVlc>)>> = const { RefCell::new(None) };
}

fn load_lib(vlc_app: &Path) -> Option<Rc<LibVlc>> {
    LIB.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            match unsafe { LibVlc::load(vlc_app) } {
                Ok(l) => *slot = Some((vlc_app.to_path_buf(), Rc::new(l))),
                Err(e) => {
                    eprintln!("[feraille] VLC backend unavailable: {e}");
                    return None;
                }
            }
        }
        Some(slot.as_ref().unwrap().1.clone())
    })
}

pub fn backend(vlc_app: &Path) -> Option<Box<dyn VideoBackend>> {
    let lib = load_lib(vlc_app)?;
    Some(Box::new(VlcBackend { lib }))
}

struct VlcBackend {
    lib: Rc<LibVlc>,
}

impl VideoBackend for VlcBackend {
    fn open(
        &self,
        path: &Path,
        on_ended: Box<dyn Fn() + Send + 'static>,
        enhance: VideoEnhance,
    ) -> Option<Box<dyn VideoStream>> {
        let lib = self.lib.clone();

        // Per-stream instance: filters are instance args, so a stream with a
        // different filter chain needs its own instance. `--quiet` etc. cut
        // libvlc's console chatter. Args are consumed during `libvlc_new`,
        // so the CStrings only need to outlive the call.
        let mut argv_s = vec![
            "--quiet".to_string(),
            "--no-osd".to_string(),
            "--no-video-title-show".to_string(),
        ];
        argv_s.extend(enhance_args(enhance));
        let cargs: Vec<CString> = argv_s
            .iter()
            .filter_map(|s| CString::new(s.as_str()).ok())
            .collect();
        let argv: Vec<*const c_char> = cargs.iter().map(|c| c.as_ptr()).collect();
        let inst = (lib.new)(argv.len() as c_int, argv.as_ptr());
        if inst.is_null() {
            return None;
        }

        // Strip the Windows `\\?\` extended-length prefix the file list uses;
        // libvlc's path parsing rejects it. No-op on macOS/Linux paths.
        let path_str = path.to_string_lossy();
        let path_str = path_str.strip_prefix(r"\\?\").unwrap_or(&path_str);
        let cpath = match CString::new(path_str.as_bytes().to_vec()) {
            Ok(c) => c,
            Err(_) => {
                (lib.release)(inst);
                return None;
            }
        };
        let media = (lib.media_new_path)(inst, cpath.as_ptr());
        if media.is_null() {
            (lib.release)(inst);
            return None;
        }
        let mp = (lib.mp_new_from_media)(media);
        (lib.media_release)(media); // the player retains it
        if mp.is_null() {
            (lib.release)(inst);
            return None;
        }
        let ctx = Box::into_raw(Box::new(Ctx {
            decode: AtomicPtr::new(ptr::null_mut()),
            decode_len: AtomicUsize::new(0),
            ready: Mutex::new(Ready {
                buf: Vec::new(),
                w: 0,
                h: 0,
            }),
            seq: AtomicU64::new(0),
            on_ended,
        }));
        (lib.set_format_callbacks)(mp, fmt_setup, fmt_cleanup);
        (lib.set_callbacks)(mp, lock, unlock, display, ctx as *mut c_void);
        let em = (lib.event_manager)(mp);
        (lib.event_attach)(em, EVENT_END_REACHED, on_end, ctx as *mut c_void);
        if (lib.mp_play)(mp) != 0 {
            (lib.event_detach)(em, EVENT_END_REACHED, on_end, ctx as *mut c_void);
            (lib.mp_release)(mp);
            (lib.release)(inst);
            unsafe { drop(Box::from_raw(ctx)) };
            return None;
        }
        Some(Box::new(VlcStream {
            lib,
            inst,
            mp,
            em,
            ctx,
            last_read: 0,
        }))
    }
}

struct VlcStream {
    lib: Rc<LibVlc>,
    inst: *mut c_void,
    mp: *mut c_void,
    em: *mut c_void,
    ctx: *mut Ctx,
    last_read: u64,
}

impl VideoStream for VlcStream {
    fn copy_frame(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        let ctx = unsafe { &*self.ctx };
        let seq = ctx.seq.load(Ordering::SeqCst);
        if seq == self.last_read {
            return None;
        }
        self.last_read = seq;
        let r = ctx.ready.lock().ok()?;
        if r.w == 0 || r.h == 0 || r.buf.is_empty() {
            return None;
        }
        Some((r.w, r.h, r.buf.clone()))
    }

    fn set_paused(&mut self, paused: bool) {
        (self.lib.set_pause)(self.mp, paused as c_int);
    }

    fn seek(&mut self, seconds: f64) {
        (self.lib.set_time)(self.mp, (seconds * 1000.0) as i64);
    }

    fn step(&mut self, frames: i64) {
        if frames > 0 {
            for _ in 0..frames {
                (self.lib.next_frame)(self.mp);
            }
        } else if frames < 0 {
            // libvlc has no reverse step — nudge the clock (~30 fps) + pause.
            let t = (self.lib.get_time)(self.mp);
            let back = 33 * frames.unsigned_abs() as i64;
            (self.lib.set_time)(self.mp, (t - back).max(0));
            (self.lib.set_pause)(self.mp, 1);
        }
    }

    fn time(&self) -> (f64, f64) {
        let t = (self.lib.get_time)(self.mp).max(0) as f64;
        let d = (self.lib.get_length)(self.mp).max(0) as f64;
        (t / 1000.0, d / 1000.0)
    }

    fn natural_size(&self) -> (f64, f64) {
        let ctx = unsafe { &*self.ctx };
        match ctx.ready.lock() {
            Ok(r) => (r.w as f64, r.h as f64),
            Err(_) => (0.0, 0.0),
        }
    }

    fn set_adjust(&mut self, a: VideoAdjust) -> bool {
        let enable = if a.is_neutral() { 0 } else { 1 };
        (self.lib.set_adjust_int)(self.mp, ADJUST_ENABLE, enable);
        // Map bipolar [-1, 1] → libvlc's 1.0-neutral ranges.
        (self.lib.set_adjust_float)(self.mp, ADJUST_BRIGHTNESS, 1.0 + a.brightness);
        (self.lib.set_adjust_float)(self.mp, ADJUST_CONTRAST, 1.0 + a.contrast);
        (self.lib.set_adjust_float)(self.mp, ADJUST_SATURATION, 1.0 + a.saturation);
        // Hue is degrees (-180..180); gamma is a 0.01..10 multiplier around
        // 1.0 — map [-1, 1] exponentially so ±1 ≈ 0.5×/2× and 0 = neutral.
        (self.lib.set_adjust_float)(self.mp, ADJUST_HUE, a.hue * 180.0);
        (self.lib.set_adjust_float)(self.mp, ADJUST_GAMMA, 2.0_f32.powf(a.gamma));
        true
    }
}

impl Drop for VlcStream {
    fn drop(&mut self) {
        // Stop + release first so no callback fires after we free `ctx`.
        (self.lib.event_detach)(self.em, EVENT_END_REACHED, on_end, self.ctx as *mut c_void);
        (self.lib.mp_stop)(self.mp);
        (self.lib.mp_release)(self.mp);
        (self.lib.release)(self.inst);
        unsafe { drop(Box::from_raw(self.ctx)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// End-to-end against the *real* crate code (per-stream instance with a
    /// filter chain, format callbacks, vmem, event attach, set_adjust, Drop).
    /// Skips when VLC.app or the probe clip aren't present. Generate it with:
    ///   ffmpeg -f lavfi -i testsrc=duration=3:size=320x240:rate=10 \
    ///          -pix_fmt yuv420p /tmp/vlc_probe.mp4
    #[test]
    fn opens_pulls_a_frame_and_adjusts() {
        let app = Path::new("/Applications/VLC.app");
        let clip = Path::new("/tmp/vlc_probe.mp4");
        if !app.exists() || !clip.exists() {
            eprintln!("skip: VLC.app or /tmp/vlc_probe.mp4 missing");
            return;
        }
        let Some(backend) = backend(app) else {
            eprintln!("skip: libvlc could not be loaded");
            return;
        };
        // Open with a sharpen + denoise filter chain to exercise that path.
        let mut stream = backend
            .open(
                clip,
                Box::new(|| {}),
                VideoEnhance {
                    denoise: 0.5,
                    sharpen: 0.5,
                    banding: 0.5,
                    grain: 0.3,
                },
            )
            .expect("open clip");

        let start = Instant::now();
        let mut frame = None;
        while start.elapsed() < Duration::from_secs(5) {
            if let Some(f) = stream.copy_frame() {
                frame = Some(f);
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        let (w, h, bytes) = frame.expect("a decoded frame within 5s");
        assert!(w > 0 && h > 0, "native size resolved");
        assert_eq!(bytes.len(), (w * h * 4) as usize, "BGRA buffer matches dims");

        assert!(stream.set_adjust(VideoAdjust {
            brightness: 0.4,
            ..Default::default()
        }));
        let (_, dur) = stream.time();
        assert!(dur > 0.0, "duration reads back");
    }
}
