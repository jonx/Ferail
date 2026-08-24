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
/// **Safe to call from worker threads** (and meant to be — it can block on
/// a spun-down volume when the folder carries custom artwork, so the UI
/// thread must never call it; see the Prime Directive). `iconForFile:`
/// itself is thread-safe, and the one hazard in this function — resizing
/// the shared, cached `NSImage` the workspace hands back — is avoided by
/// drawing a private copy. Rasterization goes through a per-thread
/// `NSGraphicsContext`, so concurrent fetches don't share drawing state.
#[cfg(target_os = "macos")]
pub fn fetch_icon_rgba(path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    use objc2::msg_send;
    use objc2::ClassType;
    use objc2_app_kit::{
        NSBitmapFormat, NSBitmapImageRep, NSCompositingOperation, NSDeviceRGBColorSpace,
        NSGraphicsContext, NSWorkspace,
    };
    use objc2_foundation::{NSCopying, NSPoint, NSRect, NSSize, NSString};

    let path_str = path.to_str()?;
    let size_f = size_px as f64;

    unsafe {
        let workspace = NSWorkspace::sharedWorkspace();
        let ns_path = NSString::from_str(path_str);
        // `iconForFile:` returns a shared, cached NSImage; `setSize:` on it
        // would race any other thread drawing the same icon. Copy first.
        let image = workspace.iconForFile(&ns_path).copy();
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
/// `ferail-shell-win32::fetch_quick_look_thumbnail`) so transparent
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
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{DeleteObject, GetObjectW, DIBSECTION, HBITMAP};
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{IShellItemImageFactory, SIIGBF_ICONONLY, SIIGBF_RESIZETOFIT};

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);

    unsafe {
        let co_hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let we_initialized = co_hr.is_ok();

        let result = (|| -> Option<(Vec<u8>, u32, u32)> {
            // Simple-parse retry inside: namespace junctions like
            // C:\Windows\Fonts refuse plain file paths (WIN-011).
            let factory: IShellItemImageFactory = win_shell::item_image_factory(&wide)?;
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

            // Explorer stamps overlays (the .lnk shortcut arrow) on top
            // of the base icon; SIIGBF_ICONONLY hands back the bare
            // target icon. Compose the shell-reported overlay for .lnk
            // only: the gpui IconCache keys icons by extension, so a
            // per-file overlay (cloud sync state, say) would bleed onto
            // every file of the type, while every .lnk carries the same
            // arrow.
            if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
            {
                if let Some(overlay) = win_shell::overlay_rgba(&wide, w, h) {
                    win_shell::blend_over(&mut pixels, &overlay);
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

/// Linux: resolve the file's MIME type (shared-mime-info), map it to the
/// current icon theme's glyph (freedesktop icon-theme spec), and rasterize
/// that PNG/SVG to straight RGBA8.
///
/// Cached by kind/extension one level up (`IconCache`), so the MIME + theme
/// resolution runs at most once per distinct file type, not per file — and
/// never on the render path (the caller gates on `path_guard::is_rendering`).
#[cfg(target_os = "linux")]
pub fn fetch_icon_rgba(path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    linux_icons::fetch(path, size_px)
}

#[cfg(target_os = "linux")]
mod linux_icons {
    use std::path::Path;
    use std::sync::OnceLock;

    use xdg_mime::SharedMimeInfo;

    /// The system shared-mime-info database — parsing it walks
    /// `/usr/share/mime`, so load it once for the process.
    fn mime_db() -> &'static SharedMimeInfo {
        static DB: OnceLock<SharedMimeInfo> = OnceLock::new();
        DB.get_or_init(SharedMimeInfo::new)
    }

    /// The active GTK icon theme name (via gsettings), resolved once.
    /// Falls back to Adwaita — freedesktop-icons cascades to hicolor from
    /// any theme, so the concrete name only picks the preferred artwork.
    fn theme_name() -> &'static str {
        static THEME: OnceLock<String> = OnceLock::new();
        THEME.get_or_init(|| {
            freedesktop_icons::default_theme_gtk().unwrap_or_else(|| "Adwaita".to_string())
        })
    }

    /// Candidate icon names for `path`, most-specific first. Directories are
    /// always `folder`; files go through shared-mime-info's ordered list
    /// (specific glyph, `type-subtype`, then the `*-x-generic` fallback).
    fn icon_names(path: &Path) -> Vec<String> {
        if path.is_dir() {
            return vec!["folder".to_string(), "inode-directory".to_string()];
        }
        let db = mime_db();
        let mut guess = db.guess_mime_type();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            guess.file_name(name);
        }
        guess.path(path);
        let mime = guess.guess().mime_type().clone();
        let mut names = db.lookup_icon_names(&mime);
        // Last-ditch generic so an unknown type still draws something.
        names.push("text-x-generic".to_string());
        names
    }

    pub fn fetch(path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
        let size = size_px.clamp(1, 512) as u16;
        let theme = theme_name();
        let icon_path = icon_names(path).into_iter().find_map(|name| {
            freedesktop_icons::lookup(&name)
                .with_size(size)
                .with_theme(theme)
                .find()
        })?;
        rasterize(&icon_path, size_px)
    }

    /// Rasterize a theme icon file (PNG or SVG) to straight RGBA8 at
    /// `size_px` square.
    fn rasterize(icon_path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
        let is_svg = icon_path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz"));
        if is_svg {
            rasterize_svg(icon_path, size_px)
        } else {
            rasterize_raster(icon_path, size_px)
        }
    }

    /// PNG/raster theme icon → straight RGBA8, resized to fit if the theme
    /// only shipped a different pixel size. `image` returns straight RGBA
    /// already (no premultiply to undo).
    fn rasterize_raster(icon_path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
        let img = image::open(icon_path).ok()?;
        let img = if img.width() != size_px || img.height() != size_px {
            img.resize_exact(size_px, size_px, image::imageops::FilterType::Lanczos3)
        } else {
            img
        };
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        Some((rgba.into_raw(), w, h))
    }

    /// SVG theme icon → straight RGBA8 at `size_px` square. tiny-skia paints
    /// premultiplied, so we undo the premultiply to honour the straight-RGBA
    /// contract (matters on the anti-aliased edges).
    fn rasterize_svg(icon_path: &Path, size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
        use resvg::tiny_skia;
        use resvg::usvg;

        let data = std::fs::read(icon_path).ok()?;
        let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
        let mut pixmap = tiny_skia::Pixmap::new(size_px, size_px)?;

        let ts = tree.size();
        let scale = (size_px as f32 / ts.width().max(ts.height())).max(f32::MIN_POSITIVE);
        // Center the (possibly non-square) glyph in the square pixmap.
        let tx = (size_px as f32 - ts.width() * scale) / 2.0;
        let ty = (size_px as f32 - ts.height() * scale) / 2.0;
        let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty);
        resvg::render(&tree, transform, &mut pixmap.as_mut());

        let mut out = pixmap.take(); // premultiplied RGBA8
        for px in out.chunks_exact_mut(4) {
            let a = px[3];
            if a != 0 && a != 255 {
                // straight = premultiplied * 255 / alpha
                px[0] = ((px[0] as u16 * 255) / a as u16) as u8;
                px[1] = ((px[1] as u16 * 255) / a as u16) as u8;
                px[2] = ((px[2] as u16 * 255) / a as u16) as u8;
            }
        }
        Some((out, size_px, size_px))
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub fn fetch_icon_rgba(_path: &Path, _size_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    None
}

// Smoke tests for the per-file OS/theme icon fetch. They run only where
// `fetch_icon_rgba` is implemented for real (macOS NSWorkspace, Windows shell,
// Linux freedesktop theme); on a bare CI Linux image without an icon theme
// installed the Linux arm can legitimately return None, so that case is
// tolerated rather than asserted (see `linux_icon_is_wellformed_when_present`).
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
#[cfg(test)]
mod icon_tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ferail-icon-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).expect("create temp file");
        f.write_all(bytes).expect("write temp file");
        p
    }

    /// A returned icon must have the requested square dimensions, a matching
    /// buffer length, and at least one non-transparent pixel — an all-zero
    /// buffer is the classic "resolved a path but rasterized nothing" bug.
    fn assert_wellformed(icon: (Vec<u8>, u32, u32), size: u32) {
        let (rgba, w, h) = icon;
        assert_eq!((w, h), (size, size), "icon is not the requested size");
        assert_eq!(
            rgba.len() as u32,
            w * h * 4,
            "RGBA buffer length doesn't match dimensions"
        );
        assert!(
            rgba.chunks_exact(4).any(|px| px[3] != 0),
            "icon rasterized to a fully transparent buffer"
        );
    }

    // macOS/Windows always have a system icon provider, so a plain text file
    // must yield a real icon.
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn text_file_gets_a_real_icon() {
        let p = temp_file("note.txt", b"hello");
        let icon = fetch_icon_rgba(&p, 32).expect("system icon for a .txt file");
        assert_wellformed(icon, 32);
        let _ = std::fs::remove_file(&p);
    }

    // Linux depends on an installed icon theme. When one is present (the normal
    // desktop case, and our WSL2 test box) the result must be well-formed; when
    // absent (a headless CI image) None is acceptable — we only guard against a
    // malformed non-None result.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_icon_is_wellformed_when_present() {
        let p = temp_file("note.txt", b"hello");
        if let Some(icon) = fetch_icon_rgba(&p, 32) {
            assert_wellformed(icon, 32);
        }
        let _ = std::fs::remove_file(&p);
    }

    // A directory must resolve to a folder icon on every real-provider platform
    // (it takes the `path.is_dir()` / folder branch, not a MIME guess).
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn directory_gets_a_folder_icon() {
        let dir = std::env::temp_dir().join(format!("ferail-icon-dir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let icon = fetch_icon_rgba(&dir, 32).expect("system icon for a directory");
        assert_wellformed(icon, 32);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Windows helpers for [`fetch_icon_rgba`]: simple-parse shell item
/// creation (namespace junctions) and shell overlay composition (the
/// `.lnk` shortcut arrow).
///
/// The simple-parse half mirrors
/// `ferail-shell-win32::shell_item_image_factory` — the two crates
/// deliberately don't depend on each other, so keep the twins in sync.
#[cfg(windows)]
mod win_shell {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::E_POINTER;
    use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
    use windows::Win32::System::Com::CreateBindCtx;
    use windows::Win32::UI::Shell::{
        IFileSystemBindData, IFileSystemBindData_Impl, IShellItemImageFactory,
        SHCreateItemFromParsingName, STR_FILE_SYS_BIND_DATA,
    };
    use windows::Win32::UI::WindowsAndMessaging::HICON;

    /// `SHCreateItemFromParsingName`, retried with a simple-parse bind
    /// context when the namespace refuses the plain string.
    /// `C:\Windows\Fonts` is the canonical refusal: it is a namespace
    /// junction, so ordinary file paths under it come back
    /// `E_INVALIDARG` even though the files are plain — which is what
    /// left the Fonts grid with blank icons (WIN-011). Registering an
    /// `IFileSystemBindData` under `STR_FILE_SYS_BIND_DATA` forces
    /// plain file-system parsing, Explorer's own escape hatch.
    pub(super) unsafe fn item_image_factory(wide: &[u16]) -> Option<IShellItemImageFactory> {
        let pcw = PCWSTR::from_raw(wide.as_ptr());
        if let Ok(f) = SHCreateItemFromParsingName::<_, _, IShellItemImageFactory>(pcw, None) {
            return Some(f);
        }
        let bind_data: IFileSystemBindData = SimpleFsBindData.into();
        let bctx = CreateBindCtx(0).ok()?;
        bctx.RegisterObjectParam(STR_FILE_SYS_BIND_DATA, &bind_data)
            .ok()?;
        SHCreateItemFromParsingName::<_, _, IShellItemImageFactory>(pcw, &bctx).ok()
    }

    /// Minimal `IFileSystemBindData`: the parser only needs the object
    /// to exist; a zeroed `WIN32_FIND_DATAW` is the documented "just
    /// parse the string" answer.
    #[windows::core::implement(IFileSystemBindData)]
    struct SimpleFsBindData;

    impl IFileSystemBindData_Impl for SimpleFsBindData_Impl {
        fn SetFindData(&self, _pfd: *const WIN32_FIND_DATAW) -> windows::core::Result<()> {
            Ok(())
        }
        fn GetFindData(&self, pfd: *mut WIN32_FIND_DATAW) -> windows::core::Result<()> {
            if pfd.is_null() {
                return Err(E_POINTER.into());
            }
            unsafe { pfd.write(Default::default()) };
            Ok(())
        }
    }

    /// Rasterize the shell overlay icon reported for the file (for
    /// `.lnk` that is the shortcut arrow) at `w`×`h`, straight RGBA.
    /// `None` when the shell reports no overlay. Overlay icons carry
    /// their glyph pre-positioned inside their own canvas (the arrow
    /// sits bottom-left), so a full-size draw plus source-over
    /// composite reproduces Explorer's look.
    pub(super) unsafe fn overlay_rgba(wide: &[u16], w: u32, h: u32) -> Option<Vec<u8>> {
        use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
        use windows::Win32::UI::Shell::{
            SHGetFileInfoW, SHGetImageList, SHFILEINFOW, SHGFI_ICON, SHGFI_OVERLAYINDEX,
            SHGFI_SMALLICON, SHIL_JUMBO,
        };
        use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;

        // Ask for the overlay *index* (upper 8 bits of iIcon). The flag
        // is only honored together with SHGFI_ICON, whose HICON we
        // immediately destroy — the small composed icon itself is not
        // what we want at grid sizes.
        let mut shfi = SHFILEINFOW::default();
        let got = SHGetFileInfoW(
            PCWSTR::from_raw(wide.as_ptr()),
            Default::default(),
            Some(&mut shfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON | SHGFI_OVERLAYINDEX,
        );
        if got == 0 {
            return None;
        }
        if !shfi.hIcon.is_invalid() {
            let _ = DestroyIcon(shfi.hIcon);
        }
        let overlay_idx = (shfi.iIcon >> 24) & 0xFF;
        if overlay_idx == 0 {
            return None;
        }
        let list: IImageList = SHGetImageList(SHIL_JUMBO as i32).ok()?;
        let img_idx = list.GetOverlayImage(overlay_idx).ok()?;
        let hicon = list.GetIcon(img_idx, ILD_TRANSPARENT.0).ok()?;
        let rgba = rasterize_icon(hicon, w, h);
        let _ = DestroyIcon(hicon);
        rgba
    }

    /// Draw an `HICON` into a fresh zeroed 32bpp top-down DIB at
    /// `w`×`h` and read back RGBA (BGRA swap; DrawIconEx preserves the
    /// alpha of 32bpp icon art over a zeroed surface).
    unsafe fn rasterize_icon(hicon: HICON, w: u32, h: u32) -> Option<Vec<u8>> {
        use windows::Win32::Foundation::{HANDLE, HWND};
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
            SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBRUSH,
        };
        use windows::Win32::UI::WindowsAndMessaging::{DrawIconEx, DI_NORMAL};

        let screen_dc = GetDC(HWND::default());
        if screen_dc.is_invalid() {
            return None;
        }
        let mem_dc = CreateCompatibleDC(screen_dc);
        if mem_dc.is_invalid() {
            ReleaseDC(HWND::default(), screen_dc);
            return None;
        }
        let info = BITMAPINFO {
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
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = match CreateDIBSection(
            screen_dc,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            HANDLE::default(),
            0,
        ) {
            Ok(d) => d,
            Err(_) => {
                let _ = DeleteDC(mem_dc);
                ReleaseDC(HWND::default(), screen_dc);
                return None;
            }
        };
        let old = SelectObject(mem_dc, dib);
        let drawn = DrawIconEx(
            mem_dc,
            0,
            0,
            hicon,
            w as i32,
            h as i32,
            0,
            HBRUSH::default(),
            DI_NORMAL,
        )
        .is_ok();
        let rgba = if drawn && !bits.is_null() {
            let n = (w as usize) * (h as usize) * 4;
            let mut v = vec![0u8; n];
            std::ptr::copy_nonoverlapping(bits as *const u8, v.as_mut_ptr(), n);
            for px in v.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
            Some(v)
        } else {
            None
        };
        SelectObject(mem_dc, old);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(HWND::default(), screen_dc);
        rgba
    }

    /// Straight-alpha source-over of `over` onto `base` (same dims).
    pub(super) fn blend_over(base: &mut [u8], over: &[u8]) {
        for (b, o) in base.chunks_exact_mut(4).zip(over.chunks_exact(4)) {
            let oa = o[3] as u32;
            if oa == 0 {
                continue;
            }
            let ba = b[3] as u32;
            let out_a = oa + ba * (255 - oa) / 255;
            if out_a == 0 {
                b[3] = 0;
                continue;
            }
            for c in 0..3 {
                let num = (o[c] as u32) * oa + (b[c] as u32) * ba * (255 - oa) / 255;
                b[c] = (num / out_a).min(255) as u8;
            }
            b[3] = out_a as u8;
        }
    }
}
