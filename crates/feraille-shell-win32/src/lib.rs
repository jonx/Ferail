//! Windows shell integration for Feraille.
//!
//! Mirrors the public surface of `feraille-shell-mac` so the
//! `platform_shell` cfg-alias in `feraille-gpui` can swap one for the
//! other based on target OS. Each function has a `cfg(windows)` arm
//! (today a stub; future home of real Win32 implementations ported
//! from `/Users/jkn/Source/Ferail/crates/ferail-win32/`) and a
//! `cfg(not(windows))` no-op arm so the workspace `cargo check` keeps
//! compiling this crate on macOS / Linux dev hosts.
//!
//! The winit-window-taking subset of shell-mac (`begin_drag`,
//! `show_context_menu`, `install_services_anchor`, `show_share_picker`,
//! `apply_native_chrome`) is intentionally **omitted**: those signatures
//! served the soft-renderer (`feraille-app`) stack and are not called
//! from `feraille-gpui`. If a future GPUI Windows surface needs them,
//! they grow back through the `windows` crate's HWND, not through
//! winit.
//!
//! Stub bodies match shell-mac's non-macOS arms (no-op / `false` /
//! `Vec::new()` / `Err("...stub")`) so the existing call sites' error
//! handling continues to apply unchanged.

// =============================================================
// Types — defined unconditionally so callers can name them on
// either platform; the shell-mac equivalents have the same shape.
// =============================================================

/// One app the system would offer in the Open With submenu for a
/// given file. Shape mirrored from `feraille-shell-mac::OpenWithCandidate`.
#[derive(Clone, Debug)]
pub struct OpenWithCandidate {
    pub name: String,
    pub path: std::path::PathBuf,
    pub is_default: bool,
}

/// Result of [`set_app_icon_from_png_bytes`], mirrored from shell-mac.
/// `NotMacOs` is retained as a variant name purely so the `Debug`
/// output matches shell-mac one-for-one — a future real Windows impl
/// will return `Ok` / `DecodeFailed` and stop hitting `NotMacOs`.
#[derive(Debug)]
pub enum SetIconResult {
    Ok,
    NotMacOs,
    NotMainThread,
    DecodeFailed,
}

// =============================================================
// App menu / About
// =============================================================

/// Install the application menu. macOS has a global menu bar
/// (`NSApp.mainMenu`); Windows has per-window `HMENU` (or a
/// hamburger). This stub no-ops until the Windows main-window menu
/// wiring lands.
pub fn install_app_menu(_app_name: &str, _tagline: &str, _version: &str, _copyright: &str) {}

/// Register the host-app callback for catalogued menu commands. No-op
/// stub on Windows — the catalogue dispatcher fires actions directly
/// through gpui's keymap until a Windows menu surface lands.
pub fn register_command_callback(
    _cb: Option<Box<dyn Fn(feraille_core::commands::CommandId) + 'static>>,
) {
}

/// Show the standard About panel. Windows has no equivalent
/// system-level About dialog; the eventual impl will build an
/// in-process gpui modal.
pub fn show_about_panel() {}

/// Configure About-panel content. No-op until [`show_about_panel`]
/// has a real implementation to read it.
pub fn set_about_options(_app_name: &str, _tagline: &str, _version: &str, _copyright: &str) {}

/// Update the menu's snapshot of the host's tab count.
pub fn set_tab_count(_n: usize) {}

/// Set a command's checkmark state in the menu.
pub fn set_command_state(_id: feraille_core::commands::CommandId, _on: bool) {}

// =============================================================
// Alerts / prompts / URLs
// =============================================================

/// Show a modal alert. macOS uses `NSAlert`; Windows: `MessageBoxW`
/// with the information icon and a single OK button. Synchronous —
/// blocks the calling thread while the dialog is up, matching the
/// shell-mac contract.
#[cfg(windows)]
pub fn show_alert(title: &str, body: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{
        MB_ICONINFORMATION, MB_OK, MessageBoxW,
    };
    let title_w = to_wide(title);
    let body_w = to_wide(body);
    unsafe {
        // hWnd=None -> system-modal dialog. When we have a real main
        // window we'll thread the HWND through so the dialog is owned
        // by the app window and gets the correct Z-order behavior.
        MessageBoxW(
            None,
            windows::core::PCWSTR(body_w.as_ptr()),
            windows::core::PCWSTR(title_w.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

#[cfg(not(windows))]
pub fn show_alert(_title: &str, _body: &str) {}

/// Show a modal text-input prompt. Windows has no single API for
/// this — the future impl will build a small gpui modal.
pub fn prompt_for_text(_title: &str, _body: &str, _default: &str) -> Option<String> {
    None
}

/// Open `url` in the default handler. macOS uses NSWorkspace; Linux
/// `xdg-open`. Windows: `ShellExecuteW`. Today: shell out to
/// `cmd /C start` which works on every Windows host without an
/// external crate dep.
#[cfg(windows)]
pub fn open_url(url: &str) {
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
}

#[cfg(not(windows))]
pub fn open_url(_url: &str) {}

// =============================================================
// Clipboard / reveal
// =============================================================

/// Place a string on the system clipboard. Windows: open the clipboard
/// (no owner HWND), empty it, allocate a moveable HGLOBAL holding the
/// UTF-16 text with terminator, hand it to `SetClipboardData` under
/// `CF_UNICODETEXT`, then close. Best-effort — failures are silent
/// (matches shell-mac's contract).
#[cfg(windows)]
pub fn copy_to_clipboard(text: &str) {
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GHND, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    /// RAII guard so every early-return path still pairs the
    /// `OpenClipboard` call with `CloseClipboard`.
    struct CloseGuard;
    impl Drop for CloseGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }

    // UTF-16 + null terminator. SetClipboardData transfers ownership
    // of the HGLOBAL to the system on success, so we don't free here.
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = wide.len() * std::mem::size_of::<u16>();

    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _guard = CloseGuard;
        let _ = EmptyClipboard();
        let Ok(handle) = GlobalAlloc(GHND, byte_len) else {
            return;
        };
        let ptr = GlobalLock(handle) as *mut u16;
        if ptr.is_null() {
            return;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        let _ = GlobalUnlock(handle);
        // HGLOBAL → HANDLE for SetClipboardData per CF_UNICODETEXT.
        let _ = SetClipboardData(
            CF_UNICODETEXT.0 as u32,
            windows::Win32::Foundation::HANDLE(handle.0),
        );
    }
}

#[cfg(not(windows))]
pub fn copy_to_clipboard(_text: &str) {}

/// Open the OS file browser with `path` selected. macOS: `open -R`.
/// Windows: `explorer.exe /select,<path>`.
#[cfg(windows)]
pub fn reveal_in_finder(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
}

#[cfg(not(windows))]
pub fn reveal_in_finder(_path: &std::path::Path) {}

// =============================================================
// File operations
// =============================================================

/// Duplicate `src` next to itself with Explorer's " - Copy" /
/// " - Copy (2)" naming. Files use `fs::copy`; directories use a
/// recursive copy walk (std has no recursive copy). Returns the
/// destination path on success.
///
/// Collision strategy: try `<stem> - Copy.<ext>` first, then
/// `<stem> - Copy (2).<ext>`, … up to 99 — matches what Explorer
/// surfaces in its UI. The caller is expected to run this on a
/// worker thread (large copies block).
pub fn duplicate_path(src: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let parent = src
        .parent()
        .ok_or_else(|| format!("no parent dir for {}", src.display()))?;
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("unsupported filename for {}", src.display()))?;
    let ext = src
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{s}"))
        .unwrap_or_default();

    // First attempt: "stem - Copy.ext". Subsequent: "stem - Copy (N).ext".
    for n in 1..=99 {
        let candidate = if n == 1 {
            parent.join(format!("{stem} - Copy{ext}"))
        } else {
            parent.join(format!("{stem} - Copy ({n}){ext}"))
        };
        if candidate.exists() {
            continue;
        }
        let metadata = std::fs::metadata(src).map_err(|e| e.to_string())?;
        if metadata.is_dir() {
            copy_dir_recursive(src, &candidate).map_err(|e| e.to_string())?;
        } else {
            std::fs::copy(src, &candidate).map_err(|e| e.to_string())?;
        }
        return Ok(candidate);
    }
    Err("duplicate_path: exhausted ' - Copy (1..99)' slots".into())
}

/// Recursive directory copy, used by `duplicate_path` because the
/// std doesn't ship one. Bails on the first I/O error.
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Make a shortcut (.lnk) pointing at `target`. Windows: future impl
/// uses `IShellLink` from the `windows` crate.
pub fn make_alias(_target: &std::path::Path) -> Result<std::path::PathBuf, String> {
    Err("make_alias: Windows stub".into())
}

/// Compress `targets` into a `.zip`. Windows: future impl uses the
/// `zip` crate or shells out to `tar.exe` (ships in Win10+).
pub fn compress_paths(_targets: &[&std::path::Path]) -> Result<std::path::PathBuf, String> {
    Err("compress_paths: Windows stub".into())
}

// =============================================================
// Quick Look — N/A on Windows (no system Quick Look)
// =============================================================

/// macOS Quick Look has no Windows equivalent. The eventual port will
/// either drop the feature or build an in-process preview window.
pub fn show_quick_look(_paths: &[&std::path::Path]) -> Result<(), String> {
    Err("show_quick_look: not available on Windows".into())
}

/// Generate a thumbnail. Windows: future impl uses
/// `IShellItemImageFactory::GetImage`.
pub fn fetch_quick_look_thumbnail(
    _path: &std::path::Path,
    _size_px: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    None
}

// =============================================================
// Finder Tags — no Windows equivalent
// =============================================================

/// Read canonical Finder colour tags. Windows has no system-wide tag
/// store; either drop the feature on Windows or back it with
/// `feraille-meta` (SQLite) only.
pub fn read_canonical_tags(_path: &std::path::Path) -> Vec<feraille_core::commands::TagColor> {
    Vec::new()
}

/// Toggle a Finder colour tag. See note above on `read_canonical_tags`.
pub fn toggle_tag(
    _path: &std::path::Path,
    _color: feraille_core::commands::TagColor,
) -> Result<(), String> {
    Err("toggle_tag: not available on Windows".into())
}

/// Strip every tag (including user-defined). See note above.
pub fn clear_tags(_path: &std::path::Path) -> Result<(), String> {
    Err("clear_tags: not available on Windows".into())
}

// =============================================================
// Open With — IAssocHandler is the rough Windows equivalent
// =============================================================

/// Enumerate Windows apps that would open `path`. Future impl uses
/// `IAssocHandler` via the `windows` crate.
pub fn open_with_candidates(_path: &std::path::Path) -> Vec<OpenWithCandidate> {
    Vec::new()
}

/// Open `target` with the app at `app_path`. Future impl uses
/// `ShellExecuteExW` with the `runas`/default verb.
pub fn open_with_app(
    _target: &std::path::Path,
    _app_path: &std::path::Path,
) -> Result<(), String> {
    Err("open_with_app: Windows stub".into())
}

// =============================================================
// App icon / theme
// =============================================================

/// Set the running process's icon. macOS sets `NSApp` icon. Windows
/// usually attaches an `.ico` via the manifest; runtime icon updates
/// go through `WM_SETICON`. The stub returns `NotMacOs` for now
/// (matches shell-mac's stub Debug output).
pub fn set_app_icon_from_png_bytes(_png_bytes: &[u8]) -> SetIconResult {
    SetIconResult::NotMacOs
}

/// `true` if Windows is in dark mode (per the "Apps" preference, not
/// the "Windows" preference, which matches what most native apps use).
/// Reads the DWORD at:
/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme`
/// which is `0` for dark, `1` for light. Missing key → defaults to
/// light (`false`). Matches the convention in Windows 10 1809+.
#[cfg(windows)]
pub fn system_is_dark() -> bool {
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW,
    };
    use windows::core::PCWSTR;

    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
        .encode_utf16()
        .collect();
    let value_name: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();
    let mut data: u32 = 0;
    let mut size: u32 = std::mem::size_of::<u32>() as u32;
    let res = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value_name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut std::ffi::c_void),
            Some(&mut size),
        )
    };
    if res.is_err() {
        return false;
    }
    // 0 = dark, 1 = light.
    data == 0
}

#[cfg(not(windows))]
pub fn system_is_dark() -> bool {
    false
}

/// Subscribe to system theme changes. Windows: post a hidden HWND
/// and listen for `WM_SETTINGCHANGE` with the `ImmersiveColorSet`
/// lParam. No-op stub for now — needs integration with gpui's
/// Windows event loop, which lands when the main window does.
pub fn start_system_theme_observer(_callback: Box<dyn Fn(bool) + 'static>) {}

// =============================================================
// Internal helpers
// =============================================================

/// Encode `s` as a null-terminated UTF-16 buffer for the Win32 W APIs.
#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
