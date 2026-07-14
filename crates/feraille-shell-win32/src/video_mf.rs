//! Native Windows video provider for the viewer, behind the same windowless
//! frame-pull contract as the macOS AVFoundation backend (`video_overlay_*`).
//!
//! Uses **Media Foundation's `IMFMediaEngine` in frame-server mode**: the engine
//! demuxes, decodes, plays audio and keeps A/V sync internally; we hand it a
//! D3D11 device via a DXGI device manager and, each viewer tick, pull the
//! current frame with `TransferVideoFrame` into a render-target texture, copy
//! that into a CPU-readable staging texture, and read the BGRA bytes back.
//!
//! Threading: every `video_overlay_*` call comes from the GPUI main thread, so
//! the engine + D3D objects live in a **thread-local** registry (no `Send`
//! dance for COM/D3D). The engine raises events on its own MF worker threads;
//! the `IMFMediaEngineNotify` callback shares only `Arc<AtomicBool>` flags and
//! the `Send` end-of-clip closure with the main thread. The D3D device has
//! multithread protection enabled because the engine touches it off-thread.
//!
//! Any failure (no codec, file missing, COM error) returns handle `0` /
//! `None`, so the viewer silently falls back to the still poster — no crash,
//! no regression from the previous stub.
#![cfg(windows)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once};

use windows::core::{implement, Interface, BSTR};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFDXGIDeviceManager, IMFMediaEngine, IMFMediaEngineClassFactory,
    IMFMediaEngineNotify, IMFMediaEngineNotify_Impl, MFCreateAttributes,
    MFCreateDXGIDeviceManager, MFStartup, MFVideoNormalizedRect, MFSTARTUP_LITE, MF_MEDIA_ENGINE_CALLBACK,
    MF_MEDIA_ENGINE_DXGI_MANAGER, MF_MEDIA_ENGINE_EVENT_CANPLAY, MF_MEDIA_ENGINE_EVENT_ENDED,
    MF_MEDIA_ENGINE_EVENT_ERROR, MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
};
use windows::Win32::Foundation::{RECT, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};

/// MF version arg to `MFStartup` (MF_VERSION = 0x00020070).
const MF_VERSION: u32 = 0x0002_0070;

// CLSID_MFMediaEngineClassFactory — not surfaced as a constant by windows 0.58.
const CLSID_MF_MEDIA_ENGINE_CLASS_FACTORY: windows::core::GUID =
    windows::core::GUID::from_u128(0xb44392da_499b_446b_a4cb_005fead0e6d5);

/// Process-wide `MFStartup`, once.
fn ensure_mf_started() -> bool {
    static MF: Once = Once::new();
    static OK: AtomicBool = AtomicBool::new(false);
    MF.call_once(|| {
        // SAFETY: idempotent; MFSTARTUP_LITE skips the (unused-here) sockets.
        let hr = unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) };
        OK.store(hr.is_ok(), Ordering::SeqCst);
    });
    OK.load(Ordering::SeqCst)
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// Live players, keyed by the opaque handle handed back to the viewer.
    /// Main-thread only, so the !Send COM/D3D handles never cross threads.
    static PLAYERS: RefCell<HashMap<u64, Player>> = RefCell::new(HashMap::new());
}

/// The `IMFMediaEngineNotify` COM object. Holds only `Send` state shared with
/// the main thread; raised on MF worker threads.
#[implement(IMFMediaEngineNotify)]
struct Notify {
    ready: Arc<AtomicBool>,
    on_ended: Mutex<Option<Box<dyn Fn() + Send + 'static>>>,
}

impl Notify {
    /// Fire the viewer's end-of-playback callback (at most once).
    fn fire_ended(&self) {
        if let Ok(mut guard) = self.on_ended.lock() {
            if let Some(cb) = guard.take() {
                cb();
            }
        }
    }
}

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, _param1: usize, _param2: u32) -> windows::core::Result<()> {
        if event == MF_MEDIA_ENGINE_EVENT_CANPLAY.0 as u32 {
            self.ready.store(true, Ordering::SeqCst);
        } else if event == MF_MEDIA_ENGINE_EVENT_ENDED.0 as u32 {
            self.fire_ended();
        } else if event == MF_MEDIA_ENGINE_EVENT_ERROR.0 as u32 {
            // Treat a load/decode error as "ended" — the callback is the only
            // signal the viewer gets, so without it a broken file would stall
            // playlist auto-advance forever.
            self.fire_ended();
        }
        Ok(())
    }
}

/// One live native player (main-thread owned).
struct Player {
    engine: IMFMediaEngine,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    // Lazily created once the native video size is known.
    rgb_tex: Option<ID3D11Texture2D>,
    staging: Option<ID3D11Texture2D>,
    width: u32,
    height: u32,
    started: bool,
    ready: Arc<AtomicBool>,
    last_pts: i64,
    // Keep COM initialized until after the Media Engine interfaces above drop.
    _com: ComApartment,
}

/// COM apartment ownership for the thread that owns a `Player`.
///
/// A Media Foundation player stores COM interfaces in the thread-local
/// registry, so COM must stay initialized until the player is removed. If this
/// thread was already initialized in a different apartment, proceed without
/// calling `CoUninitialize`; COM is still active and we do not own that count.
struct ComApartment {
    uninitialize_on_drop: bool,
}

impl ComApartment {
    fn init() -> Option<Self> {
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() };
        match result {
            Ok(()) => Some(Self {
                uninitialize_on_drop: true,
            }),
            Err(e) if e.code() == RPC_E_CHANGED_MODE => Some(Self {
                uninitialize_on_drop: false,
            }),
            Err(e) => {
                eprintln!("[mf] CoInitializeEx failed: {:?}", e);
                None
            }
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.uninitialize_on_drop {
            unsafe { CoUninitialize() };
        }
    }
}

/// Build a D3D11 device with BGRA + video support and multithread protection
/// (the media engine drives it from its own threads).
fn create_device() -> Option<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let hr = unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            None,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    };
    hr.ok()?;
    let device = device?;
    let context = context?;
    // The engine accesses the device off-thread; protect it.
    if let Ok(mt) = device.cast::<ID3D11Multithread>() {
        let _ = unsafe { mt.SetMultithreadProtected(true) };
    }
    Some((device, context))
}

/// Open `path` and start a frame-server media engine. Returns a handle, or 0 on
/// any failure (caller falls back to the poster).
pub fn video_overlay_show(path: &Path, on_ended: Box<dyn Fn() + 'static + Send>) -> u64 {
    if !ensure_mf_started() {
        return 0;
    }
    match try_show(path, on_ended) {
        Some(id) => id,
        None => 0,
    }
}

/// Log a `windows::core::Result` failure and convert to `None` for the `?`
/// chain — temporary diagnostics for the MF backend bring-up.
macro_rules! mf_step {
    ($e:expr, $what:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[mf] {} failed: {:?}", $what, e);
                return None;
            }
        }
    };
}

fn try_show(path: &Path, on_ended: Box<dyn Fn() + 'static + Send>) -> Option<u64> {
    let com = ComApartment::init()?;

    let (device, context) = match create_device() {
        Some(d) => d,
        None => {
            eprintln!("[mf] create_device failed");
            return None;
        }
    };

    // DXGI device manager bound to our device.
    let mut token: u32 = 0;
    let mut manager: Option<IMFDXGIDeviceManager> = None;
    mf_step!(
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) },
        "MFCreateDXGIDeviceManager"
    );
    let manager = manager?;
    mf_step!(unsafe { manager.ResetDevice(&device, token) }, "ResetDevice");

    // Shared flags + the notify COM object.
    let ready = Arc::new(AtomicBool::new(false));
    let notify: IMFMediaEngineNotify = Notify {
        ready: ready.clone(),
        on_ended: Mutex::new(Some(on_ended)),
    }
    .into();

    // Engine attributes: device manager + callback + BGRA output.
    let mut attributes: Option<IMFAttributes> = None;
    mf_step!(
        unsafe { MFCreateAttributes(&mut attributes, 3) },
        "MFCreateAttributes"
    );
    let attributes = attributes?;
    mf_step!(
        unsafe { attributes.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &manager) },
        "SetUnknown(DXGI_MANAGER)"
    );
    mf_step!(
        unsafe { attributes.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify) },
        "SetUnknown(CALLBACK)"
    );
    mf_step!(
        unsafe {
            attributes.SetUINT32(
                &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
            )
        },
        "SetUINT32(VIDEO_OUTPUT_FORMAT)"
    );

    let factory: IMFMediaEngineClassFactory = mf_step!(
        unsafe { CoCreateInstance(&CLSID_MF_MEDIA_ENGINE_CLASS_FACTORY, None, CLSCTX_INPROC_SERVER) },
        "CoCreateInstance(MFMediaEngineClassFactory)"
    );
    // dwFlags = 0; frame-server mode is implied by the DXGI manager + output
    // format with no playback HWND.
    let engine: IMFMediaEngine =
        mf_step!(unsafe { factory.CreateInstance(0, &attributes) }, "CreateInstance(engine)");

    // Point it at the file. The media engine rejects the `\\?\` extended-length
    // prefix the file list uses (ERROR_INVALID_NAME), so strip it; a plain
    // `C:\…` path is accepted as the source URL.
    let cleaned = crate::strip_verbatim(path);
    let url = BSTR::from(cleaned.to_string_lossy().as_ref());
    mf_step!(unsafe { engine.SetSource(&url) }, "SetSource");

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    PLAYERS.with(|p| {
        p.borrow_mut().insert(
            id,
            Player {
                engine,
                device,
                context,
                rgb_tex: None,
                staging: None,
                width: 0,
                height: 0,
                started: false,
                ready,
                last_pts: -1,
                _com: com,
            },
        );
    });
    Some(id)
}

/// Pull the current frame as BGRA, or `None` if nothing new is ready.
pub fn video_overlay_copy_frame(id: u64) -> Option<(u32, u32, Vec<u8>)> {
    PLAYERS.with(|p| {
        let mut map = p.borrow_mut();
        let player = map.get_mut(&id)?;

        // Autoplay once the engine reports it can play.
        if !player.started && player.ready.load(Ordering::SeqCst) {
            let _ = unsafe { player.engine.Play() };
            player.started = true;
        }
        if !player.started {
            return None;
        }

        // Resolve the native size + (re)allocate textures on first frame.
        if player.width == 0 || player.height == 0 {
            let mut w: u32 = 0;
            let mut h: u32 = 0;
            if unsafe { player.engine.GetNativeVideoSize(Some(&mut w), Some(&mut h)) }.is_err()
                || w == 0
                || h == 0
            {
                return None;
            }
            player.width = w;
            player.height = h;
            player.rgb_tex = make_texture(&player.device, w, h, false);
            player.staging = make_texture(&player.device, w, h, true);
            if player.rgb_tex.is_none() || player.staging.is_none() {
                player.width = 0;
                return None;
            }
        }

        // Only transfer when a *new* frame is ready. OnVideoStreamTick returns
        // the new frame's presentation time on success; when no new frame is
        // ready the windows wrapper still yields `Ok` (S_FALSE isn't an error)
        // with a sentinel/garbage timestamp — real presentation times are
        // non-negative, so treat `pts < 0` (e.g. the `i64::MIN` sentinel) as
        // "nothing new", and dedupe identical timestamps.
        let pts = match unsafe { player.engine.OnVideoStreamTick() } {
            Ok(p) => p,
            Err(_) => return None,
        };
        if pts < 0 || pts == player.last_pts {
            return None;
        }
        player.last_pts = pts;

        let rgb = player.rgb_tex.as_ref()?;
        let staging = player.staging.as_ref()?;
        let (w, h) = (player.width, player.height);
        let dst = RECT {
            left: 0,
            top: 0,
            right: w as i32,
            bottom: h as i32,
        };
        // Whole source frame (normalized 0..1), no letterbox border.
        let src = MFVideoNormalizedRect {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        };
        if unsafe {
            player
                .engine
                .TransferVideoFrame(rgb, Some(&src as *const _), &dst as *const _, None)
        }
        .is_err()
        {
            return None;
        }

        // Copy GPU frame into the CPU-readable staging texture and map it.
        unsafe { player.context.CopyResource(staging, rgb) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        if unsafe {
            player
                .context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .is_err()
        {
            return None;
        }

        let row_bytes = (w as usize) * 4;
        let mut out = vec![0u8; row_bytes * h as usize];
        unsafe {
            let src_ptr = mapped.pData as *const u8;
            for row in 0..h as usize {
                let s = src_ptr.add(row * mapped.RowPitch as usize);
                let d = out.as_mut_ptr().add(row * row_bytes);
                std::ptr::copy_nonoverlapping(s, d, row_bytes);
            }
            player.context.Unmap(staging, 0);
        }
        Some((w, h, out))
    })
}

/// Allocate a BGRA texture: a render target (engine transfer dest) or a
/// CPU-readable staging copy.
fn make_texture(device: &ID3D11Device, w: u32, h: u32, staging: bool) -> Option<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: if staging {
            D3D11_USAGE_STAGING
        } else {
            D3D11_USAGE_DEFAULT
        },
        BindFlags: if staging {
            0
        } else {
            (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32
        },
        CPUAccessFlags: if staging {
            D3D11_CPU_ACCESS_READ.0 as u32
        } else {
            0
        },
        MiscFlags: 0,
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.ok()?;
    tex
}

pub fn video_overlay_remove(id: u64) {
    PLAYERS.with(|p| {
        if let Some(player) = p.borrow_mut().remove(&id) {
            let _ = unsafe { player.engine.Shutdown() };
        }
    });
}

pub fn video_overlay_set_paused(id: u64, paused: bool) {
    PLAYERS.with(|p| {
        if let Some(player) = p.borrow().get(&id) {
            unsafe {
                if paused {
                    let _ = player.engine.Pause();
                } else {
                    let _ = player.engine.Play();
                }
            }
        }
    });
}

pub fn video_overlay_restart(id: u64) {
    PLAYERS.with(|p| {
        if let Some(player) = p.borrow().get(&id) {
            unsafe {
                let _ = player.engine.SetCurrentTime(0.0);
                let _ = player.engine.Play();
            }
        }
    });
}

pub fn video_overlay_time(id: u64) -> (f64, f64) {
    PLAYERS.with(|p| {
        if let Some(player) = p.borrow().get(&id) {
            unsafe {
                let cur = player.engine.GetCurrentTime();
                let dur = player.engine.GetDuration();
                // GetDuration is NaN/Inf for unknown/live; clamp to 0.
                let dur = if dur.is_finite() && dur >= 0.0 { dur } else { 0.0 };
                (cur.max(0.0), dur)
            }
        } else {
            (0.0, 0.0)
        }
    })
}

pub fn video_overlay_natural_size(id: u64) -> (f64, f64) {
    PLAYERS.with(|p| {
        if let Some(player) = p.borrow().get(&id) {
            (player.width as f64, player.height as f64)
        } else {
            (0.0, 0.0)
        }
    })
}

pub fn video_overlay_seek(id: u64, seconds: f64) {
    PLAYERS.with(|p| {
        if let Some(player) = p.borrow().get(&id) {
            let _ = unsafe { player.engine.SetCurrentTime(seconds.max(0.0)) };
        }
    });
}

pub fn video_overlay_step(id: u64, frames: i64) {
    // No native frame-step on the media engine; nudge the clock ~30fps + pause.
    PLAYERS.with(|p| {
        if let Some(player) = p.borrow().get(&id) {
            unsafe {
                let t = player.engine.GetCurrentTime();
                let dt = frames as f64 / 30.0;
                let _ = player.engine.SetCurrentTime((t + dt).max(0.0));
                let _ = player.engine.Pause();
            }
        }
    });
}
